//! The group-loot roll popups (decision 0591, `assets/ui/GroupLootFrame.xml`): the four stacked
//! `GroupLootFrame`s that answer `START_LOOT_ROLL`/`CANCEL_LOOT_ROLL` off a pushed
//! [`LootRollsState`] snapshot (the `loot_roll.rs` seam's own harness idiom, mirrored here the way
//! `loot_tests.rs` mirrors it for `set_loot`/`LootState`).

use benilla_ui::script::{
    ExtractedQuad, LootRollEntry, LootRollsState, QuadContent, ScriptValue, UiScript,
};

/// Load one shipped `assets/ui/<file>` into `s` (the loot tests' loader, duplicated here so this
/// file is self-contained), panicking on any loader error and returning the frame count.
fn load_xml(s: &UiScript, file: &str) -> usize {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/ui")
            .join(file),
    )
    .unwrap();
    let doc = benilla_ui::framexml::parse(&text).unwrap();
    let report = benilla_ui::loader::load(s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "{file}: loader errors: {:?}",
        report.errors
    );
    report.frames
}

/// Like [`load_xml`], but also demands zero loader WARNINGS — the bar this file's own assignment
/// set for `GroupLootFrame.xml` itself (a stale "unknown template" warning, e.g. the kind
/// LootFrame.xml's now-obsolete `GameFontNormalSmall` caveat would have produced, is exactly what
/// this catches). Not used for the prerequisite files below: Fonts/UiPanels/GameTooltip aren't this
/// assignment's to police and may carry warnings of their own.
fn load_xml_no_warnings(s: &UiScript, file: &str) -> usize {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/ui")
            .join(file),
    )
    .unwrap();
    let doc = benilla_ui::framexml::parse(&text).unwrap();
    let report = benilla_ui::loader::load(s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "{file}: loader errors: {:?}",
        report.errors
    );
    assert!(
        report.warnings.is_empty(),
        "{file}: loader warnings: {:?}",
        report.warnings
    );
    report.frames
}

/// The centre of the first texture quad whose path contains `needle` (a button's art), for clicking
/// it — the `loot_tests.rs` `icon_center` idiom.
fn quad_center(quads: &[ExtractedQuad], needle: &str) -> (f32, f32) {
    let r = quads
        .iter()
        .find(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains(needle))
        })
        .and_then(|q| q.rect)
        .unwrap_or_else(|| panic!("no quad for {needle}"));
    ((r.left + r.right) * 0.5, (r.bottom + r.top) * 0.5)
}

/// The colour of the first text quad whose text equals `t` (the `loot_tests.rs` `text_color` idiom).
fn text_color(quads: &[ExtractedQuad], t: &str) -> Option<[f32; 4]> {
    quads.iter().find_map(|q| match &q.content {
        QuadContent::Text {
            text: Some(x),
            color,
            ..
        } if x == t => Some(*color),
        _ => None,
    })?
}

fn setup() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml"); // ITEM_QUALITY_COLORS + GameFontNormalSmall
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml"); // PASS/NEED/GREED + item hovers
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "ActionBar.xml"); // BENILLA_FALLBACK_ICON (the in-flight icon fallback) —
                                   // buff_tests.rs's own load-order precedent for this same global.
    s
}

/// A resolved Epic (BoP) roll and an in-flight one (item template not landed yet) — the same two
/// shapes `loot_roll.rs`'s own test module fixtures, reused here against the real shipped XML.
fn rolls() -> LootRollsState {
    LootRollsState {
        rolls: vec![
            LootRollEntry {
                roll_id: 7,
                name: Some("Staff of Jordan".into()),
                texture: Some("Interface\\Icons\\INV_Staff_12".into()),
                quantity: 1,
                quality: Some(4), // Epic -> purple
                bind_on_pickup: true,
                time_left_ms: 42_000,
                item_id: 17182,
            },
            LootRollEntry {
                roll_id: 8,
                name: Some("Worn Shortsword".into()),
                texture: Some("Interface\\Icons\\INV_Sword_04".into()),
                quantity: 1,
                quality: Some(1), // Common -> white
                bind_on_pickup: false,
                time_left_ms: 60_000,
                item_id: 25,
            },
            // The item-template query hasn't landed: name/texture/quality all nil (loot_roll.rs).
            LootRollEntry {
                roll_id: 9,
                name: None,
                texture: None,
                quantity: 1,
                quality: None,
                bind_on_pickup: false,
                time_left_ms: 55_000,
                item_id: 4306,
            },
        ],
    }
}

/// The XML loads with zero loader warnings/errors, and all four `GroupLootFrame1..4` exist
/// and start hidden.
#[test]
fn shipped_group_loot_frame_loads_clean_and_starts_hidden() {
    let s = setup();
    // Per instance: the frame + IconFrame + PassButton + RollButton + GreedButton + Timer (6),
    // times 4, plus the one BenillaGroupLootFrameDriver event-listener frame.
    assert_eq!(
        load_xml_no_warnings(&s, "GroupLootFrame.xml"),
        4 * 6 + 1,
        "4 x (frame + icon + pass + need + greed + timer) + the START_LOOT_ROLL driver"
    );

    for i in 1..=4 {
        let name = format!("GroupLootFrame{i}");
        assert!(
            !s.eval::<bool>(&format!("return {name}:IsVisible()"))
                .unwrap(),
            "{name} starts hidden"
        );
    }
}

/// `START_LOOT_ROLL` claims the first free frame and shows it; a second `START_LOOT_ROLL` claims
/// the second frame, the first staying up. Also exercises the resolved-roll paint (name text +
/// quality colour + the BoP gold decoration swap + the Timer's primed max) and the Need/Greed/Pass
/// click wiring end-to-end through `RollOnLoot`.
#[test]
fn start_loot_roll_claims_frames_in_order_and_paints_the_roll() {
    let mut s = setup();
    load_xml(&s, "GroupLootFrame.xml");
    s.set_loot_rolls(rolls());

    // Roll 7 (BoP, Epic) claims frame 1.
    s.fire_event(
        "START_LOOT_ROLL",
        vec![ScriptValue::Int(7), ScriptValue::Int(42_000)],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(s
        .eval::<bool>("return GroupLootFrame1:IsVisible()")
        .unwrap());
    assert_eq!(s.eval::<i64>("return GroupLootFrame1.rollID").unwrap(), 7);
    // The Timer's max was primed to the roll's countdown before Show() (GroupLootFrame_OpenNewFrame).
    assert_eq!(
        s.eval::<(f64, f64)>("return GroupLootFrame1Timer:GetMinMaxValues()")
            .unwrap(),
        (0.0, 42_000.0)
    );
    // BoP swaps in the gold decoration; frame 2 doesn't exist yet to compare against.
    assert!(s
        .eval::<bool>("return GroupLootFrame1Decoration:IsShown()")
        .unwrap());

    // Roll 8 (BoE, Common) claims frame 2 — frame 1 stays up, untouched.
    s.fire_event(
        "START_LOOT_ROLL",
        vec![ScriptValue::Int(8), ScriptValue::Int(60_000)],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(s
        .eval::<bool>("return GroupLootFrame1:IsVisible()")
        .unwrap());
    assert!(s
        .eval::<bool>("return GroupLootFrame2:IsVisible()")
        .unwrap());
    assert_eq!(s.eval::<i64>("return GroupLootFrame2.rollID").unwrap(), 8);
    // A BoE roll keeps the plain decoration hidden.
    assert!(!s
        .eval::<bool>("return GroupLootFrame2Decoration:IsShown()")
        .unwrap());

    // The names painted, quality-coloured (frame 1 purple/Epic, frame 2 white/Common).
    s.resolve();
    let quads = s.extract();
    assert_eq!(
        s.eval::<String>("return GroupLootFrame1Name:GetText()")
            .unwrap(),
        "Staff of Jordan"
    );
    let purple = text_color(&quads, "Staff of Jordan").expect("Staff of Jordan colour");
    assert!(
        (purple[0] - 0.64).abs() < 0.02 && (purple[1] - 0.21).abs() < 0.02,
        "Epic item text is purple, got {purple:?}"
    );
    let white = text_color(&quads, "Worn Shortsword").expect("Worn Shortsword colour");
    assert!(
        (white[0] - 1.0).abs() < 0.02 && (white[1] - 1.0).abs() < 0.02,
        "Common item text is white, got {white:?}"
    );

    // Clicking Need on frame 1's roll button reaches RollOnLoot(7, 1) — the same click-through
    // proof loot_tests.rs runs for LootSlot. Roll 7 is BIND-ON-PICKUP, so the seam's gate holds
    // the vote back and asks instead (decision 0594): nothing on the wire until the popup is
    // accepted.
    let (nx, ny) = quad_center(&quads, "UI-GroupLoot-Dice-Up");
    s.mouse_button(nx, ny, "LeftButton", true);
    s.mouse_button(nx, ny, "LeftButton", false);
    assert!(
        s.take_loot_roll_votes().is_empty(),
        "a BoP Need must not reach the wire before the confirm"
    );
    assert_eq!(s.take_loot_roll_confirms(), vec![(7, 1)], "Need on roll 7");
}

/// The bind-on-pickup confirm, end to end through the real shipped XML (decision 0594): the
/// driver turns `CONFIRM_LOOT_ROLL` into the popup carrying `(rollID, rollType)`, and the popup's
/// OK re-enters `ConfirmLootRoll` to land the vote the gate withheld.
///
/// 0591 shipped this wrong — a Need on a BoP roll went straight to the wire with no prompt, which
/// binds an epic to you on a single click. The five-line path below is the whole correction.
#[test]
fn the_bop_confirm_popup_lands_the_withheld_vote() {
    let mut s = setup();
    load_xml(&s, "GroupLootFrame.xml");
    s.set_loot_rolls(rolls());

    // The app fires this after draining the seam's confirm queue (`ui_loot_roll::drain_loot_rolls`).
    s.fire_event(
        "CONFIRM_LOOT_ROLL",
        vec![ScriptValue::Int(7), ScriptValue::Int(1)],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // The popup is up and carries BOTH halves of the identity — data2 is the rollType, which the
    // reference threads through precisely so OnAccept can replay the original click.
    assert!(
        s.eval::<bool>("return StaticPopup_FindVisible(\"CONFIRM_LOOT_ROLL\") ~= nil")
            .unwrap(),
        "the confirm popup should be visible"
    );
    let (data, data2) = s
        .eval::<(i64, i64)>(
            "local d = StaticPopup_FindVisible(\"CONFIRM_LOOT_ROLL\")\nreturn d.data, d.data2",
        )
        .unwrap();
    assert_eq!((data, data2), (7, 1), "rollID and rollType ride the dialog");

    // The real 1.12 LOOT_NO_DROP/OKAY/CANCEL text, the same bar delete_item_tests holds its own
    // popup to — these are quoted locals precisely so they render under test too.
    let dialog = s
        .eval::<String>("return StaticPopup_FindVisible(\"CONFIRM_LOOT_ROLL\"):GetName()")
        .unwrap();
    assert_eq!(
        s.eval::<String>(&format!("return {dialog}Text:GetText()"))
            .unwrap(),
        "Looting this item will bind it to you."
    );
    assert_eq!(
        s.eval::<String>(&format!("return {dialog}Button1:GetText()"))
            .unwrap(),
        "Okay"
    );

    // Still nothing on the wire while the question is open.
    assert!(s.take_loot_roll_votes().is_empty());

    // Accepting re-enters ConfirmLootRoll, which bypasses the bind gate and queues the real vote.
    s.eval::<bool>(
        "local d = StaticPopup_FindVisible(\"CONFIRM_LOOT_ROLL\")\n\
         StaticPopupDialogs[\"CONFIRM_LOOT_ROLL\"].OnAccept(d.data, d.data2)\nreturn true",
    )
    .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.take_loot_roll_votes(),
        vec![(7, 1)],
        "OK on the confirm sends the Need the gate withheld"
    );
}

/// `CANCEL_LOOT_ROLL` for a rollID hides that frame, and ONLY that one.
#[test]
fn cancel_loot_roll_hides_only_that_frame() {
    let mut s = setup();
    load_xml(&s, "GroupLootFrame.xml");
    s.set_loot_rolls(rolls());

    s.fire_event(
        "START_LOOT_ROLL",
        vec![ScriptValue::Int(7), ScriptValue::Int(42_000)],
    );
    s.fire_event(
        "START_LOOT_ROLL",
        vec![ScriptValue::Int(8), ScriptValue::Int(60_000)],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // Cancel roll 7 (frame 1): only frame 1 hides, frame 2 (roll 8) is untouched.
    s.fire_event("CANCEL_LOOT_ROLL", vec![ScriptValue::Int(7)]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(!s
        .eval::<bool>("return GroupLootFrame1:IsVisible()")
        .unwrap());
    assert!(s
        .eval::<bool>("return GroupLootFrame2:IsVisible()")
        .unwrap());

    // A cancel for a rollID nobody holds any more (or never held) is a harmless no-op.
    s.fire_event("CANCEL_LOOT_ROLL", vec![ScriptValue::Int(7)]);
    s.fire_event("CANCEL_LOOT_ROLL", vec![ScriptValue::Int(999)]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(s
        .eval::<bool>("return GroupLootFrame2:IsVisible()")
        .unwrap());

    // Cancel roll 8 (frame 2): the last one standing hides too.
    s.fire_event("CANCEL_LOOT_ROLL", vec![ScriptValue::Int(8)]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(!s
        .eval::<bool>("return GroupLootFrame2:IsVisible()")
        .unwrap());
}

/// A roll whose `GetLootRollItemInfo` answers all-nil (the item template still in flight) opens its
/// frame without erroring, and paints the fallback icon / blank name / common-quality colour rather
/// than crashing on the nils.
#[test]
fn in_flight_roll_does_not_error_and_falls_back() {
    let mut s = setup();
    load_xml(&s, "GroupLootFrame.xml");
    s.set_loot_rolls(rolls());

    s.fire_event(
        "START_LOOT_ROLL",
        vec![ScriptValue::Int(9), ScriptValue::Int(55_000)],
    );
    assert!(
        s.errors().is_empty(),
        "an in-flight roll (all-nil GetLootRollItemInfo) must not error: {:?}",
        s.errors()
    );
    assert!(s
        .eval::<bool>("return GroupLootFrame1:IsVisible()")
        .unwrap());
    assert_eq!(s.eval::<i64>("return GroupLootFrame1.rollID").unwrap(), 9);
    // Blank name (not an error, not the literal "nil").
    assert_eq!(
        s.eval::<String>("return GroupLootFrame1Name:GetText()")
            .unwrap(),
        ""
    );
    // No BoP decoration for an unresolved (default-false bindOnPickUp) roll.
    assert!(!s
        .eval::<bool>("return GroupLootFrame1Decoration:IsShown()")
        .unwrap());

    // The fallback icon painted rather than an empty/erroring SetTexture(nil).
    s.resolve();
    let has_fallback_icon = s.extract().iter().any(|q| {
        matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                if p.contains("INV_Misc_QuestionMark"))
    });
    assert!(has_fallback_icon, "in-flight roll shows the fallback icon");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **The production ordering** — the one every other test in this file quietly assumes away.
///
/// `feed_loot_rolls` drains `rolls.opened` in the *same pass* that builds the snapshot, so the model
/// a `START_LOOT_ROLL` OnShow reads can never already contain the roll that just opened: the entry
/// is added to `LootRolls::active` and to `opened` in one call. Every other test here calls
/// `set_loot_rolls` **first** and so paints from a model the app would not have had yet — which is
/// exactly how a roll dialog that shipped "green" reached the director showing a `?` icon and a
/// blank name.
///
/// The guarantee this pins is therefore not an ordering but a *repair*: whenever a roll's display
/// identity changes under an open frame — the snapshot finally arriving, or a late item template —
/// `UPDATE_LOOT_ROLL(rollID)` repaints it.
#[test]
fn a_roll_that_opens_before_its_snapshot_repaints_when_it_lands() {
    let mut s = setup();
    load_xml(&s, "GroupLootFrame.xml");

    // The app's order: the event first, against a model that has no such roll at all.
    s.fire_event(
        "START_LOOT_ROLL",
        vec![ScriptValue::Int(7), ScriptValue::Int(42_000)],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(s
        .eval::<bool>("return GroupLootFrame1:IsVisible()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return GroupLootFrame1Name:GetText()")
            .unwrap(),
        "",
        "nothing to paint yet — this is the state the director saw"
    );

    // ...and then the snapshot carrying it lands.
    s.set_loot_rolls(rolls());
    s.fire_event("UPDATE_LOOT_ROLL", vec![ScriptValue::Int(7)]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert_eq!(
        s.eval::<String>("return GroupLootFrame1Name:GetText()")
            .unwrap(),
        "Staff of Jordan",
        "the repaint fills in the name the OnShow could not know"
    );
    assert!(
        s.eval::<bool>("return GroupLootFrame1Decoration:IsShown()")
            .unwrap(),
        "and the BoP gold decoration, which OnShow also painted from nothing"
    );
    s.resolve();
    let quads = s.extract();
    assert!(
        quads.iter().any(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                if p.contains("INV_Staff_12"))
        }),
        "and the real icon replaced the fallback"
    );
    // An update for a roll this frame does not hold leaves it alone.
    s.fire_event("UPDATE_LOOT_ROLL", vec![ScriptValue::Int(999)]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.eval::<String>("return GroupLootFrame1Name:GetText()")
            .unwrap(),
        "Staff of Jordan"
    );
}

/// The bare-name payoff (the header's "THE FOUR INSTANCES KEEP THE REFERENCE'S BARE NAMES"): the
/// ref's `UIPARENT_MANAGED_FRAME_POSITIONS["GroupLootFrame1"]` row actually engages, so the roll
/// dialogs ride the bottom-bar stack instead of sitting on top of the action bars.
///
/// This is the test that FALSIFIES the naming choice: under a `Benilla`-prefixed name
/// `UIParent_ManageFramePositions`'s literal `getglobal(name)` misses, the row no-ops through the
/// ref's own `if ( frame )` guard, and every assertion below collapses to the static XML 60.
/// Mirrors `cast_tests::managed_positions_track_the_bottom_bar_stack` (the same mechanism, the
/// same bar-visibility stubs); the arithmetic is the ref table's own row — baseY 60, bottomEither
/// 42, pet 42.
#[test]
fn managed_positions_engage_for_the_bare_frame_name() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UIParent.xml");
    // UiPanels.xml before GroupLootFrame.xml, mirroring the shipped manifest order
    // (`ui_script::load_default_ui`): the roll file's CONFIRM_LOOT_ROLL entry indexes
    // `StaticPopupDialogs`, and indexing a nil there aborts the WHOLE inline <Script> chunk —
    // taking every BenillaGroupLootFrame_* function down with it, not just the popup.
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GroupLootFrame.xml");

    let bottom = |s: &UiScript| s.eval::<f64>("return GroupLootFrame1:GetBottom()").unwrap();

    // The loader's post-load bootstrap with no bars in existence: the row's bare base, which
    // already differs from nothing here (the XML anchor is also 60) — the real proof is below.
    s.run("UIParent_ManageFramePositions()").unwrap();
    s.resolve();
    assert_eq!(bottom(&s), 60.0, "baseY");

    // The always-on bottom multibars (0270) appear.
    s.run("BenillaMultiBarBottomLeft = { IsShown = function() return true end }; BenillaMultiBarBottomRight = BenillaMultiBarBottomLeft; UIParent_ManageFramePositions()")
        .unwrap();
    s.resolve();
    assert_eq!(bottom(&s), 102.0, "60 + bottomEither 42 — the row engaged");

    // The stance bar shows on top of them.
    s.run("BenillaShapeshiftBarFrame = { IsShown = function() return true end }; UIParent_ManageFramePositions()")
        .unwrap();
    s.resolve();
    assert_eq!(bottom(&s), 144.0, "60 + 42 + pet 42");

    // And it settles back when the stance bar hides.
    s.run("BenillaShapeshiftBarFrame = { IsShown = function() return false end }; UIParent_ManageFramePositions()")
        .unwrap();
    s.resolve();
    assert_eq!(bottom(&s), 102.0, "back to the multibar-only stack");
}
