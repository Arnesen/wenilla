//! The world-map bindings (decision 0203 phase 2) — the Era-shaped map-navigation surface the
//! reference `WorldMapFrame.lua` reads, over the questgiver/quest-log seam shape: the app pushes
//! a static **catalog** once (continents → zones, from `WorldMapArea`/`AreaTable`/
//! `WorldMapContinent`) and a small per-frame **feed** (the player's resolved zone, projected map
//! position, and facing — all projection math stays app-side in `map_proj`); the engine owns the
//! **selection** (which map is displayed) so `SetMapZoom` → `GetCurrentMapContinent` reads back
//! synchronously in the same click, exactly like the quest-log selection precedent.
//!
//! `SetMapZoom`/`SetMapToCurrentZone`/`ProcessMapClick` queue `WORLD_MAP_UPDATE` through the
//! model's pending-event queue (fired at the next tick — the reference's synchronous fire is a
//! repaint trigger, and one tick is invisible at frame rate).
//!
//! Selection encoding (the Lua-visible one, wow-re-confirmed — the client's internal indices
//! `+1`): continent `0` = the world sheet, `1..` = the catalog's continents; zone `0` = the
//! whole continent, `1..` = that continent's zone list. The catalog's *order* defines those
//! indices everywhere (dropdowns, `SetMapZoom`, the feed) — the app builds it under the
//! verified rules (WorldMapArea file order for continents, case-insensitive display-name sort
//! for zones; wow-re Q1(d)/Q3(b) verdicts, 2026-07-07).
//!
//! Continent-level hover/click resolve through the pushed **zone grid** — the 128×128 area
//! bitmap (`Interface\WorldMap\<Continent>.zmp`, remapped app-side to 1-based zone indices; the
//! source + cell law are wow-re-verified, `0x4a6ec0` / the Q1 §5 verdict 2026-07-07). The cell
//! law itself is transcribed here ([`area_grid_cell`]) rather than in the app's `map_proj`
//! because its only callers are these bindings. `UpdateMapHighlight` returns the hovered zone's
//! NAME only for now — the highlight-texture halves (fileName + the `0x4a7fa0` POT extents) land
//! with phase 3's overlay machinery, which shares them.
//!
//! Zone maps carry **exploration fog**: the base tiles are the unexplored parchment, and each
//! discovered sub-area's overlay art (`GetNumMapOverlays`/`GetMapOverlayInfo`, filtered by the
//! pushed `PLAYER_EXPLORED_ZONES` bitset) fills it in — the reference's own overlay pool draws
//! the returned pieces. The landmark family (`GetNumMapLandmarks`/`GetMapLandmarkInfo`) answers
//! with the guard-directions marker (decision 1514); the `AreaPOI.dbc` rows that also belong in
//! it are 0203's still-deferred slice.

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// One zone row in a continent's list. Its 1-based position IS the zone index Lua sees.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorldMapZoneView {
    /// The display name (AreaTable's localized string — "Elwynn Forest").
    pub name: String,
    /// The AreaTable id (the feed's player-zone resolution joins on this; phase 3 overlays too).
    pub area_id: u32,
    /// The `Interface\WorldMap\<file>\` art folder (WorldMapArea's internal name — "Elwynn").
    pub map_file: String,
    /// This zone's WorldMapArea loc rect `(left, right, top, bottom)` — the window the continent
    /// highlight sizes/seats against, and the rect the zone-level `ProcessMapClick` un-lerps
    /// through (wow-re 15b2a8ea: the zone is a rect window onto the one per-continent bitmap).
    pub loc_rect: (f32, f32, f32, f32),
    /// The zone's discovery overlays (WorldMapOverlay rows) — the art that fills the parchment
    /// as sub-areas are explored (`GetNumMapOverlays`/`GetMapOverlayInfo`).
    pub overlays: Vec<WorldMapOverlayView>,
}

/// One **map landmark** — a POI icon on the displayed map (`GetNumMapLandmarks` /
/// `GetMapLandmarkInfo`). The app projects it, so what arrives here is already map UV.
///
/// Today the only landmark is the guard-directions marker (`SMSG_GOSSIP_POI` — the flag a guard
/// drops when you ask where the warrior trainer is), and the reference routes it through this same
/// list: its landmark-list builder `0x4a67a0` appends the marker as the one element with
/// `+0x10 == 1`, exempt from every gate a real AreaPOI must pass (VERIFIED, wow-re
/// `system/ui/scratch/gossip-poi-marker.md`). The `AreaPOI.dbc` rows that also belong here are
/// 0203's still-deferred landmark slice, and land in this list when they do.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorldMapLandmarkView {
    /// The label its tooltip shows ("Stormwind Warrior Trainer").
    pub name: String,
    /// The second tooltip line — a live status string on the AreaPOI rows that carry one (a
    /// battleground node's "In Conflict"); empty for the guard marker.
    pub description: String,
    /// The `Interface\Minimap\POIIcons` cell, 8×8 grid (the guard marker's is `6` — the red flag).
    pub texture_index: u32,
    /// Position on the DISPLAYED map, `[0,1]` UV from its top-left.
    pub uv: (f32, f32),
}

/// One discovery overlay: a sub-area's map art, drawn over the base tiles once any of its
/// explore bits is set (decision 0203 phase 3).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorldMapOverlayView {
    /// The full texture path prefix (`Interface\WorldMap\<Zone>\<TexName>`) — the reference Lua
    /// appends the piece number (`..1`, `..2`, …) exactly as the client's C return does.
    pub texture: String,
    /// The overlay's art size in map px (the Lua slices it into 256-tiles with POT file sizes).
    pub width: u32,
    pub height: u32,
    /// Placement within the 1002×668 detail frame, px from its TOPLEFT.
    pub offset_x: u32,
    pub offset_y: u32,
    /// The AreaTable `exploreFlag` bit indices that reveal this overlay (any set bit shows it;
    /// empty = never shown — an overlay whose areas are unknown).
    pub explore_bits: Vec<u32>,
}

/// One continent. Its 1-based position IS the continent index Lua sees.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorldMapContinentView {
    /// The display name — Map.dbc's localized MapName ("Eastern Kingdoms", "Kalimdor"), the
    /// client's own dropdown-label source (`0x4a65a0`; wow-re Q3(a) verdict), never the art
    /// folder string.
    pub name: String,
    /// The `Interface\WorldMap\<file>\` art folder.
    pub map_file: String,
    /// This continent's rect on the world sheet, normalized UV `(u0, v0, u1, v1)` — world-level
    /// `ProcessMapClick` picks the first continent (catalog order) containing the click, exactly
    /// the client's AABB walk (`0x4a7100`'s world branch over the record trailer floats). The
    /// app computes it with the verified `0x4a5d00` kernel (`map_proj::continent_sheet_rect`,
    /// from the WorldMapContinent tile bounds); the 5875 rects are disjoint.
    pub world_rect: (f32, f32, f32, f32),
    /// The continent's WorldMapArea world rect (loc left/right/top/bottom) — the rect the
    /// `0x4a6ec0` cell law un-lerps hover/click UVs through ([`area_grid_cell`]).
    pub loc_rect: (f32, f32, f32, f32),
    /// The 128×128 area bitmap, remapped app-side to **1-based zone indices** into
    /// [`Self::zones`] (0 = no zone; empty = no bitmap shipped — hover/click inert). Row-major,
    /// indexed by [`area_grid_cell`].
    pub zone_grid: Vec<u16>,
    /// The continent's zones, in Lua index order.
    pub zones: Vec<WorldMapZoneView>,
}

/// The `0x4a6ec0` cell law, verbatim: clamp the UV, un-lerp through the continent's WorldMapArea
/// rect back to world coords, quantize on the half-ADT world grid (2.9296876e-5 = 1/(64·533.33),
/// the binary's 0x806544), range-check, index `col − row·128` (row is truncated from the negated
/// axis, so it's ≤ 0). Verified against the shipped Azeroth.zmp: Goldshire's world position
/// lands on cell 12735 = Elwynn Forest.
fn area_grid_cell(loc: (f32, f32, f32, f32), u: f32, v: f32) -> Option<usize> {
    const K: f32 = 2.929_687_6e-5;
    let (left, right, top, bottom) = loc;
    let clamp01 = |p: f32| {
        if p >= 1.0 {
            1.0
        } else if p >= 0.0 {
            p
        } else {
            0.0
        }
    };
    let (u, v) = (clamp01(u), clamp01(v));
    let span_u = left - right;
    let span_v = top - bottom;
    if span_u == 0.0 || span_v == 0.0 {
        return None;
    }
    let f1 = 0.5 - ((1.0 - u) * span_u + right) * K;
    let f2 = 0.5 - ((1.0 - v) * span_v + bottom) * K;
    if !(0.0..=1.0).contains(&f1) || !(0.0..=1.0).contains(&f2) {
        return None;
    }
    let col = (f1 * 128.0) as i32; // __ftol: truncate toward zero
    let row = (f2 * -128.0) as i32;
    usize::try_from(col - row * 128)
        .ok()
        .filter(|&i| i < 128 * 128)
}

/// The hovered/clicked child (zone OR city) at the current CONTINENT or ZONE selection: the grid
/// cell's 1-based index into the continent's `zones`, or `None` off-land / off-grid / at world
/// level. The rect that windows the one per-continent 128×128 bitmap is the continent's own loc
/// rect at continent level, the CURRENT zone's loc rect at zone level — there is no per-zone
/// bitmap, a zone/city is just a different rect window (wow-re 15b2a8ea, FUN_004a6ec0 §1c). This
/// is what lets a click on a city's footprint from its neighbouring zone map drill into the city.
fn grid_area(state: &WorldMapState, u: f32, v: f32) -> Option<u16> {
    let (c, z) = state.selection;
    if c == 0 {
        return None;
    }
    let cont = state.continents.get(c as usize - 1)?;
    if cont.zone_grid.is_empty() {
        return None;
    }
    let rect = if z == 0 {
        cont.loc_rect
    } else {
        cont.zones.get(z as usize - 1)?.loc_rect
    };
    let cell = area_grid_cell(rect, u, v)?;
    match cont.zone_grid.get(cell).copied().unwrap_or(0) {
        0 => None,
        zi => Some(zi),
    }
}

/// Power-of-two round-up (≥ n, ≥ 1) — the client's `worldmap_pot_dim` round-up, used for the
/// continent highlight's vertical texcoord crop (`texPercentageY = dim / potdim`).
fn next_pow2(n: i64) -> i64 {
    let mut p = 1i64;
    while p < n {
        p <<= 1;
    }
    p
}

/// The engine-side world-map state: the pushed catalog + feed, and the engine-owned selection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorldMapState {
    /// The static catalog (pushed once at startup; empty until then — every binding degrades to
    /// the world level, where `GetMapInfo` returns nil and the reference Lua falls back to
    /// `"World"`).
    pub continents: Vec<WorldMapContinentView>,
    /// The displayed map: `(continent, zone)`, `(0, 0)` = the world sheet.
    pub selection: (u32, u32),
    /// Where `SetMapToCurrentZone` lands — the player's current `(continent, zone)` resolved by
    /// the app (AreaTable parent walk × the catalog). `None` = unresolvable (instance, loading) →
    /// the world sheet.
    pub player_zone: Option<(u32, u32)>,
    /// `GetPlayerMapPosition("player")` for the CURRENT selection — projected app-side each frame
    /// (`map_proj`), `None`/(0,0) = off this map (the reference hides the blip).
    pub player_uv: Option<(f32, f32)>,
    /// `GetPlayerFacing()` — wow orientation radians (0 = north, counterclockwise-positive). Our
    /// transcribed frame spins its arrow texture with it (`SetRotation`); the reference rotates
    /// an engine-rendered arrow model instead (0203 flags the stand-in).
    pub player_facing: f32,
    /// `GetCorpseMapPosition()` for the CURRENT selection — the corpse-run marker (decision 0308
    /// §5), projected app-side like [`Self::player_uv`]; `None`/(0,0) = no corpse or off this map
    /// (the reference's WorldMapFrame.lua:443-452 hide law).
    pub corpse_uv: Option<(f32, f32)>,
    /// The discovery bitset (`PLAYER_EXPLORED_ZONES_1` — 64 u32 = 2048 bits, bit n = AreaTable
    /// exploreFlag n), pushed by the app when the descriptor changes. Empty = nothing explored.
    pub explored: Vec<u32>,
    /// The POI icons on the displayed map (`GetNumMapLandmarks`/`GetMapLandmarkInfo`), already
    /// projected into its UV — pushed by the app on change, which repaints an open map.
    pub landmarks: Vec<WorldMapLandmarkView>,
}

/// Is explore-bit `n` set in the pushed bitset?
fn explored_bit(explored: &[u32], n: u32) -> bool {
    explored
        .get((n / 32) as usize)
        .is_some_and(|w| w & (1 << (n % 32)) != 0)
}

/// The current selection's zone view, if a zone map is displayed.
fn current_zone(state: &WorldMapState) -> Option<&WorldMapZoneView> {
    let (c, z) = state.selection;
    if c == 0 || z == 0 {
        return None;
    }
    state
        .continents
        .get(c as usize - 1)?
        .zones
        .get(z as usize - 1)
}

/// The displayed zone's REVEALED overlays, in catalog order (the i-th is `GetMapOverlayInfo(i)`).
fn revealed_overlays(state: &WorldMapState) -> Vec<&WorldMapOverlayView> {
    current_zone(state)
        .map(|zone| {
            zone.overlays
                .iter()
                .filter(|o| {
                    o.explore_bits
                        .iter()
                        .any(|&b| explored_bit(&state.explored, b))
                })
                .collect()
        })
        .unwrap_or_default()
}

impl super::UiScript {
    /// Push the static catalog (startup, once the DBCs are read). Queues `WORLD_MAP_UPDATE` so an
    /// already-open frame repaints (startup normally precedes any frame, harmlessly).
    pub fn set_world_map_catalog(&mut self, continents: Vec<WorldMapContinentView>) {
        let mut model = self.model_mut();
        model.worldmap.continents = continents;
        model
            .pending_events
            .push(("WORLD_MAP_UPDATE".to_string(), Vec::new()));
    }

    /// Push the discovery bitset (the app calls this when `PLAYER_EXPLORED_ZONES` changes —
    /// including the first stream-in). Queues `WORLD_MAP_UPDATE` so an open zone map fills in
    /// newly explored art immediately.
    pub fn set_world_map_explored(&mut self, explored: Vec<u32>) {
        let mut model = self.model_mut();
        if model.worldmap.explored != explored {
            model.worldmap.explored = explored;
            model
                .pending_events
                .push(("WORLD_MAP_UPDATE".to_string(), Vec::new()));
        }
    }

    /// Push the displayed map's landmarks (the app projects them; see [`WorldMapLandmarkView`]).
    /// On change, queues `WORLD_MAP_UPDATE` — the repaint that re-seats the POI icons, exactly
    /// the event the overlay push above uses for the same reason.
    pub fn set_world_map_landmarks(&mut self, landmarks: Vec<WorldMapLandmarkView>) {
        let mut model = self.model_mut();
        if model.worldmap.landmarks != landmarks {
            model.worldmap.landmarks = landmarks;
            model
                .pending_events
                .push(("WORLD_MAP_UPDATE".to_string(), Vec::new()));
        }
    }

    /// Push the per-frame feed (player zone/projection/facing — see [`WorldMapState`]).
    pub fn set_world_map_feed(
        &mut self,
        player_zone: Option<(u32, u32)>,
        player_uv: Option<(f32, f32)>,
        player_facing: f32,
        corpse_uv: Option<(f32, f32)>,
    ) {
        let mut model = self.model_mut();
        model.worldmap.player_zone = player_zone;
        model.worldmap.player_uv = player_uv;
        model.worldmap.player_facing = player_facing;
        model.worldmap.corpse_uv = corpse_uv;
    }

    /// The engine-owned selection `(continent, zone)` — the app reads it each frame to project
    /// the feed for the displayed map.
    pub fn world_map_selection(&self) -> (u32, u32) {
        self.model_ref().worldmap.selection
    }

    /// The **normalized position within the map art** at UI-space `(x, y)`, or `None` when the
    /// point isn't over it. Reproduces the reference's own click normalization verbatim —
    /// `WorldMapFrame.xml`'s `WorldMapButton_OnClick`: `u = (x − left)/width`,
    /// `v = (top − y)/height` over `WorldMapButton`'s rect — so the UV handed back is the same one
    /// [`ProcessMapClick`](install) reads and the same one the blips are placed in. The Lua's
    /// effective-scale division cancels here: point and rect are both already in screen units.
    ///
    /// A pure query (fires nothing, mutates nothing). Its consumer is the app-side dev map-jump,
    /// which needs a click's UV *without* going through the faithful click path — that path drills
    /// into a zone, and the reference's law for it must not grow a second meaning.
    pub fn world_map_uv_at(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let id = self.hit_test(x, y)?;
        let model = self.model_ref();
        let fh = model.id_to_frame.get(&id).copied()?;
        if model.arena.frame(fh)?.name.as_deref() != Some("WorldMapButton") {
            return None;
        }
        let r = model.resolved.get(&fh)?;
        let (w, h) = (r.right - r.left, r.top - r.bottom);
        (w > 0.0 && h > 0.0).then(|| ((x - r.left) / w, (r.top - y) / h))
    }
}

/// Clamp + store a selection and queue the repaint event. The shared tail of
/// `SetMapZoom`/`SetMapToCurrentZone`/`ProcessMapClick`.
fn select(model: &mut Model, continent: i64, zone: i64) {
    let c = continent.clamp(0, model.worldmap.continents.len() as i64) as u32;
    let z = if c == 0 {
        0
    } else {
        let n = model.worldmap.continents[c as usize - 1].zones.len() as i64;
        zone.clamp(0, n) as u32
    };
    model.worldmap.selection = (c, z);
    model
        .pending_events
        .push(("WORLD_MAP_UPDATE".to_string(), Vec::new()));
}

/// Register the world-map globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // GetMapContinents() → name1, name2, … (the dropdown list; index = continent number).
    g.set(
        "GetMapContinents",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let names: Vec<Value> = model
                .worldmap
                .continents
                .iter()
                .map(|c| Ok(Value::String(lua.create_string(&c.name)?)))
                .collect::<mlua::Result<_>>()?;
            Ok(MultiValue::from_vec(names))
        })?,
    )?;

    // GetMapZones(continent) → name1, name2, … (that continent's zone list; index = zone number).
    g.set(
        "GetMapZones",
        lua.create_function(|lua, c: i64| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let zones = usize::try_from(c)
                .ok()
                .and_then(|c| c.checked_sub(1))
                .and_then(|c| model.worldmap.continents.get(c))
                .map(|cont| cont.zones.as_slice())
                .unwrap_or(&[]);
            let names: Vec<Value> = zones
                .iter()
                .map(|z| Ok(Value::String(lua.create_string(&z.name)?)))
                .collect::<mlua::Result<_>>()?;
            Ok(MultiValue::from_vec(names))
        })?,
    )?;

    // GetCurrentMapContinent() / GetCurrentMapZone() — the selection halves (0 = world / whole
    // continent). The reference reads them right after SetMapZoom in the same click.
    g.set(
        "GetCurrentMapContinent",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(model.worldmap.selection.0))
        })?,
    )?;
    g.set(
        "GetCurrentMapZone",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(model.worldmap.selection.1))
        })?,
    )?;

    // SetMapZoom(continent[, zone]) — the dropdowns' and zoom-out button's navigation verb.
    g.set(
        "SetMapZoom",
        lua.create_function(|lua, (c, z): (i64, Option<i64>)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            select(&mut model, c, z.unwrap_or(0));
            Ok(())
        })?,
    )?;

    // SetMapToCurrentZone() — the OnShow verb: jump to the player's zone (app-resolved feed).
    g.set(
        "SetMapToCurrentZone",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let (c, z) = model.worldmap.player_zone.unwrap_or((0, 0));
            select(&mut model, i64::from(c), i64::from(z));
            Ok(())
        })?,
    )?;

    // GetMapInfo() → mapFileName (the Interface\WorldMap\<name>\ folder). World level → nil —
    // the reference Lua's own `"World"` fallback exists because the client returned nil there
    // (INFERRED, wow-re Q3 pending).
    g.set(
        "GetMapInfo",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let wm = &model.worldmap;
            let file = match wm.selection {
                (0, _) => None,
                (c, z) => wm.continents.get(c as usize - 1).map(|cont| match z {
                    0 => cont.map_file.clone(),
                    z => cont
                        .zones
                        .get(z as usize - 1)
                        .map(|zone| zone.map_file.clone())
                        .unwrap_or_else(|| cont.map_file.clone()),
                }),
            };
            Ok(match file {
                Some(f) => Value::String(lua.create_string(&f)?),
                None => Value::Nil,
            })
        })?,
    )?;

    // GetCorpseMapPosition() → x, y in [0,1] map UV; (0,0) = no corpse / not on this map — the
    // reference's hide sentinel (WorldMapFrame.lua:445), same shape as the player position below.
    g.set(
        "GetCorpseMapPosition",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let (x, y) = model.worldmap.corpse_uv.unwrap_or((0.0, 0.0));
            Ok((f64::from(x), f64::from(y)))
        })?,
    )?;

    // GetPlayerMapPosition(unit) → x, y in [0,1] map UV; (0,0) = not on this map (hide the
    // blip). Only "player" resolves — party/raid are the party arc's, not this one's (0203).
    g.set(
        "GetPlayerMapPosition",
        lua.create_function(|lua, unit: String| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let (x, y) = match unit.as_str() {
                "player" => model.worldmap.player_uv.unwrap_or((0.0, 0.0)),
                _ => (0.0, 0.0),
            };
            Ok((f64::from(x), f64::from(y)))
        })?,
    )?;

    // GetPlayerFacing() → wow orientation radians (counterclockwise-positive, 0 = north). Feeds
    // the transcribed frame's arrow SetRotation (the reference's engine arrow reads facing
    // natively; a Lua-visible getter is our seam for the texture stand-in).
    g.set(
        "GetPlayerFacing",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(f64::from(model.worldmap.player_facing))
        })?,
    )?;

    // ProcessMapClick(x, y) — normalized click within WorldMapButton (wow-re 15b2a8ea, Part 2).
    // World level: pick the first continent whose sheet-rect contains the click (the 0x4a7100
    // AABB walk; the 5875 rects are disjoint). Continent OR zone level: identical instructions —
    // the 0x4a6ec0 grid cell through the CURRENT selection's rect (the continent's, or the current
    // zone's) → a continent child index → drill. Because a city is just another child, a click on
    // its footprint — from the continent map, or from a neighbouring zone map when the footprint
    // falls in that zone's window — drills into the city's own map.
    g.set(
        "ProcessMapClick",
        lua.create_function(|lua, (x, y): (f32, f32)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if model.worldmap.selection.0 == 0 {
                let hit = model.worldmap.continents.iter().position(|cont| {
                    let (u0, v0, u1, v1) = cont.world_rect;
                    (u0..=u1).contains(&x) && (v0..=v1).contains(&y)
                });
                if let Some(i) = hit {
                    select(&mut model, i as i64 + 1, 0);
                }
            } else if let Some(zi) = grid_area(&model.worldmap, x, y) {
                let c = i64::from(model.worldmap.selection.0);
                select(&mut model, c, i64::from(zi));
            }
            Ok(())
        })?,
    )?;

    // UpdateMapHighlight(x, y) → name, fileName, texPctX, texPctY, textureX, textureY,
    // scrollChildX, scrollChildY (wow-re 15b2a8ea, 0x4a7fa0). Continent level: the hovered zone
    // lights up — fileName = its art folder (the frame draws `<file>\<file>Highlight`), the six
    // coords seat/size/crop that texture over the zone's rect within the continent, keeping the
    // client's f32/f64 asymmetric narrowing. World-level continent highlight and zone-level
    // overlay-name are deferred: those exits return the nil/zero tail (the frame hides the quad).
    g.set(
        "UpdateMapHighlight",
        lua.create_function(|lua, (x, y): (f32, f32)| {
            // (name, fileName, texPctY, textureX, textureY, scrollChildX, scrollChildY); texPctX
            // is the constant 1.0 the continent branch always returns.
            let hit: Option<(String, String, f64, f64, f64, f64, f64)> = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                let wm = &model.worldmap;
                let (c, z) = wm.selection;
                if c == 0 || z != 0 {
                    None
                } else {
                    grid_area(wm, x, y)
                        .and_then(|zi| wm.continents.get(c as usize - 1).zip(Some(zi)))
                        .and_then(|(cont, zi)| cont.zones.get(zi as usize - 1).zip(Some(cont)))
                        .and_then(|(zone, cont)| {
                            let (cl, cr, ct, cb) = cont.loc_rect;
                            let (zl, zr, zt, zb) = zone.loc_rect;
                            let (w, h) = (zl - zr, zt - zb);
                            let (cont_w, cont_h) = (cl - cr, ct - cb);
                            if w == 0.0 || h == 0.0 || cont_w == 0.0 || cont_h == 0.0 {
                                return None;
                            }
                            let recip_x = 1.0f32 / cont_w;
                            let recip_y_f32 = 1.0f32 / cont_h;
                            let texture_x = f64::from(recip_x * w);
                            let texture_y = (1.0f64 / f64::from(cont_h)) * f64::from(h);
                            let scroll_x = f64::from(recip_x * (cl - zl));
                            let scroll_y = f64::from(recip_y_f32 * (ct - zt));
                            // texPctY = dim/potdim, dim = __ftol((h·128)/w) (rect-form K = 128).
                            let dim = ((h * 128.0) / w) as i64;
                            let potdim = next_pow2(dim);
                            let tex_pct_y = if potdim > 0 {
                                dim as f64 / potdim as f64
                            } else {
                                0.0
                            };
                            Some((
                                zone.name.clone(),
                                zone.map_file.clone(),
                                tex_pct_y,
                                texture_x,
                                texture_y,
                                scroll_x,
                                scroll_y,
                            ))
                        })
                }
            };
            match hit {
                Some((name, file, tex_pct_y, tx, ty, sx, sy)) => Ok((
                    Value::String(lua.create_string(&name)?),
                    Value::String(lua.create_string(&file)?),
                    1.0f64,
                    tex_pct_y,
                    tx,
                    ty,
                    sx,
                    sy,
                )),
                None => Ok((
                    Value::Nil,
                    Value::Nil,
                    0.0f64,
                    0.0f64,
                    0.0f64,
                    0.0f64,
                    0.0f64,
                    0.0f64,
                )),
            }
        })?,
    )?;

    // GetNumMapLandmarks() — how many POI icons the displayed map carries. Today that is the
    // guard-directions marker and nothing else; the `AreaPOI.dbc` rows are 0203's deferred
    // landmark slice, and they join this same list when it lands.
    g.set(
        "GetNumMapLandmarks",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.worldmap.landmarks.len() as i64)
        })?,
    )?;

    // GetMapLandmarkInfo(i) → name, description, textureIndex, x, y — the i-th (1-based) landmark
    // (return shape VERIFIED at `0x4a8740`, wow-re `system/ui/scratch/gossip-poi-marker.md`).
    // `textureIndex` indexes `Interface\Minimap\POIIcons`' 8×8 grid — for the guard's marker it is
    // the packet's own `Icon`, verbatim, exempt from the substitution an ordinary landmark gets at
    // zone level; x/y are `[0,1]` UV on the displayed map, the same space `GetPlayerMapPosition`
    // answers in. A landmark with no description answers **nil** there, as the reference does
    // (`0x6f3890` pushes nil on a NULL string) — the guard's marker never carries one.
    g.set(
        "GetMapLandmarkInfo",
        lua.create_function(|lua, i: i64| {
            let landmark = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                usize::try_from(i)
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| model.worldmap.landmarks.get(i).cloned())
            };
            let Some(l) = landmark else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let description = match l.description.is_empty() {
                true => Value::Nil,
                false => Value::String(lua.create_string(&l.description)?),
            };
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(&l.name)?),
                description,
                Value::Integer(i64::from(l.texture_index)),
                Value::Number(f64::from(l.uv.0)),
                Value::Number(f64::from(l.uv.1)),
            ]))
        })?,
    )?;

    // GetNumMapOverlays() — how many of the displayed zone's overlays are REVEALED by the
    // explored bitset (the client's C side filters the same way; the reference Lua draws every
    // returned overlay). 0 at world/continent level.
    g.set(
        "GetNumMapOverlays",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(revealed_overlays(&model.worldmap).len() as i64)
        })?,
    )?;

    // GetMapOverlayInfo(i) → textureName, textureWidth, textureHeight, offsetX, offsetY,
    // mapPointX, mapPointY — the i-th (1-based) revealed overlay. The texture is the full path
    // prefix (the Lua appends the piece number); mapPointX/Y are 0 on every 5875 row.
    g.set(
        "GetMapOverlayInfo",
        lua.create_function(|lua, i: i64| {
            let overlay = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                usize::try_from(i)
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| revealed_overlays(&model.worldmap).get(i).cloned().cloned())
            };
            let Some(o) = overlay else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(&o.texture)?),
                Value::Integer(i64::from(o.width)),
                Value::Integer(i64::from(o.height)),
                Value::Integer(i64::from(o.offset_x)),
                Value::Integer(i64::from(o.offset_y)),
                Value::Integer(0),
                Value::Integer(0),
            ]))
        })?,
    )?;

    Ok(())
}
