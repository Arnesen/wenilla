//! The capture *scenarios* — the named deterministic viewpoints (camera eye/look in raw WoW
//! coords, pinned game-minute, optional UI fixture) and the golden-scenario table itself. Data
//! only; the capture lifecycle (settle, screenshot, probe) stays in `super`.

/// A named, fully-deterministic capture viewpoint: where the camera sits + looks (raw WoW coords) and
/// the game-minute to render. One scenario → one golden PNG.
#[derive(Clone, Copy)]
pub(super) struct Scenario {
    pub(super) name: &'static str,
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
    /// The chat window with the edit box OPEN over a say/yell line mix (decision 0288 P5's look
    /// instrument, added for the centered-"Say:"/invisible-typing regression): the box focused
    /// with a typed draft through the live open path (`focus_editbox` + `chat_edit_live`), so the
    /// capture checks the header (left-flush, Say-white), the typed text past the live insets,
    /// and the three-piece input border.
    ChatEdit,
}

/// The capture viewpoints, anchored at Northshire Valley (the Human start, around `SPAWN_XY`
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

pub(super) const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "house-north",
        eye: HOUSE_EYE,
        look: [-9389.1, 71.2, 58.0],
        minute: 720,
        ui: None,
    },
    Scenario {
        name: "house-south",
        eye: HOUSE_EYE,
        look: [-9489.1, 71.2, 58.0],
        minute: 720,
        ui: None,
    },
    Scenario {
        name: "house-west",
        eye: HOUSE_EYE,
        look: [-9439.1, 121.2, 58.0],
        minute: 720,
        ui: None,
    },
    Scenario {
        name: "house-east",
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
        eye: [-9463.3, 4.4, 58.8],
        look: [-9462.1, -5.6, 58.5],
        minute: 720,
        ui: None,
    },
    Scenario {
        name: "northshire-noon",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: None,
    },
    Scenario {
        name: "northshire-dusk",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 1170, // 19:30 — warm dusk light + fog
        ui: None,
    },
    Scenario {
        name: "northshire-night",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 0, // midnight — dark DBC colours + ambient
        ui: None,
    },
    Scenario {
        name: "northshire-sky-noon",
        eye: SKY_EYE,
        look: SKY_LOOK,
        minute: 720, // day sky-dome gradient + fog horizon
        ui: None,
    },
    Scenario {
        name: "northshire-sky-dusk",
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
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Merchant),
    },
    Scenario {
        name: "ui-gossip",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Gossip),
    },
    Scenario {
        name: "ui-bank",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Bank),
    },
    Scenario {
        name: "ui-quest",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Quest),
    },
    Scenario {
        name: "ui-questgreeting",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::QuestGreeting),
    },
    Scenario {
        name: "ui-questlog",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::QuestLog),
    },
    Scenario {
        name: "ui-loot",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Loot),
    },
    Scenario {
        name: "ui-bag",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Bag),
    },
    // The GameTooltip forced open over a seeded bag slot (a green-quality item). Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-tooltip`.
    Scenario {
        name: "ui-tooltip",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::Tooltip),
    },
    // The world-mouseover tooltip at the DEFAULT corner (screen bottom-right, −13/+70) over a
    // seeded hostile wolf. Run with `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-tooltip-world`.
    Scenario {
        name: "ui-tooltip-world",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::TooltipWorld),
    },
    // The character window's paper doll over a fully-seeded synthetic self player. Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-char`.
    Scenario {
        name: "ui-char",
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
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::WorldMap),
    },
    // The spellbook over a seeded mage book. Run with `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-spellbook`.
    Scenario {
        name: "ui-spellbook",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::SpellBook),
    },
    // The chat edit box open with a typed draft over seeded lines. Run with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-chatedit`.
    Scenario {
        name: "ui-chatedit",
        eye: GROUND_EYE,
        look: GROUND_LOOK,
        minute: 720,
        ui: Some(UiFixture::ChatEdit),
    },
];
