//! The always-up world-state readout (`assets/ui/WorldStateFrame.xml`) against the shipped XML —
//! report B190's second half, driven the way the client drives it: rows pushed, then
//! `UPDATE_WORLD_STATES` fired.
//!
//! What these pin is the *frame's* half of the contract — that it reads the ten returns in the
//! right order, swaps the dynamic icon on the row's own state, and empties itself when a scope
//! admits nothing. Which rows exist at all is [`crate::world_state_ui`]'s, tested there against
//! the real `WorldStateUI.dbc`.

use benilla_ui::script::{QuadContent, ScriptValue, UiScript, WorldStateUiView};

fn load_xml(s: &UiScript, file: &str) {
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
}

fn harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for f in [
        "Fonts.xml",
        "MoneyFrame.xml",
        "UiPanels.xml",
        "UIParent.xml",
        "GameTooltip.xml",
        "WorldStateFrame.xml",
    ] {
        load_xml(&s, f);
    }
    s
}

/// An Eastern Plaguelands-shaped row.
fn row(icon: &str, text: &str, tooltip: &str) -> WorldStateUiView {
    WorldStateUiView {
        ui_state: 1,
        text: text.into(),
        icon: icon.into(),
        tooltip: tooltip.into(),
        ..Default::default()
    }
}

/// Every shown text string, in draw order.
fn texts(s: &mut UiScript) -> Vec<String> {
    s.resolve();
    s.extract()
        .iter()
        .filter_map(|q| match &q.content {
            QuadContent::Text { text: Some(t), .. } if !t.is_empty() => Some(t.clone()),
            _ => None,
        })
        .collect()
}

/// Every shown texture path, in draw order.
fn textures(s: &mut UiScript) -> Vec<String> {
    s.resolve();
    s.extract()
        .iter()
        .filter_map(|q| match &q.content {
            QuadContent::Texture { path: Some(p), .. } => Some(p.clone()),
            _ => None,
        })
        .collect()
}

fn push(s: &mut UiScript, rows: Vec<WorldStateUiView>) {
    s.set_world_state_ui(rows);
    s.tick(0.0);
    s.fire_event("UPDATE_WORLD_STATES", vec![]);
}

/// The Eastern Plaguelands readout: two labelled tower counters, each with its faction icon.
#[test]
fn the_tower_counters_draw_with_their_icons() {
    let mut s = harness();
    s.fire_event("PLAYER_ENTERING_WORLD", vec![ScriptValue::Str("".into())]);
    assert!(
        !texts(&mut s).iter().any(|t| t.contains("Towers")),
        "nothing before a push"
    );

    push(
        &mut s,
        vec![
            row(
                "Interface\\WorldStateFrame\\AllianceTower",
                "Towers Controlled: 3",
                "Alliance Towers Controlled",
            ),
            row(
                "Interface\\WorldStateFrame\\HordeTower",
                "Towers Controlled: 1",
                "Horde Towers Controlled",
            ),
        ],
    );

    let shown = texts(&mut s);
    assert!(
        shown.contains(&"Towers Controlled: 3".to_string())
            && shown.contains(&"Towers Controlled: 1".to_string()),
        "both counters on screen: {shown:?}"
    );
    let art = textures(&mut s);
    assert!(
        art.iter().any(|p| p.contains("AllianceTower"))
            && art.iter().any(|p| p.contains("HordeTower")),
        "both faction icons on screen: {art:?}"
    );
}

/// A scope that admits nothing empties the readout — and the pooled rows from the busier scope
/// are hidden rather than left painting a stale count.
#[test]
fn leaving_the_zone_clears_the_readout() {
    let mut s = harness();
    s.fire_event("PLAYER_ENTERING_WORLD", vec![ScriptValue::Str("".into())]);
    push(
        &mut s,
        vec![row(
            "Interface\\WorldStateFrame\\AllianceTower",
            "Towers Controlled: 3",
            "Alliance Towers Controlled",
        )],
    );
    assert!(texts(&mut s).contains(&"Towers Controlled: 3".to_string()));

    push(&mut s, Vec::new());
    assert!(
        !texts(&mut s).iter().any(|t| t.contains("Towers")),
        "no stale count left painting: {:?}",
        texts(&mut s)
    );
    assert!(
        !textures(&mut s).iter().any(|p| p.contains("AllianceTower")),
        "and no orphaned icon"
    );
}

/// The dynamic icon: a row whose own state is live draws the alternate art (Warsong Gulch's enemy
/// flag) instead of its static icon, and reverts when the state drops. `uiState` is what decides,
/// which is the whole reason the first return is a number and not the text.
#[test]
fn the_dynamic_icon_follows_the_rows_own_state() {
    let mut s = harness();
    s.fire_event("PLAYER_ENTERING_WORLD", vec![ScriptValue::Str("".into())]);

    let flag_row = |ui_state| WorldStateUiView {
        ui_state,
        text: "2/3".into(),
        icon: "Interface\\TargetingFrame\\UI-PVP-Alliance".into(),
        dynamic_icon: "Interface\\WorldStateFrame\\HordeFlag".into(),
        tooltip: "Alliance flag captures".into(),
        dynamic_tooltip: "Horde flag has been picked up".into(),
        ..Default::default()
    };

    push(&mut s, vec![flag_row(0)]);
    let art = textures(&mut s);
    assert!(
        art.iter().any(|p| p.contains("UI-PVP-Alliance")),
        "state down — the static icon: {art:?}"
    );
    assert!(!art.iter().any(|p| p.contains("HordeFlag")));

    push(&mut s, vec![flag_row(1)]);
    let art = textures(&mut s);
    assert!(
        art.iter().any(|p| p.contains("HordeFlag")),
        "state up — the enemy flag replaces it: {art:?}"
    );
    assert!(!art.iter().any(|p| p.contains("UI-PVP-Alliance")));
}

/// A row with no icon at all (the Eastern Plaguelands progress line) still draws its text — the
/// icon is optional, and the row must not collapse when the DBC column is empty.
#[test]
fn a_row_without_an_icon_still_shows_its_text() {
    let mut s = harness();
    s.fire_event("PLAYER_ENTERING_WORLD", vec![ScriptValue::Str("".into())]);
    push(
        &mut s,
        vec![WorldStateUiView {
            ui_state: 1,
            text: "Progress: 60".into(),
            extended_ui: "CAPTUREPOINT".into(),
            extended_ui_state: [60, 40, 0],
            ..Default::default()
        }],
    );
    assert!(texts(&mut s).contains(&"Progress: 60".to_string()));
}

/// The bindings' own edges, read from Lua the way an addon would. Two are easy to get wrong and
/// both are the reference's (`0x4c5a40`/`0x4c5a70`): an out-of-range index answers exactly one
/// value — the number `0`, not nil and not ten nils — and a non-number argument raises.
#[test]
fn the_bindings_answer_the_reference_shape() {
    let mut s = harness();
    s.set_world_state_ui(vec![WorldStateUiView {
        ui_state: 7,
        text: "Towers Controlled: 3".into(),
        icon: "Interface\\WorldStateFrame\\AllianceTower".into(),
        tooltip: "Alliance Towers Controlled".into(),
        extended_ui_state: [1, 2, 3],
        ..Default::default()
    }]);

    assert_eq!(s.eval::<i64>("return GetNumWorldStateUI()").unwrap(), 1);
    // Ten values, in order, with no nils among the strings.
    assert_eq!(
        s.eval::<String>(
            "local a,b,c,d,e,f,g,h,i,j = GetWorldStateUIInfo(1)\n\
             return a..'|'..b..'|'..c..'|'..d..'|'..e..'|'..f..'|'..g..'|'..h..'|'..i..'|'..j"
        )
        .unwrap(),
        "7|Towers Controlled: 3|Interface\\WorldStateFrame\\AllianceTower||\
         Alliance Towers Controlled|||1|2|3",
        "empty columns are empty strings, never nil"
    );
    assert_eq!(
        s.eval::<i64>("return select('#', GetWorldStateUIInfo(1))")
            .unwrap(),
        10
    );

    // Out of range: ONE value, the number 0.
    assert_eq!(
        s.eval::<i64>("return select('#', GetWorldStateUIInfo(2))")
            .unwrap(),
        1
    );
    assert_eq!(s.eval::<i64>("return GetWorldStateUIInfo(2)").unwrap(), 0);
    assert_eq!(
        s.eval::<i64>("return GetWorldStateUIInfo(0)").unwrap(),
        0,
        "the index is 1-based"
    );

    // A non-number argument raises rather than answering quietly.
    assert!(
        s.eval::<i64>("return GetWorldStateUIInfo('nope')").is_err(),
        "a non-numeric argument is an error, not a nil"
    );
    // ...but a numeric string coerces, as `lua_isnumber` does.
    assert_eq!(
        s.eval::<i64>("return (GetWorldStateUIInfo('1'))").unwrap(),
        7
    );
}
