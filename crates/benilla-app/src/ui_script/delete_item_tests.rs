//! The delete-item confirm popup driver (decision 0216 §3, UiPanels.xml's
//! `BenillaDeleteItemConfirmDriver`) — Lua wiring the Rust-side cursor tests can't reach: the
//! world-drop's `DELETE_ITEM_CONFIRM` showing the ref's `DELETE_ITEM` StaticPopup entry (decision
//! 0308 §3's engine) with the real `DELETE_ITEM`/`YES`/`NO` GlobalStrings, its Yes/No/ESC routing
//! to `DeleteCursorItem`/`ClearCursor`, and the entry's own `OnUpdate` auto-hide poll.

use benilla_ui::script::{ContainerSlot, ContainerState, UiScript};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error (the panel/loot
/// tests' loader, duplicated so this file is self-contained).
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

/// A one-item, one-slot backpack: a quality-3 item (`Tough Jerky`) so the confirm text and the
/// wire's destroy count are exercisable end to end.
fn one_item_backpack() -> ContainerState {
    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
            count: 5,
            quality: Some(3),
            item_id: 117,
            link: Some("|cffffffff|Hitem:117|h[Tough Jerky]|h|r".into()),
            locked: false,
            equip_slots: Vec::new(),
            cooldown: None,
            readable: false,
            creator: None,
            flags: 0,
            enchants: Vec::new(),
        },
    );
    ContainerState {
        name: Some("Backpack".into()),
        num_slots: 16,
        slots,
    }
}

fn setup() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml"); // BenillaMoney_Set, BagFrame's isolation dep
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);
    s
}

/// Pick up the fixture item and click-carry it into the world — a completed LEFT CLICK (press +
/// release, both over nothing; 0218's byte-verified trigger) fires the world-drop
/// `DELETE_ITEM_CONFIRM(name, quality)` the driver listens for.
fn pick_up_and_drop_in_world(s: &mut UiScript) {
    s.set_container(0, Some(one_item_backpack()));
    s.run("PickupContainerItem(0, 1)").unwrap();
    assert!(s.cursor_item().is_some(), "fixture: the item is held");
    // Off past every frame (the bag sits bottom-right; off-screen negative is always clear).
    s.mouse_button(-50.0, -50.0, "LeftButton", true);
    assert!(
        s.mouse_button(-50.0, -50.0, "LeftButton", false),
        "a world drop consumes the completed click"
    );
    s.tick(0.01); // flush the queued DELETE_ITEM_CONFIRM into the driver's OnEvent
}

/// The popup shows with the real 1.12 `DELETE_ITEM`/`YES`/`NO` GlobalStrings text, and Yes runs
/// `DeleteCursorItem()` — the item leaves the cursor and queues its wire destroy.
#[test]
fn delete_item_confirm_shows_the_real_strings_and_yes_deletes() {
    let mut s = setup();
    pick_up_and_drop_in_world(&mut s);

    assert!(
        s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "the confirm popup shows on DELETE_ITEM_CONFIRM"
    );
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "Do you want to destroy Tough Jerky?",
        "the real GlobalStrings DELETE_ITEM text, formatted with the item name"
    );
    assert_eq!(
        s.eval::<String>("return StaticPopup1Button1:GetText()")
            .unwrap(),
        "Yes"
    );
    assert_eq!(
        s.eval::<String>("return StaticPopup1Button2:GetText()")
            .unwrap(),
        "No"
    );

    // Yes → StaticPopup_OnClick(dialog, 1) → the entry's OnAccept → DeleteCursorItem(): the
    // payload clears and the destroy queues (count 0 = the fixture's whole 5-stack was picked up).
    s.run("StaticPopup_OnClick(StaticPopup1, 1)").unwrap();
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "Yes hides the popup"
    );
    assert!(s.cursor_item().is_none(), "DeleteCursorItem cleared it");
    assert_eq!(s.take_container_destroys(), vec![(0, 1, 0)]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// No runs the entry's OnCancel (`ClearCursor()`) instead — the item is dropped, not destroyed (no
/// wire send) — and the bag REPAINTS off the clear's `ITEM_LOCK_CHANGED`, un-darkening the source
/// slot. The repaint is the 0218 stuck-darkening regression: the popup's No is a clear the bag
/// never clicked through, so only the event wiring (BagFrame's ITEM_LOCK_CHANGED registration)
/// can reach it.
#[test]
fn delete_item_confirm_no_clears_without_destroying() {
    let mut s = setup();
    pick_up_and_drop_in_world(&mut s);
    // Count repaints from here — the No-click path must trigger one via the event, not a click.
    s.run(
        "repaints = 0\n\
         local real = BenillaBagFrame_Update\n\
         BenillaBagFrame_Update = function(...) repaints = repaints + 1; return real(...) end",
    )
    .unwrap();

    s.run("StaticPopup_OnClick(StaticPopup1, 2)").unwrap();
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "No hides the popup"
    );
    assert!(s.cursor_item().is_none(), "ClearCursor cleared it");
    assert!(s.take_container_destroys().is_empty(), "No never destroys");
    s.tick(0.01); // ITEM_LOCK_CHANGED fires from the pending queue at the next tick
    assert!(
        s.eval::<i64>("return repaints").unwrap() >= 1,
        "the clear's ITEM_LOCK_CHANGED repaints the bag (the stuck-darkened slot, 0218)"
    );
    assert!(
        !s.eval::<bool>("local _, _, locked = GetContainerItemInfo(0, 1) return locked")
            .unwrap(),
        "the source slot reads unlocked again"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// ESC routes through the ref's `StaticPopup_EscapePressed` — DELETE_ITEM is `hideOnEscape`, so
/// its OnCancel runs with reason "clicked" exactly as a real No-click would. (`ToggleGameMenu`'s
/// existing unconditional `ClearCursor()` already empties the cursor before the popup branch, so
/// this mainly proves the popup itself closes.)
#[test]
fn escape_closes_the_delete_confirm_popup() {
    let mut s = setup();
    pick_up_and_drop_in_world(&mut s);

    s.run("ToggleGameMenu()").unwrap();
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "ESC closes the confirm popup"
    );
    assert!(s.cursor_item().is_none());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The reference's DELETE_ITEM `OnUpdate` auto-hide poll (StaticPopup.lua:669-673), now VERBATIM
/// as the registry entry's own OnUpdate: if the cursor empties by any OTHER path (not a popup
/// button — say, a same-slot click cancelling a fresh pickup) while the confirm is still showing,
/// the popup auto-hides on the next tick. The poll lives inside the DELETE_ITEM entry, so a
/// DIFFERENT visible dialog is untouched by cursor traffic.
#[test]
fn the_delete_entry_polls_itself_hidden_and_other_dialogs_are_untouched() {
    let mut s = setup();
    pick_up_and_drop_in_world(&mut s);
    assert!(s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap());

    // The cursor empties via ClearCursor() directly (not a popup button) — the entry's own
    // OnUpdate poll hides the still-showing confirm on the next tick.
    s.run("ClearCursor()").unwrap();
    s.tick(0.01);
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "the DELETE_ITEM OnUpdate poll auto-hides once the cursor is empty"
    );

    // An unrelated registered dialog must NOT be auto-hidden by cursor traffic — only the
    // DELETE_ITEM entry polls, and its OnUpdate only runs while DELETE_ITEM is the shown `which`.
    s.run(
        r#"StaticPopupDialogs["TEST_UNRELATED"] = { text = "unrelated?", button1 = "Yes",
           button2 = "No", timeout = 0, whileDead = 1 }
           StaticPopup_Show("TEST_UNRELATED")"#,
    )
    .unwrap();
    assert!(s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap());
    s.fire_event("CURSOR_UPDATE", vec![]);
    s.tick(0.01);
    assert!(
        s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "an unrelated dialog is never touched by the delete-confirm poll"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
