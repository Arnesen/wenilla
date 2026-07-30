//! **The exterior scene draws only through doorways you can see.**
//!
//! Standing inside a WMO interior, the reference does not draw the outdoor world at large — it draws
//! it once per *portal window* left over by the interior portal flood, with the view frustum narrowed
//! to that window. We drew it unconditionally, which is why a hillside tree 200 yd outside Stratholme
//! showed through the city's walls (the director's report; wow-re
//! `system/terrain/scratch/interior-exterior-scene-cull.md`, decision 0774).
//!
//! ## The carved law
//!
//! The world-scene driver `0x681070` branches at `0x681101` on `[0xc7b748]`, the camera-containing map
//! object (0 = outdoors):
//!
//! - **Outside leg** (`0x6811ca`): one populate walk `0x682fa0` against the literal full-screen rect
//!   `{0, 0, 1, 1}` — the ordinary frustum. [`ExteriorWindows::Unrestricted`].
//! - **Inside leg** (`0x681120`): the flood leaves a deferred window worklist (count `[0xcbe320]`,
//!   records `0xcbe324`, stride `0x14`), which the driver `rep movsd`s into `0xc7cb7c` (`0x68118b`)
//!   and then walks **once per window** (`0x682fa0`), frustum narrowed to that window's rect.
//!   **`0x681199 jbe 0x681204` skips the entire walk when the count is zero** — a sealed room draws no
//!   exterior at all.
//!
//! `0x682fa0` is the *only* producer for every exterior bucket — ADT terrain (`0x683bf0`), ADT doodads
//! (`0x683700`), the second placement walk (`0x683340`), world WMO placements (`0x6856c0`), all three
//! liquid layers (`0x683ab0`) and the far band (`0x683040`) — and each drain unlinks what it walks. So
//! "no window" really does mean "no exterior content", with no second path to leak through.
//!
//! **Units are exempt and must stay exempt.** A CGUnit rides ClntObjMgr → `0x481540` → `0x607da0` →
//! `0x710b90` into the M2Scene worklist, drained unconditionally at `0x483460`+`0x48368a`; that
//! function contains zero references to `[0xc7b748]`/`[0xcbe320]`. The reference submits outdoor mobs
//! from a sealed room and lets the building's own geometry z-reject them.
//!
//! ## The clip geometry
//!
//! Per window, the reference bilerps the camera's four global corner rays (`0xc7bcd8`) by the window's
//! rect into 8 corners, and `0x6865f0`→`0x686640` builds **6 planes** from them. So the clip volume is
//! a **sub-frustum whose cross-section is the portal's screen-space AABB** (accumulated along the
//! portal chain), *not* its polygon — and the test is **per object, whole AABB** (`0x682f40`), never
//! per primitive. There is no scissor rect and no user clip plane anywhere on the path: a doodad
//! straddling the doorway edge is drawn **whole**, and the silhouette comes from the interior geometry
//! in front of it.
//!
//! We build the same volume the cheap way: an NDC rect is a scale+offset on clip space, so
//! `sub_clip_from_world = rect_to_ndc * clip_from_world` and Bevy's own
//! [`Frustum::from_clip_from_world`] extracts the identical 6 planes. That keeps us on one
//! plane-extraction implementation instead of a private corner-ray port.
//!
//! Windows narrower than [`MIN_WINDOW_NDC`] in either axis are dropped, matching the reference's
//! `[0x8029d0]` reject.
//!
//! ## What is gated so far, and what is knowingly not
//!
//! **Gated:** ADT terrain (`0x683bf0`), ADT doodad placements (`0x683700`) and the WDL far band
//! (`0x683040`) — the buckets that produce the reported symptom, and the ones whose entities have no
//! other `Visibility` writer, so this is their sole authority (decision 0025).
//!
//! **The unit is the drawn object, and for terrain that is the 33.333 yd MCNK cell, not the 533 yd
//! tile** (decision 0780). This is a property of the *spawner*, not of anything here — but the cull is
//! where it bites: a tile-sized box always contains the camera's own ground, so it intersects every
//! window and is admitted whichever way the doorway faces. The cull was correct and looked broken.
//! The far band keeps its whole-tile box, which is the reference's own far-tier granularity.
//!
//! **NOT yet gated, deliberately:** world **WMO placements** (`0x6856c0`) and open-world **liquid**
//! (`0x683ab0`). Both already have a visibility authority — WMO group submeshes are written by the
//! portal PVS apply, liquid by its own lane — and bolting a second writer onto the same entities is
//! precisely the two-authorities bug 0025 forbids. Folding them in means teaching *those* authorities
//! to consume [`ExteriorWindows`], which is a real design step and not a line to sneak in here. Until
//! then a distant building or lake still shows through a wall where the reference would hide it.
//!
//! **Deviation to be honest about:** the reference tests one AABB per *object*; a doodad placement has
//! no root entity in our graph, so we tag and test each **submesh**. Their union is the object, so the
//! only divergence is at a doorway's edge — a submesh entirely outside the window is dropped where the
//! reference would draw the doodad whole. Visible only as a partially-clipped prop in a doorway.

use bevy::camera::primitives::{Aabb, Frustum};
use bevy::camera::visibility::VisibilitySystems;
use bevy::prelude::*;

use crate::player::WorldCamera;
use crate::wmo_portal::{ExteriorWindows, Rect, WmoPvsSet};

/// A window narrower than this fraction of the screen in either axis is dropped — the reference's
/// `0x682fa0` test against `[0x8029d0]` (0.01 of a 0..1 screen; our rects are NDC, so twice that).
const MIN_WINDOW_NDC: f32 = 0.02;

/// Tag for a piece of the **exterior scene** — the content the window worklist gates. Put this on ADT
/// terrain **chunks**, ADT doodad placements, the WDL far band, world WMO placements and open-world
/// liquid. Tag the object that is *drawn*: this is tested per entity, so tagging a container whose box
/// spans far more than any one draw admits the lot (decision 0780).
///
/// **Never on units, players, or anything parented to a WMO group**: the former are exempt by the
/// carved law above, and the latter are already culled by the portal PVS
/// ([`crate::wmo_portal::WmoGroupVis`]) — double-gating them would blank building interiors.
#[derive(Component)]
pub struct ExteriorScene;

/// Ordering handle: [`ExteriorWindows`] is consumed and `Visibility` written after this set.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExteriorCullSet;

/// What the cull actually did on its last run — the instrument that makes a "why is that still
/// drawn?" report answerable without guessing. `windows` is the worklist it read (`None` =
/// [`ExteriorWindows::Unrestricted`], the stand-down leg), `frusta` how many survived
/// [`MIN_WINDOW_NDC`], and `tested`/`hidden` how many tagged objects it reached and rejected.
///
/// Its whole point is that `tested` is the number no other instrument can see: an object drawn
/// through a wall while `hidden == tested` is an object the cull **never reached**, which is a
/// different defect from one it reached and admitted.
#[derive(Resource, Default, Clone, Copy)]
pub struct ExteriorCullVerdict {
    pub windows: Option<usize>,
    pub frusta: usize,
    pub tested: usize,
    pub hidden: usize,
    /// Tagged objects with **no `Aabb`**, which the fail-open arm below admits unconditionally.
    /// Non-zero is not automatically a defect (a tagged non-drawing entity has no bound and draws
    /// nothing), but it is the one number that says how much of the scene the cull is not deciding.
    pub unbounded: usize,
}

pub(crate) struct ExteriorCullPlugin;

impl Plugin for ExteriorCullPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ExteriorCullVerdict>().add_systems(
            PostUpdate,
            apply_exterior_cull
                .in_set(ExteriorCullSet)
                // The windows are written by the flood in `Update`; the cull reads them and must land
                // before Bevy's own visibility pass consumes the result this same frame.
                .after(WmoPvsSet)
                .after(bevy::transform::TransformSystems::Propagate)
                .before(VisibilitySystems::CheckVisibility),
        );
    }
}

/// The sub-frustum for one NDC window rect.
///
/// An NDC rect maps to the full screen by a scale+offset **on clip space** — `clip.x` scaled by
/// `2/(x1-x0)` plus `clip.w` times `-(x0+x1)/(x1-x0)`, and likewise in y — so pre-multiplying
/// `clip_from_world` by that matrix yields a projection whose 6 extracted planes bound exactly the
/// window's sub-frustum. Depth rows are untouched: the window narrows the view laterally and inherits
/// the camera's own near/far.
fn window_frustum(rect: Rect, clip_from_world: &Mat4) -> Option<Frustum> {
    let [x0, y0, x1, y1] = rect;
    let (w, h) = (x1 - x0, y1 - y0);
    if w < MIN_WINDOW_NDC || h < MIN_WINDOW_NDC {
        return None;
    }
    // Column-major: `Mat4::from_cols` takes columns, so the `w`-column carries the offsets.
    let rect_to_ndc = Mat4::from_cols(
        Vec4::new(2.0 / w, 0.0, 0.0, 0.0),
        Vec4::new(0.0, 2.0 / h, 0.0, 0.0),
        Vec4::new(0.0, 0.0, 1.0, 0.0),
        Vec4::new(-(x0 + x1) / w, -(y0 + y1) / h, 0.0, 1.0),
    );
    Some(Frustum::from_clip_from_world(
        &(rect_to_ndc * *clip_from_world),
    ))
}

/// This frame's exterior gate — the window worklist turned into clip volumes, built once and asked
/// per object. **Two systems ask it**, which is the whole point of it being a value rather than a
/// loop: [`apply_exterior_cull`] owns the exterior objects nothing else writes (terrain cells, the
/// far band), and [`crate::debug_panel`]'s model-visibility authority folds the same answer into its
/// own AND for every model submesh, because those already have an owner (decision 0784).
pub(crate) enum ExteriorGate {
    /// Outdoors — the driver's outside leg (`0x6811ca`): the ordinary frustum is the window, so the
    /// gate admits everything and each object goes back to whatever else owns it.
    Open,
    /// Indoors: an object draws only where one of these sub-frusta admits it. **Empty admits
    /// nothing** — the sealed-room case (`0x681199`'s skip), and the one branch that must not fail
    /// open, because failing open here is the bug this whole module exists to fix.
    Windows(Vec<Frustum>),
}

impl ExteriorGate {
    /// Build from the worklist + the world camera. No camera yet ⇒ [`Self::Open`]: nothing is on
    /// screen to leak, and a verdict taken without a view matrix would be arbitrary.
    pub(crate) fn build(
        windows: &ExteriorWindows,
        cam: Option<(&GlobalTransform, &Projection)>,
    ) -> Self {
        let ExteriorWindows::Windows(rects) = windows else {
            return Self::Open;
        };
        let Some((cam_t, proj)) = cam else {
            return Self::Open;
        };
        let clip_from_world = proj.get_clip_from_view() * cam_t.to_matrix().inverse();
        Self::Windows(
            rects
                .iter()
                .filter_map(|r| window_frustum(*r, &clip_from_world))
                .collect(),
        )
    }

    /// Whole-AABB, per object — the reference's `0x682f40`. Not per primitive: a doodad straddling
    /// the doorway edge draws whole, and the interior geometry cuts its silhouette.
    ///
    /// No `Aabb` (mesh still loading, or an entity that draws nothing) ⇒ admitted. A missing bound
    /// is a *timing* gap, not a visibility verdict, and blanking on it would flicker the world as
    /// tiles stream in.
    pub(crate) fn admits(&self, gt: &GlobalTransform, aabb: Option<&Aabb>) -> bool {
        match (self, aabb) {
            (Self::Open, _) | (_, None) => true,
            (Self::Windows(frusta), Some(aabb)) => {
                let world_from_local = gt.affine();
                frusta
                    .iter()
                    .any(|f| f.intersects_obb(aabb, &world_from_local, true, true))
            }
        }
    }

    /// The same test against a WORLD-space bounding **sphere** — the form the particle lane speaks
    /// (`EmitterFade`'s `[rec+0x68]` fade sphere is the owner doodad's bound, and an emitter has no
    /// mesh of its own to carry an `Aabb`). The sphere's axis-aligned box is what gets tested, which
    /// is looser than the sphere and therefore never hides something a sphere test would admit.
    pub(crate) fn admits_sphere(&self, center: Vec3, radius: f32) -> bool {
        let aabb = Aabb::from_min_max(center - Vec3::splat(radius), center + Vec3::splat(radius));
        self.admits(&GlobalTransform::IDENTITY, Some(&aabb))
    }

    fn frusta(&self) -> usize {
        match self {
            Self::Open => 0,
            Self::Windows(f) => f.len(),
        }
    }
}

/// What [`apply_exterior_cull`] reads per object.
type UnownedScene = (
    &'static GlobalTransform,
    Option<&'static Aabb>,
    &'static mut Visibility,
);

/// …and the objects it is allowed to write: exterior scene that nothing else owns. `ModelPart` and
/// `WmoGroupVis` both mean "the model-visibility authority writes this one".
type UnownedSceneFilter = (
    With<ExteriorScene>,
    Without<crate::debug_panel::ModelPart>,
    Without<crate::wmo_portal::WmoGroupVis>,
);

/// Hide every [`ExteriorScene`] object that no window admits — **only those with no other
/// `Visibility` owner**. A model submesh (`ModelPart`) and a WMO group piece (`WmoGroupVis`) are
/// written by [`crate::debug_panel`]'s model-visibility authority, which composes the toggles, the
/// far-clip wall, the distance fade and the portal PVS; a second writer here does not "also cull"
/// them, it *overwrites* all of that every frame (decision 0784). So this system now owns exactly
/// what it is the sole authority for: terrain cells and the WDL far band.
fn apply_exterior_cull(
    windows: Res<ExteriorWindows>,
    cam: Query<(&GlobalTransform, &Projection), With<WorldCamera>>,
    mut scene: Query<UnownedScene, UnownedSceneFilter>,
    mut verdict: ResMut<ExteriorCullVerdict>,
) {
    let set = |vis: &mut Visibility, target: Visibility| {
        if *vis != target {
            *vis = target;
        }
    };
    let gate = ExteriorGate::build(&windows, cam.iter().next());
    let (mut tested, mut hidden, mut unbounded) = (0usize, 0usize, 0usize);
    for (gt, aabb, mut vis) in &mut scene {
        tested += 1;
        unbounded += usize::from(aabb.is_none());
        let admitted = gate.admits(gt, aabb);
        hidden += usize::from(!admitted);
        set(
            &mut vis,
            if admitted {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            },
        );
    }
    *verdict = ExteriorCullVerdict {
        windows: match &*windows {
            ExteriorWindows::Unrestricted => None,
            ExteriorWindows::Windows(rects) => Some(rects.len()),
        },
        frusta: gate.frusta(),
        tested,
        hidden,
        unbounded,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Affine3A;

    /// A camera at the origin looking down −Z (Bevy's convention), 90° fov, square aspect.
    fn clip_from_world() -> Mat4 {
        Projection::Perspective(PerspectiveProjection {
            fov: std::f32::consts::FRAC_PI_2,
            aspect_ratio: 1.0,
            near: 0.1,
            far: 1000.0,
            ..default()
        })
        .get_clip_from_view()
    }

    /// A point admitted by a frustum? (A degenerate AABB at `p`, identity placement.)
    fn admits(f: &Frustum, p: Vec3) -> bool {
        let aabb = Aabb::from_min_max(p - Vec3::splat(0.01), p + Vec3::splat(0.01));
        f.intersects_obb(&aabb, &Affine3A::IDENTITY, true, true)
    }

    /// The full-screen window must behave exactly like the ordinary view frustum — this is the
    /// outside leg's `{0,0,1,1}` rect, and if it ever narrowed, standing outdoors would start
    /// clipping the world.
    #[test]
    fn the_full_screen_window_is_the_whole_frustum() {
        let f = window_frustum([-1.0, -1.0, 1.0, 1.0], &clip_from_world()).expect("full screen");
        assert!(admits(&f, Vec3::new(0.0, 0.0, -10.0)), "straight ahead");
        // At 90° fov the frustum edge is |x| = |z|; well inside it must pass, and behind must not.
        assert!(admits(&f, Vec3::new(5.0, 0.0, -10.0)), "right of centre");
        assert!(admits(&f, Vec3::new(-5.0, 0.0, -10.0)), "left of centre");
        assert!(!admits(&f, Vec3::new(0.0, 0.0, 10.0)), "behind the eye");
    }

    /// The load-bearing property: a window covering only the RIGHT half of the screen must admit
    /// what is on the right and reject what is on the left. This is the whole cull — a doorway on
    /// one side of the view must not let the world in on the other.
    #[test]
    fn a_half_screen_window_rejects_the_other_half() {
        let f = window_frustum([0.1, -1.0, 1.0, 1.0], &clip_from_world()).expect("right half");
        assert!(admits(&f, Vec3::new(5.0, 0.0, -10.0)), "right: admitted");
        assert!(
            !admits(&f, Vec3::new(-5.0, 0.0, -10.0)),
            "left: must be rejected — the window is on the right"
        );
    }

    /// **The granularity of the tagged object is half the cull** (decision 0780). This test is the
    /// same patch of ground twice: once as the 33.333 yd MCNK cell it is drawn as now, once as the
    /// 533.333 yd ADT tile it used to be merged into. The cell is rejected; the tile is admitted,
    /// because a tile is drawn around the camera and so reaches the doorway's side of the view no
    /// matter which side the ground is on. Nothing in `apply_exterior_cull` can fix that — it is
    /// decided by what the spawner tags — which is why re-merging terrain into a per-tile mesh
    /// would silently restore "I can see the hillside through the wall" with every test still green.
    #[test]
    fn a_narrow_window_rejects_a_chunk_of_ground_but_never_a_whole_tile_of_it() {
        // A doorway on the RIGHT of the view; the ground of interest is 200 yd to the LEFT.
        let f = window_frustum([0.6, -1.0, 1.0, 1.0], &clip_from_world()).expect("a right doorway");
        let ground = Vec3::new(-200.0, -2.0, -200.0);

        const CELL: f32 = 33.333 / 2.0;
        let cell = Aabb::from_min_max(
            ground - Vec3::new(CELL, 0.5, CELL),
            ground + Vec3::new(CELL, 0.5, CELL),
        );
        assert!(
            !f.intersects_obb(&cell, &Affine3A::IDENTITY, true, true),
            "an MCNK cell to the left must not come in through a doorway on the right"
        );

        // The ADT tile that CONTAINS that cell. The camera stands on it, so it spans both sides of
        // the view — half a kilometre of ground admitted by a doorway that shows a hundredth of it.
        const TILE: f32 = 533.333 / 2.0;
        let tile = Aabb::from_min_max(Vec3::new(-TILE, -2.5, -TILE), Vec3::new(TILE, -1.5, TILE));
        assert!(
            f.intersects_obb(&tile, &Affine3A::IDENTITY, true, true),
            "a tile-sized box reaches the doorway's side of the view — it can never be culled, and \
             that is exactly why terrain is spawned per chunk"
        );
    }

    /// The reference rejects a window narrower than a hundredth of the screen (`[0x8029d0]`); a
    /// collapsed rect must produce no frustum at all rather than a degenerate one whose planes are
    /// NaN (which `intersects_obb` would answer arbitrarily).
    #[test]
    fn a_collapsed_window_is_dropped_not_degenerate() {
        assert!(window_frustum([0.5, -1.0, 0.5, 1.0], &clip_from_world()).is_none());
        assert!(window_frustum([-1.0, 0.2, 1.0, 0.2001], &clip_from_world()).is_none());
        // Just over the threshold still builds.
        assert!(
            window_frustum([0.0, -1.0, MIN_WINDOW_NDC + 0.001, 1.0], &clip_from_world()).is_some()
        );
    }
}
