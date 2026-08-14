//! **A body standing in the world** — what the engine needs to know about a unit, and nothing
//! about where it came from.
//!
//! Five engine lanes filtered on `net::NetEntity` and `net::SelfPlayer` to mean "a streamed unit"
//! and "the viewer's own", and one of them also read the wire record's scale and the game's
//! collision-height component. None of that is wire knowledge: the fade resolver wants to know
//! which root is the avatar so first person can feather it, the shade sampler wants the same, the
//! room tracker wants every body that can be inside a building, the palette census wants to label
//! its lanes, and the foam emitter wants a cylinder with a radius. A world renderer cannot depend
//! on a protocol crate's entity record to answer any of them.
//!
//! So the game states it. [`WorldUnit`] on every unit entity, [`ViewerUnit`] on the one the eye
//! belongs to — both written by whatever spawns bodies, both meaningless to a program that spawns
//! none, which is exactly the property `benilla-worldview` needs.
//!
//! **State it at the spawn when the body must be live the same frame.** `entities::
//! publish_world_units` reconciles every `NetEntity` into a `WorldUnit`, but it runs between the
//! wire drain and the rest of the frame — which is right for bodies that arrive on the wire, and
//! one frame late for anything spawned after that slot. The water-foam fixture spawns in
//! `WorldStage::Present` and lost its first frame of ripple to exactly this, shifting every
//! ripple's age for the whole aged capture. Treat `WorldUnit` like `Transform`: part of what it
//! means to spawn a body, with the reconciler as the wire path's safety net rather than the only
//! writer.
//!
//! **This is a restatement, not a second source of truth**, and the difference matters: the fields
//! are copies of facts the game already owns (`NetEntity::scale`, `CollisionHeight`), refreshed by
//! one system on change. A stale copy is a wrong foam radius, not a wrong world — and the
//! alternative, handing the engine the wire record, is the dependency this whole stage exists to
//! delete.

use bevy::prelude::*;

/// A unit body the world can act on: it wades, it takes ground shade, it claims a WMO room, it
/// holds a rig palette slot.
///
/// **Every field is an engine fact.** The first cut carried the wire's own `EntityKind` here and
/// let the foam ask "is this a `TYPEID_UNIT`" — which made a 37 k-line protocol crate a dependency
/// of the renderer for one three-line question, and put the wire's vocabulary in the engine's
/// mouth. 1177 swapped the question for the one the engine actually has ([`wades`](Self::wades))
/// and `benilla-protocol` left this crate's manifest; that absence is the whole gate.
#[derive(Component, Clone, Copy, Debug)]
pub struct WorldUnit {
    /// Does this body **displace water** — does it push a ripple ring and drag a wake.
    ///
    /// A creature or a player does; a chest, a mailbox or a spell's ground anchor standing in a
    /// lake does not. The game decides (it is the only side that knows what it spawned), the
    /// engine only reads.
    ///
    /// A required field rather than a marker component, deliberately: an unstated marker is a body
    /// that silently makes no foam, and this component exists precisely because that class of
    /// omission "nobody notices until a screenshot". A field the compiler makes you answer cannot
    /// be forgotten.
    pub wades: bool,
    /// The instance's model scale, as the ripple ring's radius input.
    pub scale: f32,
    /// The collision cylinder's height in yards, the ring's other input.
    ///
    /// A body whose display has not resolved yet carries the **client's ctor default**, never
    /// `0.0` — at zero every depth line collapses and the body swims on dry land, which is what
    /// `entities::CollisionHeight`'s own `Default` impl exists to prevent. The first cut of this
    /// component defaulted it to zero and the water-foam fixture caught it: 91 k pixels of wrong
    /// ripple, in a lane none of the six scenery captures covers.
    pub height: f32,
    /// The box the world may **cull this body by**, in model space — or `None` for a body the world
    /// must not decide.
    ///
    /// The third of this component's size facts, beside [`scale`](Self::scale) and
    /// [`height`](Self::height), and here for the same reason they are: one place answers "how big
    /// is this body", and a second component for the culling half is the drift that ends with two
    /// extents disagreeing.
    ///
    /// It exists because standing in a sealed WMO room the reference never *submits* an outdoor
    /// object at all (`crate::exterior_cull`, decision 1270) — so the cull needs one whole-object
    /// AABB per body, on its root, which is the reference's own granularity. The game supplies the
    /// armed idle's authored CAaBox (decision 0637 — a skinned body's bind-pose box is not where it
    /// draws), unscaled: the root transform already carries the display scale.
    ///
    /// `None` says **don't decide**, and it is a real answer, not an absence — a body whose display
    /// has not resolved, and a transport, whose root `Visibility` its own tick owns every frame
    /// (a second writer there is precisely the fight decision 0025 forbids). Either way the body
    /// keeps drawing, which is what it did before this field existed.
    pub bound: Option<bevy::camera::primitives::Aabb>,
}

/// …and this one is the **viewer's own** body. A marker rather than a bool on [`WorldUnit`]
/// because every engine use of it is a query *filter* (`Has<…>`, `Without<…>`), which a field
/// cannot be.
#[derive(Component)]
pub struct ViewerUnit;
