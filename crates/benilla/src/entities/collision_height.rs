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
/// `CreatureModelData.collisionHeight × render scale`. The column is the model's own MD20
/// collision-box Z extent in raw model units — machine-pinned against the shipped client
/// (`benilla_formats` `collision_height_is_the_m2_collision_box`), which is what settles that it
/// scales with the geometry it bounds. The scale is [`NetEntity::scale`] = `OBJECT_FIELD_SCALE_X`,
/// the *complete* render scale the server has already folded the DBC scales into and the one value
/// our renderer sizes the model by — so the collision box and the drawn body cannot disagree.
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
    let raw = creatures
        .zip(display_id)
        .and_then(|(c, id)| c.collision_height(id))
        // A zero column is real in the data (67 rows — invisible triggers and the like); it means
        // "no authored box", not "a unit of no height", so it falls back like a missing row.
        .filter(|h| *h > 0.0)
        .unwrap_or(crate::player::DEFAULT_COLLISION_HEIGHT);
    CollisionHeight(raw * scale.max(f32::MIN_POSITIVE))
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
