//! The zone-channel auto-join (decision 0288 phase 6's remainder) — the client half of a
//! handshake the server deliberately does not perform.
//!
//! **Nobody joins these for you.** vmangos's `Player::UpdateLocalChannels`
//! (`src/game/Objects/Player.cpp:5121`) is an empty function whose entire body is the comment
//! `// Updated client-side`, so a client that never sends `CMSG_JOIN_CHANNEL` sits in no channel
//! at all — silently, because nothing on the wire complains. That is what benilla did until now:
//! `/join` worked, and General/Trade/LocalDefense simply never existed.
//!
//! The walk is [`ChatChannels.dbc`](benilla_formats::ChatChannelsCatalog)'s `INITIAL` rows
//! composed against the player's current **zone** and sent as ordinary joins. It re-runs whenever
//! the zone (or the in-a-capital answer) changes: the zone-dependent rows carry the zone name
//! *inside* the channel name, so crossing a border is genuinely leaving one channel and joining
//! another — LEAVE(old) then JOIN(new), interleaved per DBC row.
//!
//! The mechanism is byte-verified in wow-re's `system/ui/scratch/zone-chat-channel-autojoin.md`
//! (the whole thing is one function, `ZoneChannelRefresh 0x49a210`); each part below cites the
//! section it comes from.
//!
//! ## Why this is worth more than three channel names
//!
//! The server answers each join with `SMSG_CHANNEL_NOTIFY`/`YOU_JOINED`, which becomes a
//! `CHAT_MSG_CHANNEL_NOTICE` at the Lua VM (0288 §1's addon-API phase) — and that one event is the
//! condition for **Ace2's initialisation gate**. `AceEvent`'s `activate` registers
//! `CHAT_MSG_CHANNEL_NOTICE` and, on the first one, schedules the function that sets
//! `self.postInit = true` and fires `AceEvent_FullyInitialized`
//! (`AceEvent-2.0.lua:913-947`). Until that happens `AceEvent:IsFullyInitialized()` answers false
//! forever, with no error anywhere — and ~75 FuBar plugins, BigWigs, oRA2, RosterLib, Jostle and
//! AceComm all wait on it. `ui_chat::ace_gate_tests` is that claim, proved against the corpus's
//! own Ace2 chain.

use bevy::prelude::*;

use benilla_assets::LockRecover;
use benilla_formats::ChatChannelsCatalog;

use crate::area::AreaTableRes;
use crate::net::{ClientCommand, NetCommands};

use super::edit::ChannelState;

/// `AreaTable.dbc` flag `0x08` — vmangos calls it `AREA_FLAG_SLAVE_CAPITAL`, with the comment
/// *"Allow trade channel"* (`src/game/Database/DBCEnums.h:58`). In the shipped 5875 table exactly
/// six rows carry it, all top-level zones and all with flags `0x138`: Undercity, Stormwind City,
/// Ironforge, Orgrimmar, Thunder Bluff and Darnassus. (`0x100` = `AREA_FLAG_CAPITAL` selects the
/// identical six in this build; `0x08` is the bit named for this job, so it is the one read.)
const AREA_FLAG_TRADE_CHANNEL: u32 = 0x08;

/// `AreaTable.dbc` flag `0x200` — the sentinel marking the row whose name is the **shared city
/// word** the Trade channel is named after.
///
/// Not a geographic area at all: the client scans the table once at load for this bit and keeps
/// the row's name at `0xb4e4f0` (single writer `0x4985fd`), then splices it into `Trade - %s`
/// (wow-re `system/ui/scratch/zone-chat-channel-autojoin.md` §3, VERIFIED). Exactly one row in the
/// shipped 5875 table carries it — **id 3459, `AreaName[enUS] = "City"`** — which is the whole
/// reason the word appears nowhere in `WoW.exe`. Reading it here rather than hardcoding `"City"`
/// is what keeps a localized install localized.
const AREA_FLAG_CITY_NAME_ROW: u32 = 0x200;

/// The shared city word, scanned out of `AreaTable.dbc` — the client's own load-time scan
/// ([`AREA_FLAG_CITY_NAME_ROW`]). `None` when no row carries the sentinel, in which case the
/// city-named rows are skipped rather than joined under a half-formed name.
pub(crate) fn city_word(areas: &benilla_formats::AreaTableCatalog) -> Option<&str> {
    areas
        .rows()
        .find(|r| r.flags & AREA_FLAG_CITY_NAME_ROW != 0)
        .map(|r| r.name.as_str())
        .filter(|n| !n.is_empty())
}

/// What the walk currently believes it holds, and the state it composed that from.
///
/// Deliberately **not** [`ChannelState::joined`], which is server truth fed by YOU_JOINED /
/// YOU_LEFT notices. This is the *request* side: keying the diff off what we asked for is what
/// stops a join the server refuses (wrong faction, banned) from being re-sent every frame.
#[derive(Resource, Default)]
pub(crate) struct ZoneChannelWalk {
    /// The composed names last requested, in `ChatChannels.dbc` row order.
    held: Vec<String>,
    /// The `(zone name, in a capital)` the held set was composed for. `None` = never walked, or
    /// the session ended — either way the next in-world frame re-walks from scratch.
    at: Option<(String, bool)>,
    /// **Is there a character session to join channels for?** Armed by `EnteredWorldMessage`,
    /// disarmed by [`end_channel_session`] — a positive edge in both directions, and the reason
    /// this is a field rather than an ordering.
    ///
    /// Without it the walk is live on the *logout frame*: `ClientState` has not left `InWorld`
    /// yet, the avatar is still standing there settled, and the session-end clear has just emptied
    /// `at` — so the walk faithfully re-diffs from nothing and sends a JOIN for the zone the
    /// character is *leaving*. Caught live by the `/logout` + `Enter` probe (1284): two joins
    /// stamped at the same millisecond as "tearing down the streamed world".
    live: bool,
}

impl ZoneChannelWalk {
    /// Forget everything: leaving the world took our channel membership with it, so the next entry
    /// must re-join rather than assume. See [`end_channel_session`].
    fn clear_session(&mut self) {
        self.held.clear();
        self.at = None;
        self.live = false;
    }
}

/// **Channel membership dies with the character session, so this state must die with it too**
/// (1284).
///
/// Server-side that is not a policy but a destructor: `Player::CleanupChannels`
/// (vmangos `src/game/Objects/Player.cpp:5107`) walks every channel the player is in and leaves
/// it, and it is called from `~Player` and from the logout cleanup. A new character — or the same
/// character after a reconnect — enters the world in **zero** channels, always.
///
/// Ours used to survive that boundary, and the director caught it on a character switch: the walk
/// still `held` the previous character's `General - Tanaris`, so its first diff on the new
/// character sent LEAVE(Tanaris) — which the new session is genuinely not in, so the server
/// answered "Not on channel 1. General - Tanaris." — and [`ChannelState::joined`] still listed
/// those two dead rows, which is why the real joins came back numbered 3 and 4 instead of 1 and 2.
///
/// Three things end together because they are one fact: what we asked for ([`ZoneChannelWalk`]),
/// what the server confirmed ([`ChannelState::joined`]), and the VM's mirror of the latter (what
/// `GetChannelName` answers an addon). The edit box's channel target goes too — it holds the wire
/// name of a channel that no longer exists for us, and a `/2` typed on the new character would
/// otherwise send into it.
fn end_channel_session(
    script: Option<&mut benilla_ui::script::UiScript>,
    channels: &mut ChannelState,
    walk: &mut ZoneChannelWalk,
    edit: &mut super::edit::ChatEditState,
) {
    walk.clear_session();
    channels.joined.clear();
    edit.channel_target.clear();
    edit.channel_number = 0;
    if let Some(script) = script {
        script.set_joined_channels(Vec::new());
    }
}

/// The session-end edge: a confirmed `/logout` back to the glue layer (`OnExit(InWorld)`), which is
/// the character switch the director's screenshot caught.
pub(super) fn end_session_channels(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut channels: ResMut<ChannelState>,
    mut walk: ResMut<ZoneChannelWalk>,
    mut edit: ResMut<super::edit::ChatEditState>,
) {
    end_channel_session(
        script.map(NonSendMut::into_inner),
        &mut channels,
        &mut walk,
        &mut edit,
    );
}

/// The other end: a socket that died. A **recoverable** drop never leaves `InWorld` (0065 keeps the
/// avatar as the local puppet for the reconnect), so the edge above does not fire for it — but the
/// reconnect still builds a fresh `Player` server-side, with the same empty channel list a fresh
/// login has. Both edges therefore clear, and they clear the same four things through the same
/// function so neither can drift into being the more thorough one.
pub(super) fn end_session_channels_on_disconnect(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut channels: ResMut<ChannelState>,
    mut walk: ResMut<ZoneChannelWalk>,
    mut edit: ResMut<super::edit::ChatEditState>,
    mut disconnects: MessageReader<crate::net::DisconnectedMessage>,
) {
    if disconnects.read().next().is_none() {
        return;
    }
    end_channel_session(
        script.map(NonSendMut::into_inner),
        &mut channels,
        &mut walk,
        &mut edit,
    );
}

/// Startup: read `ChatChannels.dbc` into [`ChannelState`], which owns it because the two things
/// that need it — composing the auto-join names and answering a chat event's arg7 — are both its
/// business. Absent install ⇒ an empty catalog: no auto-join, arg7 stays 0, nothing errors.
pub(super) fn load_chat_channels(
    mut channels: ResMut<ChannelState>,
    assets: Option<Res<benilla_assets::WorldAssets>>,
) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_chat_channels_catalog(&mut chain)
    };
    match loaded {
        Ok(cat) => {
            info!("chat: {} ChatChannels rows", cat.rows().len());
            channels.channels = cat;
        }
        Err(e) => warn!("chat: ChatChannels.dbc failed to load — no zone channels: {e:#}"),
    }
}

/// The names the auto-join rows compose to for a player standing in `zone_name`, in table order.
///
/// `in_city` gates the city-**only** rows (the `0x10` bit); `city_word` is what a city-**named**
/// row (`0x20`) puts in its `%s`, and an absent one skips those rows rather than composing
/// `"Trade - "`. The client has no such guard — its capital arm null-checks the DBC row pointer
/// but not the string, so a locale with that row's name blank would compose exactly that
/// (wow-re `zone-chat-channel-autojoin.md`, branch caveat). We decline instead: joining the wrong
/// channel name is invisible and permanent, and joining none is neither.
pub(crate) fn wanted_channels(
    catalog: &ChatChannelsCatalog,
    zone_name: &str,
    in_city: bool,
    city_word: Option<&str>,
) -> Vec<String> {
    catalog
        .auto_join_rows()
        .filter(|r| in_city || !r.is_city_only())
        .filter(|r| !r.takes_city_name() || city_word.is_some())
        // A zone-dependent name with no zone to put in it would join a channel called
        // "General - " — join nothing rather than something wrong.
        .filter(|r| !r.is_zone_dependent() || !zone_name.is_empty())
        .map(|r| r.joinable_name(zone_name, city_word.unwrap_or_default()))
        .collect()
}

/// Is the zone under the player the one they are actually standing in?
///
/// The walk's gate (1280). Three states answer no, and each one cost real wire traffic before this
/// existed: no avatar at all, an avatar not yet active, and — the expensive one — an avatar still
/// **settling**, which is benilla's stand-in for the reference's loading screen: the body has been
/// snapped but the destination's terrain and WMOs are still arriving, so the leaf area under it is
/// still moving. The reference walks *after* that window, always; we polled inside it.
fn zone_is_settled(player: Option<&crate::player::Player>) -> bool {
    player.is_some_and(|p| p.active && !p.settling)
}

/// The walk: re-join the zone channels whenever the zone changes, and drop the ones that no
/// longer apply.
///
/// Reads the zone the same way [`crate::area`]'s splash does — the leaf `AreaTable` id under the
/// player, walked to its top-level parent — but takes the **real** zone name rather than the
/// display text. That is not a preference: the client composes from `GetRealZoneText`'s own cache
/// (`0xb4b404`, written from the parent zone's `AreaName_lang`), and the indoor/WMO name override
/// `0x67e670` rewrites only the slot feeding `GetZoneText`, never the one that becomes a channel
/// name (wow-re `zone-chat-channel-autojoin.md` §3). So a building never renames your channel.
///
/// **Timing:** the client re-walks inside `UpdateZoneText 0x494780` on every zone change (`§1`,
/// callsite `0x494931`), immediately *before* it fires `ZONE_CHANGED_NEW_AREA`, plus once at world
/// entry. This polls the same two inputs each frame and early-outs when they have not moved, which
/// reaches the same states; it is a poll rather than a hook because the zone is already derived
/// here, not published as an event payload.
///
/// **…but NOT while the world is still arriving** ([`Player::settling`], 1280). The reference's
/// "world entry" is behind a loading screen: by the time it walks, the destination ADT and its WMOs
/// are resident and the zone under the player is final. Ours streams asynchronously, so the leaf
/// area moves *twice* on the way in — first while the body still holds a pre-snap position (the
/// map centre: tile 32,32 is Eastern Plaguelands on map 0, The Barrens on map 1 — the two bogus
/// zones in the director's login screenshot), then again when the WMO interior claim lands a beat
/// after the outdoor MCNK area (Tanaris → Caverns of Time, reproduced live). Each transient cost a
/// real JOIN and a real LEAVE on the wire, for zones the player was never in — chat lines and all.
/// `settling` is exactly the loading screen's own gate (released by the terrain streamer once the
/// destination is resident, decision 0737), which makes this the reference's timing rather than an
/// arbitrary debounce.
///
/// **Selection:** the client's live predicate is the *saved* `ZONECHANNELS` mask, not `flags & 1`
/// read fresh — but that mask is **seeded from `flags & 1` when there is no usable chat-cache
/// file** (`0x4997fc`, gated `0x4997e8`). We persist no chat cache, so every session is that
/// fresh-character path and reading the DBC bit directly is exactly right. It stops being right
/// the day per-window channel settings are persisted.
pub(super) fn auto_join_zone_channels(
    commands: Res<NetCommands>,
    channels: Res<ChannelState>,
    areas: Option<Res<AreaTableRes>>,
    world: benilla_world::world_point::WorldPoint,
    player: Option<Res<crate::player::Player>>,
    mut walk: ResMut<ZoneChannelWalk>,
    mut entered: MessageReader<crate::net::EnteredWorldMessage>,
) {
    // World entry arms the walk; the session-end clears disarm it ([`end_session_channels`] and its
    // disconnect twin, which own the whole fact — walk, confirmed list, VM mirror, edit target).
    if entered.read().next().is_some() {
        walk.live = true;
    }
    if !walk.live {
        return; // no character session — see `ZoneChannelWalk::live`
    }
    let (Some(areas), false) = (areas, channels.channels.is_empty()) else {
        return;
    };
    // Not until the world under the player is the one they are actually standing in — see the
    // "Timing" note above.
    if !zone_is_settled(player.as_deref()) {
        return;
    }
    // The zone under the player, or nothing to do yet (tiles not streamed, no body).
    let Some(zone_row) = world
        .area()
        .and_then(|leaf| areas.0.top_zone(leaf))
        .and_then(|zone| areas.0.get(zone))
    else {
        return;
    };
    let at = (
        zone_row.name.clone(),
        zone_row.flags & AREA_FLAG_TRADE_CHANNEL != 0,
    );
    if walk.at.as_ref() == Some(&at) {
        return; // same zone as last frame — the common case, and free
    }

    let wanted = wanted_channels(&channels.channels, &at.0, at.1, city_word(&areas.0));

    // The diff is **per DBC row**, not per name — because a zone change does not add and remove
    // channels, it *renames* one: `General - Felwood` and `General - Winterspring` are the same
    // row (ChannelID 1) with a different `%s`. Keying on the row is what makes that a leave/join
    // pair instead of an unrelated drop and add.
    let row_of = |name: &str| channels.channels.zone_channel_id(name);
    let leave = |name: &str| {
        debug!("chat: leaving zone channel {name:?}");
        let _ = commands.0.send(ClientCommand::LeaveChannel {
            name: name.to_string(),
        });
    };
    let join = |name: &str| {
        debug!("chat: joining zone channel {name:?}");
        let _ = commands.0.send(ClientCommand::JoinChannel {
            name: name.to_string(),
            password: String::new(),
        });
    };

    // LEAVE(old) immediately followed by JOIN(new), per row, interleaved — the client does both in
    // one iteration of its own walk (`0x49a367` sits earlier in the loop body than the join), and a
    // retail sniff shows the pairs adjacent on the wire in exactly that order: LEAVE
    // `General - Felwood` → JOIN `General - Winterspring` → LEAVE `LocalDefense - Felwood` → …
    // (wow-re `zone-chat-channel-autojoin.md` §5).
    for name in &wanted {
        match walk.held.iter().find(|h| row_of(h) == row_of(name)) {
            Some(old) if old == name => continue, // this row's name did not move
            Some(old) => leave(old),
            None => {}
        }
        join(name);
    }
    // Rows that stopped applying entirely — walking out of a capital drops Trade with nothing to
    // replace it.
    for old in walk
        .held
        .iter()
        .filter(|h| !wanted.iter().any(|w| row_of(w) == row_of(h)))
    {
        leave(old);
    }

    walk.held = wanted;
    walk.at = Some(at);
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_formats::ChatChannelRow;

    /// The shipped table, hand-built so this runs without an install (the real rows are asserted
    /// against the DBC in `benilla_formats::chat_channels`).
    fn catalog() -> ChatChannelsCatalog {
        ChatChannelsCatalog::from_rows(
            [
                (1, 0x00003, "General - %s", "General"),
                (2, 0x0003B, "Trade - %s", "Trade"),
                (22, 0x10003, "LocalDefense - %s", "LocalDefense"),
                (23, 0x10004, "WorldDefense", "WorldDefense"),
                (24, 0x00000, "LookingForGroup", "LookingForGroup"),
                (25, 0x20032, "GuildRecruitment - %s", "GuildRecruitment"),
            ]
            .into_iter()
            .map(|(id, flags, pattern, shortcut)| ChatChannelRow {
                id,
                flags,
                pattern: pattern.into(),
                shortcut: shortcut.into(),
            })
            .collect(),
        )
    }

    /// The city word as the shipped `AreaTable.dbc` supplies it (row 3459, the `Flags & 0x200`
    /// sentinel — see [`city_word`]).
    const CITY: Option<&str> = Some("City");

    /// Out in the world: General and LocalDefense, both zone-named. Trade is a city channel and
    /// stays out; WorldDefense, LookingForGroup and GuildRecruitment carry no INITIAL bit.
    #[test]
    fn an_ordinary_zone_joins_general_and_local_defense() {
        assert_eq!(
            wanted_channels(&catalog(), "Elwynn Forest", false, CITY),
            vec!["General - Elwynn Forest", "LocalDefense - Elwynn Forest"]
        );
    }

    /// Inside a capital, Trade joins too — under the one shared name, not the city's own.
    #[test]
    fn a_capital_adds_the_shared_trade_channel() {
        assert_eq!(
            wanted_channels(&catalog(), "Stormwind City", true, CITY),
            vec![
                "General - Stormwind City",
                "Trade - City",
                "LocalDefense - Stormwind City",
            ]
        );
    }

    /// No zone name ⇒ no zone-dependent join. "General - " is a real channel on the server, and
    /// joining it would be a bug nothing else would ever report.
    #[test]
    fn an_unknown_zone_joins_nothing_zone_dependent() {
        assert!(wanted_channels(&catalog(), "", false, CITY).is_empty());
        assert!(wanted_channels(&catalog(), "", true, CITY).is_empty());
    }

    /// **The walk does not believe the zone until the world has settled** (1280).
    ///
    /// The director's login screenshot is what this guards: three zones' channels joined on the
    /// way in — two of them the map centre (tile 32,32 = Eastern Plaguelands on map 0, The Barrens
    /// on map 1), read while the body still sat at its pre-snap position — and then leave requests
    /// for zones they had never been in, which the server answered "Not on channel …". Every one
    /// of those lines existed because the walk ran a frame too early.
    #[test]
    fn the_walk_waits_for_the_world_under_the_player() {
        use crate::player::Player;

        assert!(!zone_is_settled(None), "no avatar: no zone to believe");

        let mut p = Player::default();
        assert!(!zone_is_settled(Some(&p)), "inactive avatar");

        p.active = true;
        p.settling = true;
        assert!(
            !zone_is_settled(Some(&p)),
            "settling — the destination's terrain and WMOs are still arriving, so the leaf area \
             under the body is still moving"
        );

        p.settling = false;
        assert!(zone_is_settled(Some(&p)), "settled: the zone is now final");
    }

    /// **Leaving the world ends the channel session — all four halves of it** (1284).
    ///
    /// The director's character switch: the walk still held the previous character's
    /// `General - Tanaris`, so its first diff on the new character sent a LEAVE the new session
    /// answered "Not on channel 1. General - Tanaris.", and the stale confirmed list pushed the
    /// real joins to slots 3 and 4. Server-side there is nothing to keep — `Player::CleanupChannels`
    /// runs in `~Player` — so every one of these must be empty at the next world entry.
    #[test]
    fn leaving_the_world_ends_the_channel_session() {
        let mut channels = ChannelState::default();
        let mut walk = ZoneChannelWalk::default();
        let mut edit = super::super::edit::ChatEditState::default();

        // A live session: two zone channels asked for, both confirmed, `/2` targeted.
        walk.held = vec!["General - Tanaris".into(), "LocalDefense - Tanaris".into()];
        walk.at = Some(("Tanaris".into(), false));
        channels.joined = walk.held.clone();
        edit.channel_target = "LocalDefense - Tanaris".into();
        edit.channel_number = 2;

        // No VM in a unit test; the mirror leg is the one line this cannot reach, and
        // `end_session_channels` is a two-line wrapper over exactly this call.
        end_channel_session(None, &mut channels, &mut walk, &mut edit);

        assert!(walk.held.is_empty(), "nothing is held");
        assert_eq!(walk.at, None, "and the next entry re-walks from scratch");
        assert!(
            channels.joined.is_empty(),
            "the confirmed list is the server's, and the server just destroyed it — a survivor \
             here is what renumbers the next character's channels"
        );
        assert_eq!(edit.channel_target, "");
        assert_eq!(edit.channel_number, 0, "a `/2` now targets nothing");
    }

    /// No city word (a locale whose sentinel row ships blank) ⇒ the city-named rows are skipped,
    /// not composed as `"Trade - "`. The client has no such guard; declining is ours.
    #[test]
    fn a_missing_city_word_skips_the_trade_channel_rather_than_half_naming_it() {
        assert_eq!(
            wanted_channels(&catalog(), "Stormwind City", true, None),
            vec!["General - Stormwind City", "LocalDefense - Stormwind City"]
        );
    }

    /// A zone change **renames** a row rather than adding one: both zones' General lines carry
    /// ChannelID 1, which is what makes the walk's diff a leave/join pair on one row instead of an
    /// unrelated drop and add.
    #[test]
    fn crossing_a_zone_border_renames_the_same_rows() {
        let cat = catalog();
        let felwood = wanted_channels(&cat, "Felwood", false, CITY);
        let winterspring = wanted_channels(&cat, "Winterspring", false, CITY);
        assert_ne!(felwood, winterspring, "the names differ");
        let ids = |v: &[String]| -> Vec<u32> { v.iter().map(|n| cat.zone_channel_id(n)).collect() };
        assert_eq!(
            ids(&felwood),
            ids(&winterspring),
            "…but they are the same DBC rows: [1, 22] either side of the border"
        );
        assert_eq!(ids(&felwood), vec![1, 22]);
    }

    /// arg7: the composed names resolve back to their `ChatChannels.dbc` id, custom ones to 0 —
    /// matched by the server's own substring rule.
    #[test]
    fn composed_names_carry_their_channel_id_back() {
        let cat = catalog();
        assert_eq!(cat.zone_channel_id("General - Elwynn Forest"), 1);
        assert_eq!(cat.zone_channel_id("Trade - City"), 2);
        assert_eq!(cat.zone_channel_id("LocalDefense - Durotar"), 22);
        assert_eq!(cat.zone_channel_id("World"), 0);
    }
}
