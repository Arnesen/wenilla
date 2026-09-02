//! The chat tab's **options menu**, end to end (decision 1589, fixing **B246**: *"no chat options
//! at all — background transparency has no home, and chat can be hard to read"*).
//!
//! These drive the shipped `ChatFrame.xml` through the shipped dropdown kit and the shipped colour
//! picker, from the mouse event inward — a right-click on the real tab, a click on the real
//! **Background** row, a drag of the real `OpacitySliderFrame` — because the report is about a
//! path, not about a function. Every hop it names (is the menu reachable? does the row open the
//! picker? does the slider move the chat box? does the value survive?) is a hop where the feature
//! could be absent while every unit underneath it passed.
//!
//! The manifest is `color_picker_tests`' (fonts → panel manager → widget kit → picker) plus
//! `ChatFrame.xml` last, which is `benilla.toc`'s own order.

use benilla_ui::script::UiScript;

use super::test_ui::load_ui as load_xml;

/// The GlobalStrings the menu labels itself with. The app runs the real
/// `Interface\FrameXML\GlobalStrings.lua` off the player's own patch chain at boot
/// (`load_global_strings`); these stand in for it here, at their real 1.12.1 values —
/// `BACKGROUND` l.131 (whose own trailing comment in that file reads *"Title in the chat
/// preferences menu"*, i.e. this exact row), `DISPLAY` l.937, `FONT_SIZE` l.1983,
/// `FONT_SIZE_TEMPLATE` l.1984, `CHAT_OPTIONS_LABEL` l.673, `NEWBIE_TOOLTIP_CHATOPTIONS` l.2724.
fn bake_strings(s: &UiScript) {
    s.run(
        r#"
        BACKGROUND = "Background"
        DISPLAY = "Display"
        FONT_SIZE = "Font Size"
        FONT_SIZE_TEMPLATE = "%d pt"
        CHAT_OPTIONS_LABEL = "Chat Options"
        NEWBIE_TOOLTIP_CHATOPTIONS = "Right-click to get a list of customizable options for this window. Left-click and drag to move the window."
    "#,
    )
    .unwrap();
}

/// The chat window with everything its tab menu reaches under it.
fn chat_with_menu() -> UiScript {
    let mut s = UiScript::new().unwrap();
    for file in [
        "Fonts.xml",
        "MoneyFrame.xml",
        "UiPanels.xml",
        "UIParent.xml",
        "GameTooltip.xml",
        "UIDropDownMenu.xml",
        "ScrollTemplates.xml",
        "UIPanelTemplates.xml",
        "Interface\\FrameXML\\ColorPickerFrame.xml",
        "Interface\\FrameXML\\UIMenu.xml", // the kit ChatMenu/EmoteMenu/VoiceMacroMenu build from
        "ChatFrame.xml",
    ] {
        load_xml(&s, file);
    }
    bake_strings(&s);
    s.set_screen_size(1600.0, 900.0);
    s.resolve();
    s
}

/// Right-click a frame's centre through the real hit path.
fn right_click(s: &mut UiScript, frame: &str) {
    let (x, y) = s
        .eval::<(f64, f64)>(&format!("return {frame}:GetCenter()"))
        .unwrap();
    s.mouse_button(x as f32, y as f32, "RightButton", true);
    s.mouse_button(x as f32, y as f32, "RightButton", false);
    s.resolve();
}

/// Move the mouse onto a frame's centre through the real hit path (fires its `OnEnter`).
fn hover(s: &mut UiScript, frame: &str) {
    let (x, y) = s
        .eval::<(f64, f64)>(&format!("return {frame}:GetCenter()"))
        .unwrap();
    s.mouse_move(x as f32, y as f32);
    s.resolve();
}

/// Settle the dock's hover fade all the way open: park the cursor in the middle of the window and
/// run past the 0.2 s stationary arm plus the 0.15 s ramp (`chat_tests`' own idiom).
fn reveal_dock(s: &mut UiScript) {
    let (x, y): (f32, f32) = s
        .eval(
            "return (ChatFrame1:GetLeft() + ChatFrame1:GetRight()) / 2, \
             (ChatFrame1:GetBottom() + ChatFrame1:GetTop()) / 2",
        )
        .unwrap();
    s.mouse_move(x, y);
    for _ in 0..25 {
        s.tick(0.016);
        s.resolve();
    }
}

/// Left-click a frame's centre through the real hit path.
fn left_click(s: &mut UiScript, frame: &str) {
    let (x, y) = s
        .eval::<(f64, f64)>(&format!("return {frame}:GetCenter()"))
        .unwrap();
    s.mouse_button(x as f32, y as f32, "LeftButton", true);
    s.mouse_button(x as f32, y as f32, "LeftButton", false);
    s.resolve();
}

/// **B246's first half: the menu exists and the right button reaches it.**
///
/// Before 1589 the tab registered only `LeftButtonUp`, so the right-click was swallowed by the
/// engine's default click set and no handler ever ran — which is why the report reads "no chat
/// options at all" rather than "the menu is missing a row".
#[test]
fn right_clicking_a_chat_tab_opens_its_options_menu() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = chat_with_menu();
    assert!(
        s.eval::<bool>("return not DropDownList1:IsVisible()")
            .unwrap(),
        "no menu before any click"
    );

    right_click(&mut s, "ChatFrame1Tab");
    assert!(
        s.eval::<bool>("return DropDownList1:IsVisible()").unwrap(),
        "the tab's right-click opens the options menu"
    );
    // The window verb, then the DISPLAY block, in the reference's own order
    // (FloatingChatFrame.lua l.236-307).
    assert_eq!(
        s.eval::<i64>("return DropDownList1.numButtons").unwrap(),
        4,
        "Lock Window + Display title + Font Size + Background"
    );
    // **Unlock**, not Lock: both dock windows ship LOCKED (the stock chat-cache `LOCKED 1`), so
    // the row offers the way out of it. `chat_resize_tests` drives the toggle itself.
    assert_eq!(
        s.eval::<String>("return DropDownList1Button1:GetText()")
            .unwrap(),
        "Unlock Window"
    );
    assert_eq!(
        s.eval::<String>("return DropDownList1Button2:GetText()")
            .unwrap(),
        "Display"
    );
    assert_eq!(
        s.eval::<String>("return DropDownList1Button3:GetText()")
            .unwrap(),
        "Font Size"
    );
    assert_eq!(
        s.eval::<String>("return DropDownList1Button4:GetText()")
            .unwrap(),
        "Background"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A LEFT click still selects the tab and does not open the menu — the control that must not
/// change. (The reference's own fork: the right-button arm returns before the select.)
#[test]
fn a_left_click_still_selects_the_tab_and_opens_no_menu() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = chat_with_menu();
    left_click(&mut s, "ChatFrame2Tab");
    assert_eq!(
        s.eval::<i64>("return BenillaFCF.selected").unwrap(),
        2,
        "left-click selects"
    );
    assert!(
        s.eval::<bool>("return not DropDownList1:IsVisible()")
            .unwrap(),
        "a left click opens no menu"
    );
    // …and it closes one that is open (ref FCF_Tab_OnClick's `CloseDropDownMenus()`).
    right_click(&mut s, "ChatFrame1Tab");
    assert!(s.eval::<bool>("return DropDownList1:IsVisible()").unwrap());
    left_click(&mut s, "ChatFrame1Tab");
    assert!(
        s.eval::<bool>("return not DropDownList1:IsVisible()")
            .unwrap(),
        "the left click closed the open menu"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **B246's actual ask: the background slider.** Background → the colour picker with its opacity
/// slider → dragging the thumb moves the chat window's stored alpha *and* the pixels.
///
/// The two numbers this pins are the ones a wrong implementation gets wrong in opposite
/// directions: the slider is **reversed** (0 at the top is fully opaque), so the seed is `1 - a`
/// and the read-back is `1 - value`; and the store is a **byte**, so 0.8 comes back as
/// `204/255`, not as 0.8.
#[test]
fn the_background_row_opens_the_picker_and_its_opacity_slider_drives_the_window() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = chat_with_menu();
    right_click(&mut s, "ChatFrame1Tab");

    // The shipped window is fully transparent at rest — the classic "text over the world".
    assert_eq!(
        s.eval::<f64>("local _,_,_,_,_,a = GetChatWindowInfo(1) return a")
            .unwrap(),
        0.0
    );
    // …so the reversed slider is seeded at its far end.
    assert_eq!(
        s.eval::<f64>("return DropDownList1Button4.opacity")
            .unwrap(),
        1.0,
        "info.opacity is 1 - a (the ref's own 'the slider is reversed')"
    );

    left_click(&mut s, "DropDownList1Button4ColorSwatch");
    assert!(
        s.eval::<bool>("return ColorPickerFrame:IsVisible()")
            .unwrap(),
        "the Background swatch opens the colour picker"
    );
    assert!(
        s.eval::<bool>("return OpacitySliderFrame:IsVisible()")
            .unwrap(),
        "and the picker wears its opacity slider — B246's 'background slider'"
    );
    assert_eq!(
        s.eval::<f64>("return OpacitySliderFrame:GetValue()")
            .unwrap(),
        1.0,
        "the slider opens seeded from the window"
    );

    // Drag the thumb to 80% opaque. The slider's OnValueChanged is the live-preview hop.
    s.run("OpacitySliderFrame:SetValue(1 - 0.8)").unwrap();
    // **203, not 204** — and that is the reference, not a rounding bug of ours. Its setter is
    // `__ftol(x · 255.0)`, which TRUNCATES, and `0.8 × 255` is `203.99999999999997` in binary.
    let stored: f64 = s
        .eval("local _,_,_,_,_,a = GetChatWindowInfo(1) return a")
        .unwrap();
    assert!(
        (stored - 203.0 / 255.0).abs() < 1e-9,
        "the drag reached the engine store, quantized to its byte: {stored}"
    );

    // And the pixels follow: one FCF_OnUpdate tick with the cursor nowhere near the dock, and the
    // background wears the new base rather than fading back to nothing.
    //
    // **The paint takes the FLOAT the slider produced, not the byte the store kept** — that split
    // is the reference's, not ours: its `FCF_SetWindowAlpha` calls `SetAlpha(alpha)` with the raw
    // value and `SetChatWindowAlpha(id, alpha)` with the same one, and only the second quantizes.
    // The two reconcile at the next `FloatingChatFrame_Update`, i.e. the next login.
    s.run("FCF_OnUpdate(1.0)").unwrap();
    let painted: f64 = s.eval("return ChatFrame1Background:GetAlpha()").unwrap();
    assert!(
        (painted - 0.8).abs() < 1e-6,
        "a window the player made solid stays solid off-hover: {painted}"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The default look is **unchanged** by all of the above — the alpha rule generalised, it did not
/// move. A window at the shipped base of 0 still rides the full 0 → `DEFAULT_CHATFRAME_ALPHA`
/// hover ramp, which is the look the director signed off in 0288.
#[test]
fn the_shipped_window_still_fades_zero_to_a_quarter() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = chat_with_menu();
    s.mouse_move(1500.0, 850.0);
    for _ in 0..4 {
        s.tick(0.016);
        s.resolve();
    }
    assert_eq!(
        s.eval::<f64>("return ChatFrame1Background:GetAlpha()")
            .unwrap(),
        0.0,
        "at rest: invisible"
    );
    reveal_dock(&mut s);
    assert_eq!(
        s.eval::<f64>("return ChatFrame1Background:GetAlpha()")
            .unwrap(),
        0.25,
        "hovered: DEFAULT_CHATFRAME_ALPHA, exactly as before 1589"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The **Font Size** submenu — B246's other half, *"chat can be hard to read"*. The four heights
/// are the reference's `CHAT_FONT_HEIGHTS`, the tick follows the font the frame is wearing, and a
/// pick moves both the live font and the stored `SIZE`.
#[test]
fn the_font_size_submenu_resizes_the_window_and_stores_the_pick() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = chat_with_menu();
    right_click(&mut s, "ChatFrame1Tab");

    // Open the submenu the way a player does — hover the `hasArrow` row (UIDropDownMenu.xml's
    // row OnEnter → `UIDropDownMenuRow_OpenSubmenu`).
    hover(&mut s, "DropDownList1Button3");
    assert_eq!(
        s.eval::<i64>("return DropDownList2.numButtons").unwrap(),
        4,
        "CHAT_FONT_HEIGHTS is 12, 14, 16, 18"
    );
    assert_eq!(
        s.eval::<String>("return DropDownList2Button1:GetText()")
            .unwrap(),
        "12 pt"
    );
    // ChatFontNormal is 14 px, so the second row is the one already ticked.
    assert!(
        s.eval::<bool>("return DropDownList2Button2Check:IsVisible()")
            .unwrap(),
        "the tick follows the font the frame is actually wearing"
    );

    left_click(&mut s, "DropDownList2Button3"); // 16 pt
    let (_, height): (String, f64) = s
        .eval("local f, h = ChatFrame1:GetFont() return f, h")
        .unwrap();
    assert_eq!(height, 16.0, "the live font moved");
    assert_eq!(
        s.eval::<i64>("local _, size = GetChatWindowInfo(1) return size")
            .unwrap(),
        16,
        "and the pick is stored as the cache's SIZE"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The menu belongs to the tab that opened it: window 2's menu writes window 2. This is what the
/// `id` on the tab and the per-tab capsule buy — `FCF_GetCurrentChatFrameID` reads the open menu's
/// parent, so a single shared capsule would have written window 1 whichever tab was clicked.
#[test]
fn each_tabs_menu_writes_its_own_window() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = chat_with_menu();
    right_click(&mut s, "ChatFrame2Tab");
    assert_eq!(
        s.eval::<i64>("return FCF_GetCurrentChatFrameID()").unwrap(),
        2
    );
    left_click(&mut s, "DropDownList1Button4ColorSwatch");
    s.run("OpacitySliderFrame:SetValue(0)").unwrap(); // reversed: 0 = fully opaque
    assert_eq!(
        s.eval::<f64>("local _,_,_,_,_,a = GetChatWindowInfo(2) return a")
            .unwrap(),
        1.0,
        "window 2 took the write"
    );
    assert_eq!(
        s.eval::<f64>("local _,_,_,_,_,a = GetChatWindowInfo(1) return a")
            .unwrap(),
        0.0,
        "window 1 did not"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Cancel restores the colour and the alpha the row was opened with — the picker's own
/// `previousValues` contract, which the reference reaches through the same `cancelFunc` field.
#[test]
fn cancelling_the_picker_puts_the_window_back() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = chat_with_menu();
    // Start from a non-default look so "restored" is distinguishable from "reset".
    s.run("FCF_SetWindowAlpha(ChatFrame1, 0.4) FCF_SetWindowColor(ChatFrame1, 0.2, 0.4, 0.6)")
        .unwrap();
    let before: f64 = s
        .eval("local _,_,_,_,_,a = GetChatWindowInfo(1) return a")
        .unwrap();

    right_click(&mut s, "ChatFrame1Tab");
    left_click(&mut s, "DropDownList1Button4ColorSwatch");
    s.run("OpacitySliderFrame:SetValue(0)").unwrap();
    assert_eq!(
        s.eval::<f64>("local _,_,_,_,_,a = GetChatWindowInfo(1) return a")
            .unwrap(),
        1.0,
        "the drag previewed live"
    );

    left_click(&mut s, "ColorPickerCancelButton");
    let after: f64 = s
        .eval("local _,_,_,_,_,a = GetChatWindowInfo(1) return a")
        .unwrap();
    assert!(
        (after - before).abs() < 1e-9,
        "Cancel restored the alpha: {after} vs {before}"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The `UPDATE_CHAT_WINDOWS` seam: the host restores a player's saved file into the engine table
/// and fires the reference's own event; the window re-reads it and repaints. Without this the
/// restore would land in the store and show up only after something else happened to invalidate
/// the fade latch.
#[test]
fn the_restore_event_repaints_the_window_from_the_store() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = chat_with_menu();
    s.set_chat_window_looks([(
        0,
        benilla_ui::script::ChatWindowLook {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
            font_size: 18,
            locked: true,
            docked: Some(1),
        },
    )]);
    s.fire_event("UPDATE_CHAT_WINDOWS", vec![]);
    s.run("BenillaFCF.hover = false FCF_OnUpdate(1.0)").unwrap();
    assert_eq!(
        s.eval::<f64>("return ChatFrame1Background:GetAlpha()")
            .unwrap(),
        1.0,
        "the restored alpha is on screen"
    );
    let (r, g, b): (f64, f64, f64) = s
        .eval("return ChatFrame1Background:GetVertexColor()")
        .unwrap();
    assert_eq!((r, g, b), (1.0, 0.0, 0.0), "and the restored tint");
    let (_, height): (String, f64) = s
        .eval("local f, h = ChatFrame1:GetFont() return f, h")
        .unwrap();
    assert_eq!(height, 18.0, "and the restored font size");
    // Restoring is not a player edit: nothing was queued back at the host.
    assert!(
        s.take_chat_window_changes().is_empty(),
        "the restore path does not dirty the file it came from"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
