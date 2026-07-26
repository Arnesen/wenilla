//! The shipped `assets/ui/GameMenuFrame.xml` — the frame ESC opens (decision 0674).
//!
//! What these guard, in order: the ref geometry (195×246 and the eight-button ladder, because the
//! whole point of shipping five dead buttons is that the frame keeps its exact silhouette); the
//! disabled five; the three live buttons' wire intents and sounds; the ESC ladder's two new rungs
//! (open when nothing is left to eat, close before everything else); the micro button's `clicked`
//! toggle; the native-center rule that makes the menu take the screen (windows close on the way in,
//! and nothing opens while it is up); and the camp/quit countdown dialogs end to end.

use benilla_ui::script::{
    ContainerSlot, ContainerState, LootRow, LootState, SessionRequest, SoundRequest, UiScript,
};

/// The engine the menu needs behind it: fonts, the panel manager + popup engine, and the menu.
/// `extra` adds the files a given test wants in the way (a bag, a loot window).
fn harness_with(extra: &[&str]) -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    let files: Vec<&str> = ["Fonts.xml", "UiPanels.xml"]
        .into_iter()
        .chain(extra.iter().copied())
        .chain(std::iter::once("GameMenuFrame.xml"))
        .collect();
    for file in files {
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
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s
}

fn harness() -> UiScript {
    harness_with(&[])
}

/// The eight buttons, top to bottom — the ref's own order (GameMenuFrame.xml l.48-186).
const LADDER: [&str; 8] = [
    "GameMenuButtonOptions",
    "GameMenuButtonSoundOptions",
    "GameMenuButtonUIOptions",
    "GameMenuButtonKeybindings",
    "GameMenuButtonMacros",
    "GameMenuButtonLogout",
    "GameMenuButtonQuit",
    "GameMenuButtonContinue",
];

/// The frame's geometry, quoted from the reference: 195×246 centered, each button 144×21, the first
/// centered on the frame's TOP at −37, each next 1 px under the last, and Return to Game hung 16
/// under Exit Game — the single gap that makes the last entry read as separate. Shipping the five
/// unbacked buttons disabled (rather than cutting them) is exactly what keeps this true, so it is
/// the first thing to break if someone "tidies" the ladder.
#[test]
fn the_menu_has_the_reference_frame_and_button_ladder() {
    let mut s = harness();
    s.run("ShowUIPanel(GameMenuFrame)").unwrap();
    s.resolve();

    let (w, h) = s
        .eval::<(f64, f64)>("return GameMenuFrame:GetWidth(), GameMenuFrame:GetHeight()")
        .unwrap();
    assert_eq!((w, h), (195.0, 246.0), "the ref frame size");

    let top = s.eval::<f64>("return GameMenuFrame:GetTop()").unwrap();
    // Button i's own top, measured down from the frame's: the first is centered at −37 (so its top
    // is 37 − 21/2 = 26.5 below), then a 22 px stride (21 tall + the 1 px gap), with the last
    // pushed a further 15 (its −16 anchor instead of −1).
    let mut expected = top - 26.5;
    for (i, name) in LADDER.iter().enumerate() {
        let (bw, bh, btop, bleft) = s
            .eval::<(f64, f64, f64, f64)>(&format!(
                "return {name}:GetWidth(), {name}:GetHeight(), {name}:GetTop(), {name}:GetLeft()"
            ))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            (bw, bh),
            (144.0, 21.0),
            "{name} is a GameMenuButtonTemplate"
        );
        assert!(
            (btop - expected).abs() < 0.001,
            "{name} top: expected {expected}, got {btop}"
        );
        // Every button is centered in the frame (the ladder is one column).
        assert!(
            (bleft - (512.0 - 72.0)).abs() < 0.001,
            "{name} is centered in the 1024-wide screen"
        );
        expected -= if i == 6 { 21.0 + 16.0 } else { 21.0 + 1.0 };
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The five with nothing behind them are DISABLED — the director's call: the ladder keeps the ref's
/// shape, and a button that does nothing looks like it. The three that work are enabled.
#[test]
fn the_five_unbacked_buttons_are_disabled_and_the_three_live_ones_are_not() {
    let s = harness();
    s.run("ShowUIPanel(GameMenuFrame)").unwrap();

    for name in &LADDER[..5] {
        assert!(
            !s.eval::<bool>(&format!("return {name}:IsEnabled()"))
                .unwrap(),
            "{name} has no panel behind it and must read that way"
        );
    }
    for name in &LADDER[5..] {
        assert!(
            s.eval::<bool>(&format!("return {name}:IsEnabled()"))
                .unwrap(),
            "{name} is live"
        );
    }
    // The labels are the reference GlobalStrings, read back through the globals (not literals).
    assert_eq!(
        s.eval::<String>("return GameMenuButtonContinue:GetText()")
            .unwrap(),
        "Return to Game"
    );
    assert_eq!(
        s.eval::<String>("return GameMenuButtonOptions:GetText()")
            .unwrap(),
        "Video Options"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The ESC ladder's two new rungs (`ToggleGameMenu`, UIParent.lua l.1485-1495): a press with
/// nothing left to eat OPENS the menu (igMainMenuOpen), and the next press closes it
/// (igMainMenuQuit — the reference's own choice of kit) without reaching any rung below.
#[test]
fn escape_opens_the_menu_only_when_nothing_else_wants_the_press_and_then_closes_it() {
    let mut s = harness_with(&["MerchantFrame.xml", "Cooldown.xml", "BagFrame.xml"]);
    s.set_money(0);
    s.set_container(0, Some(backpack()));

    // A press with a window open is eaten by CloseAllWindows — the menu stays down.
    s.run("BenillaBagToggle_OnClick()").unwrap();
    let _ = s.take_sounds();
    s.run("ToggleGameMenu()").unwrap();
    assert!(
        !s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap(),
        "the press closed the bag"
    );
    assert!(
        !s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap(),
        "and did NOT also open the menu — one eater per press"
    );

    // Nothing left: the next press opens it.
    let _ = s.take_sounds();
    s.run("ToggleGameMenu()").unwrap();
    assert!(s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap());
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuOpen".into())));

    // And the press after that closes it again.
    s.run("ToggleGameMenu()").unwrap();
    assert!(!s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap());
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuQuit".into())));
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The micro button's form (`ToggleGameMenu(1)`, l.1466-1480) is a plain TOGGLE, not the ladder:
/// clicking it with a window open closes the window AND opens the menu in one go — the press-eating
/// rule is the ESC key's, not the button's.
#[test]
fn the_clicked_form_closes_everything_and_opens_the_menu_in_one_go() {
    let mut s = harness_with(&["MerchantFrame.xml", "Cooldown.xml", "BagFrame.xml"]);
    s.set_money(0);
    s.set_container(0, Some(backpack()));
    s.run("BenillaBagToggle_OnClick()").unwrap();
    assert!(s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap());

    s.run("ToggleGameMenu(1)").unwrap();
    assert!(
        !s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap(),
        "the click closed the bag"
    );
    assert!(
        s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap(),
        "…and opened the menu in the SAME click"
    );

    s.run("ToggleGameMenu(1)").unwrap();
    assert!(!s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The native-center rule (`ShowUIPanel` l.697-702 + `CanOpenPanels`): opening the menu closes the
/// panel slots and the bags, and while it holds the center NOTHING else opens — which is what makes
/// "you can't open your bags with the ESC menu up" true rather than merely greyed.
#[test]
fn the_open_menu_takes_the_screen_and_refuses_every_other_panel() {
    let mut s = harness_with(&[
        "MerchantFrame.xml",
        "LootFrame.xml",
        "Cooldown.xml",
        "BagFrame.xml",
    ]);
    s.set_money(0);
    s.set_container(0, Some(backpack()));
    s.run("BenillaBagToggle_OnClick()").unwrap();
    s.set_loot(Some(LootState {
        rows: vec![LootRow {
            item_id: 0,
            name: Some("Wool Cloth".into()),
            texture: Some("Interface\\Icons\\INV_Fabric_Wool_01".into()),
            quantity: 1,
            quality: Some(1),
            is_coin: false,
        }],
    }));
    s.fire_event("LOOT_OPENED", vec![]);
    assert!(s
        .eval::<bool>("return GetLeftFrame():GetName() == \"BenillaLootFrame\"")
        .unwrap());

    s.run("ToggleGameMenu(1)").unwrap();
    assert!(
        s.eval::<bool>("return GetLeftFrame() == nil").unwrap(),
        "the panel slot vacated on the way in"
    );
    assert!(
        !s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap(),
        "and the bags closed (CloseAllBags)"
    );
    assert!(
        s.eval::<bool>("return GetCenterFrame():GetName() == \"GameMenuFrame\"")
            .unwrap(),
        "the menu holds the CENTER slot, not the left one"
    );
    assert!(
        !s.eval::<bool>("return CanOpenPanels() and true or false")
            .unwrap(),
        "a native-center frame is up: nothing may open"
    );

    // The refusal itself: a panel asked to show while the menu is up simply doesn't.
    s.run("ShowUIPanel(BenillaLootFrame)").unwrap();
    assert!(
        !s.eval::<bool>("return BenillaLootFrame:IsVisible()")
            .unwrap(),
        "ShowUIPanel refuses a left-area panel behind the menu"
    );

    // …and works again the moment the menu goes down.
    s.run("HideUIPanel(GameMenuFrame)").unwrap();
    s.run("ShowUIPanel(BenillaLootFrame)").unwrap();
    assert!(s
        .eval::<bool>("return BenillaLootFrame:IsVisible()")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The three live buttons: each plays its own reference kit and does its one thing. Logout and Exit
/// Game both queue on the session seam — the difference between them is entirely app-side (whether
/// the completed logout ends the process), which is why they look identical from here.
#[test]
fn the_live_buttons_queue_their_intents_and_play_their_kits() {
    let mut s = harness();

    // Return to Game — just closes, no intent.
    s.run("ToggleGameMenu()").unwrap();
    let _ = s.take_sounds();
    s.run("GameMenuButtonContinue:Click()").unwrap();
    assert!(!s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap());
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuContinue".into())));
    assert!(s.take_session_requests().is_empty());

    // Logout — the request, and the menu goes away behind it.
    s.run("ToggleGameMenu()").unwrap();
    let _ = s.take_sounds();
    s.run("GameMenuButtonLogout:Click()").unwrap();
    assert_eq!(s.take_session_requests(), vec![SessionRequest::Logout]);
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuLogout".into())));
    assert!(!s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap());

    // Exit Game — the same shape, its own kit.
    s.run("ToggleGameMenu()").unwrap();
    let _ = s.take_sounds();
    s.run("GameMenuButtonQuit:Click()").unwrap();
    assert_eq!(s.take_session_requests(), vec![SessionRequest::Quit]);
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuQuit".into())));
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The camp countdown end to end (ref StaticPopup.lua l.564-582 + UIParent.lua l.304-315): the
/// server's non-instant answer becomes `PLAYER_CAMPING`, the dialog counts down from the server's
/// own 20 s, and closing it EARLY calls the logout off. The `%d %s` text is written by the popup
/// engine's countdown branch, not by the entry — a dialog that opened blank and stayed blank would
/// mean CAMP fell out of that which-list.
#[test]
fn player_camping_opens_a_counting_dialog_whose_early_close_cancels() {
    let mut s = harness();
    s.fire_event("PLAYER_CAMPING", vec![]);
    assert_eq!(
        s.eval::<String>("return StaticPopup_Visible(\"CAMP\") or \"\"")
            .unwrap(),
        "StaticPopup1",
        "the camp dialog took an instance"
    );

    // One tick of the engine's countdown fills the text from CAMP_TIMER.
    s.run("StaticPopup_OnUpdate(StaticPopup1, 0.5)").unwrap();
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "20 seconds until logout",
        "the countdown text is the engine's, from the server's 20 s clock"
    );
    // Cancel is the only button (the ref's CAMP_NOW is commented out in 1.12).
    assert_eq!(
        s.eval::<String>("return StaticPopup1Button1:GetText()")
            .unwrap(),
        "Cancel"
    );
    assert!(
        !s.eval::<bool>("return StaticPopup1Button2:IsShown()")
            .unwrap(),
        "no second button"
    );

    // Closing it early cancels the logout — twice over in the reference's own shape (the button's
    // OnAccept, and the OnHide guard that catches every other way out); a doubled cancel is
    // harmless on the wire, and losing it would strand the character mid-countdown.
    s.run("StaticPopup1Button1:Click()").unwrap();
    assert!(
        s.take_session_requests()
            .contains(&SessionRequest::CancelLogout),
        "the early close called the logout off"
    );
    assert!(s
        .eval::<bool>("return StaticPopup_Visible(\"CAMP\") == nil")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A countdown that RUNS OUT must not cancel itself: the engine zeroes `timeleft` before hiding, and
/// the entry's OnHide guard reads exactly that. (Get this wrong and every logout in the field is
/// cancelled by its own dialog at t=0 — the character never leaves.)
#[test]
fn a_countdown_that_expires_does_not_cancel_the_logout() {
    let mut s = harness();
    s.fire_event("PLAYER_CAMPING", vec![]);
    s.run("StaticPopup_OnUpdate(StaticPopup1, 25)").unwrap();
    assert!(
        s.eval::<bool>("return StaticPopup_Visible(\"CAMP\") == nil")
            .unwrap(),
        "the dialog closed itself at zero"
    );
    assert!(
        !s.take_session_requests()
            .contains(&SessionRequest::CancelLogout),
        "…and did NOT call off the logout it was counting"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The quit dialog is the camp dialog under another name, plus the one thing a quit can do without
/// the server: "Exit now". And `LOGOUT_CANCEL` (the server's cancel ack) takes either one down.
#[test]
fn player_quiting_offers_exit_now_and_logout_cancel_closes_it() {
    let mut s = harness();
    s.fire_event("PLAYER_QUITING", vec![]);
    s.run("StaticPopup_OnUpdate(StaticPopup1, 0.5)").unwrap();
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "20 seconds until exit"
    );
    assert_eq!(
        s.eval::<String>("return StaticPopup1Button1:GetText()")
            .unwrap(),
        "Exit now"
    );
    assert_eq!(
        s.eval::<String>("return StaticPopup1Button2:GetText()")
            .unwrap(),
        "Cancel"
    );
    s.run("StaticPopup1Button1:Click()").unwrap();
    assert!(s
        .take_session_requests()
        .contains(&SessionRequest::ForceQuit));

    // The server's own cancel ack hides whichever is up, without queueing anything further.
    s.fire_event("PLAYER_CAMPING", vec![]);
    assert!(s
        .eval::<bool>("return StaticPopup_Visible(\"CAMP\") ~= nil")
        .unwrap());
    let _ = s.take_session_requests();
    s.fire_event("LOGOUT_CANCEL", vec![]);
    assert!(s
        .eval::<bool>("return StaticPopup_Visible(\"CAMP\") == nil")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The reference's own OnShow guard on both live buttons: a menu re-opened over a running countdown
/// shows them dead, because re-arming a logout that is already counting is meaningless.
///
/// Opened with the micro button's form on purpose — the ESC key can't get you here at all, which is
/// the next test.
#[test]
fn logout_and_exit_read_disabled_while_a_countdown_runs() {
    let mut s = harness();
    s.fire_event("PLAYER_CAMPING", vec![]);
    s.run("ToggleGameMenu(1)").unwrap();
    assert!(
        !s.eval::<bool>("return GameMenuButtonLogout:IsEnabled()")
            .unwrap(),
        "Logout is dead while the camp timer runs"
    );
    assert!(!s
        .eval::<bool>("return GameMenuButtonQuit:IsEnabled()")
        .unwrap());

    // With the countdown gone, a re-opened menu has them back.
    s.run("HideUIPanel(GameMenuFrame)").unwrap();
    s.fire_event("LOGOUT_CANCEL", vec![]);
    s.run("ToggleGameMenu(1)").unwrap();
    assert!(s
        .eval::<bool>("return GameMenuButtonLogout:IsEnabled()")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// ESC with a countdown up never reaches the menu: the popup rung is FIRST in the ladder
/// (`StaticPopup_EscapePressed`, l.1482) and CAMP is `hideOnEscape`, so the press dismisses the
/// dialog — which, per the entry's own OnHide, calls the logout off. Pressing ESC to "get out of"
/// a logout you didn't mean is therefore the whole gesture, and it must not also open the menu.
#[test]
fn escape_during_a_countdown_cancels_it_and_does_not_open_the_menu() {
    let mut s = harness();
    s.fire_event("PLAYER_CAMPING", vec![]);
    s.run("ToggleGameMenu()").unwrap();
    assert!(
        s.eval::<bool>("return StaticPopup_Visible(\"CAMP\") == nil")
            .unwrap(),
        "the press dismissed the countdown"
    );
    assert!(
        s.take_session_requests()
            .contains(&SessionRequest::CancelLogout),
        "…which called the logout off"
    );
    assert!(
        !s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap(),
        "and the same press did NOT also open the menu — one eater per press"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The world map is subject to the menu like everything else (director-reported: it was "the only
/// one not blocked"). It was a plain toplevel showing itself with a bare `WorldMapFrame:Show()`,
/// so the panel manager never saw it and `CanOpenPanels()` never got a say; it is now the ref's own
/// `area = "full"` row and its toggle goes through ShowUIPanel.
///
/// Both directions matter, and the second is the one a bare `IsVisible()` check would miss: the map
/// must also VACATE the full-screen slot when it closes, or the stale slot refuses every panel
/// afterwards.
#[test]
fn the_world_map_cannot_open_behind_the_menu_and_gives_its_slot_back() {
    let s = harness_with(&[
        "GameTooltip.xml",
        "UIDropDownMenu.xml", // the map's continent/zone pickers initialize into it at OnLoad
        "ScrollTemplates.xml",
        "WorldMapFrame.xml",
    ]);

    // Opens normally, and takes the full-screen slot.
    s.run("ToggleWorldMap()").unwrap();
    assert!(s.eval::<bool>("return WorldMapFrame:IsVisible()").unwrap());
    assert!(s
        .eval::<bool>("return GetFullScreenFrame():GetName() == \"WorldMapFrame\"")
        .unwrap());

    // Closing gives the slot back — a stale full-screen frame would block every later panel.
    s.run("ToggleWorldMap()").unwrap();
    assert!(!s.eval::<bool>("return WorldMapFrame:IsVisible()").unwrap());
    assert!(
        s.eval::<bool>("return GetFullScreenFrame() == nil")
            .unwrap(),
        "the map vacated the full-screen slot"
    );

    // With the menu up, it must not open — from the M binding or from the micro button, both of
    // which are this same ToggleWorldMap.
    s.run("ToggleGameMenu(1)").unwrap();
    assert!(s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap());
    s.run("ToggleWorldMap()").unwrap();
    assert!(
        !s.eval::<bool>("return WorldMapFrame:IsVisible()").unwrap(),
        "the map must not open behind the game menu"
    );
    assert!(s
        .eval::<bool>("return GetFullScreenFrame() == nil")
        .unwrap());

    // And it opens again the moment the menu is gone.
    s.run("ToggleGameMenu(1)").unwrap();
    s.run("ToggleWorldMap()").unwrap();
    assert!(s.eval::<bool>("return WorldMapFrame:IsVisible()").unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The other direction of the same rule (ref `ShowUIPanel` l.668-675): with the map holding the
/// full screen, nothing opens behind IT either — and ESC closes the map first, so the press that
/// would have opened the menu goes to the map instead (one eater per press).
#[test]
fn nothing_opens_behind_the_world_map_and_escape_closes_it_first() {
    let s = harness_with(&[
        "GameTooltip.xml",
        "UIDropDownMenu.xml",
        "ScrollTemplates.xml",
        "WorldMapFrame.xml",
        "MerchantFrame.xml",
        "LootFrame.xml",
    ]);
    s.run("ToggleWorldMap()").unwrap();

    s.run("ShowUIPanel(BenillaLootFrame)").unwrap();
    assert!(
        !s.eval::<bool>("return BenillaLootFrame:IsVisible()")
            .unwrap(),
        "a left-area panel must not open behind the full-screen map"
    );

    // ESC: the map's own rung eats the press (it stands in for the reference frame's OnKeyDown).
    s.run("ToggleGameMenu()").unwrap();
    assert!(!s.eval::<bool>("return WorldMapFrame:IsVisible()").unwrap());
    assert!(
        !s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap(),
        "…and did not also open the menu"
    );
    assert!(s
        .eval::<bool>("return GetFullScreenFrame() == nil")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The bag row under an open menu, pinned at the QUAD level — because "greyed out" and "gone" are
/// the same thing to an `IsEnabled()` assertion, and the difference is what the director saw: the
/// first cut of `Disable_BagButtons` disabled buttons whose art was their NormalTexture, and a
/// disabled button with no DisabledTexture draws no state texture at all
/// (`ButtonState::region_visible`, the byte rule at `SetState 0x779790`) — so the backpack icon
/// vanished off the bar instead of dimming.
///
/// What must hold with the menu up: every one of the five buttons still DRAWS its art, and every
/// one is tinted grey. And on the way back out, tinted white again.
#[test]
fn the_bag_row_greys_under_the_menu_without_any_of_it_disappearing() {
    let mut s = harness_with(&[
        "MerchantFrame.xml",
        "Cooldown.xml",
        "ActionBar.xml",
        "BagFrame.xml",
    ]);
    s.set_money(0);
    s.set_container(0, Some(backpack()));
    s.resolve();

    // The backpack icon + the four slot rings, by name.
    let art = |s: &UiScript, owner: &str| -> Vec<(String, Option<[f32; 4]>)> {
        s.extract()
            .into_iter()
            .filter(|eq| s.quad_owner_name(eq.target).as_deref() == Some(owner))
            .filter_map(|eq| match &eq.content {
                benilla_ui::script::QuadContent::Texture {
                    path: Some(p),
                    color,
                    ..
                } => Some((p.clone(), *color)),
                _ => None,
            })
            .collect()
    };

    for owner in [
        "BenillaBagToggle",
        "BenillaBagBarSlot1",
        "BenillaBagBarSlot2",
        "BenillaBagBarSlot3",
        "BenillaBagBarSlot4",
    ] {
        assert!(
            !art(&s, owner).is_empty(),
            "{owner} draws art before the menu opens"
        );
    }

    s.run("ToggleGameMenu(1)").unwrap();
    s.resolve();
    for owner in [
        "BenillaBagToggle",
        "BenillaBagBarSlot1",
        "BenillaBagBarSlot2",
        "BenillaBagBarSlot3",
        "BenillaBagBarSlot4",
    ] {
        let drawn = art(&s, owner);
        assert!(
            !drawn.is_empty(),
            "{owner} must still DRAW under the open menu — greyed is not gone"
        );
    }
    // The backpack icon specifically: still its own art, and tinted to SetDesaturation's grey.
    let toggle = art(&s, "BenillaBagToggle");
    assert!(
        toggle
            .iter()
            .any(|(p, _)| p == "Interface\\Buttons\\Button-Backpack-Up"),
        "the backpack image is still on the bar: {toggle:?}"
    );
    assert!(
        toggle
            .iter()
            .any(|(p, c)| p == "Interface\\Buttons\\Button-Backpack-Up"
                && c.is_some_and(|c| (c[0] - 0.5).abs() < 0.01)),
        "…and it is greyed, not full-bright: {toggle:?}"
    );

    s.run("ToggleGameMenu(1)").unwrap();
    s.resolve();
    let toggle = art(&s, "BenillaBagToggle");
    assert!(
        toggle
            .iter()
            .any(|(p, c)| p == "Interface\\Buttons\\Button-Backpack-Up"
                && c.is_none_or(|c| (c[0] - 1.0).abs() < 0.01)),
        "closing the menu restores full colour: {toggle:?}"
    );
    assert!(
        s.eval::<bool>("return BenillaBagToggle:IsEnabled()")
            .unwrap(),
        "…and the button works again"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A one-item backpack (the escape/bag tests' fixture, duplicated so this file is self-contained).
fn backpack() -> ContainerState {
    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
            count: 1,
            quality: Some(1),
            item_id: 117,
            link: Some("|cffffffff|Hitem:117|h[Tough Jerky]|h|r".into()),
            locked: false,
            equip_slots: Vec::new(),
            cooldown: None,
            readable: false,
            creator: None,
            flags: 0,
        },
    );
    ContainerState {
        name: Some("Backpack".into()),
        num_slots: 16,
        slots,
    }
}
