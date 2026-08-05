//! The capture *scenarios* — the named deterministic viewpoints (camera eye/look in raw WoW
//! coords, pinned game-minute, optional UI fixture) and the golden-scenario table itself. Data
//! only; the capture lifecycle (settle, screenshot, probe) stays in `super`.

/// A named, fully-deterministic capture viewpoint: where the camera sits + looks (raw WoW coords) and
/// the game-minute to render. One scenario → one golden PNG.
#[derive(Clone, Copy)]
pub(super) struct Scenario {
    pub(super) name: &'static str,
    /// `Map.dbc` id the eye/look coords belong to. Raw WoW coords repeat on every continent — the
    /// Felwood spot's tile (`33_24`) exists in Azeroth too, empty — so a scenario that could not
    /// name its map silently photographed the wrong world. The harness seeds
    /// [`crate::world_map::CurrentMap`] from this before streaming starts (decision 0743).
    pub(super) map: u32,
    /// Camera eye, raw WoW coords `(x, y, z)`.
    pub(super) eye: [f32; 3],
    /// Camera look-at target, raw WoW coords.
    pub(super) look: [f32; 3],
    /// Game minute of day (`0..1440`) — pins the time-of-day lighting.
    pub(super) minute: u32,
    /// Open this UI window with canned state before the shot (the UI half of the harness — the
    /// look-pass instrument the 2026-07-03 director round demanded: window fidelity gets checked
    /// by MY eyes on a capture before it ever reaches the director's).
    pub(super) ui: Option<UiFixture>,
}

/// A UI window opened with synthetic-but-realistic state for a deterministic capture. The seed data
/// mirrors what the live server sends (names/icons resolve through the same offline caches the app
/// uses), so the capture exercises the real feed → VM → extract → render chain end to end.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum UiFixture {
    Merchant,
    Gossip,
    Quest,
    /// The bank window (decision 0604 phase 4) fed the REAL way (the QuestLog/Character pattern):
    /// a synthetic self-player whose descriptor carries occupied `PLAYER_FIELD_BANK_SLOT` guids, a
    /// held bank bag, a purchased count in `PLAYER_BYTES_2` byte 2, and a coinage purse — so the
    /// capture exercises the whole live chain (descriptor → `feed_bank`/`ui_items` → Lua →
    /// render): vault slots with icons, bag buttons (owned icon / bought-empty / red unpurchased),
    /// and the purchase row's DBC cost.
    Bank,
    /// The multi-quest greeting panel (`QUEST_GREETING`, `BenillaQuestGreetingPanel`): a greeting
    /// line plus an "Available Quests" list of `UI-Quest-BulletPoint` title rows — the frame the
    /// gossip-vs-greeting confusion turned on, and previously uncaptured (bullet/title seating had
    /// no regression baseline).
    QuestGreeting,
    /// The quest-log book window (decision 0109) fed the REAL way: a synthetic self-player entity
    /// whose descriptor carries occupied `PLAYER_QUEST_LOG` slots, so the capture exercises the
    /// whole live chain (descriptor → `feed_quest_log` → template cache → seam → Lua → render) —
    /// nothing pushed to the VM by hand.
    QuestLog,
    Loot,
    Bag,
    /// The bag window with the GameTooltip forced open over a known slot — the tooltip look-pass
    /// instrument (crisp border, tiled edges, tinted plate, quality-coloured item name, snug size).
    Tooltip,
    /// The WORLD-mouseover tooltip forced open over a seeded unit — the default-anchor
    /// instrument: the plate must sit at the screen's bottom-right corner
    /// (−CONTAINER_OFFSET_X−13, +CONTAINER_OFFSET_Y — ref GameTooltip.lua l.73-77), never on
    /// the hovered model. Captures the wiring whose absence parked it at screen center.
    TooltipWorld,
    /// The character window's paper doll (decision 0208 phase 1a) fed the REAL way (the QuestLog
    /// pattern): a synthetic self player whose descriptor carries the full stat block + equipped
    /// item guids, item objects/templates in the [`crate::items::Items`] stores — so the capture
    /// exercises the whole live chain (descriptor → `ui_char` feed → snapshots/events → Lua →
    /// render): slot icons, the attribute/resistance panes with buff coloring, melee + ranged
    /// blocks, the ammo count, the level line.
    Character,
    /// A V-key nameplate over a synthetic Timber Wolf (the reference client's own screenshot
    /// subject: entry 69, level 2, faction 32, display 604) — the plate look-pass instrument.
    /// At the 1024×768 window this scenario forces, one gx unit = 1280 px, so the 0.1 × 0.025
    /// plate must land at exactly 128×32 logical px: the border texture's native size, directly
    /// diffable against the decoded BLP.
    VPlates,
    /// The world map in its default windowed mode (the 1.14-style small window), opened at the
    /// Elwynn zone map with alternating explore bits — one frame exercising the window chrome,
    /// the scaled map block, the exploration fog (revealed overlays over the parchment base),
    /// and the enlarged player arrow.
    WorldMap,
    /// The spellbook (decision 0216 §8) opened over a seeded known-spell set that resolves
    /// through the REAL chain (`PlayerActions.spells` → `Spell.dbc` × `SkillLineAbility.dbc` →
    /// the book feed → Lua → render): the panel plates, the 12-slot page with name/rank text,
    /// passive graying, the skill-line tab strip, and the page footer.
    SpellBook,
    /// The macro window (decision 0983) over a seeded macro set, second slot selected — the look
    /// instrument the window shipped WITHOUT, which is why "CREATE_MACROS" across its title bar
    /// reached the director's screen instead of a capture (0991). Pins the two-tab row inside the
    /// 384-wide plate, the 18-slot grid, the selected-macro detail pane, the body box, and the
    /// bottom button row. Macros are made through the live `CreateMacro` path, so the fixture
    /// exercises the real engine table → `UPDATE_MACROS` → window chain.
    Macro,
    /// The macro window's NAME/ICON POPUP open over the same set (0991) — the other half of the
    /// window, and the denser one: the 5×4 icon grid off the real `SpellIcon.dbc` catalog, its
    /// faux scroll bar, the name box, and the Okay/Cancel row.
    MacroPopup,
    /// The chat window with the edit box OPEN over a say/yell line mix (decision 0288 P5's look
    /// instrument, added for the centered-"Say:"/invisible-typing regression): the box focused
    /// with a typed draft through the live open path (`focus_editbox` + `chat_edit_live`), so the
    /// capture checks the header (left-flush, Say-white), the typed text past the live insets,
    /// and the three-piece input border.
    ChatEdit,
    /// The era-styled Options window (decisions 0950/0951), opened through the live panel path —
    /// the look-pass instrument for the whole options arc: chrome nine-slice seams, tab plates,
    /// search-box seat, category list art, the window scale. Static (no server state touched),
    /// so its pixels move only when the window or the atlas seam does.
    Options,
    /// The Options window ON THE AUDIO PAGE (decision 0957) — the setting-row look instrument:
    /// checkbox art at its era seats, the 1.12 slider groove + thumb on the bare full-width bar
    /// (steppers cut 0989), child-row indent/small-font, the percent readouts, Defaults armed.
    /// Rows read the CVar registration defaults (hermetic capture = no config file), so the
    /// pixels move only with the window, the atlas seam, or a registered default.
    OptionsAudio,
    /// The Options window ON THE GRAPHICS PAGE (decision 0959; farclip row retired 0961;
    /// Environment Detail joined 0992) — uiScale at its 0.64..1.0 panel range with the percent
    /// readout, and the dropdown row's closed capsule (the 1.12 kit art) reading "High". Rows
    /// read the CVar registration defaults (hermetic capture = no config file), so the pixels
    /// move only with the window, the atlas seam, or a registered default.
    OptionsGraphics,
    /// The Graphics page with the Environment Detail MENU OPEN (0992) — the dropdown-list look
    /// instrument: the shared DropDownList1 at the window's effective scale (the kit's uiScale
    /// correction), three entries with High checked, the kit's dialog backdrop.
    OptionsWorldDetail,
    /// The Options window MID-SEARCH (decision 0984) — the results-view look instrument: the
    /// "volume" query reflows the four live volume sliders under the clickable Audio head
    /// (GameFontNormalLarge), title "Search Results", Defaults hidden, the clear-X shown in
    /// the box. Hermetic like the other options fixtures; pixels move only with the window
    /// or a registered default.
    OptionsSearch,
    /// The KEY BINDINGS window (decision 0997) — the era-shaped standalone KeyBindingFrame on
    /// its Movement page: the category sidebar (gold-locked Movement Keys), Command/Key 1/
    /// Key 2 columns over the honest tree's rows with the byte-real 1.12 defaults ("Move and
    /// Steer — Middle Mouse" leading, straight off the real GlobalStrings the capture VM loads
    /// like any run), the character-specific checkbox, the four bottom buttons. The registry
    /// registers in-fixture (hermetic capture = the plugin's PostStartup seed is not raced),
    /// so the pixels move only with the window, a registered default, or a 1.12 string.
    KeyBindings,
    /// A **floating overhead name with the river surface behind it** — the world-text-vs-liquid
    /// draw-order instrument. A named unit stands 25 yd out in the Elwynn river (the `water-noon`
    /// camera), so its name projects onto the water *beyond* it: the exact geometry of the
    /// director's Stormwind-canal report, where the plate sorted BEFORE the liquid and deep
    /// water (`WATER_DEEP_ALPHA` = 1.0, fully opaque) painted the glyphs out. Nothing in the
    /// sweep covered this — decision 0519 wrote the law and shipped it with the sort bias
    /// pointing the wrong way, invisible to every gate. The name must read at full strength.
    NameWater,
    /// One cell of the **lighting matrix** (decision 0744): a creature or a GameObject spawned
    /// through the live path at `at`, so each lane an object's light can take is photographed from
    /// two sides. See [`SubjectKind`] and the matrix note above [`SUBJECT_SUN`].
    Subject {
        kind: SubjectKind,
        /// Where the subject stands, raw WoW coords — its FEET, not its body centre.
        at: [f32; 3],
    },
}

/// What the lighting matrix puts in frame. Both spawn with the same live component set a streamed
/// entity gets (`NetEntity` + descriptor), differing only in `EntityKind` — so the matrix exercises
/// the real unit and GameObject paths rather than a bespoke preview.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum SubjectKind {
    /// The Timber Wolf (entry 69, display 604) — the same subject `vplates`/`name-water` use, so a
    /// defect seen here can be A/B'd against them directly.
    Creature,
    /// `World\SkillActivated\Containers\TreasureChest01.mdx` (`GameObjectDisplayInfo` 259) — the
    /// canonical world chest, at its closed rest pose.
    Chest,
}

/// The ON-DEMAND Northshire framings, anchored at the Human start (around `SPAWN_XY`
/// `(-8949.95, -132.49)`, ground ≈ 83.5). Two framings from one spot exercise the whole stack the
/// linear-HDR rework rebuilds:
/// - a **ground** overlook (camera pitched down at textured terrain + the Abbey) across the day arc —
///   terrain lighting, ambient, shadows, distance fog, model lighting;
/// - a **sky** view (camera pitched up at the dome + horizon) at day and dusk — the sky-dome gradient,
///   the fog horizon, and (at dusk) the warp + low sun + emerging stars.
pub(super) const GROUND_EYE: [f32; 3] = [-8980.0, -160.0, 110.0];
pub(super) const GROUND_LOOK: [f32; 3] = [-8949.95, -132.49, 84.0];
pub(super) const SKY_EYE: [f32; 3] = [-8980.0, -160.0, 112.0];
pub(super) const SKY_LOOK: [f32; 3] = [-8740.0, 80.0, 168.0]; // up + out: horizon in the lower third, dome above

// Farmhouse viewpoints (decision 0071): compass looks from the human-start login spot. Kept
// permanently — the pale-film regression was invisible for hours because every baseline framed the
// Abbey, one of the few buildings immune to it. Baselines must cover ordinary buildings too.
pub(super) const HOUSE_EYE: [f32; 3] = [-9439.1, 71.2, 68.0];

/// `Map.dbc` ids the golden spots stand on.
pub(super) const MAP_AZEROTH: u32 = 0;
pub(super) const MAP_KALIMDOR: u32 = 1;
pub(super) const MAP_DEEPRUN_TRAM: u32 = 369;

/// THE golden baseline: **three spots FRAMED BY THE DIRECTOR, each at noon and at night — six
/// captures, and that is the whole sweep.** A Stormwind canal view, Elwynn water, and a Felwood
/// hollow on Kalimdor. The numbers below are verbatim from their `~/.benilla/shots.txt` ([`/shot`]).
///
/// **Held at six on purpose (decision 0817).** 0632 cut a thirty-shot set down to six for exactly
/// this reason and stated the law — *a few spots chosen with the director, times a couple of day
/// times* — and then the set grew back to **twenty-one** one well-argued addition at a time: an
/// interior, a second continent, a shadow-line fence, four creature cells, four chest cells, two
/// indoor creature cells. Every one had a real case. The aggregate was 42 windows popping open on the
/// director's screen per `selfcheck`, several of them not reproducible, and a standing invitation to
/// spend sessions chasing harness ghosts — which is what happened (0810, 0815). The director's call:
/// *"get rid of that shit, making more problems than it's helping… just keep the simple stuff we had
/// before, 3 frames 2 times of day."*
///
/// So growth is now the thing to resist, not to justify. **Nothing is deleted** — every evicted
/// viewpoint is still capturable by name from [`ON_DEMAND`], which is where a subject being worked on
/// belongs. A shot earns a place here only by the director putting it here.
///
/// The Northshire overlook toward the **Abbey** was 0632's shot 1 and is the one this set drops, at
/// the director's instruction, in favour of the Felwood hollow — which covers strictly more: a second
/// continent, its own tileset, fog palette, liquid type and WDL horizon, none of which the Elwynn
/// spots can photograph at all.
pub(super) const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "canal-noon",
        map: MAP_AZEROTH,
        eye: CANAL_EYE,
        look: CANAL_LOOK,
        minute: 720,
        ui: None,
    },
    Scenario {
        name: "canal-night",
        map: MAP_AZEROTH,
        eye: CANAL_EYE,
        look: CANAL_LOOK,
        minute: 0,
        ui: None,
    },
    Scenario {
        name: "water-noon",
        map: MAP_AZEROTH,
        eye: WATER_EYE,
        look: WATER_LOOK,
        minute: 720,
        ui: None,
    },
    Scenario {
        name: "water-night",
        map: MAP_AZEROTH,
        eye: WATER_EYE,
        look: WATER_LOOK,
        minute: 0,
        ui: None,
    },
    Scenario {
        name: "felwood-noon",
        map: MAP_KALIMDOR,
        eye: FELWOOD_EYE,
        look: FELWOOD_LOOK,
        minute: 720,
        ui: None,
    },
    Scenario {
        name: "felwood-night",
        map: MAP_KALIMDOR,
        eye: FELWOOD_EYE,
        look: FELWOOD_LOOK,
        minute: 0,
        ui: None,
    },
];

/// Director shot 1 — the Northshire overlook, north-east of the Abbey looking at it: terrain,
/// trees, the Abbey WMO, stained glass, props.
pub(super) const OVERLOOK_EYE: [f32; 3] = [-8955.0, -98.5, 91.1];
pub(super) const OVERLOOK_LOOK: [f32; 3] = [-8912.9, -125.4, 87.7];

/// Director shot 2 — the Stormwind canal (Cathedral Square side): water, bridge, walls, spires,
/// far mountains.
pub(super) const CANAL_EYE: [f32; 3] = [-8871.7, 724.6, 110.7];
pub(super) const CANAL_LOOK: [f32; 3] = [-8836.7, 760.3, 112.3];

/// Director shot 3 — Elwynn river, south-east of Northshire: open water dominant, shoreline blend,
/// murloc camp, fog.
pub(super) const WATER_EYE: [f32; 3] = [-9527.0, -310.6, 70.8];
pub(super) const WATER_LOOK: [f32; 3] = [-9499.4, -351.3, 61.4];

/// Director shot 4 (decision 0743) — INSIDE the Lion's Pride Inn at Goldshire, standing in the
/// common room: the hearth (the MOCV-alpha self-illum bake), a daylight window, the lit chandelier,
/// the stair run, floor boards and ceiling beams, and a room full of props. The sweep's only
/// interior, and the only shot that exercises portal culling, the INT bake, the MOLT point pools
/// and WMO props at all — the surface the ledger's dungeon reports live on, and the one B101's
/// prop-cull regression (which hid the Blackrock lava falls) crossed unseen.
///
/// The agent's own interior framing (`inn-interior`, a tight crop of the kitchen hearth) was judged
/// badly framed and stays in [`ON_DEMAND`]; this is the director's, and it frames the ROOM.
///
/// NB the camera must stand over a FLOOR FACE: over a floorless pocket the portal cull's down-ray
/// reads "outside" and faithfully culls the containing group, which vanishes the room.
pub(super) const INN_EYE: [f32; 3] = [-9471.4, 39.4, 59.9];
pub(super) const INN_LOOK: [f32; 3] = [-9458.8, -7.5, 48.2];

/// Director shot 6 (decision 0749) — an Elwynn rail fence lying ACROSS the sun's shadow boundary,
/// at 10:24: the right-hand span is in full sun, the left-hand span in shade, and the same edge
/// runs on across the road behind it. One frame holding **both** states of the MCSH sun term, on
/// terrain and on a doodad at once, with the ramp visible as the hard line between them.
///
/// The lighting matrix (0746) samples the lit and shadowed lanes as SEPARATE cells at separate
/// positions, so a regression that scaled both equally could pass both. Here the two states share
/// one frame, one model and one texture, so only their DIFFERENCE can carry the shot — the control
/// is inside the picture. It also lights the third lane neither matrix subject touches: a
/// world-placed **doodad**, which takes its shade per-vertex at bake rather than through
/// `entity_shade`'s per-object ramp.
///
/// Found by the director, who framed it because they had already spotted it was "both lit and unlit
/// from sun, half half".
pub(super) const FENCE_EYE: [f32; 3] = [-9511.9, -4.0, 61.9];
pub(super) const FENCE_LOOK: [f32; 3] = [-9552.0, 18.6, 42.4];

/// Director shot 5 (decision 0743) — a Felwood hollow on **Kalimdor** (`MAP_KALIMDOR`): the
/// corrupted forest floor's root mat, a stand of emissive `felwoodmushroom` doodads, a pool of
/// green sludge (a liquid type no other golden shot contains), the vast trunks behind, and the
/// zone's sick-green fog and light palette.
///
/// The sweep's only shot outside Azeroth and outside the Elwynn/Stormwind palette — until this one
/// landed, a fog, tileset, WDL-horizon or per-map lighting regression anywhere else in the world
/// had nowhere to show up. It is also what forced [`Scenario::map`]: this spot's ADT tile
/// (`33_24`) exists in Azeroth too, empty, so on the old table it would have photographed a void.
pub(super) const FELWOOD_EYE: [f32; 3] = [4060.9, -944.3, 256.8];
pub(super) const FELWOOD_LOOK: [f32; 3] = [4014.0, -954.4, 242.9];

// ---------------------------------------------------------------------------------------------
// The LIGHTING MATRIX (decision 0744) — one subject, three lanes, two sides.
//
// The golden spots are landscape: they photograph terrain, buildings and water, and (captures being
// server-less) contain no creature or GameObject at all. So the *object* light path — the one every
// creature, player, pet and chest in the game is lit by — had no regression coverage whatsoever,
// while the bug ledger's unit reports (blacked-out textures, mis-lit NPCs across city WMOs) sit
// squarely on it.
//
// The three positions are the three lanes an object's light can take, and they were CHOSEN FROM THE
// DATA, not by eye — `WOW_LIGHT_AT`/`WOW_LIGHT_GRID` (`wmo_portal::audit::light_probe`) reports the
// lane and the terrain MCSH bit at any world point:
//
//   SUN    (-9500, 56)   terrain z 56.48   MCSH false  lane exterior-on-terrain  — sun term at full
//   SHADE  (-9500, 44)   terrain z 55.95   MCSH true   lane exterior-on-terrain  — sun term dimmed
//   INDOOR (-9469.4, 31.9)  terrain z none  zone-text indoor 5, lane BAKE g05    — no sun at all
//
// SUN and SHADE are twelve yards apart on the same open ground, so between that pair the ONLY
// changed input is the baked shadow bit — exactly the discriminator `entity_shade` ramps on
// (2.5 lit → 0.5 shadowed). INDOOR stands in the Goldshire inn's common room, where the interior
// classifier owns the tag instead and the sun never reaches.
//
// Two sides per lane, straddling the light: the game's LIGHTING sun is near-fixed at azimuth 45°
// (`sun::follow`), so `front` puts the camera on that bearing (sun behind us — the subject's lit
// face) and `rear` puts it opposite (the shadowed face). Indoors there is no sun, so the pair simply
// samples the bake from two sides. Cameras sit at a fixed offset off the subject's feet, eye a
// little above and aimed at the body, so every cell frames the subject identically and a diff is
// about light, never framing.

/// Lighting-matrix subject positions (feet, raw WoW coords) — see the note above.
pub(super) const SUBJECT_SUN: [f32; 3] = [-9500.0, 56.0, 56.48];
pub(super) const SUBJECT_SHADE: [f32; 3] = [-9500.0, 44.0, 55.95];
pub(super) const SUBJECT_INDOOR: [f32; 3] = [-9469.4, 31.9, 57.9];

/// Every remaining named viewpoint — the UI look-pass fixtures, the sun/moon/sky regression
/// fixtures, the house-compass and street scenes. Capturable by name (`WOW_CAPTURE=<name>`) for
/// debugging and look passes, but NOT part of the blessed baseline sweep.
pub(super) const ON_DEMAND: &[Scenario] = &[
    // ---- The Deeprun Tram's undersea tube (map 369) ----
    // The one shipped map with NO `Light.dbc` row at all — not even the falloff-0 global that maps
    // 0/1 carry — so its whole atmosphere has to come from the building's own MFOG (record 2:
    // RGB(30,53,100), end 236.1 yd, start scalar 0.05; the camera's group `Subway_002` names it).
    // A global (WDT `MODF`) WMO too, the placement shape no other scenario exercises.
    //
    // ⚠ **This shot currently photographs NOTHING — do not baseline it.** The building registers
    // ("terrain: map DeeprunTram has no tiles — its world is one WMO") but no group ever reaches
    // the frame in server-less capture, so the shot is the bare sky dome from any eye, inside the
    // tube or between the tubes, at `WOW_CAPTURE_STABLE` 30 or 1800 alike (the 1800 run reported
    // "never settled", i.e. it was still waiting, not still loading). The live client draws this
    // map fine — the director's screenshots are of it — so the gap is the harness's, and it is
    // kept here as the reproducer rather than deleted: a WMO-only map is exactly the residency
    // shape 0688 wired and nothing has ever photographed it.
    Scenario {
        name: "tram-undersea",
        map: MAP_DEEPRUN_TRAM,
        eye: TRAM_EYE,
        look: TRAM_LOOK,
        minute: 720,
        ui: None,
    },
    // ---- Evicted from the blessed sweep by decision 0817, NOT deleted ----
    // The sweep had grown from 0632's six to twenty-one, i.e. 42 windows on the director's screen per
    // `selfcheck`, and the director cut it back to three spots x two day times. Everything below this
    // note used to be in SCENARIOS and is still capturable by name — which is the right home for a
    // subject somebody is actively working on. `chest-shade-{front,rear}` additionally measured NOT
    // reproducible on the sweep that prompted the cut (MAE 2.721 / 2.649, ~7.8 % of pixels, two runs
    // of one build) — same coplanar-batch draw-order defect as `chest-indoor-*` below (0815 Open).
    Scenario {
        name: "overlook-noon",
        map: MAP_AZEROTH,
        eye: OVERLOOK_EYE,
        look: OVERLOOK_LOOK,
        minute: 720,
        ui: None,
    },
    Scenario {
        name: "overlook-night",
        map: MAP_AZEROTH,
        eye: OVERLOOK_EYE,
        look: OVERLOOK_LOOK,
        minute: 0,
        ui: None,
    },
    // The sweep's only interior until 0817 moved it here; no `inn-night` (measured MAE 0.198 against
    // `inn-noon` — the room is lit by hearth and candles, so the clock barely reaches it).
    Scenario {
        name: "inn-noon",
        map: MAP_AZEROTH,
        eye: INN_EYE,
        look: INN_LOOK,
        minute: 720,
        ui: None,
    },
    // The day cell keeps the director's own minute (10:24) rather than the sweep's noon: the subject
    // IS the shadow boundary, and where it falls is a function of the sun's angle, so at noon it
    // slides off the rails and the shot degrades into an ordinary fence.
    Scenario {
        name: "fence-shadowline-day",
        map: MAP_AZEROTH,
        eye: FENCE_EYE,
        look: FENCE_LOOK,
        minute: 624,
        ui: None,
    },
    // The night twin, at the director's call — this record's own first draft argued against it on
    // the grounds that there is no shadow line without a sun, which was reasoning from the mechanism
    // and never measured. The bit it photographs is BAKED, present at every hour; what the night
    // frame pins is the other half of the mechanism — how much of the shade term survives once the
    // sun's contribution is gone and the ambient/moon palette carries the frame. That is a question
    // about the light curves, and a baseline is the right place to hold the answer (0749 addendum).
    Scenario {
        name: "fence-shadowline-night",
        map: MAP_AZEROTH,
        eye: FENCE_EYE,
        look: FENCE_LOOK,
        minute: 0,
        ui: None,
    },
    Scenario {
        name: "creature-sun-front",
        map: MAP_AZEROTH,
        eye: [-9496.46, 59.54, 58.28],
        look: [-9500.00, 56.00, 57.28],
        minute: 720,
        ui: Some(UiFixture::Subject {
            kind: SubjectKind::Creature,
            at: SUBJECT_SUN,
        }),
    },
    Scenario {
        name: "creature-sun-rear",
        map: MAP_AZEROTH,
        eye: [-9503.54, 52.46, 58.28],
        look: [-9500.00, 56.00, 57.28],
        minute: 720,
        ui: Some(UiFixture::Subject {
            kind: SubjectKind::Creature,
            at: SUBJECT_SUN,
        }),
    },
    Scenario {
        name: "creature-shade-front",
        map: MAP_AZEROTH,
        eye: [-9496.46, 47.54, 57.75],
        look: [-9500.00, 44.00, 56.75],
        minute: 720,
        ui: Some(UiFixture::Subject {
            kind: SubjectKind::Creature,
            at: SUBJECT_SHADE,
        }),
    },
    Scenario {
        name: "creature-shade-rear",
        map: MAP_AZEROTH,
        eye: [-9503.54, 40.46, 57.75],
        look: [-9500.00, 44.00, 56.75],
        minute: 720,
        ui: Some(UiFixture::Subject {
            kind: SubjectKind::Creature,
            at: SUBJECT_SHADE,
        }),
    },
    Scenario {
        name: "creature-indoor-front",
        map: MAP_AZEROTH,
        eye: [-9466.57, 34.73, 59.70],
        look: [-9469.40, 31.90, 58.70],
        minute: 720,
        ui: Some(UiFixture::Subject {
            kind: SubjectKind::Creature,
            at: SUBJECT_INDOOR,
        }),
    },
    Scenario {
        name: "creature-indoor-rear",
        map: MAP_AZEROTH,
        eye: [-9472.23, 29.07, 59.70],
        look: [-9469.40, 31.90, 58.70],
        minute: 720,
        ui: Some(UiFixture::Subject {
            kind: SubjectKind::Creature,
            at: SUBJECT_INDOOR,
        }),
    },
    Scenario {
        name: "chest-sun-front",
        map: MAP_AZEROTH,
        eye: [-9496.82, 59.18, 57.98],
        look: [-9500.00, 56.00, 56.93],
        minute: 720,
        ui: Some(UiFixture::Subject {
            kind: SubjectKind::Chest,
            at: SUBJECT_SUN,
        }),
    },
    Scenario {
        name: "chest-sun-rear",
        map: MAP_AZEROTH,
        eye: [-9503.18, 52.82, 57.98],
        look: [-9500.00, 56.00, 56.93],
        minute: 720,
        ui: Some(UiFixture::Subject {
            kind: SubjectKind::Chest,
            at: SUBJECT_SUN,
        }),
    },
    Scenario {
        name: "chest-shade-front",
        map: MAP_AZEROTH,
        eye: [-9496.82, 47.18, 57.45],
        look: [-9500.00, 44.00, 56.40],
        minute: 720,
        ui: Some(UiFixture::Subject {
            kind: SubjectKind::Chest,
            at: SUBJECT_SHADE,
        }),
    },
    Scenario {
        name: "chest-shade-rear",
        map: MAP_AZEROTH,
        eye: [-9503.18, 40.82, 57.45],
        look: [-9500.00, 44.00, 56.40],
        minute: 720,
        ui: Some(UiFixture::Subject {
            kind: SubjectKind::Chest,
            at: SUBJECT_SHADE,
        }),
    },
    // The chest INDOORS is out of the blessed sweep and in here instead: measured over two runs of
    // one build it is not reproducible (front MAE 1.551 / 9.2 % of pixels, rear 0.529 / 7.6 %), and
    // the diff is the whole body shifting brightness in perfect registration — LIGHT, not pose. The
    // GameObject's interior light lane does not converge by the shutter, while the CREATURE at the
    // same spot is bit-identical. That is the ledger's "mis-lit objects inside city WMOs" class, and
    // it is open (decision 0746). Capture them by name to work on it; they rejoin the sweep the day
    // they are deterministic.
    Scenario {
        name: "chest-indoor-front",
        map: MAP_AZEROTH,
        eye: [-9466.85, 34.45, 59.40],
        look: [-9469.40, 31.90, 58.35],
        minute: 720,
        ui: Some(UiFixture::Subject {
            kind: SubjectKind::Chest,
            at: SUBJECT_INDOOR,
        }),
    },
    Scenario {
        name: "chest-indoor-rear",
        map: MAP_AZEROTH,
        eye: [-9471.95, 29.35, 59.40],
        look: [-9469.40, 31.90, 58.35],
        minute: 720,
        ui: Some(UiFixture::Subject {
            kind: SubjectKind::Chest,
            at: SUBJECT_INDOOR,
        }),
    },
    Scenario {
        name: "house-north",
        map: MAP_AZEROTH,
        eye: HOUSE_EYE,
        look: [-9389.1, 71.2, 58.0],
        minute: 720,
        ui: None,
    },
    Scenario {
        name: "house-south",
        map: MAP_AZEROTH,
        eye: HOUSE_EYE,
        look: [-9489.1, 71.2, 58.0],
        minute: 720,
        ui: None,
    },
    Scenario {
        name: "house-west",
        map: MAP_AZEROTH,
        eye: HOUSE_EYE,
        look: [-9439.1, 121.2, 58.0],
        minute: 720,
        ui: None,
    },
    Scenario {
        name: "house-east",
        map: MAP_AZEROTH,
        eye: HOUSE_EYE,
        look: [-9439.1, 21.2, 58.0],
        minute: 720,
        ui: None,
    },
    // Inside the Lion's Pride Inn KITCHEN (its hearth carries the building's strongest MOCV-alpha
    // bake, α≈100 at the firebox — group-local (-32.9, 1.5, 2), world ≈ (-9461.7, -8.4, 58) per
    // the real MODF: uid 71414 on tile 31,49, origin (-9464.25, 24.39, 56.53), rot -97°). The
    // WMO-interior baseline: the INT bake, the MOCV-alpha self-illum glow on the hearth bricks
    // (frame right), the MOLT point pools on props, the fire doodads. Interiors previously had no
    // capture at all. NB the camera must stand over a FLOOR FACE: over a floorless pocket the
    // portal cull's down-ray reads "outside" and faithfully culls the containing group (the real
    // client does the same — the audit's "faithful-cull residue"), which vanishes the room.
    Scenario {
        name: "inn-interior",
        map: MAP_AZEROTH,
        eye: [-9463.3, 4.4, 58.8],
        look: [-9462.1, -5.6, 58.5],
        minute: 720,
        ui: None,
    },
    // `northshire-noon`/`northshire-night` are gone: the director's `overlook` frames the same
    // valley from a spot they chose, at the same two day times. Dusk has no golden twin, so it
    // stays as the warm-light/fog fixture.
    Scenario {
        name: "northshire-dusk",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 1170, // 19:30 — warm dusk light + fog
        ui: None,
    },
    Scenario {
        name: "northshire-sky-noon",
        map: MAP_AZEROTH,
        eye: SKY_EYE,
        look: SKY_LOOK,
        minute: 720, // day sky-dome gradient + fog horizon
        ui: None,
    },
    Scenario {
        name: "northshire-sky-dusk",
        map: MAP_AZEROTH,
        eye: SKY_EYE,
        look: SKY_LOOK,
        minute: 1170, // dusk dome warp + low sun + stars emerging
        ui: None,
    },
    // Straight INTO the visible sun with the view lerp at max (f = 1) — the lens-flare regression
    // fixture (decision 0500). At 17:30 the sun sits at elev ≈30°, azimuth 45°, clear of the
    // Northshire ridge (the sky-dusk scene's 19:30 sun hides BEHIND the mountains, which is how
    // the halo-edge artifact escaped every baseline): the full 20-unit sunGlare star-ray quad must
    // fade off smoothly with no hard edge (the old far-placed quad depth-fought the sky dome and
    // was cut along a giant faceted circle).
    Scenario {
        name: "northshire-sun-flare",
        map: MAP_AZEROTH,
        eye: SKY_EYE,
        look: [-8797.0, 23.0, 264.0], // eye + 300·(elev 30°, az 45°) — the sun's spot at 17:30
        minute: 1050,
        ui: None,
    },
    // The rising MOON low over the same bearing (az 45°, elev ≈15° at 22:44). Originally the flare
    // occlusion-gate fixture (decision 0502); since the byte-pinned moon dnCurve landed (0508) the
    // halo is dark here BY LAW — the curve is flat zero until 22:45 — so this now regression-checks
    // two things: the disc rises edge-first behind the ridge (per-pixel terrain occlusion), and NO
    // glare ring exists anywhere this early (a halo at 22:44 = the dn gate broke). The live halo's
    // appearance is `northshire-moon-halo` below; the gate's die-on-the-rock behavior keeps its
    // unit tests (`flare_ray_*`) and the sun fixtures.
    Scenario {
        name: "northshire-moonrise",
        map: MAP_AZEROTH,
        eye: SKY_EYE,
        look: [-8775.0, 45.0, 190.0], // eye + 300·(elev 15°, az 45°) — the moon's spot at 22:44
        minute: 1364,
        ui: None,
    },
    // The moon's halo at its byte-law PEAK — midnight, moon overhead (az 45°, elev 55°), dnCurve
    // 1.0, dense stars (star curve 1.0): the warm disc + the gamma-added soft glare ring at full
    // envelope over the star field. The regression baseline for the halo's correct look (0508) —
    // the moonrise fixture above proves its absence early, this one its presence at depth of night.
    Scenario {
        name: "northshire-moon-halo",
        map: MAP_AZEROTH,
        eye: SKY_EYE,
        look: [-8858.0, -38.0, 358.0], // eye + 300·(elev 55°, az 45°) — the moon's spot at 00:00
        minute: 0,
        ui: None,
    },
    // The `.tele Stormwind` spot (vmangos game_tele: -8833.38, 628.63, 94.01, o=1.065), eye at
    // head height looking along the tele facing into the Trade District — the city-scale perf
    // scene (the 2026-07-13 "20–30 fps in Stormwind" report). The whole city WMO + its doodad
    // load is resident here; Northshire scenes never exercise that scale.
    Scenario {
        name: "stormwind",
        map: MAP_AZEROTH,
        eye: [-8833.38, 628.63, 96.0],
        look: [-8809.1, 672.3, 94.0],
        minute: 720,
        ui: None,
    },
    // The UI window fixtures (2026-07-03): each opens a shipped window with canned state over the
    // noon ground view. These are the look-pass instrument — window fidelity gets checked on these
    // captures (by the coordinator's own reading + benilla-visual regression diffs) BEFORE any
    // director look pass, so "looks like shit" gets caught in-loop, not on the director's screen.
    Scenario {
        name: "ui-merchant",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Merchant),
    },
    Scenario {
        name: "ui-gossip",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Gossip),
    },
    Scenario {
        name: "ui-bank",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Bank),
    },
    Scenario {
        name: "ui-quest",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Quest),
    },
    Scenario {
        name: "ui-questgreeting",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::QuestGreeting),
    },
    Scenario {
        name: "ui-questlog",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::QuestLog),
    },
    Scenario {
        name: "ui-loot",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Loot),
    },
    Scenario {
        name: "ui-bag",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Bag),
    },
    // The GameTooltip forced open over a seeded bag slot (a green-quality item). Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-tooltip`.
    Scenario {
        name: "ui-tooltip",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Tooltip),
    },
    // The world-mouseover tooltip at the DEFAULT corner (screen bottom-right, −13/+70) over a
    // seeded hostile wolf. Run with `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-tooltip-world`.
    Scenario {
        name: "ui-tooltip-world",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::TooltipWorld),
    },
    // The character window's paper doll over a fully-seeded synthetic self player. Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-char`.
    Scenario {
        name: "ui-char",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Character),
    },
    // The player + target unit frames (no window fixture — the frames come from `demo_unit_feed`,
    // which seeds synthetic "player"/"target" snapshots whenever WOW_CAPTURE_UI=1). Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-unitframes`.
    Scenario {
        name: "ui-unitframes",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: None,
    },
    // The combo-point dots at the target frame's top-right (decisions 0869/0875). `demo_unit_feed`
    // seeds a ROGUE with four points banked on the selected wolf for this scenario only — the demo
    // player is a warrior everywhere else, and a warrior authentically lights no dot. Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-combopoints`.
    Scenario {
        name: "ui-combopoints",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: None,
    },
    // The re-skinned main action bar (no window fixture — the bar loads by default under
    // WOW_CAPTURE_UI=1, and `demo_unit_feed` seeds the action slots + player XP). The bar is 1024
    // wide + 128px end caps, so this fixture takes a WIDER, shorter window (see main.rs's per-capture
    // sizing) — the default 640px UI window would crop it. Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-actionbar`.
    Scenario {
        name: "ui-actionbar",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: None,
    },
    // The V-key nameplate over a synthetic Timber Wolf, framed like the reference screenshot
    // (an eye-height look at a wolf ~8 yd off, Northshire ground). Plates draw through the
    // UiQuads overlay, which renders in every capture — no WOW_CAPTURE_UI needed. Run with
    // `WOW_CAPTURE=vplates` (main.rs sizes this window 1024×768 — the 1:1 gx window).
    Scenario {
        name: "vplates",
        map: MAP_AZEROTH,
        eye: [-8956.5, -137.5, 85.6],
        look: [-8949.95, -132.49, 84.8],
        minute: 720,
        ui: Some(UiFixture::VPlates),
    },
    // The fullscreen world map (decision 0203 phase 2), forced open at the world sheet. The frame's
    // 1024×768 chrome needs the taller window main.rs gives the 1:1 gx captures. Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-worldmap`.
    Scenario {
        name: "ui-worldmap",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::WorldMap),
    },
    // The spellbook over a seeded mage book. Run with `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-spellbook`.
    Scenario {
        name: "ui-spellbook",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::SpellBook),
    },
    // The macro window over a seeded macro set. Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-macro`.
    Scenario {
        name: "ui-macro",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Macro),
    },
    // The same window with the name/icon popup open. Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-macro-popup`.
    Scenario {
        name: "ui-macro-popup",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::MacroPopup),
    },
    // The chat edit box open with a typed draft over seeded lines. Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-chatedit`.
    Scenario {
        name: "ui-chatedit",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::ChatEdit),
    },
    // The era Options window over the ground scene. Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-options`.
    Scenario {
        name: "ui-options",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Options),
    },
    // The same window on the AUDIO page (0957). Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-options-audio`.
    Scenario {
        name: "ui-options-audio",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::OptionsAudio),
    },
    // The same window on the GRAPHICS page (0959). Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-options-graphics`.
    Scenario {
        name: "ui-options-graphics",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::OptionsGraphics),
    },
    // The Graphics page with the Environment Detail dropdown OPEN (0992). Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-options-worlddetail`.
    Scenario {
        name: "ui-options-worlddetail",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::OptionsWorldDetail),
    },
    // The options window's Keybindings page, Movement expanded (1008). Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-keybindings`.
    Scenario {
        name: "ui-keybindings",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::KeyBindings),
    },
    // The same window MID-SEARCH (0984): the "volume" results view. Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-options-search`.
    Scenario {
        name: "ui-options-search",
        map: MAP_AZEROTH,
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::OptionsSearch),
    },
    // An overhead NAME with the river surface behind it — the world-text-vs-liquid draw-order
    // instrument (see [`UiFixture::NameWater`]). Same camera as `water-noon`, plus a named unit
    // out in the water. Run with `WOW_CAPTURE=name-water`.
    Scenario {
        name: "name-water",
        map: MAP_AZEROTH,
        eye: WATER_EYE,
        look: WATER_LOOK,
        minute: 720,
        ui: Some(UiFixture::NameWater),
    },
];

/// The Deeprun Tram undersea tube — see the `tram-undersea` scenario. Raw WoW coords; the Subway
/// WMO is the map's global `MODF` at the origin with identity rotation, so these are also its
/// model-space coords.
pub(super) const TRAM_EYE: [f32; 3] = [-2.44, -1250.0, -120.0];
pub(super) const TRAM_LOOK: [f32; 3] = [-2.44, -1400.0, -118.0];
