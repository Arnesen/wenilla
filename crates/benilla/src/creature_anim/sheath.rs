//! The sheath **policy layer**'s types + ceremony mechanics (decision 0080): the one-setter
//! request, the client-side state cache's visual pin, the draw/stow ceremony overlays, and the
//! `AnimationData.dbc` policy table. The driver systems in [`super::driver`] execute these — the setter,
//! the field-apply adopt, and the per-animation reconcile all live in `drive_animations`, so
//! every sheath transition has exactly one author.

use benilla_assets::ModelAnimations;
use benilla_formats::AnimDataCatalog;
use bevy::prelude::*;

use crate::assets::{LockRecover, WorldAssets};

use super::{find_resolved, select, AnimDriver, Wielded};

/// Holds a unit's *visual* sheath state at the pre-transition value while the draw/stow one-shot
/// plays, so the weapon models swap hands↔hip/back at the animation's authored `$SHL`/`$SHR` event
/// (~the moment the hand reaches the stow point — 500/567 ms into the 1 s clips, dumped from
/// HumanMale.m2) instead of the instant the state changes. Removed at the swap point — the
/// equipment resolver then falls through to the committed sheath state. Absent = no transition
/// in flight.
#[derive(Component, Clone, Copy)]
pub(crate) struct VisualSheath(pub(crate) u8);

/// The draw/stow ceremony in flight: up to two **per-arm masked one-shots** (mainhand on the right
/// arm, offhand on the left — the client's per-slot `0x60b770` plays, composed over whatever the
/// body is doing: walk, run, jump; never cancelled), plus the pending mid-animation weapon swap —
/// [`VisualSheath`] pins the old placement until a clip crosses `swap_at` (the authored
/// `$SHL`/`$SHR` moment).
pub(super) struct SheathSwap {
    /// The playing masked nodes (right-arm, left-arm) — `None` per empty hand.
    pub(super) nodes: [Option<bevy::animation::graph::AnimationNodeIndex>; 2],
    pub(super) swap_at: f32,
    pub(super) unpinned: bool,
}

/// The per-arm overlay's weight over the gait on the arm bones both key: the ceremony dominates the
/// walk's arm-swing ≈ 8:1 (the client's per-bone arming gives it the arm outright; a small bleed is
/// the cost of Bevy's weighted blend). Legs are excluded entirely by the mask.
const SHEATH_OVERLAY_WEIGHT: f32 = 8.0;

/// A request to change a unit's sheath state — benilla's analogue of the client's **one setter**
/// `SetSheatheState(newState, bInstant, bFireEvent)` (`0x611cf0`, decision 0080 structure 1).
/// Every path that changes the state funnels here — the manual Z toggle (the only `ceremony`
/// sender), the attack-start auto-draw, the stand-state stow rider — and `drive_animations` is
/// the sole executor: the idempotency refusal, the commit to the client-side cache, the
/// `CMSG_SETSHEATHED` volunteer for the local player (`bFireEvent = 1`), and the ceremony-vs-snap
/// visual all live in that one place. Byte-verified across all 24 client call sites (wow-re
/// `sheath-policy.md`): **only the manual `ToggleSheath` passes `bInstant = 0`** — every reactive
/// trigger and the server-field apply snap.
#[derive(Message, Clone, Copy)]
pub(crate) struct SheathRequest {
    pub(crate) entity: Entity,
    /// The requested state: 0 unarmed/stowed · 1 melee drawn · 2 ranged drawn.
    pub(crate) state: u8,
    /// Play the draw/stow ceremony (the manual toggle's `bInstant = 0`); everything else snaps.
    pub(crate) ceremony: bool,
}

/// One executed draw/stow swap at its hand-touches-weapon moment — fired only by the *ceremony*
/// (the [`VisualSheath`] release at the authored `$SHL`/`$SHR` event). Snap transitions never
/// fire it: in the client the draw/stow sound rides the ceremony clip's own keyframes, and a
/// `bInstant` path plays no clip — so snaps are silent (director-verified on the ref).
/// `sound::sheathe` rings each wielded hand off it.
#[derive(Message, Clone, Copy)]
pub(crate) struct SheathSwapMessage {
    pub(crate) entity: Entity,
    /// `true` = the transition drew the weapons (sheath state nonzero); `false` = stowed them.
    pub(crate) drawing: bool,
}

/// `AnimationData.dbc` policy rows — the WeaponFlags column driving the per-animation sheath
/// reconcile (decision 0080 structure 3). Optional: absent (no client data) the reconcile
/// degrades to the engaged-draw + the remote server-byte pull-through.
#[derive(Resource)]
pub(crate) struct AnimData(pub(crate) benilla_formats::AnimDataCatalog);

/// Load `AnimationData.dbc` off the patch chain at startup (the sound catalogs' pattern).
pub(super) fn load_anim_data(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_anim_data_catalog(&mut chain)
    };
    match loaded {
        Ok(cat) => {
            info!("anim: {} AnimationData policy rows", cat.len());
            commands.insert_resource(AnimData(cat));
        }
        Err(e) => warn!("anim: AnimationData failed to load: {e:#}"),
    }
}

/// Start the draw/stow **ceremony** for a [`SheathRequest`] that asked for one (the manual
/// toggle's `bInstant = 0`): up to two per-arm masked overlays (mainhand → right arm, offhand →
/// left, each by its item's stow family — the byte-verified `& 0x88` pick, resolved through the
/// model's own baked fallback first — decision 0082), plus the [`VisualSheath`] pin holding the
/// *pre-transition* placement (`old_state`) until a clip crosses the authored `$SHL`/`$SHR` swap
/// moment. No playable clips (no weapons, no arm masks — including after resolution) → no overlay,
/// and the transition degrades to the snap.
#[allow(clippy::too_many_arguments)]
pub(super) fn start_sheath_ceremony(
    commands: &mut Commands,
    entity: Entity,
    drv: &mut AnimDriver,
    player: &mut AnimationPlayer,
    anims: &ModelAnimations,
    wielded: Option<&Wielded>,
    old_state: u8,
    catalog: Option<&AnimDataCatalog>,
) {
    let w = wielded.copied().unwrap_or_default();
    let arm_ids = [
        w.main.map(|_| select::sheath_clip(w.main_sheath)),
        w.off.map(|_| select::sheath_clip(w.off_sheath)),
    ];
    let mut nodes = [None, None];
    let mut swap_at = f32::MAX;
    for (arm, id) in arm_ids.iter().enumerate() {
        let Some(c) = id.and_then(|id| find_resolved(anims, id, catalog)) else {
            continue;
        };
        let Some(arm_nodes) = c.arm_nodes else {
            continue;
        };
        let node = if arm == 0 { arm_nodes.0 } else { arm_nodes.1 };
        let active = player.play(node);
        active.replay();
        active.set_weight(SHEATH_OVERLAY_WEIGHT);
        nodes[arm] = Some(node);
        swap_at = swap_at.min(
            c.events
                .iter()
                .find(|e| matches!(&e.ident, b"$SHL" | b"$SHR"))
                .map(|e| e.time)
                .unwrap_or(c.duration * 0.5),
        );
    }
    if nodes.iter().any(Option::is_some) {
        drv.sheath_swap = Some(SheathSwap {
            nodes,
            swap_at,
            unpinned: false,
        });
        commands.entity(entity).insert(VisualSheath(old_state));
    }
}
