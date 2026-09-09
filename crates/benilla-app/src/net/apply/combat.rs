//! Melee-engagement arm bodies for [`super::apply_net_updates`]'s dispatch match — the attack
//! start/stop brackets, the aggro/alert flare, and the completed-swing record with its client-side
//! full-block synthesis. Each `pub(super)` fn here is exactly one arm's body; the match at the
//! call site stays the dispatcher, one call per arm.

use benilla_protocol::messages::{AttackSwingError, AttackerState};
use bevy::prelude::*;

use crate::creature_anim::{
    Engaged, RangedHold, SheathRequest, SwingFlush, SwingImpact, SwingMessage,
};
use crate::swing_refusal::SwingRefusalEdge;
use crate::ui_action::{UiError, UiErrorKeys};
use crate::ui_unit::CombatTextEvent;

use super::super::{AiReactionMessage, GuidIndex, SelfGuid};

/// A unit began melee auto-attack (`SMSG_ATTACKSTART`, including our own echo).
pub(super) fn attack_start(attacker: u64, victim: u64, commands: &mut Commands, index: &GuidIndex) {
    // Engagement brackets (decision 0073): the standing Ready idle rides this window —
    // the client's gate is the auto-attack-target GUID being set, mirrored here as a
    // marker component on the attacker (including our own echo).
    debug!("net: attack start {attacker:#x} → {victim:#x}");
    if let Some(&e) = index.0.get(&attacker) {
        // Melee-start drops the `0x400` weapon-visual hold unconditionally (the client's
        // `0x60fc50` sibling clear) — a shooter that closes to melee leaves the drawn idle.
        // The LOCAL player's melee paths additionally run the full cancel funnel at send.
        commands.entity(e).insert(Engaged).remove::<RangedHold>();
    }
}

/// A unit stopped melee auto-attack (`SMSG_ATTACKSTOP`).
pub(super) fn attack_stop(
    attacker: u64,
    victim: u64,
    commands: &mut Commands,
    index: &GuidIndex,
    flushes: &mut MessageWriter<SwingFlush>,
) {
    debug!("net: attack stop {attacker:#x} → {victim:#x}");
    if let Some(&e) = index.0.get(&attacker) {
        commands.entity(e).remove::<Engaged>();
        // The client's `0x624e40` (death/stun arrive as this packet too): a pending
        // swing record flushes text-only and clears.
        flushes.write(SwingFlush(e));
    }
}

/// The server refused our melee swing (`SMSG_ATTACKSWING_NOTINRANGE`/`_BADFACING`/`_DEADTARGET`/
/// `_CANT_ATTACK`) — forwarded verbatim to [`crate::swing_refusal`], which owns the latch, the 4 s
/// repeat, and arm 4's silent StopAttack. Nothing is decided here: the arms differ only in what
/// that module does with them, and it holds the write set for all three.
pub(super) fn attack_swing_error(
    error: AttackSwingError,
    edges: &mut MessageWriter<SwingRefusalEdge>,
) {
    edges.write(SwingRefusalEdge::Refused(error));
}

/// `SMSG_CANCEL_COMBAT` — the server forced our attack to stop. The swing family's fourth arm,
/// and the same act as `0x148`/`0x149`: the reference's handler `0x5e7dd0` is arm 4's body
/// verbatim.
pub(super) fn cancel_combat(edges: &mut MessageWriter<SwingRefusalEdge>) {
    edges.write(SwingRefusalEdge::CombatCancelled);
}

/// `SMSG_FEIGN_DEATH_RESISTED` — the target shrugged off our Feign Death.
///
/// One red line and nothing else: the reference's handler `0x6e9800` is `push 0x1a5; call
/// 0x496720`, a bare `DisplayError(421)` with no latch, no cooldown and no state — the opposite of
/// its sibling above, and the reason the two do not share a path. Catalog row 421 is
/// `ERR_FEIGN_DEATH_RESISTED`, whose 1.12 string is the single word "Resisted".
///
/// It lives beside the swing arms because vmangos sends it in the same breath as
/// `SMSG_CANCEL_COMBAT` (`Objects/Unit.cpp:9445-9451`: a resisted feign death cancels the attack
/// and says so), and finding one without the other is how this family stayed half-built.
pub(super) fn feign_death_resisted(errors: &mut UiErrorKeys) {
    debug!("net: feign death resisted");
    errors.0.push(UiError::key("ERR_FEIGN_DEATH_RESISTED"));
}

/// A creature flared aggro or a stealth pre-aggro alert (`SMSG_AI_REACTION`).
pub(super) fn ai_reaction(
    unit: u64,
    reaction: u32,
    index: &GuidIndex,
    reactions: &mut MessageWriter<AiReactionMessage>,
) {
    // Aggro (2 HOSTILE) / stealth alert (0 ALERT) flare — pure audio, byte-verified
    // (`0x6056e0` is an exact two-way branch; any other value no-ops, and neither leg
    // touches animation/nameplate/UI — decision 0280). Vocals: `sound::creature`.
    debug!("net: ai reaction {reaction} on {unit:#x}");
    if matches!(reaction, 0 | 2) {
        if let Some(&e) = index.0.get(&unit) {
            reactions.write(AiReactionMessage {
                unit: e,
                hostile: reaction == 2,
            });
        }
    }
}

/// One completed melee swing (`SMSG_ATTACKERSTATEUPDATE`, decision 0073): the attacker's swing
/// anim starts NOW; the victim feedback (blood/flinch/text/impact sounds) defers to the swing
/// clip's attack-hit keyframe (`creature_anim::impact`, the client's `0x6247d0` router) — EXCEPT
/// the center combat text, which the client fires **synchronously at packet parse**
/// (`0x6255b0 → 0x629d30 → 0x703f50`, one call stack — §5-verified, wow-re
/// `combat-text-update-emission-law.md`; decision 0580's fold-back).
#[allow(clippy::too_many_arguments)] // one dispatch arm's full writer set
pub(super) fn attacker_state(
    mut s: AttackerState,
    index: &GuidIndex,
    self_guid: &SelfGuid,
    swings: &mut MessageWriter<SwingMessage>,
    impacts: &mut MessageWriter<SwingImpact>,
    center: &mut MessageWriter<CombatTextEvent>,
    sheaths: &mut MessageWriter<SheathRequest>,
    edges: &mut MessageWriter<SwingRefusalEdge>,
    stores: &Query<&mut crate::net::ObjectStore>,
    seq: u64,
) {
    let victim = index.0.get(&s.victim).copied();
    // Arm 5's `0x6259b6 call 0x5ea800`, whose first act is the swing-refusal latch clear
    // (`0x5ecdb0(0)`) — gated exactly as the reference gates it: the attacker IS the active player
    // (`0x5fa6d0`, a guid compare) and the victim resolves as a streamed unit. Written from here
    // rather than computed downstream because this is the only place holding both guids, and it
    // keeps the clear in packet order with the refusals (`crate::swing_refusal`).
    if self_guid.0 == Some(s.attacker) && victim.is_some() {
        edges.write(SwingRefusalEdge::Landed);
    }
    if benilla_assets::trace::enabled() {
        benilla_assets::trace::line(
            "fct",
            &format!(
                "recv swing atk={:#x} victim={:#x} dmg={} vstate={} hit={:#x}",
                s.attacker, s.victim, s.damage, s.victim_state, s.hit_info
            ),
        );
    }
    // The client-side FULL-BLOCK synthesis (`0x625e20`, decision 0279): a resolvable
    // victim + zero damage + a nonzero blocked amount rewrites the state to BLOCKS(5)
    // before any consumer sees the record — the only thing the wire's blocked_amount
    // ever does (a PARTIAL block stays state 1, indistinguishable from a plain hit).
    if victim.is_some() && s.damage == 0 && s.blocked != 0 {
        s.victim_state = 5;
    }
    // The center combat text (decision 0578/0580): self victim, at receive, AFTER the full-block
    // synthesis (so a full block reads BLOCK, not MISS) — the packet's absorb/resist/blocked
    // sums feed the confirmed helper-B partial trailers.
    if self_guid.0 == Some(s.victim) {
        if let Some((message_type, data, extra)) = super::combat_log::melee_center_text(
            s.hit_info,
            s.victim_state,
            s.damage,
            s.absorb,
            s.resist,
            s.blocked,
        ) {
            center.write(CombatTextEvent {
                message_type,
                data,
                extra,
            });
        }
    }
    let swing = SwingMessage {
        attacker: Entity::PLACEHOLDER, // filled per branch below
        victim,
        hit_info: s.hit_info,
        victim_state: s.victim_state,
        damage: s.damage,
        seq,
    };
    if let Some(&e) = index.0.get(&s.attacker) {
        // The **observed attacker auto-draws melee** — the ref's SECOND melee draw, independent
        // of the attack-start one, and the reason a swing is never delivered in the wrong stance:
        // `0x625829 cmp [attacker+0xd40],1; jne` → `SetSheatheState(1, bInstant=1, bFireEvent=1)`
        // at `0x62583a`, byte-read here, tabulated in wow-re `sheath-policy.md` §1. It sits
        // immediately after the attacker resolve and **before** any hit-info handling, so even a
        // swing whose animation is suppressed (`HitInfo & 0x10000`) still draws. Nothing else in
        // the policy can do this job: the per-animation reconcile's melee force is gated to
        // `CUR != 2` (`0x5fe0f9`/`0x5fe13b`), so a unit swinging with a bow drawn — a ranged
        // stance a shot left behind — would otherwise keep swinging with the bow forever. The
        // setter's own idempotency is the `cmp`: a request equal to the committed state is
        // refused there, so this is free on every swing after the first.
        sheaths.write(SheathRequest {
            entity: e,
            state: 1,
            ceremony: false,
        });
        swings.write(SwingMessage {
            attacker: e,
            ..swing
        });
    } else if swing.victim.is_some_and(|v| {
        // The receive-time arm goes THROUGH the gated dispatcher — `0x625823 je 0x625a3e` takes
        // the unresolved-attacker leg, which resolves the victim and calls `0x625a6d call
        // 0x624530`, so the LOOTABLE front gate applies here exactly as it does to the tag path
        // (`creature_anim::impact::lootable_victim`). It calls no consequence directly.
        !stores.get(v).is_ok_and(|s| s.0.unit_lootable())
    }) {
        // The client's SMSG-arm fallback: an attacker we can't resolve (out of range)
        // can't animate a swing — its victim feedback fires immediately and in FULL
        // (`0x625a6d`, the only receive-time victim dispatch). The PLACEHOLDER
        // attacker resolves nowhere downstream (blood defaults front).
        impacts.write(SwingImpact {
            swing,
            text_only: false,
            natural: None,
            // The receive-time arm: no tag fired, so the reference has no event point either —
            // the consumer falls back to the victim, the only anchor the packet leaves us.
            pos: None,
        });
    }
}
