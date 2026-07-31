//! Mounts (decision 0441): the projection from `UNIT_FIELD_MOUNTDISPLAYID` — the wire's one
//! mounted signal — to a second creature visual under the unit. The mount is a child entity
//! carrying a plain creature `NetEntity`, built by the ordinary attach path; the rider's joints
//! root under the mount's attachment-0 seat joint (the `0x60ce70` present-test law); the rider's
//! base pins to Mount(91) while the mount child locomotes through the untouched gait driver, fed
//! the host's own movement view ([`crate::creature_anim`]'s host-view redirect). This module owns
//! the transition: the diff-and-rebuild that mirrors the gear-swap refresh.

use benilla_protocol::EntityKind;
use bevy::prelude::*;

use crate::net::{NetEntity, ObjectStore};

/// The mount child — the second creature visual a mounted unit carries (the client's
/// `unit+0xdc` secondary model instance). Spawned by the attach path when the field is set,
/// despawned by [`refresh_mounts`]' rebuild (or with its parent).
#[derive(Component)]
pub(crate) struct MountBody {
    /// The mounted unit — the streamed entity whose field this child projects. The animation
    /// driver reads the HOST's movement view through this to locomote the mount.
    pub(crate) host: Entity,
}

/// On the unit: its live mount child (the client's `[unit+0xdc]` handle).
#[derive(Component)]
pub(crate) struct MountChild(pub(crate) Entity);

/// On the unit: the mount display id its current visual was BUILT with (`0` = built unmounted) —
/// [`refresh_mounts`]' diff key, the `AppliedEquipment` pattern.
#[derive(Component)]
pub(super) struct AppliedMount(pub(super) u32);

/// Rebuild a unit's visual when its mount field changes (decision 0441): 0→id (mount up), id→0
/// (dismount — the reference tears the secondary model down on the spot, byte-verified
/// `0x607ce0`: detach body, `SetMountModel(0)`, body to Stand, no transition anim), id→id′ (a
/// re-mount). The transition is a full teardown — a mount is a second SKELETON the rider's own
/// rig re-roots under, so there is nothing to re-dress in place the way a gear change is
/// (`attach::redress`): children — mount child, joints, parts, held roots — despawn, the visual components strip, and
/// `attach_entity_visuals` rebuilds next frame(s) in the right configuration, fade-skipped via
/// `Reattached` (mounting up isn't a spawn).
pub(super) fn refresh_mounts(
    mut commands: Commands,
    units: Query<
        (Entity, &NetEntity, &ObjectStore, Option<&AppliedMount>),
        With<super::VisualAttached>,
    >,
) {
    for (entity, net, store, applied) in &units {
        if !matches!(net.kind, EntityKind::Unit | EntityKind::Player) {
            continue;
        }
        let live = store.0.unit_mount_display_id();
        if live == applied.map_or(0, |a| a.0) {
            continue;
        }
        commands
            .entity(entity)
            .despawn_related::<Children>()
            .remove::<(
                super::VisualAttached,
                super::equipment::AppliedEquipment,
                AppliedMount,
                MountChild,
                AnimationPlayer,
                bevy::animation::transition::AnimationTransitions,
                AnimationGraphHandle,
                benilla_assets::ModelAnimations,
                crate::creature_anim::AnimDriver,
                (
                    crate::creature_anim::RigPose,
                    crate::creature_anim::BodyTwist,
                    crate::creature_anim::GlobalSeqDrive,
                ),
                crate::rig_palette::RigSkin,
                super::BoneAttach,
                super::equipment::HeldAttached,
            )>()
            .insert(super::equipment::Reattached);
    }
}
