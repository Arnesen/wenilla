//! The unit right-click popups reached the two ways a solo player invites someone (director
//! report, 2026-07-17): right-clicking a **target** unit frame, and right-clicking a **chat
//! name**. Both drive the shared UnitPopup engine (`UnitPopup.xml`); the chat path adds the
//! `SetItemRef` player branch + its minimal `FriendsFrame` stand-in (`ItemRef.xml`). Proven
//! serverless through the real hit/route paths.

use benilla_ui::script::{PartyRequest, UiScript, UnitState};

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

/// The production prefix the popups need (ui_script/mod.rs order): the dropdown kit, the unit
/// popups, the chat-link router (`ItemRef.xml`, after UnitPopup — its `FriendsDropDown` inherits
/// `UIDropDownMenuTemplate` and opens a UnitPopup FRIEND menu), then the unit frames.
fn load_popup_frames(s: &UiScript) {
    for file in [
        "Fonts.xml",
        "UIParent.xml",
        "GameTooltip.xml",
        "UIDropDownMenu.xml",
        "UnitPopup.xml",
        "ItemRef.xml",
        "UnitFrames.xml",
    ] {
        load_xml(s, file);
    }
}

/// The row labels the FRIEND / PLAYER menus bake at load (production reads GlobalStrings.lua; a
/// bare harness doesn't). Only the rows these menus actually SHOW need real text — the DEFERRED
/// rows are hidden before they're added.
fn bake_strings(s: &UiScript) {
    s.run(
        r#"
        WHISPER = "Whisper"
        PARTY_INVITE = "Invite"
        TRADE = "Trade"
        CANCEL = "Cancel"
        RAID_TARGET_ICON = "Raid Target Icon"
    "#,
    )
    .unwrap();
}

/// Right-clicking a **chat player name** opens the name-only FRIEND dropdown (the ref's
/// `FriendsFrame_ShowDropdown`): Whisper + Invite for a stranger. Clicking Invite queues an
/// invite-by-name; a plain left-click on the name whispers instead. This is the bug the director
/// hit — the player branch of `SetItemRef` used to be a no-op stub.
#[test]
fn chat_name_right_click_opens_the_invite_menu() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    bake_strings(&s);
    load_popup_frames(&s);
    s.resolve();
    assert!(s.errors().is_empty(), "load errors: {:?}", s.errors());

    assert!(
        s.eval::<bool>("return not DropDownList1:IsVisible()")
            .unwrap(),
        "no menu before any click"
    );

    // Right-click the |Hplayer:Bob|h link the chat frame emits (ChatFrame OnHyperlinkClick →
    // SetItemRef(link, text, "RightButton")).
    s.run(r#"SetItemRef("player:Bob", "|Hplayer:Bob|h[Bob]|h", "RightButton")"#)
        .unwrap();
    s.resolve();
    assert!(
        s.eval::<bool>("return DropDownList1:IsVisible()").unwrap(),
        "the chat-name right-click opens the FRIEND dropdown"
    );
    // title (Bob) + Whisper + Invite + Cancel — the rest of FRIEND is DEFERRED/hidden.
    assert_eq!(
        s.eval::<i64>("return DropDownList1.numButtons").unwrap(),
        4,
        "title + Whisper + Invite + Cancel"
    );
    assert_eq!(
        s.eval::<String>("return DropDownList1Button1:GetText()")
            .unwrap(),
        "Bob",
        "the clicked name titles the menu"
    );
    assert_eq!(
        s.eval::<String>("return DropDownList1Button3:GetText()")
            .unwrap(),
        "Invite"
    );

    // Click Invite through the real hit path → UnitPopup_OnClick → InviteByName("Bob").
    let (ix, iy) = s
        .eval::<(f64, f64)>("return DropDownList1Button3:GetCenter()")
        .unwrap();
    s.mouse_button(ix as f32, iy as f32, "LeftButton", true);
    s.mouse_button(ix as f32, iy as f32, "LeftButton", false);
    assert_eq!(
        s.take_party_requests(),
        vec![PartyRequest::InviteName("Bob".into())],
        "Invite on a chat name queues an invite-by-name"
    );

    // A plain LEFT-click on the name whispers instead (ref's else branch).
    s.run(r#"SetItemRef("player:Carol", "|Hplayer:Carol|h[Carol]|h", "LeftButton")"#)
        .unwrap();
    assert_eq!(
        s.take_tell_requests(),
        vec!["Carol".to_string()],
        "left-click a chat name opens a whisper"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Right-clicking a **friendly player target** solo opens the PLAYER menu with Whisper + Invite
/// (the target-frame half of the director's report). Confirms the solo target path is NOT gated
/// off for players — only NPC/self targets, whose every row needs a party, open nothing.
#[test]
fn solo_target_right_click_invites_a_player() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    bake_strings(&s);
    load_popup_frames(&s);

    s.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            name: Some("Me".into()),
            is_player: true,
            ..UnitState::default()
        }),
    );
    // A same-faction friendly PLAYER target (is_player + reaction 5 → UnitCanCooperate == 1).
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Ally".into()),
            health: 40,
            max_health: 40,
            is_player: true,
            reaction: 5,
            ..UnitState::default()
        }),
    );
    s.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    s.resolve();
    assert!(s.errors().is_empty(), "load errors: {:?}", s.errors());

    // Right-click the target frame through the real hit path.
    let (cx, cy) = s
        .eval::<(f64, f64)>("return BenillaTargetFrame:GetCenter()")
        .unwrap();
    s.mouse_button(cx as f32, cy as f32, "RightButton", true);
    s.mouse_button(cx as f32, cy as f32, "RightButton", false);
    s.resolve();
    assert!(
        s.eval::<bool>("return DropDownList1:IsVisible()").unwrap(),
        "a friendly player target opens the PLAYER menu solo"
    );
    // title (Ally) + Whisper + Invite + Trade + Cancel — Inspect/Follow/Duel are DEFERRED, the
    // raid-target submenu needs a party. (Trade un-deferred in decision 0592 P1.)
    assert_eq!(
        s.eval::<i64>("return DropDownList1.numButtons").unwrap(),
        5,
        "title + Whisper + Invite + Trade + Cancel"
    );
    assert_eq!(
        s.eval::<String>("return DropDownList1Button3:GetText()")
            .unwrap(),
        "Invite"
    );
    assert_eq!(
        s.eval::<String>("return DropDownList1Button4:GetText()")
            .unwrap(),
        "Trade"
    );

    // Click Invite → UnitPopup_OnClick → InviteToParty("target") → InviteUnit.
    let (ix, iy) = s
        .eval::<(f64, f64)>("return DropDownList1Button3:GetCenter()")
        .unwrap();
    s.mouse_button(ix as f32, iy as f32, "LeftButton", true);
    s.mouse_button(ix as f32, iy as f32, "LeftButton", false);
    assert_eq!(
        s.take_party_requests(),
        vec![PartyRequest::InviteUnit("target".into())],
        "Invite on a player target queues an invite by unit"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Clicking the **Trade** row (the un-deferred 0592 P1 button) through the real hit path fires
/// `InitiateTrade("target")` — the exact seam the director hit as "click Trade, nothing happens".
/// The other button tests click Invite; this is the only test that drives the Trade row's OnClick,
/// so a break between `this.value == "TRADE"` and the queued initiate token shows up here.
#[test]
fn solo_target_trade_click_queues_an_initiate() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    bake_strings(&s);
    load_popup_frames(&s);

    s.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            name: Some("Me".into()),
            is_player: true,
            ..UnitState::default()
        }),
    );
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Ally".into()),
            health: 40,
            max_health: 40,
            is_player: true,
            reaction: 5,
            ..UnitState::default()
        }),
    );
    s.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    s.resolve();
    assert!(s.errors().is_empty(), "load errors: {:?}", s.errors());

    // Right-click the target frame → PLAYER menu.
    let (cx, cy) = s
        .eval::<(f64, f64)>("return BenillaTargetFrame:GetCenter()")
        .unwrap();
    s.mouse_button(cx as f32, cy as f32, "RightButton", true);
    s.mouse_button(cx as f32, cy as f32, "RightButton", false);
    s.resolve();
    assert_eq!(
        s.eval::<String>("return DropDownList1Button4:GetText()")
            .unwrap(),
        "Trade",
        "Trade is the fourth row (title + Whisper + Invite + Trade)"
    );

    // Click Trade through the real hit path → UnitPopup_OnClick's TRADE arm → InitiateTrade("target").
    let (tx, ty) = s
        .eval::<(f64, f64)>("return DropDownList1Button4:GetCenter()")
        .unwrap();
    s.mouse_button(tx as f32, ty as f32, "LeftButton", true);
    s.mouse_button(tx as f32, ty as f32, "LeftButton", false);
    assert_eq!(
        s.take_trade_initiates(),
        vec!["target".to_string()],
        "clicking Trade queues an initiate against the target token"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
