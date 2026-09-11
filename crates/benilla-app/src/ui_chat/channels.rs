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

use super::edit::{ChannelState, SlotState};

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
) {
    walk.clear_session();
    channels.joined.clear();
    if let Some(script) = script {
        script.set_joined_channels(Vec::new());
    }
}

/// Seed a fresh VM's joined-channel mirror (decision 1291). The mirror is otherwise pushed only
/// on the join/leave edges ([`super::feed`]'s YOU_JOINED / YOU_LEFT arms), and a `/reload`
/// replaces the VM *between* edges — `ChannelState` (server truth) survives, but the new VM's
/// mirror would stay empty: `GetChannelName`/`GetChannelList` answer nothing, `/1`-`/9` routing
/// is dead, and every channel line renders unnumbered until the player happens to join or leave
/// something. A login goes through [`end_session_channels`] + the auto-join walk instead, where
/// this claim pushes the just-cleared (empty) list — a no-op by construction.
pub(super) fn seed_channels(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    channels: Res<ChannelState>,
    mut seeded: Local<crate::ui_script::VmMemo<bool>>,
) {
    let Some(mut script) = script else {
        return;
    };
    if seeded.claim(&script) {
        script.set_joined_channels(channels.names());
    }
}

/// The session-end edge: a confirmed `/logout` back to the glue layer (`OnExit(InWorld)`), which is
/// the character switch the director's screenshot caught.
pub(super) fn end_session_channels(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut channels: ResMut<ChannelState>,
    mut walk: ResMut<ZoneChannelWalk>,
) {
    end_channel_session(script.map(NonSendMut::into_inner), &mut channels, &mut walk);
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
    mut disconnects: MessageReader<crate::net::DisconnectedMessage>,
) {
    if disconnects.read().next().is_none() {
        return;
    }
    end_channel_session(script.map(NonSendMut::into_inner), &mut channels, &mut walk);
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
    composed_auto_rows(catalog, zone_name, city_word)
        .filter(|(r, _)| in_city || !r.is_city_only())
        .map(|(_, name)| name)
        .collect()
}

/// **Every auto-join row that COMPOSES for this zone, city gate deliberately not applied** —
/// the set that takes slots (decision 2137).
///
/// The reference registers the slot (`0x49b980`) *before* it asks whether the row is eligible:
/// `0x49a50d` precedes the city gate at `0x49a512` (wow-re `zone-chat-channel-autojoin.md` §7 pass
/// 2, §11). So a character standing outside a capital still holds a `Trade - City` slot — created,
/// never joined, state 3 — and it still occupies **number 2**, because the `N.` a channel line
/// carries is `slot[+0x00]`, the client-local index, never the `ChatChannels.dbc` ChannelID.
///
/// [`wanted_channels`] is the subset the gate then lets us actually join. Filtering the city rows
/// out *before* they could take a number is what made our `/2` reach LocalDefense where the
/// reference's reaches Trade.
pub(crate) fn tracked_channels(
    catalog: &ChatChannelsCatalog,
    zone_name: &str,
    city_word: Option<&str>,
) -> Vec<String> {
    composed_auto_rows(catalog, zone_name, city_word)
        .map(|(_, name)| name)
        .collect()
}

/// The shared half: the auto-join rows whose name can actually be composed here, in table order.
/// Both callers derive from this so the resolvability rules cannot drift apart — the city gate is
/// the *only* thing that separates them.
fn composed_auto_rows<'a>(
    catalog: &'a ChatChannelsCatalog,
    zone_name: &'a str,
    city_word: Option<&'a str>,
) -> impl Iterator<Item = (&'a benilla_formats::ChatChannelRow, String)> + 'a {
    catalog
        .auto_join_rows()
        .filter(move |r| !r.takes_city_name() || city_word.is_some())
        // A zone-dependent name with no zone to put in it would join a channel called
        // "General - " — join nothing rather than something wrong.
        .filter(move |r| !r.is_zone_dependent() || !zone_name.is_empty())
        .map(move |r| (r, r.joinable_name(zone_name, city_word.unwrap_or_default())))
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
/// file** (`0x4997fc`, gated `0x4997e8`). We now persist that file and seat the mask from it
/// ([`super::edit::ChannelState::zone_mask`], decision 2120), but the walk still reads the DBC bit
/// directly: making the mask the join predicate means a character whose mask is 0 joins nothing at
/// all, which 2120 deliberately left for its own change once the repair had swept through. Named
/// here rather than left implicit — it is the one place this file diverges from the reference on
/// purpose.
/// Every `ChatChannels.dbc` row as the VM needs it (decision 1908; wow-re chat-cache-grammar.md
/// §5-6): the id, the Shortcut the verbs compare a typed name against, the name composed for
/// `zone_name` — `None` when the composition has nothing to substitute (a zone-dependent row with
/// no zone, a city row with no city word), which is the verbs' nil leg — and whether
/// `EnumerateServerChannels` lists it here (a city-only row, `flags & 0x10`, only in a city).
pub(crate) fn zone_channel_catalog(
    catalog: &ChatChannelsCatalog,
    zone_name: &str,
    in_city: bool,
    city_word: Option<&str>,
) -> Vec<benilla_ui::script::ZoneChannelRow> {
    catalog
        .rows()
        .iter()
        .map(|r| {
            let resolvable = (!r.is_zone_dependent() || !zone_name.is_empty())
                && (!r.takes_city_name() || city_word.is_some());
            benilla_ui::script::ZoneChannelRow {
                id: r.id,
                shortcut: r.shortcut.clone(),
                resolved: resolvable
                    .then(|| r.joinable_name(zone_name, city_word.unwrap_or_default())),
                listed: !r.is_city_only() || in_city,
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)] // a Bevy system's param list IS its dependency set
pub(super) fn auto_join_zone_channels(
    commands: Res<NetCommands>,
    mut channels: ResMut<ChannelState>,
    areas: Option<Res<AreaTableRes>>,
    world: benilla_world::world_point::WorldPoint,
    // One param (clippy's argument ceiling), and they belong together: both answer "is the zone
    // under the player the one this walk may act on?" — the body's own settle, and whether the
    // view has flown off it for a cinematic.
    body: (
        Option<Res<crate::player::Player>>,
        Option<Res<crate::cinematic::Cinematic>>,
    ),
    mut walk: ResMut<ZoneChannelWalk>,
    mut entered: MessageReader<crate::net::EnteredWorldMessage>,
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut catalog_fed: Local<crate::ui_script::VmMemo<Option<(String, bool)>>>,
) {
    let (player, cinematic) = (&body.0, &body.1);
    // World entry arms the walk; the session-end clears disarm it ([`end_session_channels`] and its
    // disconnect twin, which own the whole fact — walk, confirmed list, VM mirror, edit target).
    if entered.read().next().is_some() {
        walk.live = true;
    }

    // **A cinematic suppresses the rejoin, and it resumes when the shot ends.** Two of the ten
    // sites that read the reference's cinematic-state cell exist for exactly this — `0x49491e`
    // (the zone-text update) and `0x5ff566` (a `UPDATEFLAGS` reflex) both skip
    // `ZoneChannelRefresh` (`0x49a210`) while one runs, and `EndCinematic` calls it once at
    // `0x48f1d0` (wow-re `ui/scratch/cinematic-camera-law.md` §3.3, the complete 10-site census).
    //
    // The walk stays armed and only its *zone* goes unknown, so "rejoin once at the end" is what
    // the first frame after the shot already does — there is nothing to re-arm. It matters because
    // a race intro flies the streaming focus hundreds of yards off the body, so `world.area()`
    // changes under a player who has not moved, and the joins would otherwise fire against zones
    // they are only *looking* at.
    let cinematic_running = cinematic.as_deref().is_some_and(|c| c.is_playing());
    let areas = areas.as_deref();

    // **The zone the walk may act on** — `None` whenever any gate says the leaf under the player
    // is not the one they are standing in: no character session yet ([`ZoneChannelWalk::live`]), a
    // cinematic flying the streaming focus off the body, a world still arriving ([`zone_is_settled`]),
    // or an area authority with no answer for this session (decision 2130 made that a real state
    // again rather than the previous character's zone).
    let zone = (walk.live && !cinematic_running && zone_is_settled(player.as_deref()))
        .then(|| {
            let areas = areas?;
            let row = world
                .area()
                .and_then(|leaf| areas.0.top_zone(leaf))
                .and_then(|zone| areas.0.get(zone))?;
            Some((row.name.clone(), row.flags & AREA_FLAG_TRADE_CHANNEL != 0))
        })
        .flatten();

    // **The VM's zone-channel catalog is a `ChatChannels.dbc` fact, and it is fed BEFORE any of
    // those gates** — because the thing the verbs read out of it is the row **Shortcut**, which no
    // zone ever changes.
    //
    // It used to be fed only once a zone was known, and an empty catalog is not "no zone yet" to
    // any of its three readers — it is *"no such built-in channel"*. `AddChatWindowChannel(1,
    // "General")` took the no-match leg and stored `("General", 0)` as a **custom** channel, which
    // the chat cache then wrote out as a name in the window's `CHANNELS` block with its zone bit
    // dropped from `ZONECHANNELS`; the director's own `Onewarrior` file carries exactly that
    // damage, and a window that has lost a zone channel's id drops every line the stock
    // `ChatFrame_OnEvent` matches by id (ref `ChatFrame.lua` l.1379). `JoinChannelByName("General")`
    // was worse: `(0, nil)` is the custom-channel leg, so it **sent `CMSG_JOIN_CHANNEL("General")`**
    // — a real custom channel of that name on the server.
    //
    // Fed zone-less, every zone-dependent row carries `resolved: None`, which is the reference's
    // own "matched a row but there is no zone text yet" leg: `AddChatWindowChannel` stores nothing
    // and answers nil, `JoinChannelByName` returns nil and sends nothing (chat-cache-grammar.md §5,
    // decision 1908). Right answer instead of a wrong one, and the walk re-feeds the moment a zone
    // lands.
    let mut script = script;
    if !channels.channels.is_empty() {
        if let Some(script) = script.as_mut() {
            let at = zone.clone().unwrap_or_default();
            let fed = catalog_fed.get(script);
            if fed.as_ref() != Some(&at) {
                script.set_zone_channel_catalog(zone_channel_catalog(
                    &channels.channels,
                    &at.0,
                    at.1,
                    areas.and_then(|a| city_word(&a.0)),
                ));
                *fed = Some(at);
            }
        }
    }

    let (Some(at), Some(areas)) = (zone, areas) else {
        return; // nothing to walk against yet
    };
    if walk.at.as_ref() == Some(&at) {
        return; // same zone as last frame — the common case, and free
    }

    let wanted = wanted_channels(&channels.channels, &at.0, at.1, city_word(&areas.0));
    // Every row that composes here — the set that takes SLOTS, city gate not applied
    // ([`tracked_channels`], decision 2137). `wanted` is the subset we may actually join.
    let tracked = tracked_channels(&channels.channels, &at.0, city_word(&areas.0));

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
    //
    // The renames are collected rather than applied inside the loop only because `row_of` borrows
    // the catalog that lives beside the slots; the order on the wire is unchanged.
    //
    // **The walk is over `tracked`, not `wanted`** (decision 2137): every composable row gets a
    // slot, and the city gate then decides join-versus-suspend. Registering first is what gives
    // `Trade - City` number 2 on a character who is nowhere near a city.
    let eligible = |name: &String| wanted.iter().any(|w| w == name);
    let mut renamed: Vec<(String, String)> = Vec::new();
    let mut registered: Vec<String> = Vec::new();
    let mut suspended: Vec<String> = Vec::new();
    let mut bypassed: Vec<String> = Vec::new();
    for name in &tracked {
        match walk.held.iter().find(|h| row_of(h) == row_of(name)) {
            // This row's name did not move. Normally nothing to do — **except** that a
            // locally-suspended slot bypasses the name comparison entirely (`0x49a31c`, §7 pass 1
            // step 2), which is exactly how walking back into a city re-joins `Trade - City` under
            // a name that never changed.
            Some(old) if old == name => {
                if eligible(name) {
                    if channels.slot_state(name) == Some(SlotState::Suspended) {
                        join(name);
                        // **And the slot leaves state 3 at SEND time, not when the notice lands.**
                        // The reference's rename runs on the bypass leg too, with the name
                        // unchanged, and writes `(old == 3) ? 1 : 2` — so a bypassed slot goes to
                        // **1**, not 2: a plain `YOU_JOINED` when it confirms, never
                        // `YOU_CHANGED`. Our `Joined` is that state 1 (see [`SlotState`]).
                        //
                        // Leaving it at 3 until the notice arrived would be wrong twice: a second
                        // zone change inside that window would take this arm again and send a
                        // duplicate join, and walking straight back out would find a slot that is
                        // not `Joined` and so skip the LEAVE for a channel we really had joined.
                        bypassed.push(name.clone());
                    }
                } else if channels.slot_state(name) == Some(SlotState::Joined) {
                    // Walking OUT of the city: the LEAVE goes out here, before the eligibility
                    // test suspends the slot — "the LEAVE still goes out, because it happens
                    // earlier in the same iteration" (§5).
                    leave(name);
                    suspended.push(name.clone());
                }
                continue;
            }
            // The name moved. LEAVE(old) only from a server-confirmed slot (`0x49a35e`), then the
            // rename, then the gate.
            Some(old) => {
                if channels.slot_state(old) == Some(SlotState::Joined) {
                    leave(old);
                }
                renamed.push((old.clone(), name.clone()));
            }
            // Never tracked: the slot is claimed BEFORE the gate.
            None => registered.push(name.clone()),
        }
        if eligible(name) {
            join(name);
        } else {
            suspended.push(name.clone());
        }
    }
    // Rows that stopped COMPOSING entirely — a zone with no name at all, which drops the row from
    // `tracked` rather than merely making it ineligible. The slot is kept and suspended, never
    // freed, for the same reason as everything above.
    for old in walk
        .held
        .iter()
        .filter(|h| !tracked.iter().any(|t| row_of(t) == row_of(h)))
    {
        if channels.slot_state(old) == Some(SlotState::Joined) {
            leave(old);
        }
        suspended.push(old.clone());
    }

    // **The slot is renamed in place, at send time** — [`ChannelState::rename_slot`] carries the
    // whole why. In one line: the server's `YOU_LEFT` for the old name must find no slot, or the
    // stock `ChatFrame_OnEvent` deletes the window's registration for that channel and never
    // rebuilds it.
    for (old, new) in &renamed {
        if let Some(n) = channels.rename_slot(old, new) {
            debug!("chat: zone channel slot {n} renamed {old:?} → {new:?}");
        }
    }
    // **A row we have never tracked takes its slot here — before the gate decided anything**
    // (`0x49b980` at `0x49a50d`, ahead of the city gate at `0x49a512`). In DBC row order, which is
    // what makes the numbering `1 General`, `2 Trade`, `3 LocalDefense` whether or not Trade is
    // joinable from where the player is standing.
    // The state-3 bypass's own write, before the suspends below so the two can never fight over
    // one slot in a single pass.
    for name in &bypassed {
        channels.confirm_slot(name);
    }
    for name in &registered {
        match channels.claim_slot(name) {
            Some(n) => debug!("chat: zone channel {name:?} registered as slot {n}"),
            None => warn!(
                "chat: no free slot for zone channel {name:?} — all {} are taken",
                super::edit::MAX_CHANNELS
            ),
        }
    }
    // **…and a row that stopped applying is SUSPENDED, not freed** (`0x49bcf0`, state 3). The slot
    // and its number survive, so the `YOU_LEFT` that comes back carries the `SUSPENDED` token and
    // the window keeps its registration — otherwise walking out of Stormwind kills Trade for the
    // session exactly the way a border crossing used to kill General.
    for old in &suspended {
        if let Some(n) = channels.suspend_slot(old) {
            debug!("chat: zone channel slot {n} ({old:?}) suspended — out of its city");
        }
    }
    if !(renamed.is_empty() && registered.is_empty()) {
        if let Some(script) = script.as_mut() {
            // `GetChannelName(n)` answers the new name from this frame on, as it does in the
            // reference — the slot moved, not the numbering.
            script.set_joined_channels(channels.names());
        }
    }

    // `held` tracks every COMPOSED row, not only the joined ones — that is what makes the
    // "name unchanged, eligibility changed" arms above reachable at all.
    walk.held = tracked;
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
        // A live session: two zone channels asked for, both confirmed.
        let mut walk = ZoneChannelWalk {
            held: vec!["General - Tanaris".into(), "LocalDefense - Tanaris".into()],
            at: Some(("Tanaris".into(), false)),
            ..Default::default()
        };
        channels.joined = walk
            .held
            .iter()
            .map(|n| Some(crate::ui_chat::edit::ChannelSlot::joined(n)))
            .collect();

        // No VM in a unit test; the mirror leg is the one line this cannot reach, and
        // `end_session_channels` is a two-line wrapper over exactly this call.
        end_channel_session(None, &mut channels, &mut walk);

        assert!(walk.held.is_empty(), "nothing is held");
        assert_eq!(walk.at, None, "and the next entry re-walks from scratch");
        assert!(
            channels.joined.is_empty(),
            "the confirmed list is the server's, and the server just destroyed it — a survivor \
             here is what renumbers the next character's channels"
        );
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

    /// **The slot number never moves across the whole city round trip** (decision 2137).
    ///
    /// The exact state sequence the walk drives for `Trade - City`: registered and joined on the
    /// way in, suspended on the way out (record kept, `0x49bcf0`), and put back to *joined at send
    /// time* on the way back in — the state-3 bypass, where the reference's rename writes
    /// `(old == 3) ? 1 : 2` and so lands on **1**, not 2. Our `Joined` is that state 1, which is
    /// why the re-join confirms as a plain `YOU_JOINED` rather than `Changed Channel`.
    ///
    /// Leaving the slot at `Suspended` until the notice arrived would break two things: a second
    /// zone change inside that window would send a duplicate join, and walking straight back out
    /// would skip the LEAVE for a channel we had really joined.
    ///
    /// **What this does NOT cover:** that the walk actually calls `confirm_slot` on the bypass leg.
    /// The state machine is asserted here; the wiring is not, because the walk is a Bevy system
    /// with no harness yet. That harness is the obvious next instrument for this module.
    #[test]
    fn a_city_round_trip_keeps_the_slot_and_its_number() {
        let mut c = ChannelState::default();
        assert_eq!(c.claim_slot("General - Elwynn Forest"), Some(1));
        assert_eq!(c.claim_slot("Trade - City"), Some(2));
        assert_eq!(c.claim_slot("LocalDefense - Elwynn Forest"), Some(3));

        // Out of the city: the record and the number survive.
        assert_eq!(c.suspend_slot("Trade - City"), Some(2));
        assert_eq!(c.slot_state("Trade - City"), Some(SlotState::Suspended));
        assert_eq!(c.number_of("Trade - City"), Some(2));
        assert_eq!(
            c.number_of("LocalDefense - Elwynn Forest"),
            Some(3),
            "and nothing above it renumbered — the slot went quiet, it did not go away"
        );

        // Back in: the bypass sends the join AND clears the suspension in the same pass.
        c.confirm_slot("Trade - City");
        assert_eq!(c.slot_state("Trade - City"), Some(SlotState::Joined));
        assert_eq!(c.number_of("Trade - City"), Some(2), "still slot 2");
    }

    /// **A city-only row still takes a slot when you are nowhere near a city** (decision 2137).
    ///
    /// The reference registers the slot (`0x49b980` at `0x49a50d`) *before* the city gate
    /// (`0x49a512`), so a character standing in Elwynn Forest holds three slots — General, a
    /// created-but-never-joined `Trade - City`, and LocalDefense — and the `N.` a channel line
    /// carries is `slot[+0x00]`, the client-local index. `/2` therefore reaches **Trade** there.
    ///
    /// We filtered the city rows out before they could take a number, so ours gave LocalDefense 2
    /// and `/2` reached a different channel than the reference's. Invisible inside a capital, where
    /// all three join and the numbering agrees — which is why the director's screenshot, taken on a
    /// login into Stormwind, does not show it.
    #[test]
    fn a_city_row_takes_its_number_even_outside_a_city() {
        let cat = catalog();
        assert_eq!(
            tracked_channels(&cat, "Elwynn Forest", CITY),
            vec![
                "General - Elwynn Forest",
                "Trade - City",
                "LocalDefense - Elwynn Forest",
            ],
            "all three rows take slots in DBC order, city or no city"
        );
        assert_eq!(
            wanted_channels(&cat, "Elwynn Forest", false, CITY),
            vec!["General - Elwynn Forest", "LocalDefense - Elwynn Forest"],
            "…but only two of them are JOINED out here — the gate decides the packet, not the slot"
        );

        // Inside a capital the two lists agree, which is the case that hid this.
        assert_eq!(
            tracked_channels(&cat, "Stormwind City", CITY),
            wanted_channels(&cat, "Stormwind City", true, CITY)
        );
    }

    /// The two lists never disagree about *resolvability* — only about the city gate. A missing
    /// city word or an unknown zone drops the row from both, so a slot is never taken for a name
    /// we could not compose.
    #[test]
    fn the_slot_set_and_the_join_set_differ_only_by_the_city_gate() {
        let cat = catalog();
        assert_eq!(
            tracked_channels(&cat, "Stormwind City", None),
            wanted_channels(&cat, "Stormwind City", true, None),
            "no city word: the city-NAMED rows are unresolvable, so neither list carries them"
        );
        assert!(
            tracked_channels(&cat, "", CITY).is_empty(),
            "no zone: nothing zone-dependent composes, so nothing takes a slot either"
        );
    }

    /// **A catalog fed with no zone is not the same thing as no catalog** (decision 2130).
    ///
    /// The VM's three readers all treat an *empty* catalog as "no such built-in channel":
    /// `AddChatWindowChannel` stores the name as a custom channel with id 0, and
    /// `JoinChannelByName` returns `(0, nil)` — the custom leg — and **sends
    /// `CMSG_JOIN_CHANNEL("General")`**. Fed zone-less instead, every zone-dependent row is present
    /// but carries `resolved: None`, which is the reference's own "matched a row, no zone text yet"
    /// leg: store nothing, answer nil, send nothing (chat-cache-grammar.md §5).
    ///
    /// That gap is what wrote `CHANNELS / General / LocalDefense` into the director's `Onewarrior`
    /// window block with the zone bits stripped, and it was open on every login until the world
    /// settled — because the catalog used to be fed only once a zone was known.
    #[test]
    fn a_zoneless_catalog_still_carries_every_row() {
        let rows = zone_channel_catalog(&catalog(), "", false, None);
        assert_eq!(rows.len(), 6, "every DBC row is present, zone or no zone");

        let by = |s: &str| rows.iter().find(|r| r.shortcut == s).unwrap().clone();
        assert_eq!(by("General").id, 1, "…and each carries its ChannelID");
        assert_eq!(
            by("General").resolved,
            None,
            "a zone-dependent row with no zone is UNRESOLVED — the nil leg, not the custom leg"
        );
        assert_eq!(by("Trade").resolved, None);
        assert_eq!(
            by("WorldDefense").resolved,
            Some("WorldDefense".to_string()),
            "a row whose name carries no %s resolves with no zone at all"
        );

        // And the moment a zone lands, the same rows resolve.
        let rows = zone_channel_catalog(&catalog(), "Elwynn Forest", false, CITY);
        let by = |s: &str| rows.iter().find(|r| r.shortcut == s).unwrap().clone();
        assert_eq!(
            by("General").resolved,
            Some("General - Elwynn Forest".to_string())
        );
        assert!(
            !by("Trade").listed,
            "…and Trade is city-only, so it is not listed out here"
        );
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
