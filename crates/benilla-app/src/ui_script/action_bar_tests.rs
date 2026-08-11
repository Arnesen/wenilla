use benilla_ui::script::{ActionSlot, QuadContent, ScriptValue, UiScript};

/// Load the real `assets/ui/ActionBar.xml` (the shipped default bar) into a bare engine and
/// drive it with a synthetic action snapshot — the slice-1 chain minus Bevy: template
/// expansion over 12 instances, the vanilla bonus-page formula, icon paint on events, empty
/// slots drawing no icon, and a physical click queuing the right UseAction id.
#[test]
fn shipped_action_bar_drives_end_to_end() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in ["Cooldown.xml", "ActionBar.xml"] {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/ui")
                .join(file),
        )
        .unwrap();
        let doc = benilla_ui::framexml::parse(&text).unwrap();
        let report = benilla_ui::loader::load(&s, &doc, &|_| None);
        assert!(
            report.errors.is_empty(),
            "{file}: loader errors: {:?}",
            report.errors
        );
        if file == "ActionBar.xml" {
            assert_eq!(
                report.frames, 34,
                "bar + XP StatusBar (+ its numerals overlay) + exhaustion tick + max-level rail + art frame + 12 buttons (each with a Cooldown child) + 2 page buttons + the performance meter and its hover button"
            );
        }
    }

    // A warrior in battle stance: offset 1 ⇒ the bar shows actions 73..84.
    s.set_bonus_bar_offset(1);
    s.set_action(
        73,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Ability_SteelMelee".into()),
            kind: 0x00,
            action: 100,
            count: 0,
        }),
    );
    s.set_action(
        74,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Ability_Rogue_Ambush".into()),
            kind: 0x00,
            action: 101,
            count: 0,
        }),
    );
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // Button 1 paints action 73's icon seated in the art frame's first well: the 1024-wide bar
    // centers at BOTTOM of the 1024-wide screen ⇒ bar/art-frame left edge = (1024-1024)/2 = 0.
    // Button1 anchors the art frame's BOTTOMLEFT +(8,4), 36×36 ⇒ x[8,44] y[4,40]; the icon fills
    // the button (owner-sized) ⇒ the same rect. The chain stride is 36 + 6 = 42, so button i's left
    // edge is 8 + (i-1)*42.
    s.resolve();
    let quads = s.extract();
    let icon = |path: &str| {
        quads
            .iter()
            .find(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p == path))
            .and_then(|q| q.rect)
    };
    let r = icon("Interface\\Icons\\Ability_SteelMelee").expect("button 1 icon");
    assert_eq!((r.left, r.bottom, r.right, r.top), (8.0, 4.0, 44.0, 40.0));
    let r2 = icon("Interface\\Icons\\Ability_Rogue_Ambush").expect("button 2 icon");
    assert_eq!(r2.left, 8.0 + 42.0); // button 2 left = 50
                                     // Twelve quickslot rings (every button draws its NormalTexture), two icons only.
    let rings = quads
        .iter()
        .filter(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                    if p.contains("UI-Quickslot2"))
        })
        .count();
    assert_eq!(rings, 12);
    let icons = quads
        .iter()
        .filter(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                    if p.contains("Icons"))
        })
        .count();
    assert_eq!(icons, 2, "empty slots draw no icon quad");

    // A physical click on button 1 queues UseAction(73) — the stance page's id, not 1. Button 1's
    // center is (8+18, 4+18) = (26, 22).
    s.mouse_button(26.0, 22.0, "LeftButton", true);
    s.mouse_button(26.0, 22.0, "LeftButton", false);
    assert_eq!(s.take_action_uses(), vec![73]);

    // The keybinding entry (the app's key feed runs `ActionButtonDown/Up(i)` on the two
    // key edges — the ref's ACTIONBUTTONn binding, ActionButton.lua:15-45): UP fires UseAction
    // directly with no checkCursor (a keybind never places, decision 0216 §7) — but ONLY from
    // the PUSHED state a DOWN set, so a stray release with no press is the ref's own no-op.
    s.run("ActionButtonUp(2)").unwrap();
    assert!(
        s.take_action_uses().is_empty(),
        "an Up without a Down is a no-op (the PUSHED gate)"
    );
    let depressed = |s: &UiScript| {
        s.extract()
            .iter()
            .filter(|q| {
                matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                        if p.contains("UI-Quickslot-Depress"))
            })
            .count()
    };
    s.run("ActionButtonDown(2)").unwrap();
    assert_eq!(depressed(&s), 1, "key DOWN shows the pushed texture");
    assert_eq!(
        s.eval::<String>("return ActionButton2:GetButtonState()")
            .unwrap(),
        "PUSHED"
    );
    s.run("ActionButtonUp(2)").unwrap();
    assert_eq!(s.take_action_uses(), vec![74], "key '2' fires action 74");
    assert_eq!(depressed(&s), 0, "key UP restores the normal state");

    // Stance drops (offset 0): the bar re-pages to actions 1..12 — all empty here, icons clear.
    s.set_bonus_bar_offset(0);
    s.fire_event("UPDATE_BONUS_ACTIONBAR", vec![]);
    s.resolve();
    let icons_after = s
        .extract()
        .iter()
        .filter(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                    if p.contains("Icons"))
        })
        .count();
    assert_eq!(icons_after, 0, "re-page to an empty page clears the icons");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

fn load_action_bar(s: &UiScript) {
    for file in ["Cooldown.xml", "ActionBar.xml"] {
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
}

/// The state/feedback layer (decision 0137 phase 4) through the REAL shipped XML: a pushed
/// cooldown + `ACTIONBAR_UPDATE_COOLDOWN` arms the button's Cooldown widget, `IsCurrentAction` +
/// `ACTIONBAR_UPDATE_STATE` checks the ring, and `IsUsableAction`'s OOM pair blue-tints the icon.
#[test]
fn state_feedback_drives_cooldown_checked_and_usable_through_the_xml() {
    use benilla_ui::script::ActionState;

    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_action_bar(&s);
    s.set_action(
        1,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Spell_Fire_FlameBolt".into()),
            kind: 0x00,
            action: 133,
            count: 0,
        }),
    );
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    s.tick(10.0); // a nonzero GetTime epoch

    // A running 10 s cooldown with 6 s left + the update event: the widget shows mid-sweep.
    s.set_action_state(
        1,
        Some(ActionState {
            usable: true,
            cooldown: Some((6_000, 10_000, true)),
            ..Default::default()
        }),
    );
    s.fire_event("ACTIONBAR_UPDATE_COOLDOWN", vec![]);
    s.resolve();
    let sweep = s.extract().into_iter().find_map(|q| match q.content {
        QuadContent::Cooldown { fraction, flash } => Some((fraction, flash)),
        _ => None,
    });
    let (fraction, flash) = sweep.expect("the button's Cooldown widget is showing");
    assert!(
        (fraction - 0.4).abs() < 1e-3,
        "6 s of 10 s left ⇒ the sweep sits at 40%, got {fraction}"
    );
    assert_eq!(flash, None);

    // The checked ring on the current action (the transcribed UpdateState).
    s.set_action_state(
        1,
        Some(ActionState {
            usable: true,
            current: true,
            cooldown: Some((6_000, 10_000, true)),
            ..Default::default()
        }),
    );
    s.fire_event("ACTIONBAR_UPDATE_STATE", vec![]);
    assert!(s.eval::<bool>("return ActionButton1:GetChecked()").unwrap());

    // The OOM blue tint (the transcribed UpdateUsable): usable=false + notEnoughMana=true.
    s.set_action_state(
        1,
        Some(ActionState {
            usable: false,
            not_enough_mana: true,
            cooldown: Some((6_000, 10_000, true)),
            ..Default::default()
        }),
    );
    s.fire_event("ACTIONBAR_UPDATE_USABLE", vec![]);
    s.resolve();
    let icon_color = s.extract().into_iter().find_map(|q| match &q.content {
        QuadContent::Texture {
            path: Some(p),
            color,
            ..
        } if p.contains("Spell_Fire_FlameBolt") => Some(*color),
        _ => None,
    });
    let c = icon_color.expect("icon quad").expect("vertex color set");
    assert_eq!(
        (c[0], c[1], c[2]),
        (0.5, 0.5, 1.0),
        "the ref's out-of-power blue-grey"
    );

    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The pie paints OVER its button's art. The Cooldown child is born at button-level+1, and the
/// draw key's LEVEL term outranks 0884's bucket-wide layer term — so the sweep quad must sort
/// after the icon (BACKGROUND) and after the button's own special textures. A regression here is
/// invisible to every store/feed instrument (the triple still pushes; only the pixels vanish
/// under the icon), which is exactly why the order is pinned end-to-end through the real XML.
#[test]
fn the_cooldown_sweep_paints_over_the_buttons_icon_and_ring() {
    use benilla_ui::script::ActionState;

    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_action_bar(&s);
    s.set_action(
        1,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Spell_Fire_FlameBolt".into()),
            kind: 0x00,
            action: 133,
            count: 0,
        }),
    );
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    s.tick(10.0);
    s.set_action_state(
        1,
        Some(ActionState {
            usable: true,
            cooldown: Some((6_000, 10_000, true)),
            ..Default::default()
        }),
    );
    s.fire_event("ACTIONBAR_UPDATE_COOLDOWN", vec![]);
    s.resolve();

    let quads = s.extract();
    let pos = |pred: &dyn Fn(&QuadContent) -> bool| quads.iter().position(|q| pred(&q.content));
    let icon = pos(&|c| {
        matches!(c, QuadContent::Texture { path: Some(p), .. } if p.contains("Spell_Fire_FlameBolt"))
    })
    .expect("the icon texture quad");
    let ring = pos(
        &|c| matches!(c, QuadContent::Texture { path: Some(p), .. } if p.contains("UI-Quickslot2")),
    )
    .expect("the NormalTexture ring quad");
    let sweep = pos(&|c| matches!(c, QuadContent::Cooldown { .. })).expect("the sweep quad");
    assert!(
        icon < sweep,
        "the sweep (index {sweep}) must paint over the icon (index {icon})"
    );
    assert!(
        ring < sweep,
        "the sweep (index {sweep}) must paint over the button ring (index {ring})"
    );
}

/// An action button is a TWO-button button (decision 0908; director's report B200: "I can't right
/// click food on my bar to eat it or right click spells"). The ref's `ActionButton_OnLoad`
/// registers `("LeftButtonUp", "RightButtonUp")` (ActionButton.lua:109) and its OnClick body reads
/// no `arg1` — so either button runs the same fork and right-click USES the action. The widget
/// default is `{"LeftButtonUp"}` (`benilla_ui::script::button::wants_click`), so without the
/// explicit registration the input path silently swallowed every right-click on the bar. Driven
/// through the real shipped XML and the real `mouse_button` path, which is the only place the
/// registration set is consulted.
#[test]
fn a_right_click_on_an_action_button_uses_the_action() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_action_bar(&s);

    s.set_action(
        1,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\INV_Misc_Food_11".into()),
            kind: 0x80, // an ITEM action — the food/mount case the report is about
            action: 4540,
            count: 5,
        }),
    );
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    s.resolve();

    // Button 1's center (geometry as in the tests above): (26, 22).
    s.mouse_button(26.0, 22.0, "RightButton", true);
    s.mouse_button(26.0, 22.0, "RightButton", false);
    assert_eq!(
        s.take_action_uses(),
        vec![1],
        "right-click queues the same UseAction a left-click does"
    );

    // The middle button is registered by neither the ref nor us: it stays swallowed, which is what
    // proves the assertion above is the REGISTRATION and not the gate having been removed.
    s.mouse_button(26.0, 22.0, "MiddleButton", true);
    s.mouse_button(26.0, 22.0, "MiddleButton", false);
    assert!(
        s.take_action_uses().is_empty(),
        "an unregistered button still reaches nothing"
    );

    // Shift+right-click picks up, exactly as shift+left does — the OnClick fork is button-blind.
    s.set_modifiers(true, false, false);
    s.mouse_button(26.0, 22.0, "RightButton", true);
    s.mouse_button(26.0, 22.0, "RightButton", false);
    s.set_modifiers(false, false, false);
    assert!(s.take_action_uses().is_empty());
    assert!(
        s.cursor_payload().is_some(),
        "shift+right-click carries the action, like shift+left"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Decision 0216 §7 (byte-verified 0218 §4) driven through the REAL shipped XML, not the engine
/// unit tests directly — the modifier-key mirror gating `PickupAction` vs `UseAction`, end to end.
#[test]
fn shift_click_picks_up_not_uses() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_action_bar(&s);

    s.set_action(
        1,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Spell_A".into()),
            kind: 0x00,
            action: 111,
            count: 0,
        }),
    );
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    s.resolve();

    // Button 1's center (unchanged geometry from the test above): (26, 22).
    s.set_modifiers(true, false, false); // IsShiftKeyDown() true
    s.mouse_button(26.0, 22.0, "LeftButton", true);
    s.mouse_button(26.0, 22.0, "LeftButton", false);
    s.set_modifiers(false, false, false);

    assert!(
        s.take_action_uses().is_empty(),
        "shift-click PICKS UP, never queues a use"
    );
    assert!(s.cursor_payload().is_some(), "action 1 is on the cursor");
    assert!(
        !s.eval::<bool>("return HasAction(1)").unwrap(),
        "slot cleared"
    );
    assert_eq!(
        s.take_action_sets(),
        vec![(1, 0)],
        "picking up queues the clear-the-slot send"
    );

    // A PLAIN click while holding routes through checkCursor=1 to a place — closes the loop back
    // onto the same (now empty) slot: no shift needed once something is already held.
    s.mouse_button(26.0, 22.0, "LeftButton", true);
    s.mouse_button(26.0, 22.0, "LeftButton", false);
    assert!(s.take_action_uses().is_empty(), "routed to place, not use");
    assert!(s.cursor_payload().is_none(), "empty destination clears");
    assert!(s.eval::<bool>("return HasAction(1)").unwrap());
    assert_eq!(s.take_action_sets(), vec![(1, 111)]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **Lock ActionBars** (decision 1136) — the `LOCK_ACTIONBAR` uvar the Options window's Action Bars
/// row and the `TOGGLEACTIONBARLOCK` binding both write, guarding the two drag ends the way the
/// reference does (ActionBarFrame.xml:23-38).
///
/// The teeth are the second half: the reference leaves the shift-click pick-up in `OnClick`
/// UNGUARDED (l.12-22), so a locked bar still yields to the deliberate gesture. Guarding it too
/// would be a "sensible" tightening that silently diverges — and would leave a locked bar with no
/// way to rearrange it at all.
#[test]
fn the_action_bar_lock_stops_the_drag_and_leaves_shift_click_alone() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_action_bar(&s);
    assert_eq!(
        s.eval::<String>("return LOCK_ACTIONBAR").unwrap(),
        "0",
        "the bar ships unlocked, the reference's own default"
    );

    s.set_action(
        1,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Spell_A".into()),
            kind: 0x00,
            action: 111,
            count: 0,
        }),
    );
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    s.resolve();

    s.run(r#"LOCK_ACTIONBAR = "1""#).unwrap();
    s.run("BenillaActionButton_OnDragStart(ActionButton1)")
        .unwrap();
    assert!(
        s.cursor_payload().is_none(),
        "a locked bar does not give the action up to a drag"
    );
    assert!(
        s.eval::<bool>("return HasAction(1)").unwrap(),
        "slot intact"
    );
    assert!(
        s.take_action_sets().is_empty(),
        "and nothing is sent to the server"
    );

    // Shift-click still picks up — the reference's unguarded fork, and the way out of a locked bar.
    s.set_modifiers(true, false, false);
    s.mouse_button(26.0, 22.0, "LeftButton", true);
    s.mouse_button(26.0, 22.0, "LeftButton", false);
    s.set_modifiers(false, false, false);
    assert!(
        s.cursor_payload().is_some(),
        "shift-click is not what the lock stops"
    );

    // The receiving end is guarded too: the held action cannot be dropped back by a drag…
    s.run("BenillaActionButton_OnReceiveDrag(ActionButton1)")
        .unwrap();
    assert!(
        s.cursor_payload().is_some(),
        "a locked slot refuses the drop"
    );
    // …and unlocking makes both ends live again.
    s.run(r#"LOCK_ACTIONBAR = "0""#).unwrap();
    s.run("BenillaActionButton_OnReceiveDrag(ActionButton1)")
        .unwrap();
    assert!(s.cursor_payload().is_none(), "unlocked, the drop lands");
    assert_eq!(
        s.take_action_sets(),
        vec![(1, 0), (1, 111)],
        "the shift-pickup's clear and the unlocked drop's set — and nothing from the two \
         refused gestures between them"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A physical drag from button 1 onto OCCUPIED button 2: the byte-verified action-bar hop (0218
/// §4) — the displaced action lands on the cursor, TWO independent `action_sets` entries across
/// the one gesture (0218 §4: "a drag-swap is two sends, never atomic").
#[test]
fn drag_drop_onto_another_button_hops_the_displaced_action() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_action_bar(&s);

    s.set_action(
        1,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Spell_A".into()),
            kind: 0x00,
            action: 111,
            count: 0,
        }),
    );
    s.set_action(
        2,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Spell_B".into()),
            kind: 0x00,
            action: 222,
            count: 0,
        }),
    );
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    s.resolve();

    // Button 1 center (26, 22), button 2 center (68, 22) — same geometry as the end-to-end test.
    s.mouse_button(26.0, 22.0, "LeftButton", true);
    s.mouse_move(40.0, 22.0); // past the 4px drag-start threshold
    let consumed = s.mouse_button(68.0, 22.0, "LeftButton", false);
    assert!(consumed, "OnReceiveDrag consumed the release");

    assert!(
        !s.eval::<bool>("return HasAction(1)").unwrap(),
        "slot 1 emptied"
    );
    assert!(s.eval::<bool>("return HasAction(2)").unwrap());
    assert_eq!(
        s.eval::<String>("return GetActionTexture(2)").unwrap(),
        "Interface\\Icons\\Spell_A",
        "slot 2 now shows the placed action"
    );
    let (kind, src) = s
        .eval::<(String, i64)>("local k, slot = GetCursorInfo() return k, slot")
        .unwrap();
    assert_eq!(
        (kind.as_str(), src),
        ("action", 2),
        "the displaced action hopped on, sourced from slot 2"
    );

    // Two independent sends across the one gesture: the pickup's clear, then the place's write.
    assert_eq!(s.take_action_sets(), vec![(1, 0), (2, 111)]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The Count fontstring, on the reference's own gate (`ActionButton_UpdateCount`, ref
/// ActionButton.lua:285-292): **`IsConsumableAction`**, never "count > 0". The director's report —
/// a mount on the bar wearing a stack number "1" — is the non-consumable case: a mount holds an
/// on-use spell with zero charges and `InventoryType` 0, so `IsConsumableAction 0x4e5250` answers
/// false and the ref paints nothing at all. It repaints on `ACTIONBAR_SLOT_CHANGED` alongside the
/// icon (the same event the identity resolve fires).
#[test]
fn count_fontstring_follows_is_consumable_action_not_the_bag_count() {
    use benilla_ui::script::ActionState;

    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_action_bar(&s);

    // Multi-digit counts, deliberately: the static HotKey labels are single characters
    // ("1".."9","0","-","="), so a single-digit count could false-positive match an unrelated
    // button's hotkey text rather than the Count fontstring actually under test.
    s.set_action(
        1,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Spell_A".into()),
            kind: 0x00, // SPELL
            action: 111,
            count: 42, // the app never actually sets this for a spell — proves the XML, not the feed
        }),
    );
    s.set_action(
        2,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
            kind: 0x80, // ITEM — a stack of food
            action: 117,
            count: 15,
        }),
    );
    s.set_action_state(
        2,
        Some(ActionState {
            consumable: true,
            ..Default::default()
        }),
    );
    // The report's own shape: an ITEM action the player holds exactly one of, which is NOT
    // consumable (a mount). The count is fed all the same; the gate is what must suppress it.
    s.set_action(
        3,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Ability_Mount_Undeadhorse".into()),
            kind: 0x80,
            action: 13332,
            count: 11,
        }),
    );
    s.set_action_state(
        3,
        Some(ActionState {
            consumable: false,
            ..Default::default()
        }),
    );
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    s.resolve();

    // Read the fontstrings by name, not by scanning painted text: a single-character count
    // ("0") is indistinguishable from a neighbouring button's static HotKey label by content.
    let count_of = |s: &UiScript, n: u32| {
        s.eval::<String>(&format!("return ActionButton{n}Count:GetText() or \"\""))
            .unwrap()
    };
    assert_eq!(
        count_of(&s, 1),
        "",
        "SPELL kind never shows a count, however GetActionCount answers"
    );
    assert_eq!(
        count_of(&s, 2),
        "15",
        "a consumable ITEM shows its bag count"
    );
    assert_eq!(
        count_of(&s, 3),
        "",
        "B201: a NON-consumable ITEM (a mount) shows no stack number, whatever the count says"
    );
    // …and it really is painted, not just set (the multi-digit value is unambiguous in the quads).
    assert!(
        s.extract()
            .iter()
            .any(|q| matches!(&q.content, QuadContent::Text { text: Some(t), .. } if t == "15")),
        "the consumable's count reaches the screen"
    );

    // Eat the stack down to nothing. A *consumable* keeps its fontstring and reads a literal "0"
    // — the ref's `SetText(GetActionCount(...))` is unconditional inside the gate, and 0216 §7's
    // `count > 0` blank was ours, not the reference's.
    s.set_action(
        2,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
            kind: 0x80,
            action: 117,
            count: 0,
        }),
    );
    s.fire_event("ACTIONBAR_SLOT_CHANGED", vec![ScriptValue::Int(2)]);
    s.resolve();
    assert_eq!(
        count_of(&s, 2),
        "0",
        "a spent consumable reads 0, it does not go blank"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

#[test]
fn shipped_bag_frame_drives_end_to_end() {
    use benilla_ui::script::{ContainerSlot, ContainerState};

    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    // The bag toggle now seats on the main action bar (MainMenuBarArtFrame, ActionBar.xml);
    // load the bar FIRST so the toggle's cross-file relativeTo resolves to the real art frame at
    // SetPoint time. The app loads ActionBar before BagFrame (load_default_ui order); without it the
    // anchor would silently fall back to the screen root (it only lands right here by the 1024-wide
    // coincidence — screen BOTTOMRIGHT == the full-width bar's art-frame BOTTOMRIGHT).
    for file in ["Cooldown.xml", "ActionBar.xml"] {
        let abtext = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/ui")
                .join(file),
        )
        .unwrap();
        let abdoc = benilla_ui::framexml::parse(&abtext).unwrap();
        benilla_ui::loader::load(&s, &abdoc, &|_| None);
    }
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui/BagFrame.xml"),
    )
    .unwrap();
    let doc = benilla_ui::framexml::parse(&text).unwrap();
    let report = benilla_ui::loader::load(&s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "loader errors: {:?}",
        report.errors
    );
    assert_eq!(
        report.frames, 259,
        "backpack (window + 16 slots × 2 [button + Cooldown child] + 3 money coins \
         + close = 37) + 4 equipped-bag windows (window + 20 slots × 2 + close = 42 each = 168) \
         + the KEYRING window (another 42 — the same template, decision 0765) \
         + the bag-bar toggle (1) + its 4 bar slots (4) + the keyring button (1) \
         — 0216 slice 2 + the slot Cooldown children \
         + one $parentItemAnim card per bag-bar button (6 — decision 0887)"
    );
    // The re-skinned bag purse reuses BenillaMoney_Set/_Clear (they live in MerchantFrame.xml); load
    // it too so the shared helper is defined when BenillaBagFrame_Update runs (the same reuse the loot
    // window makes — at runtime every FrameXML file is loaded before any event fires).
    let mtext = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui/MerchantFrame.xml"),
    )
    .unwrap();
    let mdoc = benilla_ui::framexml::parse(&mtext).unwrap();
    benilla_ui::loader::load(&s, &mdoc, &|_| None);
    // The slot OnClick hides the hover tooltip on pickup; load GameTooltip.xml so GameTooltip
    // exists (the runtime always loads it before the bag frame — ui_script::load_default_ui).
    let ttext = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui/GameTooltip.xml"),
    )
    .unwrap();
    let tdoc = benilla_ui::framexml::parse(&ttext).unwrap();
    benilla_ui::loader::load(&s, &tdoc, &|_| None);

    // The app's feed: a backpack with Tough Jerky ×5 in slot 1.
    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
            count: 5,
            quality: Some(1),
            item_id: 117,
            link: None,
            locked: false,
            equip_slots: Vec::new(),
            cooldown: None,
            readable: false,
            creator: None,
            flags: 0,
            enchants: Vec::new(),
        },
    );
    s.set_container(
        0,
        Some(ContainerState {
            name: Some("Backpack".into()),
            num_slots: 16,
            slots,
        }),
    );
    s.fire_event("BAG_UPDATE", vec![ScriptValue::Int(0)]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // Hidden by default: no jerky on screen.
    s.resolve();
    let jerky_visible = |quads: &[benilla_ui::script::ExtractedQuad]| {
        quads.iter().any(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                    if p.contains("INV_Misc_Food_16"))
        })
    };
    assert!(!jerky_visible(&s.extract()), "bag window starts hidden");

    // Click the toggle → the window opens, slot 1 paints the jerky in the TOP-LEFT well. The toggle
    // now seats on the bar's art frame BOTTOMRIGHT +(-6,2), 37×37: art frame BOTTOMRIGHT is the bar's
    // (full-width, bottom-anchored) corner (1024,0) ⇒ toggle x[981,1018] y[2,39], center (999.5,20.5).
    // The bag frame is unchanged: 192×240 at BOTTOMRIGHT (0,70) ⇒ x[832,1024] y[70,310]; the real
    // backpack chain (ContainerFrame.lua l.425-444) seats the top-left button (game slot 1) at
    // BOTTOMRIGHT frame +(-138,153), a 37×37 icon ⇒ x[849,886] y[223,260].
    s.mouse_button(999.0, 20.0, "LeftButton", true);
    s.mouse_button(999.0, 20.0, "LeftButton", false);
    s.resolve();
    let quads = s.extract();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    let r = quads
        .iter()
        .find(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                    if p.contains("INV_Misc_Food_16"))
        })
        .and_then(|q| q.rect)
        .expect("slot 1 icon visible after toggle");
    assert_eq!(
        (r.left, r.bottom, r.right, r.top),
        (849.0, 223.0, 886.0, 260.0)
    );
    // The stack count renders as text.
    assert!(
        quads
            .iter()
            .any(|q| { matches!(&q.content, QuadContent::Text { text: Some(t), .. } if t == "5") }),
        "stack count shows"
    );

    // LEFT-click picks the item up onto the cursor — a local drag, no wire use queued until a place.
    // The top-left well center is (867,241).
    s.mouse_button(867.0, 241.0, "LeftButton", true);
    s.mouse_button(867.0, 241.0, "LeftButton", false);
    assert!(
        s.take_container_uses().is_empty(),
        "left-click is a pickup, not a use"
    );
    assert!(
        s.cursor_item().is_some(),
        "left-click put the item on the cursor"
    );
    assert!(
        s.take_container_moves().is_empty(),
        "a pickup alone queues no move"
    );

    // RIGHT-click while holding cancels the pickup (client-side; nothing sent).
    s.mouse_button(867.0, 241.0, "RightButton", true);
    s.mouse_button(867.0, 241.0, "RightButton", false);
    assert!(
        s.cursor_item().is_none(),
        "right-click cancelled the held cursor"
    );
    assert!(s.take_container_uses().is_empty(), "cancel sends nothing");

    // RIGHT-click with an empty cursor uses slot 1 (UseContainerItem → the app's use/equip fork).
    s.mouse_button(867.0, 241.0, "RightButton", true);
    s.mouse_button(867.0, 241.0, "RightButton", false);
    assert_eq!(s.take_container_uses(), vec![(0, 1)]);

    // Toggle again → hidden.
    s.mouse_button(999.0, 20.0, "LeftButton", true);
    s.mouse_button(999.0, 20.0, "LeftButton", false);
    s.resolve();
    assert!(!jerky_visible(&s.extract()), "toggle closes the window");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The empty-wells regression (director-reported, 2026-07-10): the bar-level event fan ran
/// UpdateUsable on EMPTY buttons, whose `IsUsableAction` answers (nil, nil) — the 0.4 grey
/// `SetVertexColor` landed on the texture-less icon region and drew a solid grey plate over every
/// empty well. The fix is the ref's own HasAction gate on the fan (ActionButton.lua registers the
/// state handlers only while the button has an action). This drives the exact failing sequence —
/// a usable/cooldown event with empties on the bar — and asserts no empty well gains a solid.
#[test]
fn state_events_leave_empty_wells_untinted() {
    use benilla_ui::script::ActionState;

    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_action_bar(&s);
    // One occupied slot; 2..12 empty.
    s.set_action(
        1,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Spell_Fire_FlameBolt".into()),
            kind: 0x00,
            action: 133,
            count: 0,
        }),
    );
    s.set_action_state(
        1,
        Some(ActionState {
            usable: false,
            not_enough_mana: true,
            ..Default::default()
        }),
    );
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    // The failing edges: the fanned usable/cooldown/state events with empties on the bar.
    s.fire_event("ACTIONBAR_UPDATE_USABLE", vec![]);
    s.fire_event("ACTIONBAR_UPDATE_COOLDOWN", vec![]);
    s.fire_event("ACTIONBAR_UPDATE_STATE", vec![]);
    s.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    s.resolve();

    // No texture-less colored quad anywhere on the button row (the icon regions of empty wells
    // must stay path-None + color-None); the occupied icon still carries its OOM blue.
    let mut oom_icon = None;
    for q in s.extract() {
        match &q.content {
            QuadContent::Texture {
                path: None,
                color: Some(c),
                ..
            } if q.rect.is_some_and(|r| r.right - r.left <= 40.0) => {
                // Well-sized only: the XP bar's 1024-wide black backdrop is a legitimate solid.
                panic!("an empty well gained a solid color quad: {c:?}")
            }
            QuadContent::Texture {
                path: Some(p),
                color,
                ..
            } if p.contains("Spell_Fire_FlameBolt") => oom_icon = *color,
            _ => {}
        }
    }
    assert_eq!(
        oom_icon,
        Some([0.5, 0.5, 1.0, 1.0]),
        "the occupied button still tints OOM blue"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The white-buttons regression (director-reported 2026-08-07, decision 1108): a slot that was
/// OCCUPIED — UpdateUsable painted its icon's 1/1/1 usable tint — then goes EMPTY (the feed's
/// character-switch diff: `set_action(None)` + `ACTIONBAR_SLOT_CHANGED`) kept the tint on the
/// now-artless icon region and drew it as a solid WHITE square. Two laws close it, both asserted
/// here: the empty arm HIDES the icon (ref ActionButton.lua l.168), and the engine emits nothing
/// for a texture-less region whatever its surviving tint (`0x7706e0` — the draw gate is `+0xcc`,
/// never the colour).
#[test]
fn an_occupied_slot_going_empty_leaves_no_white_plate() {
    use benilla_ui::script::ActionState;

    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_action_bar(&s);
    s.set_action(
        3,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Spell_Nature_HealingTouch".into()),
            kind: 0x00,
            action: 5185,
            count: 0,
        }),
    );
    s.set_action_state(
        3,
        Some(ActionState {
            usable: true,
            ..Default::default()
        }),
    );
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    // The usable pass paints the occupied icon's 1/1/1 usable tint — the tint that survives.
    s.fire_event("ACTIONBAR_UPDATE_USABLE", vec![]);
    s.resolve();
    assert!(
        s.extract().iter().any(|q| matches!(&q.content,
            QuadContent::Texture { path: Some(p), .. } if p.contains("Spell_Nature_HealingTouch"))),
        "the occupied slot draws its icon"
    );

    // The character switch: the new character's table has nothing in slot 3.
    s.set_action(3, None);
    s.set_action_state(3, None);
    s.fire_event("ACTIONBAR_SLOT_CHANGED", vec![ScriptValue::Int(3)]);
    s.resolve();
    for q in s.extract() {
        match &q.content {
            QuadContent::Texture { path: Some(p), .. }
                if p.contains("Spell_Nature_HealingTouch") =>
            {
                panic!("the emptied slot still draws the old icon")
            }
            QuadContent::Texture {
                path: None,
                color: Some(c),
                ..
            } if q.rect.is_some_and(|r| r.right - r.left <= 40.0) => {
                // Well-sized only, as in the sibling test: page-wide solids are legitimate.
                panic!("the emptied slot draws its surviving tint as a solid plate: {c:?}")
            }
            _ => {}
        }
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
