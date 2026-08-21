//! Bridge a bridged **anim** emote (`SMSG_EMOTE`) into the general one-shot player
//! ([`super::EmoteAnim`]): resolve its `Emotes.dbc` id to an `AnimID` through the shared sound
//! catalog ([`EmoteSounds`], promoted from `crate::sound`, decision 0070 slice 4) and hand off —
//! after the **receive-side posture gate** ([`receive_eligible`]).
//!
//! **Verified against vmangos** (`/Users/sam/wre/vmangos-src/src/game/Handlers/ChatHandler.cpp`
//! `HandleTextEmoteOpcode` + `src/game/Unit.cpp` `Unit::HandleEmote`/`HandleEmoteCommand`,
//! 2026-07-03): a one-shot text emote (`/wave`, `Emotes.dbc EmoteType == 0`) sends **both**
//! `SMSG_TEXT_EMOTE` (the chat line + voice — `EmoteKind::Text`) and `SMSG_EMOTE`
//! (`HandleEmoteCommand` — `EmoteKind::Anim`) from the *same* handler, keyed on the *same*
//! `Emotes.dbc` id (`EmotesTextEntry::textid` **is** the `Emotes.dbc` foreign key
//! `EmotesText.EmoteID` — the id `SMSG_EMOTE` then carries). So consuming only `EmoteKind::Anim`
//! here is complete for `/wave`-style emotes (no need to also key animation off
//! `SMSG_TEXT_EMOTE`) and never double-fires: one player action yields exactly one `SMSG_EMOTE`,
//! hence exactly one [`EmoteAnim`]. The looping state emotes (sit/sleep/kneel, `EmoteType != 0`)
//! instead go through `Unit::HandleEmoteState` — a `UNIT_NPC_EMOTESTATE` field write, no
//! `SMSG_EMOTE` at all — so they never reach this consumer; they're the state-emote idle in the
//! gait layer instead ([`super::select::is_bare_stand`]).
//!
//! **The receive-side gate is `EmoteFlags`-BLIND** (wow-re `object-layer/scratch/emote-posture-
//! gate.md`, commit `f9584b45`, §2): the real client's `SMSG_EMOTE` handler (`0x5e66b0`) never
//! reads the `Emotes.dbc` record at all — it suppresses only on the performer's own state, **stand-
//! state == 3 (SLEEP)** or the **swim move-flag**, and nothing else (no sit check, no flags check).
//! This is why a *seated* `/clap` still plays for observers though a seated `/bow` never even sent
//! the packet (the send-side gate in `crate::ui_chat` stopped it there): the two gates are different
//! and independent. [`receive_eligible`] mirrors exactly this predicate — do not add an EmoteFlags
//! test here.

use bevy::prelude::*;

use crate::net::{EmoteKind, EmoteMessage, ObjectStore, RemoteMotion};
use crate::sound::EmoteSounds;

use super::{move_flags, EmoteAnim, MovementState};

/// Route a bridged `SMSG_EMOTE` (an anim emote) to the general one-shot player: resolve its
/// `Emotes.dbc` id through the shared catalog, gate on the performer's live posture
/// ([`receive_eligible`] — module doc), and write [`EmoteAnim`]. A text emote (`EmoteKind::Text`)
/// carries no anim id of its own here — see the module doc for why that's complete rather than a
/// gap.
pub(super) fn emote_to_anim(
    mut msgs: MessageReader<EmoteMessage>,
    mut out: MessageWriter<EmoteAnim>,
    mut play_seq: ResMut<super::PlaySeq>,
    emotes: Option<Res<EmoteSounds>>,
    // The performer's live stand-state (`ObjectStore`, any streamed unit incl. self) + movement
    // flags (`MovementState` on our own avatar, `RemoteMotion` on a remote player — see
    // `creature_anim`'s `unify`; a creature's spline carries no swim bit, so it defaults false).
    units: Query<(
        Option<&ObjectStore>,
        Option<&MovementState>,
        Option<&RemoteMotion>,
    )>,
) {
    let Some(emotes) = emotes else { return };
    for m in msgs.read() {
        let (Some(entity), Some(anim_id)) =
            (m.source, resolve_anim_emote(m.kind, |id| emotes.anim(id)))
        else {
            continue;
        };
        let (store, movement, remote) = units.get(entity).unwrap_or((None, None, None));
        if !play_eligible(store, movement, remote) {
            debug!("emote_anim: suppressed anim {anim_id} for {entity:?}");
            continue;
        }
        out.write(EmoteAnim {
            entity,
            anim_id: anim_id as u16,
            seq: play_seq.next(),
        });
    }
}

/// The **posture** half of the gate (module doc — `EmoteFlags`-BLIND, unlike the send-side gate in
/// `crate::ui_chat::emote_send_eligible`): suppress only at stand-state 3 (SLEEP) or while swimming.
/// A merely-seated performer (SIT=1 / SIT_CHAIR=2 / KNEEL=8 / chair 4-6) is **not** suppressed —
/// its anim plays and the composition layer masks it to waist-up.
///
/// Both producers apply this identically — `SMSG_EMOTE`'s handler at `0x5e6706`/`0x5e66f7`, and the
/// client-local gesture dispatcher at `0x60bb52`/`0x60bb61` — which is why it is a free function and
/// not a step of either.
pub(super) fn receive_eligible(stand_state: u8, swimming: bool) -> bool {
    stand_state != 3 && !swimming
}

/// The **shared player's** half of the gate — the client's `0x5fcd20`, the single function both
/// emote producers tail into (wow-re `chat-talk-gesture.md` §4.2/§8, gates 10 and 12). Beyond the
/// posture pair it refuses to play while the unit is **channeling** or **in combat**.
///
/// Two of `0x5fcd20`'s own tests are not repeated here because benilla already enforces them
/// downstream or cannot see them: the *already-armed* test (gate 9) is the anim driver's same-id
/// dedup, and `[+0xd58] & 0x400` (gate 11) is an internal anim-state bit with no benilla equivalent.
fn player_eligible(channeling: bool, in_combat: bool) -> bool {
    !channeling && !in_combat
}

/// The whole gate for one unit, from its live components — the predicate both producers call.
pub(super) fn play_eligible(
    store: Option<&ObjectStore>,
    movement: Option<&MovementState>,
    remote: Option<&RemoteMotion>,
) -> bool {
    let fields = store.map(|s| &s.0);
    let stand_state = fields.map_or(0, |f| f.unit_stand_state());
    let swimming = movement
        .map(|m| m.flags)
        .or_else(|| remote.map(|r| r.flags))
        .unwrap_or(0)
        & move_flags::SWIMMING
        != 0;
    let channeling = fields.is_some_and(|f| f.unit_channel_spell() != 0);
    let in_combat = fields.is_some_and(|f| f.unit_flags() & UNIT_FLAG_IN_COMBAT != 0);
    receive_eligible(stand_state, swimming) && player_eligible(channeling, in_combat)
}

/// `UNIT_FIELD_FLAGS & 0x800` — `UNIT_FLAG_IN_COMBAT`. The client's gate-12 test is "this flag **or**
/// a local auto-attack target is set"; benilla checks the wire flag alone, so a unit that has swung
/// but whose combat flag has not yet streamed can still gesture for a moment.
const UNIT_FLAG_IN_COMBAT: u32 = 0x800;

/// The pure mapping: an `Anim` emote resolves through `lookup` (the catalog's `Emotes.dbc` →
/// `AnimID`); a `Text` emote never carries an anim id through this path — its animation, when it
/// has one, arrives separately as the *same action's* `SMSG_EMOTE` (see the module doc).
fn resolve_anim_emote(kind: EmoteKind, lookup: impl Fn(u32) -> Option<u32>) -> Option<u32> {
    match kind {
        EmoteKind::Anim(id) => lookup(id),
        EmoteKind::Text(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_anim_kind_resolves_and_only_through_the_catalog() {
        let lookup = |id: u32| if id == 3 { Some(66) } else { None };
        assert_eq!(resolve_anim_emote(EmoteKind::Anim(3), lookup), Some(66));
        // A text emote never maps to an anim through this path — its SMSG_EMOTE sibling does.
        assert_eq!(resolve_anim_emote(EmoteKind::Text(101), lookup), None);
        // An anim emote absent from the catalog (or AnimID 0, already filtered by the catalog)
        // stays None rather than defaulting to something.
        assert_eq!(resolve_anim_emote(EmoteKind::Anim(999), lookup), None);
    }

    // ── The receive-side posture gate (`receive_eligible`) — EmoteFlags-BLIND (module doc):
    // stand-state 3 (SLEEP) or swimming suppresses; every other stand-state, including seated,
    // plays (the composer masks a seated performer's anim to waist-up downstream).
    #[test]
    fn sleep_suppresses_the_receive_side_anim() {
        assert!(!receive_eligible(3, false));
    }

    #[test]
    fn swimming_suppresses_the_receive_side_anim() {
        assert!(!receive_eligible(0, true));
    }

    /// The shared player refuses while channeling or in combat — the two `0x5fcd20` tests benilla
    /// was missing on BOTH producers, not just the new one (wow-re `chat-talk-gesture.md` §9 claim 2).
    #[test]
    fn channeling_or_combat_suppresses_the_play() {
        assert!(
            player_eligible(false, false),
            "idle and out of combat plays"
        );
        assert!(!player_eligible(true, false), "channeling suppresses");
        assert!(!player_eligible(false, true), "in combat suppresses");
    }

    #[test]
    fn merely_seated_is_not_suppressed_on_receive() {
        // Unlike the send-side gate, sit/sit-chair/chair/kneel all pass here — only SLEEP does not.
        for stand_state in [0u8, 1, 2, 4, 5, 6, 8] {
            assert!(
                receive_eligible(stand_state, false),
                "stand_state {stand_state}"
            );
        }
    }
}
