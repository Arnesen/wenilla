//! The engine unit tooltip builder (decision 0274 P3, law per 0276): the level-line composition
//! (four templates, the rank words, "??", Corpse, "Race Class (Player)"), the flag lines
//! (PvP white / Skinnable red / Civilian green), the world-mouseover drive (default anchor +
//! `UPDATE_MOUSEOVER_UNIT` recolor + the fade arm on loss), and the health-bar watcher.

use super::common::script;
use crate::script::*;

fn wolf() -> UnitState {
    UnitState {
        exists: true,
        name: Some("Timber Wolf".into()),
        health: 30,
        max_health: 50,
        level: 10,
        reaction: 2, // hostile
        creature_type_name: Some("Beast".into()),
        rank: 2, // rare-elite prints ELITE (the byte table)
        skinnable: true,
        ..Default::default()
    }
}

/// The creature law: gold name, subtitle, "Level 10 Beast (Elite)", red Skinnable.
#[test]
fn creature_line_law() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    let mut u = wolf();
    u.subtitle = Some("Alpha".into());
    s.set_unit("target", Some(u));
    s.set_player_req_state(PlayerReqState {
        level: 12,
        ..Default::default()
    });
    s.run(
        r#"
        local a = CreateFrame("Button", "UF1"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        assert(tt:SetUnit("target") == 1, "SetUnit returns 1 on a live unit")
        assert(TTTextLeft1:GetText() == "Timber Wolf")
        assert(TTTextLeft2:GetText() == "Alpha")
        assert(TTTextLeft3:GetText() == "Level 10 Beast (Elite)", "got " .. TTTextLeft3:GetText())
        assert(TTTextLeft4:GetText() == "Skinnable")
        assert(tt:SetUnit("nosuch") == nil, "a missing unit answers nil")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// The faction-name line sits between the level line and "PvP" (the builder-tail block the §2
/// order omitted — the director's Marshal McBride reference: Level, Stormwind, PvP); the
/// CIVILIAN line is the dishonorable-kill warning, whole gate (`0x612550`): PvP bit + civilian
/// flag + HOSTILE + GREY/trivial — a friendly (or non-grey) civilian never shows it; LEADER
/// (white) needs only the PvP bit + the flag (`0x6125c0`).
#[test]
fn faction_line_and_civilian_gate() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_player_req_state(PlayerReqState {
        level: 30,
        ..Default::default()
    });
    // The reference shot's shape: a FRIENDLY civilian guard — faction line, PvP, NO Civilian.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Marshal McBride".into()),
            level: 20,
            reaction: 6, // friendly
            creature_type_name: Some("Humanoid".into()),
            faction_name: Some("Stormwind".into()),
            pvp: true,
            civilian: true,
            ..Default::default()
        }),
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "UF9"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetUnit("target")
        assert(TTTextLeft2:GetText() == "Level 20", "friendly creature: no type word; got " .. TTTextLeft2:GetText())
        assert(TTTextLeft3:GetText() == "Stormwind", "faction line before PvP; got " .. TTTextLeft3:GetText())
        assert(TTTextLeft4:GetText() == "PvP")
        assert(TTTextLeft5 == nil or TTTextLeft5:GetText() == nil, "friendly civilian shows NO Civilian line")
    "#,
    )
    .unwrap();
    // The warning case: HOSTILE + grey (level 20 vs player 30 → gap 10 > band 8) + PvP-flagged.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Defias Civilian".into()),
            level: 20,
            reaction: 2, // hostile
            creature_type_name: Some("Humanoid".into()),
            pvp: true,
            civilian: true,
            racial_leader: true,
            ..Default::default()
        }),
    );
    s.run(
        r#"
        TT:SetOwner(UF9, "ANCHOR_RIGHT")
        TT:SetUnit("target")
        assert(TTTextLeft2:GetText() == "Level 20 Humanoid", "got " .. TTTextLeft2:GetText())
        assert(TTTextLeft3:GetText() == "PvP")
        assert(TTTextLeft4:GetText() == "Civilian", "hostile+grey+pvp civilian warns; got " .. TTTextLeft4:GetText())
        assert(TTTextLeft5:GetText() == "Leader")
    "#,
    )
    .unwrap();
    // Same unit but NOT grey (level 25, gap 5 ≤ band 8): the warning drops.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Defias Civilian".into()),
            level: 25,
            reaction: 2,
            creature_type_name: Some("Humanoid".into()),
            pvp: true,
            civilian: true,
            ..Default::default()
        }),
    );
    s.run(
        r#"
        TT:SetOwner(UF9, "ANCHOR_RIGHT")
        TT:SetUnit("target")
        assert(TTTextLeft3:GetText() == "PvP")
        assert(TTTextLeft4 == nil or TTTextLeft4:GetText() == nil, "a non-grey civilian does not warn")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// Players read "Race Class (Player)"; the dead read "Corpse"; a world boss reads "??".
#[test]
fn level_line_variants() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_player_req_state(PlayerReqState {
        level: 60,
        ..Default::default()
    });
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Bandit".into()),
            level: 32,
            is_player: true,
            race: Some("Human".into()),
            class: Some("Rogue".into()),
            pvp: true,
            ..Default::default()
        }),
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "UF2"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetUnit("target")
        assert(TTTextLeft2:GetText() == "Level 32 Human Rogue (Player)", "got " .. TTTextLeft2:GetText())
        assert(TTTextLeft3:GetText() == "PvP")
    "#,
    )
    .unwrap();
    // Dead: the class slot becomes Corpse.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Slain Wolf".into()),
            level: 3,
            dead: true,
            reaction: 2,
            creature_type_name: Some("Beast".into()),
            ..Default::default()
        }),
    );
    s.run(
        r#"
        TT:SetOwner(UF2, "ANCHOR_RIGHT")
        TT:SetUnit("target")
        assert(TTTextLeft2:GetText() == "Level 3 Corpse", "got " .. TTTextLeft2:GetText())
    "#,
    )
    .unwrap();
    // A world boss (rank 3) reads "??" + (Boss).
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Kazzak".into()),
            level: 63,
            reaction: 2,
            creature_type_name: Some("Demon".into()),
            rank: 3,
            ..Default::default()
        }),
    );
    s.run(
        r#"
        TT:SetOwner(UF2, "ANCHOR_RIGHT")
        TT:SetUnit("target")
        assert(TTTextLeft2:GetText() == "Level ?? Demon (Boss)", "got " .. TTTextLeft2:GetText())
    "#,
    )
    .unwrap();
    // The "??" gate, byte-pinned: HOSTILE (reaction ≤ 2) + 10 levels up reads "??"…
    let ten_up = |reaction: u8, is_player: bool| UnitState {
        exists: true,
        name: Some("Elder".into()),
        level: 70,
        reaction,
        is_player,
        creature_type_name: (!is_player).then(|| "Beast".into()),
        race: is_player.then(|| "Orc".into()),
        class: is_player.then(|| "Shaman".into()),
        ..Default::default()
    };
    s.set_unit("target", Some(ten_up(2, false)));
    s.run(
        r#"
        TT:SetOwner(UF2, "ANCHOR_RIGHT"); TT:SetUnit("target")
        assert(TTTextLeft2:GetText() == "Level ?? Beast", "hostile 10-up, got " .. TTTextLeft2:GetText())
    "#,
    )
    .unwrap();
    // …but UNFRIENDLY (reaction 3) does not — the internal bound is hated/hostile only…
    s.set_unit("target", Some(ten_up(3, false)));
    s.run(
        r#"
        TT:SetOwner(UF2, "ANCHOR_RIGHT"); TT:SetUnit("target")
        assert(TTTextLeft2:GetText() == "Level 70 Beast", "unfriendly 10-up, got " .. TTTextLeft2:GetText())
    "#,
    )
    .unwrap();
    // …and PLAYERS never read "??" regardless of reaction/delta.
    s.set_unit("target", Some(ten_up(2, true)));
    s.run(
        r#"
        TT:SetOwner(UF2, "ANCHOR_RIGHT"); TT:SetUnit("target")
        assert(TTTextLeft2:GetText() == "Level 70 Orc Shaman (Player)", "hostile player, got " .. TTTextLeft2:GetText())
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// The world-mouseover drive: `world_tooltip_unit` fires the default-anchor script, renders,
/// fires `UPDATE_MOUSEOVER_UNIT` (the Lua recolor), and the health bar tracks later pushes;
/// hover loss arms the fade (alpha ramps, then hides).
#[test]
fn world_hover_drive_and_health_watcher() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_unit("mouseover", Some(wolf()));
    s.run(
        r#"
        anchored, recolored = 0, 0
        -- A real full-screen UIParent, like the shipped UIParent.xml provides: the default-anchor
        -- handler must do the REAL seating (a counter-only stub is exactly the hole that let the
        -- unwired live handler park the plate at screen center).
        UIParent = CreateFrame("Frame", "UIParent"); UIParent:SetAllPoints()
        local tt = CreateFrame("GameTooltip", "GameTooltip")
        tt:Hide()
        -- The XML wiring, test-side: default anchor + the reaction recolor + a status bar child.
        tt:SetScript("OnTooltipSetDefaultAnchor", function()
            anchored = anchored + 1
            GameTooltip:SetOwner(UIParent, "ANCHOR_NONE")
            GameTooltip:SetPoint("BOTTOMRIGHT", "UIParent", "BOTTOMRIGHT", -13, 70)
        end)
        tt:RegisterEvent("UPDATE_MOUSEOVER_UNIT")
        tt:SetScript("OnEvent", function()
            recolored = recolored + 1
            getglobal("GameTooltipTextLeft1"):SetTextColor(1, 0, 0)
        end)
        local bar = CreateFrame("StatusBar", "GameTooltipStatusBar", tt)
        bar:SetPoint("TOPLEFT", tt, "BOTTOMLEFT", 2, -1); bar:SetSize(100, 8)
    "#,
    )
    .unwrap();
    assert!(s.world_tooltip_unit("mouseover"), "the hover shows");
    s.run(
        r#"
        assert(anchored == 1, "default anchor fired")
        assert(recolored == 1, "UPDATE_MOUSEOVER_UNIT fired")
        assert(GameTooltip:IsShown())
        assert(GameTooltipTextLeft1:GetText() == "Timber Wolf")
        local _, mx = GameTooltipStatusBar:GetMinMaxValues()
        assert(mx == 50 and GameTooltipStatusBar:GetValue() == 30, "bar seeded from the snapshot")
    "#,
    )
    .unwrap();
    // The handler's seating resolved: bottom-right of the 800×600 screen, −13 in, 70 up.
    // (Answer the line measures first — the auto-size needs them before the plate has a rect.)
    let answers: Vec<(u32, f32, f32, u64)> = s
        .fontstrings_needing_measure()
        .iter()
        .map(|r| (r.id, 80.0, 10.0, r.key))
        .collect();
    s.set_measured_text(&answers);
    s.resolve();
    s.run(
        r#"assert(GameTooltip:GetRight() == 787 and GameTooltip:GetBottom() == 70,
                  "plate at the default corner, got " .. tostring(GameTooltip:GetRight()) .. "," .. tostring(GameTooltip:GetBottom()))"#,
    )
    .unwrap();
    // A health push for the LIVE token re-drives the bar without a rebuild.
    let mut hurt = wolf();
    hurt.health = 12;
    s.set_unit("mouseover", Some(hurt));
    s.run(
        r#"assert(GameTooltipStatusBar:GetValue() == 12, "the health watcher tracked the push")"#,
    )
    .unwrap();
    // Hover loss: the fade arms; past the ramp the tooltip hides.
    s.world_tooltip_fade();
    s.tick(0.6);
    s.run(r#"assert(not GameTooltip:IsShown(), "faded out after the ramp")"#)
        .unwrap();
    assert!(s.take_errors().is_empty());
}

/// The minimap BLIP tooltip (`minimap_tooltip`): refuses without a Minimap widget; with one, one
/// white line seated against the widget (the INTERIM ANCHOR_LEFT law), world-owned so the shared
/// fade arm hides it on hover loss.
#[test]
fn minimap_blip_tooltip_shows_and_fades() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(r#"CreateFrame("GameTooltip", "GameTooltip")"#)
        .unwrap();
    assert!(
        !s.minimap_tooltip("Stormwind", 700.0, 500.0, false),
        "no UIParent yet: the show refuses"
    );
    s.run(
        r#"
        local up = CreateFrame("Frame", "UIParent")
        up:SetPoint("BOTTOMLEFT"); up:SetSize(800, 600)
    "#,
    )
    .unwrap();
    assert!(s.minimap_tooltip("Stormwind", 795.0, 500.0, false));
    s.run(
        r#"
        assert(GameTooltipTextLeft1:GetText() == "Stormwind")
        assert(GameTooltip:IsShown(), "the blip tooltip shows")
    "#,
    )
    .unwrap();
    // Seated 5 px from the 800-wide screen's right edge: the clamp (the client's G bit4)
    // slides the plate back inside instead of letting it clip (director-caught).
    let answers: Vec<(u32, f32, f32, u64)> = s
        .fontstrings_needing_measure()
        .iter()
        .map(|r| (r.id, 80.0, 10.0, r.key))
        .collect();
    s.set_measured_text(&answers);
    s.resolve();
    s.run(
        r#"assert(GameTooltip:GetRight() <= 800, "clamped inside the screen, got " .. tostring(GameTooltip:GetRight()))"#,
    )
    .unwrap();
    // Follow: a move re-seats without rebuilding the line.
    s.world_tooltip_move(400.0, 300.0);
    s.run(r#"assert(GameTooltipTextLeft1:GetText() == "Stormwind", "content survives a move")"#)
        .unwrap();
    // Hover loss rides the shared world fade: armed, then hidden past the ramp.
    s.world_tooltip_fade();
    s.tick(0.6);
    s.run(r#"assert(not GameTooltip:IsShown(), "faded out after the ramp")"#)
        .unwrap();
    assert!(s.take_errors().is_empty());
}
