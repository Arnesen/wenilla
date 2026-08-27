//! The **motion-jitter meter** (`WOW_JITTER=<name-substr>[,<start_s>]`) — the instrument that turns
//! "that NPC looks shaky up close" into a number, and separates the three things a screenshot
//! conflates: the camera moving, the unit's root moving, and the *pose* moving.
//!
//! A rendered bone's screen position is `pose · root · (origin − camera)`. Each of those three
//! terms is sampled here every frame and reported as its own **first and second differences**:
//!
//! - **Δ (first difference)** is motion. A slow idle has one; so does noise. On its own it cannot
//!   tell them apart, which is why "the model moved 0.4 mm this frame" is not a finding.
//! - **Δ² (second difference)** is *curvature per frame*, and that is the discriminator. Smooth
//!   motion at 60 Hz has almost none — a 2 s idle of amplitude `A` carries `A·ω²·dt² ≈ A/3600`,
//!   i.e. tens of microns for a centimetre-scale sway — while uncorrelated per-frame noise of
//!   amplitude `n` shows up at `≈2n`. Two orders of magnitude apart, from the same series.
//! - **Δ(v) = Δ²/dt²** separates *arithmetic* noise from **judder**: if the pose is smooth in time
//!   but the frame deltas are not, `Δ` is ragged while the velocity `Δ/dt` is clean. That is the
//!   whole difference between "the numbers are wrong" and "the numbers are right and arriving at
//!   uneven times", and it is invisible to any instrument that does not log `dt` beside the pose.
//!
//! **The `mm` columns are milli-YARDS** — yards × 1000, the map unit's own thousandth. On a true
//! 0.9144 m yard the f32 ULP at Elwynn is 0.893 mm, not the 0.9766 these columns print (decision
//! 1618). The pixel columns are unaffected: they are computed from yards throughout, so the unit
//! cancels. Kept as-is rather than renamed because these columns are the comparison surface for two
//! landed records (1609, 1617) and a rescale would silently break every figure in them.
//!
//! Everything is reported in **millimetres and in pixels at the subject's own distance**, because
//! a world-space wobble is only a defect in proportion to how many pixels it covers: the same
//! 1 mm is 0.03 px across a courtyard and 0.6 px pressed against the camera, which is exactly why
//! this class of report always arrives as "…when I'm zoomed right in".
//!
//! ```text
//! WOW_USER=probeN WOW_PASS=pprobeN WOW_CHAR=Probe<n> WOW_NOSOUND=1 \
//!   WOW_PROBE_CHAT=".go xyz -9481.53 76.10 56.57 0" \
//!   WOW_PROBE_CAM="-82.55,0,0@18" WOW_JITTER="guard,22" WOW_PROBE_EXIT_AT=40 cargo run -q -p benilla
//! ```
//!
//! One `JIT` line per frame, TSV, so a series is read by a script rather than by eye. The columns
//! are named in the `JIT#` header the meter prints when it arms.

use bevy::camera::Projection;
use bevy::prelude::*;

use benilla_world::rig_anim::RigPose;
use benilla_world::view::WorldCamera;

use super::ProbeClock;
use crate::names::NameCache;
use crate::net::{Guid, NetEntity, SelfPlayer};

/// `WOW_JITTER`'s parsed knobs plus the two-frame history the differences are taken over.
#[derive(Resource)]
struct JitterMeter {
    /// Lower-cased substring of the subject's name; empty = the nearest rigged non-self unit.
    want: String,
    /// Wall-clock second the meter arms (the world has to have streamed in first).
    at: f32,
    /// Whose history [`Self::prev`] holds — a different entity restarts the series.
    subject: Option<Entity>,
    /// Per-bone model-space translations, last frame and the one before.
    prev: Vec<Vec3>,
    prev2: Vec<Vec3>,
    /// **What a vertex does** — each bone's row applied to a probe point [`FLESH_R`] out along
    /// each local axis, same two-frame window. A bone origin can be bitwise still while the bone
    /// *spins*, and a standing idle is very nearly a pure rotation animation: differencing origins
    /// measures the one channel such a clip does not use. This is the series the eye reads.
    prev_flesh: Vec<Vec3>,
    prev2_flesh: Vec<Vec3>,
    /// This subject's per-bone flesh radii ([`flesh_radii`]) — bind data, so computed once.
    radii: Vec<f32>,
    /// Last frame's **animation** clock: `(node, seek_time, weight)` per playing animation.
    ///
    /// The blind spot this closes. [`ProbeClock`] is `Time<Real>` — the WALL clock — so every `dt`
    /// this meter has ever printed, and the whole `Δ/dt` judder discriminator built on it, divides
    /// by a clock the pose does not advance on. A pose stepping unevenly against a perfectly clean
    /// wall delta is invisible to every other column here, and that is exactly the shape a
    /// whole-skeleton twitch takes. `seek_time` is the clock the pose is actually sampled at.
    prev_anim: Vec<(usize, f32, f32)>,
    /// The same two-frame window on the unit's root and on the camera, world space.
    prev_root: Vec3,
    prev2_root: Vec3,
    prev_cam: Vec3,
    prev2_cam: Vec3,
    /// The consumer **anchors'** world translations (held items, helm, shoulders, cape — the
    /// entity lane 0974 left in absolute space), same two-frame window.
    prev_anc: Vec<Vec3>,
    prev2_anc: Vec<Vec3>,
    /// The **rider** frames the vertex stage actually places an attached model with (1609): the
    /// row-0 translation, in the host's rig frame. The A/B against `prev_anc` above — same
    /// attachment, same frame, one measured in absolute world space and one in the rig's.
    prev_rid: Vec<Vec3>,
    prev2_rid: Vec<Vec3>,
    /// Frames of history held (`< 2` means Δ² is not defined yet).
    depth: u32,
}

pub(crate) struct JitterMeterPlugin;

impl Plugin for JitterMeterPlugin {
    fn build(&self, app: &mut App) {
        let raw = std::env::var("WOW_JITTER").unwrap_or_default();
        // `<substr>`, `<substr>,<start>`, or a bare `<start>` — the subject is optional because
        // "the nearest rigged thing that isn't me" is the whole selection at a 1.6 yd close-up.
        let (want, at) = match raw.rsplit_once(',') {
            Some((n, t)) => (n.trim(), t.trim().parse::<f32>().unwrap_or(0.0)),
            None => match raw.trim().parse::<f32>() {
                Ok(t) => ("", t),
                Err(_) => (raw.trim(), 0.0),
            },
        };
        app.insert_resource(JitterMeter {
            want: want.to_lowercase(),
            at,
            subject: None,
            prev: Vec::new(),
            prev2: Vec::new(),
            prev_flesh: Vec::new(),
            prev2_flesh: Vec::new(),
            radii: Vec::new(),
            prev_anim: Vec::new(),
            prev_root: Vec3::ZERO,
            prev2_root: Vec3::ZERO,
            prev_cam: Vec3::ZERO,
            prev2_cam: Vec3::ZERO,
            prev_anc: Vec::new(),
            prev2_anc: Vec::new(),
            prev_rid: Vec::new(),
            prev2_rid: Vec::new(),
            depth: 0,
        })
        // `Last`, so every writer that can still move the subject this frame — the pose evaluator,
        // the rig finalize, transform propagation, the camera seat — has already run. Sampling in
        // `Update` would measure last frame's world against this frame's pose (the 1398 seam).
        .add_systems(Last, sample_jitter);
    }
}

/// A leaf bone's assumed flesh radius, model-space yards — a fingertip or an eyelid, not a limb.
const LEAF_R: f32 = 0.05;
/// The cap on a derived radius: no bone drives geometry further out than this on a humanoid.
const MAX_R: f32 = 0.45;

/// **Per-bone flesh radius** — how far off its own axis this bone's geometry actually sits, taken
/// as the distance to its farthest child (a bone's length is the best proxy we have here for the
/// flesh it carries), leaves falling back to [`LEAF_R`]. The child's own local translation IS that
/// distance; a standing clip animates rotation, so this is the bind offset in all but name.
///
/// This is load-bearing, not a detail. A single radius for all 117 bones reads a 25 mrad twitch of
/// an *eyelid* bone as ~5 mm of motion, when the eyelid's own vertices sit ~0.02 yd out and move a
/// tenth of that. Sized this way, the probe answers "how far did the geometry this bone drives
/// move", which is the question, instead of "how far did a point 0.2 yd off it move", which
/// flatters every small bone in the skeleton into looking like the worst thing on the model.
fn flesh_radii(parents: &[i16], locals: &[Transform]) -> Vec<f32> {
    let mut r = vec![0.0f32; parents.len()];
    for (child, &p) in parents.iter().enumerate() {
        let Ok(pi) = usize::try_from(p) else { continue };
        if let Some(b) = locals.get(child) {
            if let Some(slot) = r.get_mut(pi) {
                *slot = slot.max(b.translation.length());
            }
        }
    }
    r.iter().map(|&v| v.clamp(LEAF_R, MAX_R)).collect()
}

/// The probe points for one composed rig: three per bone, one along each local axis, each at that
/// bone's own [`flesh_radii`] distance.
fn flesh_probes(model: &[bevy::math::Affine3A], radii: &[f32]) -> Vec<Vec3> {
    let mut out = Vec::with_capacity(model.len() * 3);
    for (i, m) in model.iter().enumerate() {
        let r = radii.get(i).copied().unwrap_or(LEAF_R);
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            out.push(m.transform_point3(axis * r));
        }
    }
    out
}

/// What the meter reads per candidate subject: identity, name key, world pose, composed rig.
type SubjectQuery = (
    Entity,
    &'static Guid,
    &'static NetEntity,
    &'static GlobalTransform,
    &'static RigPose,
);

/// One `JIT` line per frame for the subject: the three terms' Δ and Δ², in mm and in pixels.
#[allow(clippy::too_many_arguments)] // one param per term the reading has to separate
fn sample_jitter(
    mut meter: ResMut<JitterMeter>,
    time: ProbeClock,
    names: Res<NameCache>,
    cam: Query<(&GlobalTransform, &Projection), With<WorldCamera>>,
    window: Query<&Window>,
    subjects: Query<SubjectQuery, Without<SelfPlayer>>,
    globals: Query<&GlobalTransform>,
    kids: Query<&Children>,
    meshes: Query<(), With<Mesh3d>>,
    attach: Query<&crate::entities::BoneAttach>,
    riders: Query<&benilla_world::rig_rider::RigRider>,
    palettes: Res<benilla_world::rig_palette::RigPalettes>,
    players: Query<&AnimationPlayer>,
    // The clock the POSE advances on — `ProbeClock` above is `Time<Real>`, the wall clock, and the
    // gap between the two is the whole defect (`benilla_world::frame_pace`). Logging both side by
    // side is the A/B: paced, `vdt` holds the cadence while `dt` still reports what really happened.
    vtime: Res<Time<bevy::time::Virtual>>,
) {
    let now = time.elapsed_secs();
    if now < meter.at {
        return;
    }
    let (Ok((cam_g, projection)), Ok(window)) = (cam.single(), window.single()) else {
        return;
    };
    let cam_pos = cam_g.translation();
    // Nearest match to the camera — with no substring, nearest rigged unit outright.
    let want = meter.want.clone();
    let Some((entity, guid, net, tf, rig)) = subjects
        .iter()
        .filter(|(_, guid, _, _, _)| {
            want.is_empty()
                || names
                    .peek(guid.0)
                    .is_some_and(|n| n.to_lowercase().contains(&want))
        })
        .min_by(|a, b| {
            let d = |g: &GlobalTransform| g.translation().distance_squared(cam_pos);
            d(a.3).total_cmp(&d(b.3))
        })
    else {
        return;
    };
    let root = tf.translation();
    let dist = root.distance(cam_pos).max(1.0e-3);
    // Pixels per yard AT THE SUBJECT: the vertical half-frame subtends `dist·tan(fovy/2)` yards.
    let fovy = match projection {
        Projection::Perspective(p) => p.fov,
        _ => return,
    };
    let px_per_yd = (window.physical_height() as f32 * 0.5) / (dist * (fovy * 0.5).tan());
    // The rig composes in MODEL space; the root's scale is what carries it onto the screen.
    let scale = tf.scale().max_element();

    let bones: Vec<Vec3> = rig
        .model
        .iter()
        .map(|m| Vec3::from(m.translation))
        .collect();
    if meter.radii.len() != rig.model.len() {
        meter.radii = flesh_radii(&rig.parents, &rig.locals);
    }
    let flesh = flesh_probes(&rig.model, &meter.radii);
    // The anchors are ordinary scene-graph entities in ABSOLUTE world space — the seam 0974
    // named and did not close. Their translations are read exactly as the renderer reads them.
    let ancs: Vec<Vec3> = rig
        .anchors
        .iter()
        .map(|&(_, e)| globals.get(e).map_or(Vec3::ZERO, |g| g.translation()))
        .collect();
    // The rider frames of every attached model hanging off THIS unit, in host-rig space. Their
    // origins are reported too — a rider is only exact while its origin holds still.
    let rids: Vec<(Vec3, Vec3)> = riders
        .iter()
        .filter(|r| r.host == entity)
        .filter_map(|r| palettes.rider_placement(r.slot))
        .collect();
    let dt = time.delta_secs().max(1.0e-6);

    // A new subject (or a bone count that changed under us — a re-skin) restarts the series.
    if meter.subject != Some(entity)
        || meter.prev.len() != bones.len()
        || meter.prev_flesh.len() != flesh.len()
        || meter.prev_anc.len() != ancs.len()
        || meter.prev_rid.len() != rids.len()
    {
        let name = names.peek(guid.0).unwrap_or("?").to_string();
        println!(
            "JIT# t=<s> dt=<ms> dist=<yd> pxyd=<px/yd> | camd1 camd2 (mm) | rootd1 rootd2 (mm) \
             | bone=<max bone> d1 d2 (mm) d1px d2px | flesh=<bone> d1 d2 (mm) d1px d2px v(mm/s) | \
             anc=<n> d1 d2 (mm) d1px d2px | \
             ancstep xyz (mm) | rider=<n> d1 d2 (mm) d1px d2px | window={}x{}   subject={name:?} \
             guid={:#x} display={:?} bones={} scale={scale:.3}",
            window.physical_width(),
            window.physical_height(),
            guid.0,
            net.display_id,
            bones.len(),
        );
        // Name the anchors once: bone id, and how much GEOMETRY hangs under each — the number
        // that says whether a jittering anchor is a held weapon or a bookkeeping node.
        let mut roster = Vec::new();
        for (i, &(bone, e)) in rig.anchors.iter().enumerate() {
            let mut n = 0usize;
            let mut stack = vec![e];
            while let Some(x) = stack.pop() {
                n += usize::from(meshes.contains(x));
                if let Ok(cs) = kids.get(x) {
                    stack.extend(cs.iter());
                }
            }
            roster.push(format!("{i}:bone{bone}/{n}mesh"));
        }
        println!("JIT@ anchors [{}]", roster.join(" "));
        println!(
            "JIT@ riders [{}]",
            riders
                .iter()
                .filter(|r| r.host == entity)
                .map(|r| format!("slot{}@bone{}", r.slot, r.bone))
                .collect::<Vec<_>>()
                .join(" ")
        );
        if let Ok(a) = attach.get(entity) {
            let mut pts: Vec<String> = a
                .points
                .iter()
                .map(|(slot, (bone, _))| format!("slot{slot}=bone{bone}"))
                .collect();
            pts.sort();
            println!("JIT@ attach points [{}]", pts.join(" "));
        }
        meter.subject = Some(entity);
        meter.prev = bones;
        meter.prev_flesh = flesh;
        meter.prev2_flesh = Vec::new();
        meter.prev_anc = ancs;
        meter.prev2_anc = Vec::new();
        meter.prev_rid = rids.iter().map(|&(_, t)| t).collect();
        meter.prev2_rid = Vec::new();
        meter.prev2 = Vec::new();
        meter.prev_root = root;
        meter.prev2_root = Vec3::ZERO;
        meter.prev_cam = cam_pos;
        meter.prev2_cam = Vec3::ZERO;
        meter.depth = 1;
        return;
    }
    if meter.depth < 2 {
        meter.prev2 = std::mem::take(&mut meter.prev);
        meter.prev = bones;
        meter.prev2_flesh = std::mem::take(&mut meter.prev_flesh);
        meter.prev_flesh = flesh;
        meter.prev2_anc = std::mem::take(&mut meter.prev_anc);
        meter.prev_anc = ancs;
        meter.prev2_rid = std::mem::take(&mut meter.prev_rid);
        meter.prev_rid = rids.iter().map(|&(_, t)| t).collect();
        meter.prev2_root = meter.prev_root;
        meter.prev_root = root;
        meter.prev2_cam = meter.prev_cam;
        meter.prev_cam = cam_pos;
        meter.depth = 2;
        return;
    }

    // Δ and Δ² per bone, in model space; the largest bone is the one the eye is reading.
    let (mut d2, mut worst) = (0.0f32, 0usize);
    for (i, &p) in bones.iter().enumerate() {
        let c = (p - 2.0 * meter.prev[i] + meter.prev2[i]).length();
        if c > d2 {
            d2 = c;
            worst = i;
        }
    }
    let bd1 = (bones[worst] - meter.prev[worst]).length();
    // The same two differences on the FLESH probes — rotation included. This is the number that
    // has to be small for the model to read as still; `bd*` above is only its translation half.
    let (mut fd2, mut fworst) = (0.0f32, 0usize);
    // Per-BONE, max over its three probes — so the spread below counts bones, not probe points.
    let mut per_bone = vec![0.0f32; flesh.len() / 3];
    for (i, &p) in flesh.iter().enumerate() {
        let c = (p - 2.0 * meter.prev_flesh[i] + meter.prev2_flesh[i]).length();
        let b = &mut per_bone[i / 3];
        *b = b.max(c);
        if c > fd2 {
            fd2 = c;
            fworst = i;
        }
    }
    let fd1 = (flesh[fworst] - meter.prev_flesh[fworst]).length();
    // **Is it the model, or is it one bone?** The worst bone alone cannot tell those apart, and
    // that is the whole question a report of "the WHOLE model shakes" asks. `fd2med` is the
    // median bone's curvature and `fnbig` the count above a quarter-pixel: one twitching bone
    // leaves the median at zero and the count at 1, while a model-wide shimmer lifts both.
    let mut sorted = per_bone.clone();
    sorted.sort_by(f32::total_cmp);
    let fd2med = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    // The matching Δ median. `fd1`/`fd2` above are read at whichever bone is worst THIS frame, so
    // they are not one series and cannot be differenced against each other; these two are. Δ going
    // to zero on alternate frames is a hold-then-jump stair-step; Δ steady while Δ² alternates is
    // not, and the pair is what tells those apart.
    let mut d1s: Vec<f32> = (0..per_bone.len())
        .map(|b| {
            (0..3)
                .map(|k| (flesh[b * 3 + k] - meter.prev_flesh[b * 3 + k]).length())
                .fold(0.0f32, f32::max)
        })
        .collect();
    d1s.sort_by(f32::total_cmp);
    let fd1med = d1s.get(d1s.len() / 2).copied().unwrap_or(0.0);
    let big_px = 0.25 / (scale * px_per_yd).max(1.0e-6);
    let fnbig = per_bone.iter().filter(|&&c| c > big_px).count();
    // The flesh probe's **velocity**, mm/s — `Δ/dt`, not `Δ`. This is the judder discriminator,
    // and it is the one number the series cannot reconstruct from the others: if the pose is
    // smooth in time but sampled at uneven `dt`, then `Δ ≈ v·dt` is ragged exactly as `dt` is
    // while `v` itself is clean, so `Δ²` reads large (`≈ v·Δ(dt)`) purely from the timing. A pose
    // that is genuinely noisy has no such alibi: its velocity is ragged too. Differencing THIS
    // column is what tells the two apart, and differencing `Δ` alone never can.
    let fvel = fd1 * scale / dt;
    // Same two differences on the anchors, and the WORST AXIS of the worst one: the f32 grid an
    // absolute coordinate lands on is per-axis, so at ~9.5 k yards on one axis and ~60 on another
    // the staircase is one-dimensional — which is the fingerprint, not an aside.
    let (mut ad2, mut aworst) = (0.0f32, 0usize);
    for (i, &p) in ancs.iter().enumerate() {
        let c = (p - 2.0 * meter.prev_anc[i] + meter.prev2_anc[i]).length();
        if c > ad2 {
            ad2 = c;
            aworst = i;
        }
    }
    let ad1 = if ancs.is_empty() {
        0.0
    } else {
        (ancs[aworst] - meter.prev_anc[aworst]).length()
    };
    // The same two differences on the RIDER rows — the number the fix has to move.
    let (mut rd2, mut rworst) = (0.0f32, 0usize);
    for (i, &(_, t)) in rids.iter().enumerate() {
        let c = (t - 2.0 * meter.prev_rid[i] + meter.prev2_rid[i]).length();
        if c > rd2 {
            rd2 = c;
            rworst = i;
        }
    }
    let rd1 = if rids.is_empty() {
        0.0
    } else {
        (rids[rworst].1 - meter.prev_rid[rworst]).length()
    };
    let astep = if ancs.is_empty() {
        Vec3::ZERO
    } else {
        (ancs[aworst] - meter.prev_anc[aworst]).abs()
    };
    // The animation clock, per playing node: how far the POSE's own time advanced this frame, and
    // whether the set of playing animations changed under us (a replay/stop_all snaps the pose).
    let anim: Vec<(usize, f32, f32)> = players
        .get(entity)
        .map(|p| {
            let mut v: Vec<(usize, f32, f32)> = p
                .playing_animations()
                .map(|(n, a)| (n.index(), a.seek_time(), a.weight()))
                .collect();
            v.sort_by_key(|&(n, ..)| n);
            v
        })
        .unwrap_or_default();
    // Δseek on the node we also had last frame; `restart` marks a node set that changed (or a seek
    // that went backwards by more than a loop's worth of frame) — the pose-snap fingerprint.
    let same = anim.len() == meter.prev_anim.len()
        && anim.iter().zip(&meter.prev_anim).all(|(a, b)| a.0 == b.0);
    let dseek = if same {
        anim.iter()
            .zip(&meter.prev_anim)
            .map(|(a, b)| a.1 - b.1)
            .fold(0.0f32, |acc, v| if v.abs() > acc.abs() { v } else { acc })
    } else {
        f32::NAN
    };
    let cam_d1 = (cam_pos - meter.prev_cam).length();
    let cam_d2 = (cam_pos - 2.0 * meter.prev_cam + meter.prev2_cam).length();
    let root_d1 = (root - meter.prev_root).length();
    let root_d2 = (root - 2.0 * meter.prev_root + meter.prev2_root).length();
    // Δ² over dt² is the velocity change the frame actually applied — the term that stays put
    // when a smooth pose is merely sampled at uneven times, and blows up when it is not smooth.
    let dvel = d2 * scale / (dt * dt);
    let mm = |v: f32| v * scale * 1000.0;
    println!(
        "JIT\t{now:.4}\t{:.3}\t{dist:.3}\t{px_per_yd:.1}\t{:.4}\t{:.4}\t{:.4}\t{:.4}\t{worst}\t\
         {:.4}\t{:.4}\t{:.4}\t{:.4}\t{:.1}\t\
         {}\t{:.4}\t{:.4}\t{:.4}\t{:.4}\t{:.1}\t{:.4}\t{:.4}\t{}\t{:.3}\t{}\t{:.4}\t{:.3}\t\
         {}\t{:.4}\t{:.4}\t{:.4}\t{:.4}\t\
         {:.4}\t{:.4}\t{:.4}\t{}\t{:.4}\t{:.4}\t{:.4}\t{:.4}",
        dt * 1000.0,
        cam_d1 * 1000.0,
        cam_d2 * 1000.0,
        root_d1 * 1000.0,
        root_d2 * 1000.0,
        mm(bd1),
        mm(d2),
        bd1 * scale * px_per_yd,
        d2 * scale * px_per_yd,
        dvel * 1000.0,
        fworst / 3,
        mm(fd1),
        mm(fd2),
        fd1 * scale * px_per_yd,
        fd2 * scale * px_per_yd,
        fvel * 1000.0,
        mm(fd1med),
        mm(fd2med),
        fnbig,
        vtime.delta_secs() * 1000.0,
        anim.len(),
        dseek * 1000.0,
        anim.first().map_or(-1.0, |a| a.2),
        aworst,
        ad1 * 1000.0,
        ad2 * 1000.0,
        ad1 * px_per_yd,
        ad2 * px_per_yd,
        astep.x * 1000.0,
        astep.y * 1000.0,
        astep.z * 1000.0,
        rworst,
        rd1 * 1000.0,
        rd2 * 1000.0,
        rd1 * px_per_yd,
        rd2 * px_per_yd,
    );

    meter.prev2 = std::mem::replace(&mut meter.prev, bones);
    meter.prev2_flesh = std::mem::replace(&mut meter.prev_flesh, flesh);
    meter.prev2_anc = std::mem::replace(&mut meter.prev_anc, ancs);
    meter.prev2_rid =
        std::mem::replace(&mut meter.prev_rid, rids.iter().map(|&(_, t)| t).collect());
    meter.prev2_root = std::mem::replace(&mut meter.prev_root, root);
    meter.prev2_cam = std::mem::replace(&mut meter.prev_cam, cam_pos);
    meter.prev_anim = anim;
}
