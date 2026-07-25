//! Duels — the challenge, the countdown, the bounds timer, and the outcome line (decision 0633).
//!
//! The smallest complete multiplayer verb: no window of its own, two StaticPopups, four events,
//! four Era globals, and one descriptor-driven consequence — a duel opponent reads *hostile*, so
//! the whole targeting/attack stack lights up without a special case (see
//! [`crate::target::ring`]'s duel leg).
//!
//! **Both ends of every mechanism here are byte-pinned** — vmangos for what the server sends,
//! WoW.exe's own duel TU (`E:\build\buildWoW\WoW\Source\Ui\DuelInfo.cpp`, handlers registered at
//! `0x4d4710`) for what the client does with it. The laws this module implements, each with its
//! address:
//!
//! - **The challenge is symmetric** (`0x4d49d0`). `SMSG_DUEL_REQUESTED` goes to challenger and
//!   challenged alike. The client stores the arbiter guid, then compares the challenger guid
//!   against its own: **equal** ⇒ show `ERR_DUEL_REQUESTED` ("You have requested a duel.") and
//!   **immediately send `CMSG_DUEL_ACCEPTED`** (`call 0x4d4830` — a no-op server-side, which
//!   drops an accept from the initiator, but it is what goes on the wire); **different** ⇒ resolve
//!   the challenger's player object and fire `DUEL_REQUESTED` with its name.
//! - **The countdown is client-driven** (`0x4d4ae0` → `0x4d4930`, timer named `ProcessCountdown`).
//!   The wire carries milliseconds; the client divides by 1000, prints
//!   `format(DUEL_COUNTDOWN, n)` as a **CHAT_MSG_SYSTEM chat line** — not an on-screen banner —
//!   then decrements and re-arms a 1000 ms timer while non-zero. vmangos's 3000 therefore reads
//!   "Duel starting: 3 / 2 / 1" one second apart.
//! - **Completion** (`0x4d4b20`). Only acts if an arbiter is held. `started == 0` (the duel never
//!   began) additionally shows `ERR_DUEL_CANCELLED` ("Duel cancelled."). Either way the arbiter is
//!   cleared, `DUEL_FINISHED` fires, and any running countdown is cancelled.
//! - **The outcome line** (`0x4d4ba0`) is composed client-side from `SMSG_DUEL_WINNER`'s flag and
//!   two names against `DUEL_WINNER_KNOCKOUT` / `DUEL_WINNER_RETREAT`, and printed as
//!   CHAT_MSG_SYSTEM. The server broadcasts it to everyone nearby, so bystanders read it too.
//! - **Bounds** (`0x4d4aa0` / `0x4d4ac0`) are bare event fires; the 10 s forfeit clock and its
//!   text belong to the `DUEL_OUTOFBOUNDS` popup, and the *enforcement* is entirely the server's
//!   (`Player::CheckDuelDistance`: 75 yd out, 70 yd back in, 10 s to return).
//!
//! Two deviations, both stated rather than hidden:
//!
//! 1. **The ignore branch is absent.** The reference silently answers a challenge from an ignored
//!    player with `CMSG_DUEL_CANCELLED` (`0x4d4a33`). benilla has no ignore list yet (it lands
//!    with the friends/who slice of decision 0434), so no challenge is ever auto-declined.
//! 2. **The challenger's name is resolved asynchronously.** The reference reads it off the
//!    challenger's `CGPlayer` and fires nothing at all if the object is missing (`0x4d4a72`).
//!    benilla resolves through the [`NameCache`], which may need a `CMSG_NAME_QUERY` round trip —
//!    so the popup can arrive a frame or two late instead of never. In practice the challenger is
//!    always streamed (the duel spell's range is 10 yd), which is exactly the case where the
//!    reference has the name for free.

use benilla_ui::script::{DuelRequest, ScriptValue, UiScript};
use bevy::prelude::*;

use crate::names::NameCache;
use crate::net::{ClientCommand, Guid, GuidIndex, NetCommands, SelfPlayer};
use crate::target::Selection;
use crate::ui_action::Spells;
use crate::ui_chat::{ChatEvent, ChatEventKind, ChatLog};
use crate::ui_script::UiInput;

/// `SPELL_EFFECT_DUEL` — the `Effect[0]` value that identifies the duel spell in the player's own
/// spellbook (7266 "Duel" on 1.12 data, granted to every race/class at creation). The reference
/// does exactly this lookup rather than hardcoding the id: its spell-learned walk stores the id
/// of any learned spell whose `SpellRec+0xf4` is `0x53` into the duel-spell global `[0xb71130]`
/// (`0x4b2605`), which `StartDuel`/`StartDuelUnit` then cast (`0x4d4810`). We mirror the
/// mechanism, so a data change moves with the data.
const SPELL_EFFECT_DUEL: u32 = 83;

// The GlobalStrings templates, quoted verbatim from the reference client's own patch chain
// (decision 0246 extraction; `GlobalStrings.lua` line numbers cited per constant). The two `ERR_*`
// lines are what `CGGameUI::DisplayError 0x496720` resolves for error ids `0x135`/`0x136` — the
// table at `0xb4b498` holds their keys at record 309/310 (20-byte records, key at offset 0).
const DUEL_COUNTDOWN: &str = "Duel starting: %d"; // GlobalStrings:955
const DUEL_WINNER_KNOCKOUT: &str = "%1$s has defeated %2$s in a duel"; // GlobalStrings:958
const DUEL_WINNER_RETREAT: &str = "%2$s has fled from %1$s in a duel"; // GlobalStrings:959
const ERR_DUEL_REQUESTED: &str = "You have requested a duel."; // GlobalStrings:1553
const ERR_DUEL_CANCELLED: &str = "Duel cancelled."; // GlobalStrings:1552

/// The duel session mirror. Written by the net drain's duel arms, read by [`feed_duel`] (which
/// fires the Era events on its edges) and [`tick_countdown`]. Cleared on disconnect beside the
/// other per-login resources.
#[derive(Resource, Default)]
pub(crate) struct DuelState {
    /// The duel-flag GameObject guid identifying the pending or running duel; `0` = none. This is
    /// the client's `[0xb73240]`: set by the request, echoed on accept/cancel, cleared only by
    /// completion. Its non-zero→zero edge is what fires `DUEL_FINISHED`.
    pub(crate) arbiter: u64,
    /// The challenger's guid while a challenge popup is owed to the UI — `None` once fired (or
    /// when we are ourselves the challenger, who never gets a popup).
    challenger: Option<u64>,
    /// We are outside the duel bubble; drives the `DUEL_OUTOFBOUNDS`/`DUEL_INBOUNDS` edge pair.
    out_of_bounds: bool,
    /// The running "Duel starting: N" countdown: ticks left to print, and the 1 s re-arm.
    countdown: Option<Countdown>,
}

/// The `ProcessCountdown` timer's state (`0x4d4930`): a remaining count and a one-second re-arm.
struct Countdown {
    remaining: u32,
    tick: Timer,
}

impl DuelState {
    /// `SMSG_DUEL_REQUESTED`. Returns the error line to display when the challenge is our own —
    /// the reference's `DisplayError(ERR_DUEL_REQUESTED)` — and leaves [`Self::challenger`] set
    /// for the feed to turn into the popup when it is someone else's.
    ///
    /// The caller sends the challenger's auto-accept: the reference's own request handler calls
    /// `AcceptDuel` inline (`0x4d4a12`), so the accept is on the wire before any UI runs.
    fn apply_requested(&mut self, arbiter: u64, challenger: u64, own: Option<u64>) -> bool {
        self.arbiter = arbiter;
        let ours = Some(challenger) == own;
        self.challenger = (!ours).then_some(challenger);
        ours
    }

    /// `SMSG_DUEL_COMPLETE`. Returns whether the "Duel cancelled." line is owed (the duel never
    /// started). Gated on holding an arbiter, exactly like `0x4d4b20` — a stray completion with
    /// no duel in flight is silent. Cancels any countdown unconditionally, which the reference
    /// does *outside* that gate.
    fn apply_complete(&mut self, started: bool) -> bool {
        self.countdown = None;
        if self.arbiter == 0 {
            return false;
        }
        self.arbiter = 0;
        self.challenger = None;
        self.out_of_bounds = false;
        !started
    }

    /// `SMSG_DUEL_COUNTDOWN`. A zero count arms nothing: the reference would print "Duel starting:
    /// 0" and then re-arm forever on the `dec`'s wraparound, an unreachable path on 1.12 servers
    /// (vmangos always sends 3000) that is not worth reproducing.
    fn apply_countdown(&mut self, seconds: u32) {
        self.countdown = (seconds > 0).then(|| Countdown {
            remaining: seconds,
            tick: Timer::from_seconds(1.0, TimerMode::Repeating),
        });
    }
}

/// Compose `SMSG_DUEL_WINNER`'s system line. The templates use positional `%1$s`/`%2$s` because
/// the retreat wording swaps the two names — quoted from GlobalStrings verbatim, so the
/// substitution is positional here too rather than `format!`'s.
fn winner_line(fled: bool, winner: &str, loser: &str) -> String {
    let template = if fled {
        DUEL_WINNER_RETREAT
    } else {
        DUEL_WINNER_KNOCKOUT
    };
    template.replace("%1$s", winner).replace("%2$s", loser)
}

/// The net drain's `SessionEvent::Duel*` arms, factored here so the wire laws live beside the
/// state they drive. `own` is our own guid.
pub(crate) mod apply {
    use super::*;

    /// `SMSG_DUEL_REQUESTED` — store the arbiter, and either own the challenge (error line +
    /// immediate accept) or hand the popup to the feed.
    pub(crate) fn requested(
        duel: &mut DuelState,
        chat_log: &mut ChatLog,
        commands: &NetCommands,
        arbiter: u64,
        challenger: u64,
        own: Option<u64>,
    ) {
        if duel.apply_requested(arbiter, challenger, own) {
            error_line(chat_log, ERR_DUEL_REQUESTED);
            let _ = commands.0.send(ClientCommand::DuelAccepted { arbiter });
        }
    }

    /// `SMSG_DUEL_COMPLETE` — clear the session, cancel the countdown, and show "Duel cancelled."
    /// when the duel never started.
    pub(crate) fn complete(duel: &mut DuelState, chat_log: &mut ChatLog, started: bool) {
        if duel.apply_complete(started) {
            error_line(chat_log, ERR_DUEL_CANCELLED);
        }
    }

    /// `SMSG_DUEL_WINNER` — the outcome line, as CHAT_MSG_SYSTEM.
    pub(crate) fn winner(chat_log: &mut ChatLog, fled: bool, winner: &str, loser: &str) {
        system_line(chat_log, winner_line(fled, winner, loser));
    }

    /// `SMSG_DUEL_COUNTDOWN` — arm the client-side tick.
    pub(crate) fn countdown(duel: &mut DuelState, seconds: u32) {
        duel.apply_countdown(seconds);
    }

    /// `SMSG_DUEL_OUTOFBOUNDS` / `SMSG_DUEL_INBOUNDS` — the feed turns the edge into its event.
    pub(crate) fn bounds(duel: &mut DuelState, outside: bool) {
        duel.out_of_bounds = outside;
    }
}

/// A `DisplayError`-shaped line. benilla models `CGGameUI::DisplayError 0x496720` as the red
/// `UI_ERROR_MESSAGE` toast (decision 0427, the cast-failure pipeline) — but that fires through
/// the VM, which the net drain has no handle on. Both duel error ids are pure text with no
/// arguments, so they ride the system chat channel the countdown and outcome lines already use;
/// the UIErrorsFrame routing is the open refinement, noted in decision 0633.
fn error_line(chat_log: &mut ChatLog, text: &str) {
    system_line(chat_log, text.to_string());
}

fn system_line(chat_log: &mut ChatLog, text: String) {
    chat_log.push_event(ChatEvent::text_only(ChatEventKind::System, text));
}

/// Fire the four duel events on their state edges — the [`crate::ui_party::feed`] pattern: the
/// drain mutates [`DuelState`], the feed diffs it against what it last announced.
fn feed_duel(
    script: Option<NonSendMut<UiScript>>,
    mut duel: ResMut<DuelState>,
    mut names: ResMut<NameCache>,
    commands: Res<NetCommands>,
    mut fed: Local<FedDuel>,
) {
    let Some(mut script) = script else {
        return;
    };

    // DUEL_REQUESTED(challengerName) — held until the name resolves (module doc, deviation 2).
    if let Some(guid) = duel.challenger {
        if let Some(name) = names.resolve(guid, &commands).map(str::to_string) {
            script.fire_event("DUEL_REQUESTED", vec![ScriptValue::Str(name)]);
            duel.challenger = None;
        }
    }

    // DUEL_FINISHED on the arbiter's non-zero→zero edge (`0x4d4b51`).
    let held = duel.arbiter != 0;
    if fed.held && !held {
        script.fire_event("DUEL_FINISHED", Vec::new());
    }
    fed.held = held;

    // The bounds pair (`0x4d4aa0` / `0x4d4ac0`).
    if fed.out_of_bounds != duel.out_of_bounds {
        fed.out_of_bounds = duel.out_of_bounds;
        let event = if duel.out_of_bounds {
            "DUEL_OUTOFBOUNDS"
        } else {
            "DUEL_INBOUNDS"
        };
        script.fire_event(event, Vec::new());
    }
}

/// What [`feed_duel`] last announced.
#[derive(Default)]
struct FedDuel {
    held: bool,
    out_of_bounds: bool,
}

/// `ProcessCountdown` (`0x4d4930`): print "Duel starting: N" now, then once a second while the
/// count is non-zero. The first line lands the frame the packet arrives — the reference calls the
/// tick body directly before arming the timer — so the timer here starts already elapsed.
fn tick_countdown(
    time: Res<Time>,
    mut duel: ResMut<DuelState>,
    mut chat_log: ResMut<ChatLog>,
    mut started: Local<bool>,
) {
    let Some(countdown) = duel.countdown.as_mut() else {
        *started = false;
        return;
    };
    let fire = if *started {
        countdown.tick.tick(time.delta()).just_finished()
    } else {
        *started = true;
        true
    };
    if !fire {
        return;
    }
    let line = DUEL_COUNTDOWN.replace("%d", &countdown.remaining.to_string());
    system_line(&mut chat_log, line);
    countdown.remaining -= 1;
    if countdown.remaining == 0 {
        duel.countdown = None;
        *started = false;
    }
}

/// Drain the Era API's duel intents into their sends.
#[allow(clippy::too_many_arguments)] // a Bevy system's param list IS its dependency set
fn drain_duel(
    script: Option<NonSendMut<UiScript>>,
    duel: Res<DuelState>,
    spells: Option<Res<Spells>>,
    actions: Res<crate::ui_action::PlayerActions>,
    selection: Res<Selection>,
    index: Res<GuidIndex>,
    self_q: Query<&Guid, With<SelfPlayer>>,
    names: Res<NameCache>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    let requests = script.take_duel_requests();
    if requests.is_empty() {
        return;
    }
    let self_guid = self_q.iter().next().map(|g| g.0);
    for req in requests {
        match req {
            DuelRequest::Accept => {
                if duel.arbiter != 0 {
                    let _ = commands.0.send(ClientCommand::DuelAccepted {
                        arbiter: duel.arbiter,
                    });
                }
            }
            DuelRequest::Cancel => {
                if duel.arbiter != 0 {
                    let _ = commands.0.send(ClientCommand::DuelCancelled {
                        arbiter: duel.arbiter,
                    });
                }
            }
            DuelRequest::StartByUnit(token) => {
                // The reference resolves the token to a CGObject and requires typemask 0x10 —
                // a player (`0x4d4c66`). Ours are the tokens the unit popup actually passes.
                let guid = match token.as_str() {
                    "target" => selection.guid,
                    "player" => self_guid,
                    _ => None,
                };
                challenge(guid, self_guid, &index, &spells, &actions, &commands);
            }
            DuelRequest::StartByName(name) => {
                // `/duel <name>`: the reference looks the name up in the **object manager**
                // (`0x515970`) — only a streamed player can be found this way, which is also the
                // spell's own requirement. Same population here: the streamed guid index, named
                // through the cache.
                let guid = streamed_player_named(&name, &index, &names);
                challenge(guid, self_guid, &index, &spells, &actions, &commands);
            }
        }
    }
}

/// Find a streamed player by name — the reference's object-manager name lookup. Case-insensitive:
/// the caller is a `/duel` argument the player typed.
fn streamed_player_named(name: &str, index: &GuidIndex, names: &NameCache) -> Option<u64> {
    index
        .0
        .keys()
        .copied()
        .filter(|g| benilla_protocol::guid::is_player(*g))
        .find(|g| names.peek(*g).is_some_and(|n| n.eq_ignore_ascii_case(name)))
}

/// Cast the duel spell at `guid` — the reference's `0x4d4810`, which is a plain spell cast at a
/// unit target, not a duel opcode. Silently drops a missing/self/non-player/unstreamed target the
/// way the reference's guid and typemask gates do.
fn challenge(
    guid: Option<u64>,
    own: Option<u64>,
    index: &GuidIndex,
    spells: &Option<Res<Spells>>,
    actions: &crate::ui_action::PlayerActions,
    commands: &NetCommands,
) {
    let Some(guid) = guid.filter(|g| *g != 0 && Some(*g) != own) else {
        return;
    };
    if !benilla_protocol::guid::is_player(guid) || !index.0.contains_key(&guid) {
        return;
    }
    let Some(spell_id) = duel_spell(spells, actions) else {
        return;
    };
    let _ = commands.0.send(ClientCommand::CastSpell {
        spell_id,
        target: Some(guid),
    });
}

/// The learned spell whose `Effect[0]` is [`SPELL_EFFECT_DUEL`] — the reference's `[0xb71130]`,
/// filled by its spell-learned walk rather than hardcoded (see the constant's note).
fn duel_spell(
    spells: &Option<Res<Spells>>,
    actions: &crate::ui_action::PlayerActions,
) -> Option<u32> {
    let spells = spells.as_ref()?;
    actions.spells.iter().copied().find(|id| {
        spells
            .catalog
            .get(*id)
            .is_some_and(|s| s.effect_1 == SPELL_EFFECT_DUEL)
    })
}

/// Duels: the wire session, the countdown tick, the Era events, and the outbound intents.
pub(crate) struct UiDuelPlugin;

impl Plugin for UiDuelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DuelState>().add_systems(
            Update,
            (
                tick_countdown,
                feed_duel.before(UiInput),
                drain_duel.after(UiInput),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The challenge is symmetric: our own request takes the error-line branch and leaves no
    /// popup owed; someone else's leaves the challenger for the feed. Either way the arbiter is
    /// stored — the challenger holds one too, which is why `DUEL_FINISHED` reaches both.
    #[test]
    fn our_own_challenge_takes_the_error_branch() {
        let mut duel = DuelState::default();
        assert!(duel.apply_requested(0xA1, 7, Some(7)));
        assert_eq!(duel.arbiter, 0xA1);
        assert_eq!(duel.challenger, None);

        let mut duel = DuelState::default();
        assert!(!duel.apply_requested(0xA1, 9, Some(7)));
        assert_eq!(duel.challenger, Some(9));
    }

    /// Completion is gated on holding an arbiter (`0x4d4b20`), and only an unstarted duel earns
    /// "Duel cancelled.".
    #[test]
    fn completion_is_gated_on_holding_an_arbiter() {
        let mut duel = DuelState::default();
        assert!(!duel.apply_complete(false), "no duel held → silent");

        duel.apply_requested(0xA1, 9, Some(7));
        duel.apply_countdown(3);
        assert!(duel.apply_complete(false), "unstarted → cancelled line");
        assert_eq!(duel.arbiter, 0);
        assert!(duel.countdown.is_none(), "the countdown is cancelled too");

        duel.apply_requested(0xA1, 9, Some(7));
        assert!(
            !duel.apply_complete(true),
            "a real duel ending is silent here"
        );
    }

    /// A zero countdown arms nothing (the reference's unreachable wraparound path).
    #[test]
    fn zero_countdown_arms_nothing() {
        let mut duel = DuelState::default();
        duel.apply_countdown(0);
        assert!(duel.countdown.is_none());
        duel.apply_countdown(3);
        assert_eq!(duel.countdown.as_ref().map(|c| c.remaining), Some(3));
    }

    /// The duel-spell lookup against the **real 5875 Spell.dbc** — the fact the whole challenge
    /// path rests on, and the one a unit test with a synthetic catalog could not catch. Asserts
    /// what the reference's own spell-learned walk asserts by construction: scanning a spellbook
    /// for `Effect[0] == SPELL_EFFECT_DUEL` finds **exactly one** spell, and it is 7266 "Duel"
    /// (the id vmangos grants every race/class in `playercreateinfo_spell` and re-grants at every
    /// login through `Player::LoadFromDB` → `LearnDefaultSpells`). A second match would mean the
    /// reference's single global `[0xb71130]` is ambiguous — worth knowing if it ever happens.
    /// Skips without client data.
    #[test]
    fn the_duel_spell_resolves_to_7266_on_real_data() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let catalog = benilla_formats::load_spell_catalog(&mut chain).expect("Spell.dbc");
        let mut hits: Vec<u32> = catalog
            .iter()
            .filter(|(_, s)| s.effect_1 == SPELL_EFFECT_DUEL)
            .map(|(id, _)| id)
            .collect();
        hits.sort_unstable();
        assert_eq!(hits, vec![7266], "exactly one SPELL_EFFECT_DUEL spell");
        assert_eq!(catalog.get(7266).unwrap().name, "Duel");
    }

    /// The two outcome templates take (winner, loser) in that order — the retreat wording reads
    /// them back to front, which is exactly why they are positional.
    #[test]
    fn the_outcome_line_places_both_names_positionally() {
        assert_eq!(
            winner_line(false, "Onerogue", "Twomage"),
            "Onerogue has defeated Twomage in a duel"
        );
        assert_eq!(
            winner_line(true, "Onerogue", "Twomage"),
            "Twomage has fled from Onerogue in a duel"
        );
    }
}
