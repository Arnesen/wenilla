//! Live descriptor **appearance** changes (decision 0695): a `Values` delta that moves
//! `UNIT_FIELD_DISPLAYID` / `GAMEOBJECT_DISPLAYID` swaps the entity's model in place, and one that
//! moves `OBJECT_FIELD_SCALE_X` eases its render scale — the druid-shapeshift / GM-morph gap
//! (ledger B69/F04) and `NetEntity::scale`'s old standing deferral, closed together because they
//! are one family: the create path interpreted both fields once and nothing ever re-read them.
//!
//! The reference watches both fields through its field-change registry. The DISPLAYID handler
//! reaches the model rebuild (`0x60abe0`), self-gated by "the display record actually changed"
//! (`0x60ae10` against the per-unit display cache `[unit+0xb34]`), re-resolves every model fact
//! from the new display (`0x60afb0 ResolveDisplayInfo`), and re-selects the stand/ride animation
//! (`0x60ce70`) — an **instant** swap, no morph transition (the ghost→alive revive swap rides the
//! same path; a shapeshift's green flash is the spell visual kit, a separate system). The SCALE_X
//! handler instead **eases the render scale over 2 s with a cosine smoothstep** (byte-verified,
//! `0x614bbf`). All wow-re: `questgiver-marker.md` §W6, `w2d2.md` §2.x, `object-layer.md`.
//!
//! Our shape is the house diff-and-rebuild ([`super::mount::refresh_mounts`] /
//! [`super::equipment::refresh_player_looks`]): the visual was BUILT with [`AppliedDisplay`], the
//! live truth is the descriptor store, and a difference tears the visual down for
//! `attach_entity_visuals` to rebuild — fade-skipped (a shapeshift isn't a spawn), waiting out the
//! new model's async load rather than flashing a cube. The collision height restamps **in the same
//! commit** as the swap — the 0645 rule that the collision box and the drawn body can never
//! disagree is exactly why neither restamped alone before this.

use benilla_protocol::EntityKind;
use bevy::prelude::*;

use crate::net::{Guid, NetEntity, ObjectStore};

use super::collision_height::{collision_height_for, CollisionHeight};
use super::{Creatures, VisualAttached};

/// The reference's scale-ease window: 2 s, cosine smoothstep (`0x614bbf`).
const SCALE_EASE_SECS: f32 = 2.0;

/// On the entity: the display id its current visual was BUILT with — [`refresh_live_display`]'s
/// diff key (the `AppliedEquipment` pattern). Stamped by the attach path on every (re)build,
/// cube fallback included (same read, no churn for a model-less unit); torn down with the visual.
#[derive(Component)]
pub(super) struct AppliedDisplay(pub(super) Option<u32>);

/// A live render-scale ease toward [`NetEntity::scale`]: the reference's 2 s cosine smoothstep
/// (`0x614bbf`), ticked by [`tick_scale_ease`] as absolute writes (a mid-ease visual rebuild's
/// snap is simply overwritten next frame, so the ease survives it).
#[derive(Component)]
pub(super) struct ScaleEase {
    from: f32,
    to: f32,
    elapsed: f32,
}

/// The live descriptor's display id for this entity kind — the values-delta twin of the protocol's
/// create-time interpretation (`events/decode.rs` `display_id`): per-kind field, `0`/absent → `None`
/// (a real morph never zeroes it; the create block's absent-is-zero fold means `0` also reads
/// "never sent", so neither tears a visual down to a cube).
fn live_display_id(kind: EntityKind, store: &ObjectStore) -> Option<u32> {
    let raw = match kind {
        EntityKind::Unit | EntityKind::Player => store.0.unit_displayid(),
        EntityKind::GameObject => store.0.gameobject_displayid(),
        _ => None,
    }?;
    (raw > 0).then_some(raw as u32)
}

/// Diff each attached entity's live descriptor appearance against what its visual was built with,
/// and apply the change: a **display-id** move swaps the model (teardown → rebuild, the
/// [`super::mount::refresh_mounts`] shape) and a **scale** move arms the 2 s ease — both restamp
/// [`CollisionHeight`] in the same commit (its two inputs are exactly these two fields; decision
/// 0645's stamp-once rule was correct only while neither could change).
///
/// The self-avatar needs nothing special: it is the streamed entity (decision 0042), so the swap
/// rebuilds its body like any other unit and `player::mirror_self_collision_height` re-syncs the
/// swim lines from the restamp next frame. Mount children carry no [`ObjectStore`], so they can
/// never take this path (their display is the host's field, diffed by `refresh_mounts`).
#[allow(clippy::type_complexity)]
pub(super) fn refresh_live_display(
    mut commands: Commands,
    creatures: Option<Res<Creatures>>,
    mut entities: Query<
        (
            Entity,
            &Guid,
            &mut NetEntity,
            &ObjectStore,
            &AppliedDisplay,
            Option<&CollisionHeight>,
            &Transform,
        ),
        With<VisualAttached>,
    >,
) {
    for (entity, guid, mut net, store, applied, height, tf) in &mut entities {
        let mut restamp = false;

        // ── The display-id swap ──────────────────────────────────────────────────────────────
        let live = live_display_id(net.kind, store);
        if let Some(live) = live {
            if applied.0 != Some(live) {
                info!(
                    "display swap: guid {:016x} {:?} {:?} -> {} (instant, the 0x60abe0 rebuild)",
                    guid.0, net.kind, applied.0, live
                );
                net.display_id = Some(live);
                restamp = true;
                // The refresh_mounts teardown set + our own diff key: children (parts, joints,
                // held roots, mount child) despawn, the per-instance visual components strip, and
                // `attach_entity_visuals` rebuilds next frame(s) with the new display —
                // fade-skipped via `Reattached` (a shapeshift isn't a spawn).
                commands
                    .entity(entity)
                    .despawn_related::<Children>()
                    .remove::<(
                        VisualAttached,
                        AppliedDisplay,
                        super::equipment::AppliedEquipment,
                        super::mount::AppliedMount,
                        super::mount::MountChild,
                        AnimationPlayer,
                        bevy::animation::transition::AnimationTransitions,
                        AnimationGraphHandle,
                        benilla_assets::ModelAnimations,
                        crate::creature_anim::AnimDriver,
                        super::BoneAttach,
                        super::equipment::HeldAttached,
                    )>()
                    .insert(super::equipment::Reattached);
            }
        }

        // ── The scale ease ───────────────────────────────────────────────────────────────────
        // The same kinds the create path scales (`events/decode.rs` `object_scale`): a kind whose
        // create ignored the field must keep ignoring its deltas, or the first delta would "fix"
        // a scale the create deliberately floored to 1.0.
        let scaled_kind = matches!(
            net.kind,
            EntityKind::Unit | EntityKind::Player | EntityKind::GameObject
        );
        if let Some(live) = store.0.object_scale_x().filter(|s| *s > 0.0 && scaled_kind) {
            if live != net.scale {
                info!(
                    "scale change: guid {:016x} {} -> {} (2 s cosine ease, 0x614bbf)",
                    guid.0, net.scale, live
                );
                net.scale = live;
                restamp = true;
                commands.entity(entity).insert(ScaleEase {
                    from: tf.scale.x,
                    to: live,
                    elapsed: 0.0,
                });
            }
        }

        // One restamp per frame however many inputs moved: both CollisionHeight inputs live here.
        // Snapped to the TARGET scale immediately (the swim/wade/splash lines move once) — easing
        // a collision plane would drag the resolver through two seconds of intermediate depths.
        if restamp {
            let h = collision_height_for(creatures.as_deref(), net.display_id, net.scale);
            if height != Some(&h) {
                debug!(
                    "collision height restamp: guid {:016x} {:?} -> {:.3}",
                    guid.0,
                    height.map(|c| c.0),
                    h.0
                );
            }
            commands.entity(entity).insert(h);
        }
    }
}

/// Tick every live [`ScaleEase`]: `scale(t) = from + (to − from) · (0.5 − 0.5·cos(π·t/2s))` — the
/// reference's cosine smoothstep (`0x614bbf`) — then land exactly on the target and retire.
pub(super) fn tick_scale_ease(
    mut commands: Commands,
    time: Res<Time>,
    mut easing: Query<(Entity, &mut Transform, &mut ScaleEase)>,
) {
    for (entity, mut tf, mut ease) in &mut easing {
        ease.elapsed += time.delta_secs();
        let t = (ease.elapsed / SCALE_EASE_SECS).min(1.0);
        let w = 0.5 - 0.5 * (std::f32::consts::PI * t).cos();
        tf.scale = Vec3::splat(ease.from + (ease.to - ease.from) * w);
        if t >= 1.0 {
            tf.scale = Vec3::splat(ease.to);
            commands.entity(entity).remove::<ScaleEase>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ease's shape is the reference's (`0x614bbf`): starts at `from`, cosine-smooth (half-way
    /// in value at half-way in time), lands exactly on `to` at 2 s and holds.
    #[test]
    fn scale_ease_is_the_2s_cosine_smoothstep() {
        let w = |elapsed: f32| {
            let t = (elapsed / SCALE_EASE_SECS).min(1.0);
            0.5 - 0.5 * (std::f32::consts::PI * t).cos()
        };
        assert_eq!(w(0.0), 0.0);
        assert!((w(1.0) - 0.5).abs() < 1e-6); // cos(π/2) = 0 → half-way in value at 1 s
        assert_eq!(w(2.0), 1.0);
        assert_eq!(w(3.0), 1.0); // clamped past the window
                                 // Smoothstep, not linear: the first quarter of the window covers less than a quarter
                                 // of the value (the eased head), symmetric with the tail.
        assert!(w(0.5) < 0.25);
        assert!(w(1.5) > 0.75);
    }
}
