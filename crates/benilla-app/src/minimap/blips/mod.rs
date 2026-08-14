//! The minimap **blip layer** (decision 0203 phase 3; the byte law per the wow-re fold-back,
//! decision 0337): AreaPOI landmarks + quest dots + the hover tooltip.
//!
//! **Landmarks** — byte-verified end to end (`wow-5875-re` `system/ui/scratch/
//! minimap-poi-questdot.md`, §5 four-pair cross-check; selection `0x6d9a90`, candidates
//! `0x6d8e10`, draw `0x4ed148`/`0x4ee170`):
//! - **Candidates**: `ContinentID == current map` AND `Flags & 1` — nothing else (no faction,
//!   no importance gate; both factions' rows are candidates).
//! - **In-range** (`d/viewRadius ≤ 0.8`): draws the `POIIcons.blp` cell indexed by the DBC
//!   `Icon` column at the POI's true position — but **only** rows with `Flags & 2` (in 5875
//!   data that is exactly the 32 world-PvP tower rows, e.g. the EPL towers). An ordinary
//!   in-range landmark (town/city, `Flags & 2` clear) draws nothing.
//! - **Out-of-range**, `d ≤ 694.444`: the nearest **3**, ranked by (`Importance` **signed**
//!   asc, dist asc) — the rank key is Importance, NOT AreaID (the fold-back's C2 correction —
//!   draw as `Rotating-MinimapArrow` instances ON the 0.8 rim, each **rotated to point at its
//!   POI** (the client bakes direction + `0.8·radius` into the arrow frame's matrix).
//!
//! **Quest dots** — the ObjectIcons classifier `0x4eaa90`: reads the SAME per-guid
//! DIALOG_STATUS cache the overhead `!`/`?` markers mirror (one `SMSG_QUESTGIVER_STATUS`, two
//! consumers), and draws a dot **only for status == 7** (REWARD2) → the gold **cell 3**.
//! Status 6 draws NO dot (the vmangos "6 = red dot" comment is server intent, not this
//! client; blue cell 4 is party members, green cell 2 is never populated in 1.12.1). Dots
//! hard-cull at the live zoom radius in **3-D** world distance (no rim ride), and draw
//! LAST — above every arrow.
//!
//! **Tracking dots** (decisions 0560/0564) — the classifier's fall-through for objects NOT
//! at quest status 7, **byte-carved end to end** (wow-re `minimap-poi-questdot.md` §B2 +
//! `track-predicates.md`, the §5 trio): a GameObject passing `0x5ed2b0` draws the gold
//! **cell 0** (Find Herbs/Minerals), a unit passing `0x5ed210` the red **cell 1**. The
//! masks are the `PLAYER_TRACK_CREATURES`/`PLAYER_TRACK_RESOURCES` descriptor mirror
//! (fields 1104/1105 — `values[UNIT_END] + 0x394/0x395`, byte-exact) with one bit per
//! active tracking aura's MiscValue. A GO tests its lockId → `Lock.dbc` skill-slot
//! `LockType`; a unit its creature type via the 3-way resolver `0x605570` (shapeshift-form
//! override → cached creature template → race/Humanoid), behind two always-show clauses:
//! `UNIT_DYNFLAG_TRACK_UNIT` (Hunter's Mark) and our track-stealthed bit vs the target's
//! CREEP vis-flag (the TRACK_STEALTHED consumer). No alive/dead or faction gate.
//!
//! **Sizes are byte-pinned** (wow-re §SIZE, the 0342 fold-back): every blip constant is
//! frozen once by the CGMinimapFrame ctor against the stock widget's 140.8-px screen basis —
//! dot 8 px, POI icon 16 px, orbit 0.8 × the 70.4-px half-disc; the arrows render their M2
//! models at 768 px/unit (rim, modelScale 0.6) and 1280 px/unit (player). Both arrow models
//! are plain textured quads (measured from the real M2s), so the flat-sprite stand-in is
//! geometry-exact at the model's own quad size — no uv crop, the padding is authored.
//!
//! Remaining residue: the rim arrow model stacks SIX copies of its quad (an alpha-layering
//! trick that solidifies the soft texture edges) — we draw one, a brightness nuance for the
//! director's eye; dots re-project every frame where the reference draws ~1 Hz-stale
//! snapshot coords verbatim (a throttle quirk, deliberately not aped — positions agree for
//! standing NPCs); the subzone grey tint (`0xffb0b0b0`) on dots and their tooltip is drawn
//! from the indoor-containment MISMATCH — the exact `0x670540` compare is INTERIM pending its
//! scoped pin; the tooltip anchor beside the map is an eyeball (the engine handler `0x4eb0c0`
//! law gives content, not the exact seat).

mod dots;

pub(super) use dots::{emit_party_dots, emit_quest_dots, emit_tracking_dots, SelfTracking};

use bevy::ecs::system::NonSendMut;
use bevy::math::Rect;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use benilla_assets::coords::bevy_to_wow;
use benilla_formats::{AreaPoi, AreaPoiCatalog};
use benilla_ui::script::UiScript;

use crate::go_templates::GameObjectTemplates;
use crate::names::NameCache;
use crate::net::{ActiveMover, Guid, GuidIndex, NetCommands, NetEntity, ObjectStore, SelfPlayer};
use crate::ui_pass::{UiQuad, UiQuads, UvRect};

/// The landmark rank radius in yards (`0x8116cc` = 694.444, VERIFIED).
const LANDMARK_RANK_YD: f32 = 694.444;
/// The in/out-of-range split ratio (`0x811730` = 0.8, VERIFIED): at or under it a POI is
/// in-range (icon path); over it, an edge-arrow candidate riding the rim — whose radius is
/// byte-pinned as 0.8 × the frozen 70.4-px half-disc = 56.32 px at stock (= 0.4 × side, the
/// same expression the placement below uses).
const BLIP_EDGE_RATIO: f32 = 0.8;
/// AreaPOI `Flags` bit 0 — the candidacy gate (`zone_rebuild 0x6d8e10`, VERIFIED).
const FLAG_CANDIDATE: u32 = 0x1;
/// AreaPOI `Flags` bit 1 — "draw the in-range icon" (`0x6d9a90`, VERIFIED); the world-PvP
/// tower class in 5875 data.
const FLAG_IN_RANGE_ICON: u32 = 0x2;
/// The frozen basis the client bakes every blip-size constant against: the CGMinimapFrame
/// ctor (`0x4edbc0`, wow-re §SIZE) computes them ONCE from the stock 140-XML-unit widget —
/// which lands at 140.8 screen px on the 1024×768 reference — and never recomputes. Sizes
/// below are that table's byte-derived pixel values over this basis, so at stock geometry we
/// render them exactly and any resized widget scales proportionally.
pub(super) const BLIP_BASIS_PX: f32 = 140.8;
/// The player arrow's quad: `MinimapArrow.m2` is a single full-texture 0.0262 × 0.0263-unit
/// quad rendered at 1280 px per model unit — 33.6 px at stock. Its authored centre sits
/// (+0.0004, +0.00135) model units off the frame origin: ≈(+0.5 right, 1.7 up) screen px,
/// rotating with the facing (model +y = screen-up at zero rotation).
pub(super) const PLAYER_ARROW_QUAD_PX: f32 = 33.6;
pub(super) const PLAYER_ARROW_OFFSET_PX: bevy::math::Vec2 = bevy::math::Vec2::new(0.51, -1.73);
/// The rim arrow's on-screen quad: the `Rotating-MinimapArrow.m2` geometry is a stack of six
/// identical full-texture 0.0500-unit quads (measured from the real M2's 24 vertices — the
/// stack is an alpha-layering trick, z 0/0.0145/0.0291), and the frame renders at 768 px per
/// model unit (modelScale 0.6 × 1280 px/unit): 0.0500 · 768 = 38.4 px. The full 32² texture
/// maps onto it (uv 0..1 on every layer), so the flat-sprite stand-in needs NO uv crop — the
/// art's transparent padding is authored into the model's own quad.
const ARROW_QUAD_PX: f32 = 38.4;
/// An in-range POI icon's quad: 16 × 16 px (`bc7658 = base·0.0125` × 1280, ctor-frozen).
const POI_ICON_PX: f32 = 16.0;
// The dot-layer constants (cell coords, dot sizes, the tracking-predicate constants) live
// with the dot emitters in [`dots`].

/// The blip under the cursor this frame — written by `emit_minimap`'s blip passes, consumed
/// by [`drive_blip_tooltip`]. A quest dot carries the guid (the name resolves at the drive,
/// where the ask-once `NameCache` lives).
#[derive(Resource, Default)]
pub(crate) enum MinimapBlipHover {
    #[default]
    None,
    /// A landmark's name + the cursor's UI-space point at detection (the tooltip's seat).
    Landmark(String, Vec2),
    /// A unit dot's guid (quest gold or tracked red — both resolve through the name cache,
    /// the classifier's own `GetName()` route) + cursor point + whether the dot rendered
    /// GREY (cross-interior — the tooltip line grey-wraps the same way, `|cffb0b0b0`).
    Npc(u64, Vec2, bool),
    /// A tracked GameObject dot: its template name (known synchronously — the same string
    /// `GetName()` returns for a GO) + cursor point + the grey flag.
    TrackedGo(String, Vec2, bool),
}

/// The blip layer's system inputs, tupled so `emit_minimap` stays under Bevy's 16-param
/// ceiling: the quest-status store, the guid→entity index + unit positions, the cursor's
/// window, the hover-out slot, the party state, and the tracking-dot inputs (candidates,
/// our own descriptor's masks, the creature/GO template caches, `Lock.dbc`), plus the
/// `uiScale` dial the tooltip's cursor seat converts through.
pub(super) type BlipInputs<'w, 's> = (
    Res<'w, crate::ui_quest::QuestGiver>,
    Res<'w, GuidIndex>,
    Query<'w, 's, &'static GlobalTransform, With<NetEntity>>,
    Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    ResMut<'w, MinimapBlipHover>,
    Res<'w, crate::ui_script::UiScaleCvar>,
    Res<'w, crate::ui_party::GroupState>,
    TrackedCandidates<'w, 's>,
    Query<'w, 's, &'static ObjectStore, With<SelfPlayer>>,
    Res<'w, NameCache>,
    Res<'w, GameObjectTemplates>,
    Option<Res<'w, crate::go_templates::Locks>>,
);

/// Every streamed object the tracking classifier considers (our own avatar excluded — the
/// player is the arrow, never a dot).
pub(super) type TrackedCandidates<'w, 's> = Query<
    'w,
    's,
    (
        &'static Guid,
        &'static NetEntity,
        &'static GlobalTransform,
        Option<&'static ObjectStore>,
    ),
    Without<ActiveMover>,
>;

/// The shared frame geometry the blip emitters draw in: the widget centre/side, the active
/// branch's world→px scale and view radius, the paint key, and the player's WoW position.
pub(super) struct BlipCtx {
    pub(super) center: Vec2,
    pub(super) side: f32,
    pub(super) px_per_yd: f32,
    /// The live view radius in yards (the active zoom's half-extent / interior radius) —
    /// the in/out-of-range split's denominator and the dot cull's range.
    pub(super) radius_yd: f32,
    pub(super) z: u64,
    pub(super) alpha: f32,
    pub(super) wx: f32,
    pub(super) wy: f32,
    pub(super) wz: f32,
    /// Window cursor in the same y-down logical px as the quad rects; `None` = off-window.
    pub(super) cursor: Option<Vec2>,
    /// The same cursor in UI space (y-up, from the window bottom-left) — the tooltip's
    /// anchor point (the reference seats the blip tooltip at the cursor; director-verified,
    /// exact engine offset pending the anchor-law pin).
    pub(super) cursor_ui: Option<Vec2>,
}

impl BlipCtx {
    /// A WoW world point → its screen offset from the widget centre (north-up: up = +X north,
    /// left = +Y west — the same mapping the tiles and the corpse blip use).
    fn offset(&self, w: [f32; 3]) -> Vec2 {
        Vec2::new(
            (self.wy - w[1]) * self.px_per_yd,
            -(w[0] - self.wx) * self.px_per_yd,
        )
    }
}

/// The landmark selection's two draw lists (`0x6d9a90`'s split).
pub(super) struct LandmarkSelection<'a> {
    /// In-range rows with [`FLAG_IN_RANGE_ICON`]: the `Icon` POIIcons cell at true position.
    pub(super) icons: Vec<&'a AreaPoi>,
    /// The out-of-range nearest-3 `(dist, poi)` in rank order: rim arrows.
    pub(super) arrows: Vec<(f32, &'a AreaPoi)>,
}

/// The byte-verified landmark selection: candidacy (`ContinentID` + `Flags&1`), the 0.8
/// in/out split against the live view radius, and the out-of-range nearest-3 ranked by
/// (`Importance` signed asc, dist asc) within 694.444 yd.
pub(super) fn select_landmarks<'a>(
    pois: impl Iterator<Item = &'a AreaPoi>,
    map_id: u32,
    wx: f32,
    wy: f32,
    radius_yd: f32,
) -> LandmarkSelection<'a> {
    let mut icons = Vec::new();
    let mut arrows: Vec<(f32, &AreaPoi)> = Vec::new();
    for p in pois.filter(|p| p.continent_id == map_id && p.flags & FLAG_CANDIDATE != 0) {
        let d = ((p.pos[0] - wx).powi(2) + (p.pos[1] - wy).powi(2)).sqrt();
        if d / radius_yd <= BLIP_EDGE_RATIO {
            if p.flags & FLAG_IN_RANGE_ICON != 0 {
                icons.push(p);
            }
        } else if d <= LANDMARK_RANK_YD {
            arrows.push((d, p));
        }
    }
    arrows.sort_by(|a, b| {
        (a.1.importance as i32)
            .cmp(&(b.1.importance as i32))
            .then(a.0.total_cmp(&b.0))
    });
    arrows.truncate(3);
    LandmarkSelection { icons, arrows }
}

/// `POIIcons.blp` cell for a DBC `Icon` index as `[l, r, t, b]` tex coords — the 8×8 grid of
/// 16-px cells, gated `Icon < 0x40` exactly like the client (`0x4ed15e`).
fn poi_icon_cell(icon: u32) -> Option<[f32; 4]> {
    if icon >= 64 {
        return None;
    }
    let (c, r) = ((icon % 8) as f32, (icon / 8) as f32);
    Some([c / 8.0, (c + 1.0) / 8.0, r / 8.0, (r + 1.0) / 8.0])
}

/// Draw the landmark layer: in-range POI icons at position, then the nearest-3 rim arrows,
/// each rotated to point at its POI. Records a hover hit (later-drawn wins, matching draw
/// order).
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_landmarks(
    ctx: &BlipCtx,
    cat: &AreaPoiCatalog,
    map_id: u32,
    arrow_tex: &Handle<Image>,
    poi_icons: Option<&Handle<Image>>,
    quads: &mut UiQuads,
    hover: &mut MinimapBlipHover,
) {
    let sel = select_landmarks(
        cat.rows().map(|(_, p)| p),
        map_id,
        ctx.wx,
        ctx.wy,
        ctx.radius_yd,
    );
    if let Some(icons_tex) = poi_icons {
        for poi in &sel.icons {
            let Some(cell) = poi_icon_cell(poi.icon) else {
                continue;
            };
            let rect = Rect::from_center_size(
                ctx.center + ctx.offset(poi.pos),
                Vec2::splat(ctx.side * (POI_ICON_PX / BLIP_BASIS_PX)),
            );
            quads.overlays.push(UiQuad {
                rect,
                z_key: ctx.z,
                texture: Some(icons_tex.clone()),
                uv: UvRect::from_tex_coords(cell),
                color: [1.0, 1.0, 1.0, ctx.alpha],
                ..default()
            });
            if let (Some(c), Some(ui)) = (ctx.cursor, ctx.cursor_ui) {
                if rect.contains(c) {
                    *hover = MinimapBlipHover::Landmark(poi.name.clone(), ui);
                }
            }
        }
    }
    for (_d, poi) in &sel.arrows {
        let dir = ctx.offset(poi.pos).normalize_or_zero();
        let pos = ctx.center + dir * (ctx.side * 0.5 * BLIP_EDGE_RATIO);
        // Point the arrow at its POI: quad rotation is clockwise on a y-down screen, and
        // the authored art reads screen-up at zero, so up→dir is atan2(x, -y).
        let rotation = dir.x.atan2(-dir.y);
        let rect =
            Rect::from_center_size(pos, Vec2::splat(ctx.side * (ARROW_QUAD_PX / BLIP_BASIS_PX)));
        // ONE pass of the texture. The model authors a six-quad alpha stack, but replaying it
        // as flat UI quads read as solid blobs (director, 2026-07-13 — the reference runs it
        // through the model material pipeline, whose blend mode is unread); the single soft
        // pass is the approved look.
        quads.overlays.push(UiQuad {
            rect,
            z_key: ctx.z,
            texture: Some(arrow_tex.clone()),
            color: [1.0, 1.0, 1.0, ctx.alpha],
            rotation,
            ..default()
        });
        if let (Some(c), Some(ui)) = (ctx.cursor, ctx.cursor_ui) {
            if rect.contains(c) {
                *hover = MinimapBlipHover::Landmark(poi.name.clone(), ui);
            }
        }
    }
}

/// A party member's blip position: the live streamed transform wins; out of visibility range the
/// `PARTY_MEMBER_STATS` snapshot position covers (the wire truncates to i16 — yard precision,
/// invisible at minimap scale). No source → no blip (offline members carry no position).
fn party_member_pos(
    m: &benilla_protocol::messages::GroupMemberEntry,
    group: &crate::ui_party::GroupState,
    guids: &GuidIndex,
    unit_pos: &Query<&GlobalTransform, With<NetEntity>>,
) -> Option<(f32, f32)> {
    if let Some(tf) = guids.0.get(&m.guid).and_then(|e| unit_pos.get(*e).ok()) {
        let w = bevy_to_wow(tf.translation());
        return Some((w[0], w[1]));
    }
    group
        .stats
        .get(&m.guid)
        .and_then(|s| s.position)
        .map(|(x, y)| (f32::from(x), f32::from(y)))
}

/// The party/corpse **rim arrows** — the out-of-range half of `place_party_raid_blips`
/// (`0x6dad10`, VERIFIED): `d = √(dx²+dy²)`; over `0.8·radius` the member renders as the same
/// `Rotating-MinimapArrow` the POI edge arrows use (the 5-slot sibling array `this+0x320`,
/// 4 members + own corpse), on the 0.8 rim, rotated to the atan2 bearing. Drawn with the POI
/// arrows — before the player arrow; the in-range members become dots in [`emit_party_dots`],
/// drawn last with the object dots.
pub(super) fn emit_party_arrows(
    ctx: &BlipCtx,
    group: &crate::ui_party::GroupState,
    guids: &GuidIndex,
    unit_pos: &Query<&GlobalTransform, With<NetEntity>>,
    corpse: Option<[f32; 3]>,
    arrow_tex: &Handle<Image>,
    quads: &mut UiQuads,
) {
    let members = group
        .party_slots()
        .filter_map(|m| party_member_pos(m, group, guids, unit_pos));
    for (x, y) in members.chain(corpse.map(|c| (c[0], c[1]))) {
        let d = ((x - ctx.wx).powi(2) + (y - ctx.wy).powi(2)).sqrt();
        if d / ctx.radius_yd <= BLIP_EDGE_RATIO {
            continue; // in range — the dot pass draws it
        }
        let dir = ctx.offset([x, y, 0.0]).normalize_or_zero();
        let pos = ctx.center + dir * (ctx.side * 0.5 * BLIP_EDGE_RATIO);
        let rotation = dir.x.atan2(-dir.y);
        quads.overlays.push(UiQuad {
            rect: Rect::from_center_size(
                pos,
                Vec2::splat(ctx.side * (ARROW_QUAD_PX / BLIP_BASIS_PX)),
            ),
            z_key: ctx.z,
            texture: Some(arrow_tex.clone()),
            color: [1.0, 1.0, 1.0, ctx.alpha],
            rotation,
            ..default()
        });
    }
}

/// Push the hovered blip's name tooltip: a landmark's AreaPOI name or a tracked GO's template
/// name outright, a unit dot's (quest or tracked) name through the ask-once `NameCache` (the
/// first hover may show one frame late while the `SMSG_NAME_QUERY` answer lands — the
/// reference resolves the guid through its own name cache the same way). Hover loss arms the
/// world tooltip fade, never an instant hide. Runs after
/// the mouseover drive (`UnitFeed`) so a same-frame world-hover→blip transition ends with the
/// blip tooltip shown.
pub(super) fn drive_blip_tooltip(
    script: Option<NonSendMut<UiScript>>,
    hover: Res<MinimapBlipHover>,
    mut names: ResMut<NameCache>,
    commands: Res<NetCommands>,
    mut last: Local<Option<(String, Vec2)>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let show = match &*hover {
        MinimapBlipHover::None => None,
        MinimapBlipHover::Landmark(name, at) => Some((name.clone(), *at, false)),
        MinimapBlipHover::Npc(npc, at, grey) => names
            .resolve(*npc, &commands)
            .map(|n| (n.to_string(), *at, *grey)),
        MinimapBlipHover::TrackedGo(name, at, grey) => Some((name.clone(), *at, *grey)),
    };
    match (&show, &*last) {
        // Same blip, cursor drifted: the plate FOLLOWS the pointer (anchor-only re-seat —
        // the reference's blip tooltip tracks the cursor; director-verified).
        (Some((t, at, _)), Some((lt, lat))) if t == lt => {
            if at != lat {
                script.world_tooltip_move(at.x, at.y);
                *last = Some((t.clone(), *at));
            }
        }
        (Some((t, at, grey)), _) => {
            script.minimap_tooltip(t, at.x, at.y, *grey);
            *last = Some((t.clone(), *at));
        }
        (None, Some(_)) => {
            script.world_tooltip_fade();
            *last = None;
        }
        (None, None) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poi(importance: u32, flags: u32, x: f32, name: &str) -> AreaPoi {
        AreaPoi {
            importance,
            icon: 6,
            faction_id: 0,
            pos: [x, 0.0, 0.0],
            continent_id: 0,
            flags,
            area_id: 0,
            name: name.into(),
            description: String::new(),
            world_state_id: 0,
        }
    }

    /// The real Northshire shape (5875 AreaPOI.dbc rows, distances from a player near the
    /// abbey, the default zoom's 133-yd radius): Echo Ridge Mine (Flags 4, no bit 0) is not a
    /// candidate at all; Northshire Abbey (Flags 5, in-range, bit 1 clear) draws neither icon
    /// nor arrow; Stormwind and Goldshire (out-of-range, both Importance 3) become the rim
    /// arrows in distance order.
    #[test]
    fn northshire_shape_under_the_byte_law() {
        let rows = [
            poi(3, 0x5, 46.0, "Northshire Abbey"),
            poi(0, 0x4, 237.0, "Echo Ridge Mine"),
            poi(3, 0x1d, 556.0, "Stormwind"),
            poi(3, 0x5, 601.0, "Goldshire"),
        ];
        let sel = select_landmarks(rows.iter(), 0, 0.0, 0.0, 133.0);
        assert!(sel.icons.is_empty(), "no Flags&2 row is in range");
        let names: Vec<&str> = sel.arrows.iter().map(|(_, p)| p.name.as_str()).collect();
        assert_eq!(names, ["Stormwind", "Goldshire"]);
    }

    /// The rank key is Importance (signed asc), distance only as the tiebreak — a nearer row
    /// of higher Importance ranks BELOW a farther low-Importance one (the fold-back's C2
    /// correction: the key is DBC column 1, not AreaID).
    #[test]
    fn arrow_rank_is_importance_then_distance_capped_at_three() {
        let rows = [
            poi(3, 1, 200.0, "city-near"),
            poi(0, 1, 650.0, "minor-far"),
            poi(0, 1, 300.0, "minor-near"),
            poi(3, 1, 400.0, "city-mid"),
            poi(0, 1, 695.0, "beyond-rank"),
        ];
        let sel = select_landmarks(rows.iter(), 0, 0.0, 0.0, 100.0);
        let names: Vec<&str> = sel.arrows.iter().map(|(_, p)| p.name.as_str()).collect();
        assert_eq!(names, ["minor-near", "minor-far", "city-near"]);
    }

    /// An in-range row draws the icon only with Flags bit 1; the `Icon < 64` gate and the
    /// 8×8 cell math match the client's table build.
    #[test]
    fn in_range_icons_gate_on_flag_bit1_and_the_atlas_bound() {
        let mut tower = poi(0, 0x87, 50.0, "Crown Guard Tower");
        tower.icon = 9; // col 1, row 1
        let plain = poi(3, 0x5, 50.0, "Northshire Abbey");
        let rows = [tower, plain];
        let sel = select_landmarks(rows.iter(), 0, 0.0, 0.0, 133.0);
        assert_eq!(sel.arrows.len(), 0);
        assert_eq!(sel.icons.len(), 1, "only the Flags&2 tower draws in range");
        assert_eq!(
            poi_icon_cell(9),
            Some([0.125, 0.25, 0.125, 0.25]),
            "icon 9 = col 1 row 1 of the 8x8 atlas"
        );
        assert_eq!(poi_icon_cell(64), None, "the client gates Icon < 0x40");
    }
}
