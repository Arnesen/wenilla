//! The wound-flinch secondary-blend slot logic for [`super::drive_animations`]: the per-frame decay
//! upkeep ([`wound_upkeep`]), the same-bone re-arm eviction ([`wound_evict`]), and the victim trigger
//! that arms the slot ([`wound_trigger`]) — split out of [`super`] as its own concern.

use benilla_assets::ModelAnimations;
use benilla_formats::AnimDataCatalog;
use bevy::prelude::*;

use super::super::{find_resolved, AnimDriver, MovementState, Wound};
use super::select::{self, STAND};

/// One frame's wound edge for a victim — the two client trigger paths into the same secondary
/// slot (`0x60ea70`): a landed melee hit (`SMSG_ATTACKERSTATEUPDATE`, resolved to a wound id by
/// severity + engagement at trigger time) or a spell-impact kit whose anim is the CombatWound
/// family (the kit player's own 8–10 branch — decision 0099 phase 4, carrying the kit's id).
#[derive(Clone, Copy)]
pub(super) enum WoundEdge {
    Melee(u32),
    Spell(u16),
}

/// Wound-flinch decay upkeep (decision 0111): the client's kernel advances every armed
/// SECONDARY slot per bone per frame **unconditionally** — before any state logic, through
/// death itself — and the slot self-releases at the decay's end (`0x7147b9`: `+0xd0 = -1`
/// and λ = 0 the same frame). So this runs above the death override and touches nothing
/// but its own node: λ = smoothstep(remaining)·0.75 over the clip's own span, blended out
/// and gone — never a snap, never a stop of what plays underneath.
pub(super) fn wound_upkeep(drv: &mut AnimDriver, player: &mut AnimationPlayer) {
    if let Some(wd) = drv.wound {
        let finished = match player.animation_mut(wd.node) {
            Some(a) if !a.is_finished() => {
                let remaining = 1.0 - a.seek_time() / wd.span;
                // The λ-anchor: on the masked subtree the base (1.0) and a live one-shot
                // overlay (8.0) both blend; the full-body route sits over the base alone.
                // (During a base cross-fade the transition's fading clip briefly raises the
                // real total past 1.0 — a sub-blend-time wobble we accept, the client's own
                // transitions run through this very secondary slot instead.)
                let others = if wd.masked && drv.overlay.is_some() {
                    1.0 + super::ONESHOT_OVERLAY_WEIGHT
                } else {
                    1.0
                };
                a.set_weight(select::wound_weight(remaining, others));
                false
            }
            _ => true,
        };
        if finished {
            player.stop(wd.node);
            drv.wound = None;
        }
    }
}

/// Wound-flinch eviction (decision 0114): the wound occupies its bone's SECONDARY slot,
/// and a **blended primary re-arm on the same bone overwrites it** (op4 `blendFlag≠0`
/// copies the outgoing pose over `+0xc4..` — the standard 150 ms transition fade takes
/// the slot). So this frame's full-body plays (bone 0: a swing on the base, a gait/mode
/// change) evict a FULL-BODY wound, and masked-slot plays (the key-bone: a masked swing,
/// the cast-hold retake) evict a MASKED wound — while the *other* bone's plays leave the
/// wound decaying (§3's inherited-swing case: a full-body swing under a masked wound).
/// This is exactly why the real client's flinch never smothers the next attack: the swing
/// reclaims the slot the instant it starts. Mode/gait changes proxy the mode machine's
/// plays — a change with no play (Land's re-pick) merely evicts one frame before the play
/// that follows it.
pub(super) fn wound_evict(
    drv: &mut AnimDriver,
    player: &mut AnimationPlayer,
    masked_played: bool,
    base_played: bool,
) {
    if let Some(wd) = drv.wound {
        let evicted = if wd.masked {
            masked_played
        } else {
            base_played
        };
        if evicted {
            player.stop(wd.node);
            drv.wound = None;
        }
    }
}

/// The victim wound flinch (decision 0111 — the §5 verdict, rebuilt from bytes after
/// the first routing was director-falsified): a landed hit lays the wound clip into this
/// unit's **secondary slot** (`0x60ea70` → op4 `linkFlag=0`) — a decaying 0.75-amplitude
/// blend overlay over whatever plays. It never touches the base track or the one-shot
/// slot: the victim's own in-flight swing keeps running underneath (there is NO mid-swing
/// gate — both §5 trigger agents refuted that hypothesis), and the upkeep above blends it
/// out and self-releases — until a same-bone re-arm evicts it (the block above; a wound
/// triggered here is this frame's *last* write, matching the client's packet order).
/// Client gates without a benilla counterpart yet: the creature-template no-wound
/// type-flag 0x8 (no template cache; the client itself passes on a null template) and the
/// attached-spell-effect marker (no CEffect system until the VFX phases). The client
/// calls op4 directly — not PlayAnimation — so the flinch is faithfully invisible to the
/// sheath reconcile and the event scan. `id` is the wound anim to lay (8–10), already resolved
/// by the caller — melee by severity/engagement ([`select::wound_anim`]), a spell impact by its
/// kit's own column ([`WoundEdge`]).
#[allow(clippy::too_many_arguments)] // the trigger's full live-state + rng input set
pub(super) fn wound_trigger(
    drv: &mut AnimDriver,
    player: &mut AnimationPlayer,
    anims: &ModelAnimations,
    catalog: Option<&AnimDataCatalog>,
    rng: &mut u32,
    id: u16,
    mv: &MovementState,
    mounted: bool,
) {
    // Alive only (the `0x605f90` IsDead gate): the dead branch already `continue`d on
    // health/dyn-flags; stand-state 7 (lying dead) is its third clause.
    if mv.stand_state != 7 {
        let base = drv.resolved_anim(anims, catalog).unwrap_or(STAND);
        let full = select::wound_full_body(id, base, mv.flags, mounted);
        // The wound rolls its variation like any one-shot (op4 is called with
        // variationIdx −1 — decision 0114); span 0 = the client's degenerate seed
        // (`end = clock`, expired on arrival) — skip. No resolvable clip at all is the
        // `0x711a20` asset-presence abort.
        let clip = find_resolved(anims, id, catalog)
            .and_then(|h| anims.pick_variation(h.anim_id, select::msvc_rand(rng)))
            .filter(|c| c.duration > 0.0);
        let node = clip.and_then(|c| {
            if full {
                Some((c, c.node, false))
            } else {
                // A model without the split bone degrades to the full-body node — still
                // a decaying blend over the base, never a replace.
                c.upper_node
                    .map(|n| (c, n, true))
                    .or(Some((c, c.node, false)))
            }
        });
        if let Some((c, node, masked)) = node {
            // A re-trigger re-seeds the slot (the client overwrites the secondary).
            if let Some(prev) = drv.wound.take() {
                player.stop(prev.node);
            }
            // If the node is still active after that stop, the base track owns it (a
            // fallback chain degenerating to the playing clip) — nothing to layer.
            if player.animation(node).is_none() {
                let others = if masked && drv.overlay.is_some() {
                    1.0 + super::ONESHOT_OVERLAY_WEIGHT
                } else {
                    1.0
                };
                let active = player.play(node);
                active.replay();
                // One pass only, and never a stale repeat from a prior play of this node: the
                // client does roll a replay budget on this arm too, but the flinch's decay window
                // is seeded from a single span (`+0x100 = clock + span`, decision 0111) — λ hits 0
                // and the slot self-releases at first span-end, so R is moot for the wound.
                active.set_repeat(bevy::animation::RepeatAnimation::Never);
                active.set_weight(select::wound_weight(1.0, others));
                drv.wound = Some(Wound {
                    node,
                    span: c.duration,
                    masked,
                });
            }
        }
    }
}
