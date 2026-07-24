//! **Floating combat text** (decision 0137 phase 2) — the client's WORLDTEXTSTRING engine
//! (wow-re `playername/scratch/worldtext-spawn-and-law.md`, §5-verified 2026-07-05).
//!
//! The law, transcribed: a combat-log SMSG's handler picks a **category 0–5** and submits a string
//! over the damage/outcome **recipient**; the text is **world-anchored** — the unit's OVERHEAD
//! anchor is snapshotted at spawn (`0x6c73f0` → `0x608640`: the posed PlayerName attachment,
//! slot 18 — head height, tracking model stature intrinsically; fallback `feet + scale × bbox_z ×
//! 1.25`), lifted `z − 1/3`, then re-projected to the screen every frame (`0x483ee0`), rising and
//! fading per the category's row in the config table `0xce8828`. Numbers
//! are bare unsigned `"%d"`; misses are localized WORDS (the shipped enUS `GlobalStrings.lua`
//! values, patch-2.MPQ). At most **4 concurrent texts per unit — a hard drop, not an eviction**
//! (`0x6c73f0`). Self-anchored damage text is suppressed at the emitters (Gate A, `0x607140`/
//! `0x6128b0`): outgoing damage floats over the victim, incoming damage never floats over your own
//! head; the XP emitter (category 4) is self-anchored by design and skips the gate. Heals and
//! energize NEVER float in 5875 (chat log only; there is no `CombatHealing` cvar).
//!
//! **The COLOR law** (wow-re `playername/scratch/combattext-color-law.md`, §5-verified
//! 2026-07-06 — the director's "melee vs spell differ" CONFIRMED): the emitters key two bits —
//! `B` (record NULL ⇒ melee, or **AttributesEx3 bit-15** — `SpellRec+0x24`'s word; the note's
//! "+0x25 sign" byte) and `K` (the `0x5efea0` **source-ownership** class: self / owned-by-me /
//! other). Master gate: the CombatDamage cvar. Self melee → **WHITE** (NULL override ⇒ the row
//! default); self spell/periodic → **GOLD** `0xFFFFDE00`; pet melee → **ORANGE** `0xFFFF8400`
//! (PetMeleeDamage cvar); pet spell → GOLD (PetSpellDamage cvar); **any OTHER source is
//! suppressed entirely** — another unit's fight floats nothing (the emitter returns before
//! submitting; it was never "white"). Crit does NOT recolor (it only picks the pop row); there
//! is NO school coloring. The bit-15 flip is implemented at the spell-packet emitters
//! (`net/apply/combat_log.rs::melee_styled`, decision 0376): the flag is
//! `SPELL_ATTR3_NORMAL_RANGED_ATTACK` — set on exactly the ranged basic shots on the real DBC —
//! so a Throw/Auto Shot number floats white off `SMSG_SPELLNONMELEEDAMAGELOG`. Remaining
//! divergence: the `SMSG_SPELLLOGMISS` word site's record push is unpinned — those outcome
//! words stay the row-default white (open).
//!
//! **The render geometry** (wow-re `worldtext-geometry-law.md`, §5-verified, 9af65294 — zero free
//! parameters; the anchor-seat half corrected here, see the seating comment in
//! [`float_combat_text`]): the rise is added to **world z before projection**; the block is
//! **h-centered with its bottom at** the projected point (rising above it), clamped on-screen;
//! the on-screen size is
//! **constant with unit distance** (the depth counter-scale exists only in the *nameplate* path)
//! and composes as `px = round_half_away(v × √(W²+H²))` — `v` the category's interpolated scale
//! value, the diagonal the gx screencoord unit (`0x832a44/48` hold the live aspect basis; the
//! first reading hardcoded its 4:3 value 0.6 — see [`text_px`]), the round being the gx
//! `ScreenToPixelHeight` law (`0x5c6fa0`, wow-re `crates/font/src/screen_pixel.rs`, bit-exact
//! difftested). At 1024×768 (diag 1280): normal number 23 px, crit settles 35 px, pop peaks ~70 px.
//! The font is **DAMAGE_TEXT_FONT** (`0x6c8470` reads the FrameScript global; shipped Fonts.xml:
//! `Fonts\FRIZQT__.TTF` — our atlas default), created flags 0 (`6c8498 xor edx,edx`): no outline.
//!
//! **The ALPHA + SHADOW law** (`time_alpha_fade 0x6c82e0` — wow-re
//! `playername/scratch/worldtext-alpha-shadow-law.md`, §5 pair + the emulated bit-exact difftest,
//! 2026-07-12; the director's "XP text less visible than ref"): the per-tick fade REPLACES the
//! live color's alpha byte (`mov [obj+0x23], bl` — the ARGB high byte), it never multiplies. The
//! plateau between fade-in end and fade-out start is an **unconditional `(0xFF, 0x7F)`** — so the
//! config table's packed alpha (row 4's `0x80`) is dead data, overwritten before it ever renders:
//! the XP text is FULLY OPAQUE mid-life. Fade-in ramps text `255·t`, shadow `127·t` with
//! `t = elapsed/DURATION` (the +0x0c field — not fade-in-end, so the boundary is a pop-in step,
//! not a ramp arrival), and the fade-in branch is tested FIRST (row 1's `fade_out 90 < fade_in
//! 150` has NO plateau: pop ~25 → ~244 at 150 ms, then the long taper). Fade-out over
//! `[fade_out, duration]`: text `255 − clamp(255·u)` down to 0 — but the shadow lane is the
//! byte-true inversion `255 − clamp(127·u, 0, 127)` ∈ [128, 255]: it JUMPS 0x7F→~0xFF at
//! fade-out start and decays only to 128 (verified constants `[0x7ffe58]=255.0`,
//! `[0x811310]=127.0` — not a half factor — `[0x7ffd74]=0.0`; `__ftol` truncates). The shadow is
//! black, `(al<<24)|0` → `SetShadowColor 0x5c27a0` with the static offset `0xce8804 =
//! {0.002, 0.002}` (init `0x6c7c20`), every category, unconditionally.
//!
//! **The STORE seam caps the shadow** (font node `outline-bake-tint.md` §5, byte-verified):
//! `SetShadowColor`'s persistent store `0x5cd650` writes the shadow colour's alpha byte as
//! **`min(shadowA, mainA)`** — and `time_alpha_fade` calls SetColor (text) *then* SetShadowColor
//! every tick, so the RENDERED shadow alpha is `min(shadow lane, text alpha)`. The fade-out
//! lane's byte-true `[128, 255]` inversion is dead past the text's own value: the rendered
//! shadow steps up to ~0xFF at fade-out start, then tracks the text alpha down to **0** — it
//! never lingers at the 128 floor (transcribing the raw lane as the rendered alpha was exactly
//! the black ghost the director reported in the fade tail). The offset statics are a **viewport
//! fraction**, converted per-axis at draw (`0x5c8710` → `ScreenToPixelWidth 0x5c7010` ×W /
//! `ScreenToPixelHeight 0x5c6fa0` ×H, each `+0.5`-truncate rounded): `offset_px =
//! {round(0.002·W), round(0.002·H)}`, drawn **down-right** (the Y-down ortho), shadow pass
//! before main — the whole as-rendered law is wow-re
//! `playername/scratch/worldtext-shadow-render-law.md` (§5 three pairs, 2026-07-13).
//!
//! **The anti-overlap push IS real** — decision 0367, superseding 0363's "none exists" (that
//! verdict was true of the worldtext code and wrong about the system: the push lives in the
//! shared `UIUtil\SmartScreenRect` draw-time solver, wow-re
//! `ui/scratch/smartscreenrect-solver-law.md`, §5 2026-07-13). Every frame each live string
//! submits its desired rect — the clamped projected center ± its **crit-pop-scaled** measured
//! half-extents — to claim **bucket 1** ([`crate::smart_rect`]), gets relocated off the strings
//! already seated this frame, and draws from the returned rect (`0x6c7cc0` → `0x509520`,
//! unconditional). Seat order is the ref's container walk with slots DESCENDING (`0x6c6e00`
//! reads +0x2c→+0x20) over the first-free-slot fill — so the newest co-anchored number usually
//! seats first and the OLDER one jumps to make room. The push re-derives from the anchor every
//! frame: when the blocker expires the string snaps back. On top of that layer ride the timing
//! mechanisms this module already carries (the impact-frame deferral, the fast rise, the crit
//! park-and-pop, the 4-slot hard drop).
//!
//! The emitters here are fed by `net/apply` (the SMSG spawn table lives in the handlers there,
//! mirroring the client's registration); this module owns the law: categories, words, cap,
//! timing/scale/fade, and the projection into [`UiQuads`] (appended *after* the script extract —
//! see [`crate::ui_pass::UiQuadAppend`]).

mod law;

use law::{
    argb, claimed_box_px, fade_alpha, melee_text, scale_value, shadow_offset_px, text_px,
    CATEGORIES,
};
pub(crate) use law::{damage_color, miss_word, spell_text, DamageSource};

use bevy::prelude::*;

use benilla_ui::script::{JustifyH, JustifyV, Outline};

use crate::entities::{overhead_anchor, BoneAttach, OverheadFallback};
use crate::player::WorldCamera;
use crate::ui_pass::{UiQuadAppend, UiQuads};
use crate::ui_text::{layout_text_quads, FontSpec, Justify, UiFontAtlas};

/// Spawn one floating text over `anchor` — written by the `net/apply` handlers (the SMSG spawn
/// table), consumed by [`float_combat_text`]. The emitters' self-suppression (Gate A) happens at
/// the producer, which knows the self guid; by the time a spawn reaches here it is law that it
/// shows (modulo the cap).
#[derive(Message)]
pub(crate) struct CombatTextSpawn {
    pub(crate) anchor: Entity,
    pub(crate) text: String,
    /// Config category 0–5 (see [`CATEGORIES`]).
    pub(crate) category: u8,
    /// The emitter color override (the B/K law, [`damage_color`]); `None` = the category row's
    /// default color.
    pub(crate) color: Option<u32>,
}

/// One live WORLDTEXTSTRING: the spawn-time position snapshot (already `−1/3` lifted — the text is
/// **not** a live unit-tracking handle; a unit that walks away leaves its numbers behind, like the
/// real client) plus birth time and payload. `anchor` is kept for the per-unit cap.
struct WorldText {
    anchor: Entity,
    pos: Vec3,
    born: f64,
    text: String,
    category: u8,
    /// Resolved at spawn: the emitter override, else the category row's default.
    color: u32,
    /// The unit's slot this text occupies (0–3, first-free fill — `0x6c73f0`). Drives the seat
    /// order: the draw walks each unit's slots DESCENDING (`0x6c6e00`).
    slot: u8,
}

/// The live texts (all units pooled; the client's per-unit 4-slot arrays are equivalent to the
/// per-anchor count gate applied at spawn).
#[derive(Resource, Default)]
pub(crate) struct WorldTexts(Vec<WorldText>);

/// The client's per-unit slot count (`PLAYERNAMEDESC` +0x20..+0x2c): a 5th concurrent text over
/// one unit is dropped outright.
const MAX_PER_UNIT: usize = 4;

/// World text draws beneath every scripted UI quad (the real client renders it in the world scene,
/// under all UI): z 0 sorts first against the frame quads' packed strata keys.
const Z_WORLD_TEXT: u64 = 0;

/// Admit a spawn against the per-unit cap: the client scans the unit's 4 slots for the first
/// NULL and **returns** when all are occupied — a hard drop, no eviction, no queue (`0x6c73f0`).
/// The slot index matters beyond the cap: it is the seat-order key (slots walk descending).
fn free_slot(texts: &[WorldText], anchor: Entity) -> Option<u8> {
    (0..MAX_PER_UNIT as u8).find(|s| !texts.iter().any(|t| t.anchor == anchor && t.slot == *s))
}

/// The whole per-frame engine: drain spawns (snapshot + cap), expire the dead, and re-project the
/// living into glyph quads appended to [`UiQuads`] after the script extract. Ordering (the
/// [`UiQuadAppend`] set, after [`UiInput`]) guarantees the mesh rebuild never lands between the
/// script's replace and our append — see `ui_pass`.
#[allow(clippy::too_many_arguments)] // one Bevy system's full input set
pub(crate) fn float_combat_text(
    mut spawns: MessageReader<CombatTextSpawn>,
    mut texts: ResMut<WorldTexts>,
    time: Res<Time>,
    transforms: Query<&Transform>,
    // The camera's FRESH per-frame Transform (written by the controller earlier this frame),
    // not the propagated GlobalTransform (last frame's until PostUpdate — projecting through it
    // drags the text one frame behind the render when the camera moves). The world camera is a
    // root entity (player/setup spawns it bare), so `GlobalTransform::from` is exact.
    camera: Query<(&Camera, &Transform), With<WorldCamera>>,
    mut atlas: Option<ResMut<UiFontAtlas>>,
    mut quads: ResMut<UiQuads>,
    // The overhead-anchor resolution (`0x608640`): the unit's PlayerName attachment joint, else
    // the bbox fallback.
    anchors: Query<&BoneAttach>,
    fallbacks: Query<&OverheadFallback>,
    globals: Query<&GlobalTransform>,
    mounts: Query<(), With<crate::entities::mount::MountChild>>,
    // The frame's claim bucket 1 (worldtext) — cleared and rebuilt every pass; plates own
    // bucket 0 in `vplates` and the two never interact.
    mut bucket: Local<crate::smart_rect::SmartBucket>,
) {
    let now = time.elapsed_secs_f64();
    // Headless (captures/tests) the camera is None — spawns/expiry still run, nothing draws.
    let cam = camera
        .single()
        .ok()
        .map(|(c, pose)| (c, GlobalTransform::from(*pose)));
    let viewport = cam.as_ref().and_then(|(c, _)| c.logical_viewport_size());
    for spawn in spawns.read() {
        let Some(slot) = free_slot(&texts.0, spawn.anchor) else {
            continue; // the 4-slot hard drop
        };
        let Ok(tf) = transforms.get(spawn.anchor) else {
            continue; // anchor despawned before the spawn landed
        };
        // The spawn snapshot (`0x6c73f0`): the unit's OVERHEAD anchor (`0x608640` — head height,
        // [`overhead_anchor`]), then the client's `z − 1/3` lift. NOT feet-anchored.
        let overhead = overhead_anchor(spawn.anchor, tf, &anchors, &fallbacks, &globals, &mounts);
        debug!(
            "fct: \"{}\" (cat {}) over {:?}",
            spawn.text, spawn.category, spawn.anchor
        );
        if crate::dbg_trace::enabled() {
            let via_attach = anchors
                .get(spawn.anchor)
                .is_ok_and(|a| a.points.contains_key(&crate::entities::ATTACH_OVERHEAD));
            crate::dbg_trace::line(
                "fct",
                &format!(
                    "spawn \"{}\" cat={} anchor={:?} pos=({:.3},{:.3},{:.3}) attach18={}",
                    spawn.text,
                    spawn.category,
                    spawn.anchor,
                    overhead.x,
                    overhead.y,
                    overhead.z,
                    via_attach
                ),
            );
        }
        let pos = overhead - Vec3::Y / 3.0;
        texts.0.push(WorldText {
            anchor: spawn.anchor,
            pos,
            born: now,
            text: spawn.text.clone(),
            category: spawn.category,
            color: spawn
                .color
                .unwrap_or(CATEGORIES[spawn.category as usize].color),
            slot,
        });
    }
    texts
        .0
        .retain(|t| ((now - t.born) * 1000.0) < CATEGORIES[t.category as usize].dur_ms as f64);
    if texts.0.is_empty() {
        return;
    }
    let (Some((cam, cam_tf)), Some(atlas)) = (cam, atlas.as_mut()) else {
        return; // headless (captures/tests) or pre-world: age, draw nothing
    };
    let Some(viewport) = viewport else {
        return;
    };
    // The per-frame reset (`0x509500`) + the ref's seat order: per-unit containers in their
    // (stable, arbitrary) creation-list order — the anchor id here — and each unit's slots
    // DESCENDING (`0x6c6e00` walks +0x2c→+0x20). Over the first-free-slot fill that seats the
    // newest co-anchored number first, so the OLDER one is the pushed one — the reference's
    // "the second number makes the first jump up".
    bucket.clear();
    let mut order: Vec<usize> = (0..texts.0.len()).collect();
    order.sort_by_key(|&i| (texts.0[i].anchor, std::cmp::Reverse(texts.0[i].slot)));
    for i in order {
        let t = &texts.0[i];
        let cat = &CATEGORIES[t.category as usize];
        let elapsed_ms = ((now - t.born) * 1000.0) as f32;
        let life = elapsed_ms / cat.dur_ms;
        // The rise enters world z BEFORE the projection (`6c7d29 fadd` into `+0x1c`, projected at
        // `6c7d96`) — no post-projection y term exists.
        let world = t.pos + Vec3::Y * (cat.rise * life);
        let Ok(screen) = cam.world_to_viewport(&cam_tf, world) else {
            continue; // behind the camera
        };
        // The size law: constant with distance (no depth term anywhere in the worldtext path).
        let size_value = scale_value(t.category, life);
        let target_px = text_px(size_value, viewport);
        // Shape at the atlas size nearest the target (crisp bitmaps), then scale the quad rects
        // about the anchor point for the exact size — the crit pop animates through sizes the
        // atlas never baked.
        let shaped_px = atlas.snap_size(target_px);
        let mut glyphs = layout_text_quads(
            atlas,
            &t.text,
            Rect::from_center_size(screen, Vec2::ZERO),
            argb(t.color),
            Justify {
                h: JustifyH::Center,
                v: JustifyV::Middle,
            },
            Z_WORLD_TEXT,
            FontSpec {
                path: None, // DAMAGE_TEXT_FONT = Friz Quadrata (the atlas default), no outline
                height: Some(shaped_px),
                outline: Outline::None,
                paint_halo: true,
                alpha_gradient: None,
            },
        );
        let ratio = target_px / shaped_px;
        let (alpha_text, alpha_shadow) = fade_alpha(cat, elapsed_ms);
        let mut bounds: Option<Rect> = None;
        for q in &mut glyphs {
            q.rect = Rect {
                min: screen + (q.rect.min - screen) * ratio,
                max: screen + (q.rect.max - screen) * ratio,
            };
            // REPLACE, never multiply: the fade byte IS the rendered alpha (module doc, the
            // ALPHA + SHADOW law) — the packed color's alpha byte (row 4's 0x80) never draws.
            q.color[3] = f32::from(alpha_text) / 255.0;
            bounds = Some(bounds.map_or(q.rect, |b| b.union(q.rect)));
        }
        // The seat (`0x6c7cc0` tail → `0x509520`): clamp the projected CENTER half-extents
        // inside the viewport, build the centered AABB from the crit-pop-scaled measured
        // half-extents, run it through claim bucket 1 — normalize → solve → clamp → claim
        // ([`crate::smart_rect`]) — and place the string at the SOLVED rect's center. The ink
        // then sits h-centered with its BOTTOM at that point: the string's own justify pair at
        // creation (`6c8254 push 0x2` in `string_measure_layout`'s `GxuFontCreateString` —
        // vertical justify 2 = BOTTOM through `ComputeAnchor 0x5cdf70`'s `anchor.y = h +
        // anchor.y` arm; horizontal 0 = left, the caller pre-subtracts hw → net h-centered), so
        // the ink rises above the seat point while the claimed rect brackets it — the ref's
        // exact asymmetry.
        if let Some(b) = bounds {
            // The claimed box is the ref's MEASURED BLOCK under its own units quirk — the box
            // the solver sees runs 1/G48 (~1.667×) taller and 1/G44 (~1.25×) wider than the
            // rendered glyphs, the reference's generous size-proportional padding
            // ([`claimed_box_px`], wow-re `worldtext-measured-block-wh-law.md`).
            let claim = claimed_box_px(b.width(), size_value, viewport);
            let (hw, hh) = (claim.x * 0.5, claim.y * 0.5);
            let cx = screen
                .x
                .clamp(hw.min(viewport.x - hw), (viewport.x - hw).max(hw));
            let cy = screen
                .y
                .clamp(hh.min(viewport.y - hh), (viewport.y - hh).max(hh));
            let desired = Rect::new(cx - hw, cy - hh, cx + hw, cy + hh);
            let solved = bucket.resolve(desired, viewport);
            bucket.claim(solved);
            let target = (solved.min + solved.max) * 0.5;
            let shift = target - Vec2::new((b.min.x + b.max.x) * 0.5, b.max.y);
            if shift != Vec2::ZERO {
                for q in &mut glyphs {
                    q.rect = Rect {
                        min: q.rect.min + shift,
                        max: q.rect.max + shift,
                    };
                }
            }
        }
        // The drop shadow: a second draw of the same string — black, the fade's shadow lane
        // (`0x5c27a0` + the `0xce8804` viewport-fraction offset, per-axis rounded). Pushed
        // FIRST so the stable z-sort keeps it behind the fill at the shared Z_WORLD_TEXT key —
        // the client's shadow-before-main double-walk (`0x5c8710`).
        if alpha_shadow > 0 {
            let off = shadow_offset_px(viewport);
            quads.overlays.extend(glyphs.iter().map(|q| {
                let mut s = q.clone();
                s.rect = Rect {
                    min: q.rect.min + off,
                    max: q.rect.max + off,
                };
                s.color = [0.0, 0.0, 0.0, f32::from(alpha_shadow) / 255.0];
                s
            }));
        }
        quads.overlays.append(&mut glyphs);
    }
}

/// The MELEE number/word producer: consumes [`SwingImpact`] (the swing clip's impact keyframe —
/// `creature_anim::impact`, the client's `0x6247d0 → 0x624530` deferral) rather than the packet,
/// so the number pops when the blow lands, with the blood and the flinch. Gate A on entities: a
/// self-player victim floats nothing (incoming damage is chat-log only). Then the color law's
/// source class (`K`): only MY swings (white) or my pet's (orange) draw — another unit's melee
/// on a third party floats nothing at all.
fn melee_impact_text(
    mut impacts: MessageReader<crate::creature_anim::SwingImpact>,
    self_player: Query<(), With<crate::net::SelfPlayer>>,
    self_guid: Res<crate::net::SelfGuid>,
    stores: Query<&crate::net::ObjectStore>,
    mut text: MessageWriter<CombatTextSpawn>,
) {
    for crate::creature_anim::SwingImpact { swing: s, .. } in impacts.read() {
        let Some(victim) = s.victim else { continue };
        if self_player.contains(victim) {
            continue; // Gate A: never over your own head
        }
        // The source-ownership class over the attacker ENTITY (the guid-side twin lives in
        // `net/apply/combat_log.rs::classify_source` for the packet emitters).
        let source = if self_player.contains(s.attacker) {
            DamageSource::Player
        } else if self_guid.0.is_some()
            && stores.get(s.attacker).is_ok_and(|st| {
                st.0.unit_summoned_by() == self_guid.0 || st.0.unit_created_by() == self_guid.0
            })
        {
            DamageSource::Pet
        } else {
            continue; // K = other: never drawn
        };
        let Some(color) = damage_color(source, true) else {
            continue; // the CombatDamage / PetMeleeDamage gates
        };
        if let Some((category, body)) = melee_text(s.hit_info, s.victim_state, s.damage) {
            text.write(CombatTextSpawn {
                anchor: victim,
                text: body,
                category,
                color,
            });
        }
    }
}

/// Registers the spawn message, the pool, and the per-frame engine (after the script extract,
/// inside the append window the mesh rebuild waits on).
pub(crate) struct CombatTextPlugin;

impl Plugin for CombatTextPlugin {
    fn build(&self, app: &mut App) {
        // The Update append window (see [`UiQuadAppend`]): after the camera controller, and
        // projecting through the camera's FRESH Transform (not the stale propagated global).
        app.init_resource::<WorldTexts>()
            .add_message::<CombatTextSpawn>()
            .add_systems(
                Update,
                (melee_impact_text, float_combat_text)
                    .chain()
                    .in_set(UiQuadAppend),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::law::COLOR_SPELL_GOLD;
    use super::*;

    /// The 4-per-unit hard drop and the spawn snapshot: the real system run headless — no camera,
    /// no atlas, and (with no `BoneAttach`/`OverheadFallback` on the bare test entities) the
    /// overhead resolution degenerates to feet + 0 — so the snapshot pins at exactly the −1/3
    /// lift and only the spawn/expire law engages.
    #[test]
    fn cap_is_four_per_unit_hard_drop() {
        let mut app = App::new();
        app.add_plugins(bevy::time::TimePlugin);
        app.init_resource::<UiQuads>();
        app.init_resource::<WorldTexts>();
        app.add_message::<CombatTextSpawn>();
        app.add_systems(Update, float_combat_text);
        let unit = app.world_mut().spawn(Transform::default()).id();
        let other = app.world_mut().spawn(Transform::default()).id();
        for i in 0..6 {
            app.world_mut().write_message(CombatTextSpawn {
                anchor: unit,
                text: format!("{i}"),
                category: 0,
                color: None,
            });
        }
        app.world_mut().write_message(CombatTextSpawn {
            anchor: other,
            text: "7".into(),
            category: 0,
            color: Some(COLOR_SPELL_GOLD),
        });
        app.update();
        let texts = app.world().resource::<WorldTexts>();
        assert_eq!(
            texts.0.iter().filter(|t| t.anchor == unit).count(),
            MAX_PER_UNIT,
            "the 5th and 6th spawns are hard-dropped"
        );
        assert_eq!(texts.0.iter().filter(|t| t.anchor == other).count(), 1);
        assert!((texts.0[0].pos.y - (-1.0 / 3.0)).abs() < 1e-6, "z − 1/3");
        // The color resolution at spawn: NULL override → the row default (white); an override
        // (the gold spawn over `other`) rides through verbatim.
        assert_eq!(texts.0[0].color, 0xFFFF_FFFF);
        let gold = texts.0.iter().find(|t| t.anchor == other).unwrap();
        assert_eq!(gold.color, COLOR_SPELL_GOLD);
    }
}
