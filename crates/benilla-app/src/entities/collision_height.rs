//! The per-unit **collision height** ([`CollisionHeight`]) — the reference's `CMovement+0xb4`,
//! derived from the unit's display id and stamped on every streamed entity (decision 0645).
//!
//! It lives beside the display cache because that is where a display id becomes model facts; every
//! *consumer* is elsewhere (`player::swim`, `sound::water`, `sound::footsteps`, `water_fx`,
//! `net::motion::spline`), which is exactly why it is a component and not five private constants.

use bevy::prelude::*;

use crate::net::NetEntity;

use super::Creatures;

/// A unit's **collision height** in world yards — the reference's `CMovement+0xb4`, and the `h`
/// that every depth line in the client is a fraction of: swim enters and rests at `0.75·h`, the
/// splash fires at `0.4·h`, wading ends where swimming begins (`0.75·h`), and the foam gate is
/// `max(2·h, 1.0)`. Each of those *fractions* is byte-verified against the real client (wow-re
/// `swim-transition.md`, `swim-mechanism.md`, `water-ripple-decal.md`); until decision 0645 the
/// `h` they multiplied was a **constant** — [`crate::player::DEFAULT_COLLISION_HEIGHT`], the
/// client's own empty-world ctor default — so all five lines were right for a human male (2.031,
/// within 2 mm of it) and wrong for every other race. A gnome female stands 1.15 yd and was held
/// 1.52 yd under: she could not reach the surface, which is how the defect finally showed (B76).
///
/// `CreatureModelData.collisionHeight × k`. The column is the model's own MD20 collision-box Z
/// extent in raw model units — machine-pinned against the shipped client (`benilla_formats`
/// `collision_height_is_the_m2_collision_box`), which is what settles that it scales with the
/// geometry it bounds.
///
/// **`k = max(OBJECT_FIELD_SCALE_X, CreatureDisplayInfo.creatureModelScale)`** — VERIFIED byte law
/// (wow-re `collision/scratch/mover-collision-scalars.md`): `0x60b270` fetches `SCALE_X` through
/// vtable slot `+0x1c` (`0x469f10`), compares it against the display row's own column at
/// `0x60b312` (`CreatureDisplayInfo+0x10`), and hands the **larger** to `0x6174b0`, which stores
/// `CollisionHeight · k` into `CMovement+0xb4` at `0x617501`. Its two callers include the unit
/// model (re)build `0x5fb9dd` with `force = 1`, so every unit gets it, not just the mover.
///
/// The `max` is a **floor, never a second multiplier**. The *render* scale is [`NetEntity::scale`]
/// = `OBJECT_FIELD_SCALE_X` alone, which the server has already folded the DBC scales into
/// (`CreatureModelData.modelScale × CreatureDisplayInfo.scale`, vmangos `Unit::GetScaleForDisplayId`
/// — and wow-re `object-layer.md`'s render-scale CORRECTION says in as many words that a client
/// multiplying the display column in again would *square* it). So the collision box and the drawn
/// body still cannot disagree; the floor only stops the box shrinking below the display's own size.
///
/// **At rest the floor is inert, which is why its absence hid**: not one of the 430 shipped
/// `CreatureModelData` rows scales below 1.0, so the folded `SCALE_X ≥ CreatureDisplayInfo.scale`
/// always and `max` picks `SCALE_X`. It bites exactly where `SCALE_X` drops *below* the display's
/// column — a shrink aura, and the **390** `creature_template` rows on our own server whose
/// `display_scale` override undercuts it (by up to 6×). Those units swam at a fraction of their
/// real depth. The floor can only ever *raise* `h`, so no unit that swims correctly today stops.
///
/// **Not** the movement capsule: that stays the constant-height tunable it has always been
/// ([`crate::player::CAPSULE_HEIGHT`] — see its doc for why the two are deliberately separate).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub(crate) struct CollisionHeight(pub(crate) f32);

impl Default for CollisionHeight {
    /// The client's own ctor default — never `0.0`, at which every depth line collapses and the
    /// unit swims on dry land. This is what makes the type safe to hold in a `Default`-derived
    /// struct ([`crate::player::Player`] does).
    fn default() -> Self {
        Self(crate::player::DEFAULT_COLLISION_HEIGHT)
    }
}

/// A unit's collision height from its display id and render scale — [`CollisionHeight`]'s one
/// derivation, shared by the ECS stamp below and the avatar's mirror of it
/// (`player::wire_in`), so the two can never drift into different answers.
pub(crate) fn collision_height_for(
    creatures: Option<&Creatures>,
    display_id: Option<u32>,
    scale: f32,
) -> CollisionHeight {
    let row = creatures.zip(display_id);
    let raw = row
        .and_then(|(c, id)| c.collision_height(id))
        // A zero column is real in the data (67 rows — invisible triggers and the like); it means
        // "no authored box", not "a unit of no height", so it falls back like a missing row.
        .filter(|h| *h > 0.0)
        .unwrap_or(crate::player::DEFAULT_COLLISION_HEIGHT);
    let k = prism_scale(scale, row.and_then(|(c, id)| c.catalog.display_scale(id)));
    CollisionHeight(raw * k)
}

/// `k = max(OBJECT_FIELD_SCALE_X, CreatureDisplayInfo.creatureModelScale)` — the collision prism's
/// multiplier, split out so the FLOOR is assertable on its own (see [`CollisionHeight`] for the
/// byte law and why the floor is not a second multiplier). A display we can't resolve — and the
/// three shipped rows whose column is 0 — contributes nothing and leaves `SCALE_X` standing alone.
/// Never returns 0: at zero every depth line collapses and the unit swims on dry land.
fn prism_scale(scale: f32, display_scale: Option<f32>) -> f32 {
    display_scale
        .map_or(scale, |s| scale.max(s))
        .max(f32::MIN_POSITIVE)
}

/// Give every streamed unit its [`CollisionHeight`] the frame its display resolves. Queries the
/// not-yet-stamped only, so it costs nothing in steady state and self-heals if `Creatures` (the DBC
/// catalog) lands after the first units stream in. A display id that misses the DBCs — or a unit
/// carrying none at all — is still stamped, at the ctor default, so no consumer has to tell
/// "not resolved yet" apart from "resolved to nothing".
///
/// This system stamps only the **first** value. A live change to either input — a display-id swap
/// (druid form, GM morph) or a `SCALE_X` change — restamps through
/// [`super::live_display::refresh_live_display`] (decision 0695), in the same commit as the model
/// swap it rides with: the collision box and the drawn body can never disagree, which is exactly
/// why neither restamped alone before the swap existed (the old F04 deferral).
pub(super) fn stamp_collision_heights(
    mut commands: Commands,
    creatures: Option<Res<Creatures>>,
    units: Query<(Entity, &NetEntity), Without<CollisionHeight>>,
) {
    for (entity, net) in &units {
        let h = collision_height_for(creatures.as_deref(), net.display_id, net.scale);
        commands.entity(entity).insert(h);
    }
}

#[cfg(test)]
mod prism {
    use super::prism_scale;

    /// The **floor**, and the two shapes it has to get right. `SCALE_X` normally already carries
    /// the DBC product (the server folded it), so the display column can only ever equal or
    /// undercut it — which is why the floor is inert on shipped data and bit only when a server
    /// override or a shrink aura pushed `SCALE_X` under the display's own size.
    #[test]
    fn the_display_column_is_a_floor_never_a_multiplier() {
        // The Shore Strider (display 4945): `modelScale` 1.0 × `CDI.scale` 1.75, folded → 1.75.
        // The floor is inert here, which is exactly why the height was NOT B311's cause.
        assert_eq!(prism_scale(1.75, Some(1.75)), 1.75);
        // A `creature_template.display_scale` override under the display's own column (390 rows on
        // our server), or a shrink aura: the prism holds at the display size, it does not shrink.
        assert_eq!(prism_scale(1.0, Some(6.0)), 6.0);
        // A growth aura pushes SCALE_X above the column: the prism grows with it.
        assert_eq!(prism_scale(3.5, Some(1.75)), 3.5);
        // …and it is never a product: 1.75 × 1.75 = 3.06 would be the double-apply.
        assert_ne!(prism_scale(1.75, Some(1.75)), 1.75 * 1.75);
    }

    #[test]
    fn an_unresolved_or_zero_column_leaves_scale_x_alone_and_never_collapses() {
        assert_eq!(prism_scale(1.75, None), 1.75);
        assert_eq!(prism_scale(1.75, Some(0.0)), 1.75, "3 shipped rows carry 0");
        assert!(
            prism_scale(0.0, Some(0.0)) > 0.0,
            "a 0 scale is briefly real"
        );
        assert!(prism_scale(0.0, None) > 0.0);
    }
}
