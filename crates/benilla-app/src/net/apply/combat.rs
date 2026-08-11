//! Melee-engagement arm bodies for [`super::apply_net_updates`]'s dispatch match — the attack
//! start/stop brackets, the aggro/alert flare, and the completed-swing record with its client-side
//! full-block synthesis. Each `pub(super)` fn here is exactly one arm's body; the match at the
//! call site stays the dispatcher, one call per arm.

use benilla_protocol::messages::AttackerState;
use bevy::prelude::*;

use crate::creature_anim::{
    Engaged, RangedHold, SheathRequest, SwingFlush, SwingImpact, SwingMessage,
};
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
    seq: u64,
) {
    let victim = index.0.get(&s.victim).copied();
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
    } else if swing.victim.is_some() {
        // The client's SMSG-arm fallback: an attacker we can't resolve (out of range)
        // can't animate a swing — its victim feedback fires immediately and in FULL
        // (`0x625a6d`, the only receive-time victim dispatch). The PLACEHOLDER
        // attacker resolves nowhere downstream (blood defaults front).
        impacts.write(SwingImpact {
            swing,
            text_only: false,
            natural: None,
        });
    }
}
