//! The delete-item confirm popup driver (decision 0216 §3, UiPanels.xml's
//! `BenillaDeleteItemConfirmDriver`) — Lua wiring the Rust-side cursor tests can't reach: the
//! world-drop's `DELETE_ITEM_CONFIRM` showing the ref's `DELETE_ITEM` StaticPopup entry (decision
//! 0308 §3's engine) with the real `DELETE_ITEM`/`YES`/`NO` GlobalStrings, its Yes/No/ESC routing
//! to `DeleteCursorItem`/`ClearCursor`, and the entry's own `OnUpdate` auto-hide poll.
//!
//! Plus the `arg2 >= 3` fork the driver grew in 1743 (ref `UIParent.lua:344-352`): a RARE-or-better
//! payload raises `DELETE_GOOD_ITEM` instead, whose OKAY stays disabled until the player types
//! `DELETE_ITEM_CONFIRM_STRING` into its edit box. The two fixtures are real 1.12 rows (vmangos
//! `item_template`): **Tough Jerky** 117, quality 1 — the plain arm; **Flurry Axe** 871, quality 4 —
//! the typed arm.

use benilla_ui::script::{ContainerSlot, ContainerState, UiScript};

use super::test_ui::{bag_open, bag_slot_button, load_ui as load_xml, BAG_UI};

/// A one-item, one-slot backpack holding `item_id`/`name` at `quality`, so the confirm text and
/// the wire's destroy count are exercisable end to end. Quality is what forks the driver, so it is
/// the fixture's only real parameter.
fn one_item_backpack(item_id: u32, name: &str, quality: u32) -> ContainerState {
    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            petition: None,
            already_bound: false,
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
            count: 5,
            quality: Some(quality),
            item_id,
            link: Some(format!("|cffffffff|Hitem:{item_id}|h[{name}]|h|r")),
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

/// The popup engine and its driver, and nothing else: `UiPanels.xml` owns both `StaticPopup*` and
/// `BenillaDeleteItemConfirmDriver`, and every test but the repaint one below only ever asks about
/// the dialog. It carried `MerchantFrame.xml` + `Cooldown.xml` + `BagFrame.xml` until 1751 for one
/// reason — the repaint test used to count `BenillaBagFrame_Update` — and that test now brings the
/// reference's own bag stack itself ([`bag_setup`]), so the whole tail went with it.
fn setup() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    s.set_money(0);
    s
}

/// [`setup`] plus the reference's bag stack — the harness for the one test that has to watch a bag
/// WINDOW repaint. The windows are `ContainerFrame1..12` off the player's chain now (1751), so
/// this needs client data and the caller opens with `wow_data_or_skip!`.
fn bag_setup() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in BAG_UI {
        load_xml(&s, file);
    }
    // `ContainerFrame_Update` reads `MerchantFrame:IsShown()` on any slot the tooltip owns, and
    // `BankFrame` (BAG_UI's last entry) fills its purse through `BenillaMoney_Set`.
    load_xml(&s, "Interface\\FrameXML\\MerchantFrame.xml");
    s.set_money(0);
    s
}

/// Pick up the fixture item and click-carry it into the world — a completed LEFT CLICK (press +
/// release, both over nothing; 0218's byte-verified trigger) fires the world-drop
/// `DELETE_ITEM_CONFIRM(name, quality)` the driver listens for.
fn pick_up_and_drop_in_world(s: &mut UiScript) {
    drop_in_world(s, 117, "Tough Jerky", 1);
}

/// The same world drop for an arbitrary fixture item — 1743's fork reads `arg2` (the quality), so
/// every test below picks the arm it wants by choosing the item.
fn drop_in_world(s: &mut UiScript, item_id: u32, name: &str, quality: u32) {
    s.set_container(0, Some(one_item_backpack(item_id, name, quality)));
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
/// never clicked through, so only the event wiring (the container window's own ITEM_LOCK_CHANGED
/// registration) can reach it.
///
/// **What 1751 changed, and what it did not.** The repaint counted here used to be
/// `BenillaBagFrame_Update`; the window is the reference's `ContainerFrame` now, so the body is
/// its own `ContainerFrame_Update(frame)` (`ContainerFrame.lua:234`) off the player's chain. Two
/// consequences, both faithful: the window has to be OPEN (`ContainerFrame_OnEvent` gates the
/// repaint on `this:IsShown()` at l.39-42, where ours repainted a hidden window too), and the
/// darkening the repaint undoes is the reference's own `SetItemButtonDesaturated(button, locked)`
/// at l.245 — so the slot's greyscale flag is asserted directly here rather than only through the
/// repaint that clears it.
#[test]
fn delete_item_confirm_no_clears_without_destroying() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = bag_setup();
    s.set_container(0, Some(one_item_backpack(117, "Tough Jerky", 1)));
    s.run("ToggleBackpack()").unwrap();
    assert!(bag_open(&s, 0), "the backpack window is up");
    // Asked of the buttons' own GetID: `ContainerFrame_GenerateFrame` numbers a window's buttons
    // BACKWARDS, so slot 1 is the LAST `…Item<j>`, and index arithmetic would pin a coincidence.
    let slot1 = bag_slot_button(&s, 0, 1);

    pick_up_and_drop_in_world(&mut s);
    assert!(
        desaturated(&mut s, &slot1),
        "held on the cursor, the source slot draws dark (ref SetItemButtonDesaturated(_, locked))"
    );

    // Count repaints from here — the No-click path must trigger one via the event, not a click.
    s.run(
        "repaints = 0\n\
         local real = ContainerFrame_Update\n\
         ContainerFrame_Update = function(...) repaints = repaints + 1; return real(...) end",
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
    assert!(
        !desaturated(&mut s, &slot1),
        "…and the repaint un-darkened it — the 0218 stuck-dark slot"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Is `button`'s icon drawn greyscale? The renderer-facing answer, off the quad stream, because
/// the engine publishes `Texture:SetDesaturated` with no getter to ask — and a *drawn* grey is
/// what "the slot is stuck dark" means to the player anyway.
fn desaturated(s: &mut UiScript, button: &str) -> bool {
    s.resolve();
    s.extract()
        .into_iter()
        .filter(|q| s.quad_owner_name(q.target).as_deref() == Some(button))
        .any(|q| match &q.content {
            benilla_ui::script::QuadContent::Texture {
                path: Some(p),
                desaturated,
                ..
            } => p.contains("INV_Misc_Food_16") && *desaturated,
            _ => false,
        })
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

/// The RARE-or-better fork (ref `UIParent.lua:346-350`, `arg2 >= 3`): a quality-4 payload raises
/// `DELETE_GOOD_ITEM`, not `DELETE_ITEM` — with the second GlobalString, the edit box up and
/// focused, and OKAY **disabled** until the confirm word is typed.
#[test]
fn a_rare_payload_raises_the_typed_confirm_with_okay_disabled() {
    let mut s = setup();
    drop_in_world(&mut s, 871, "Flurry Axe", 4);

    assert_eq!(
        s.eval::<String>("return StaticPopup1.which").unwrap(),
        "DELETE_GOOD_ITEM",
        "quality 4 forks to the typed-confirmation variant"
    );
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "Do you want to destroy Flurry Axe?\n\nType \"DELETE\" into the field to confirm.",
        "the real GlobalStrings DELETE_GOOD_ITEM text, formatted with the item name"
    );
    assert!(
        s.eval::<bool>("return StaticPopup1EditBox:IsShown()")
            .unwrap(),
        "hasEditBox raises the narrow box"
    );
    assert!(
        s.eval::<bool>("return StaticPopup1EditBox:HasFocus()")
            .unwrap(),
        "the entry's OnShow focuses the box, so the player can type straight away"
    );
    assert_eq!(
        s.eval::<i64>("return StaticPopup1Button1:IsEnabled()")
            .unwrap(),
        0,
        "OKAY starts disabled — nothing has been typed yet"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The typed gate itself (ref `EditBoxOnTextChanged`, `StaticPopup.lua:718-723`): only the exact
/// `DELETE_ITEM_CONFIRM_STRING` enables OKAY, it is compared through `strupper` so lower case
/// passes, and backing away from the word disables it again. Then OKAY destroys.
#[test]
fn typing_the_confirm_word_enables_okay_and_untyping_it_disables_again() {
    let mut s = setup();
    drop_in_world(&mut s, 871, "Flurry Axe", 4);

    // OKAY is enabled by the box's `OnTextChanged`, and that fire is deferred to the drain
    // (decision 1831) — so what this answers is the button state the LAST DRAINED text produced,
    // which is the state a player ever sees. Ticking here rather than after each write keeps the
    // test reading as the sequence of edits it is about.
    let enabled = |s: &mut UiScript| {
        s.tick(0.0);
        s.eval::<i64>("return StaticPopup1Button1:IsEnabled()")
            .unwrap()
            == 1
    };

    s.run(r#"StaticPopup1EditBox:SetText("DELET")"#).unwrap();
    assert!(!enabled(&mut s), "a prefix of the word is not the word");

    // Typed character by character through the engine's own input path, lower case: the ref
    // compares through strupper, so this arms OKAY exactly as shouting it would.
    s.run(r#"StaticPopup1EditBox:SetText("")"#).unwrap();
    for c in ["d", "e", "l", "e", "t", "e"] {
        assert!(s.char_input(c), "the focused box takes the keystroke");
    }
    assert_eq!(
        s.eval::<String>("return StaticPopup1EditBox:GetText()")
            .unwrap(),
        "delete"
    );
    assert!(
        enabled(&mut s),
        "the ref compares through strupper, so lower case passes"
    );

    s.run(r#"StaticPopup1EditBox:SetText("deletex")"#).unwrap();
    assert!(!enabled(&mut s), "one char past the word disables again");

    s.run(r#"StaticPopup1EditBox:SetText("DELETE")"#).unwrap();
    assert!(enabled(&mut s));

    // OKAY now does what the plain arm's does: DeleteCursorItem().
    s.run("StaticPopup_OnClick(StaticPopup1, 1)").unwrap();
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "OKAY hides the popup"
    );
    assert!(s.cursor_item().is_none(), "DeleteCursorItem cleared it");
    assert_eq!(s.take_container_destroys(), vec![(0, 1, 0)]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Enter in the box is the ref's `EditBoxOnEnterPressed` (`StaticPopup.lua:712-717`): it destroys
/// only while OKAY is enabled, so a reflexive Enter over a half-typed word does nothing at all.
#[test]
fn enter_in_the_box_destroys_only_once_okay_is_enabled() {
    let mut s = setup();
    drop_in_world(&mut s, 871, "Flurry Axe", 4);

    s.run(r#"StaticPopup1EditBox:SetText("DEL")"#).unwrap();
    assert!(s.key_input("ENTER"), "the focused box consumes ENTER");
    assert!(
        s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "Enter with OKAY disabled is inert — the dialog stays up"
    );
    assert!(s.cursor_item().is_some(), "and the item is still held");
    assert!(s.take_container_destroys().is_empty());

    s.run(r#"StaticPopup1EditBox:SetText("DELETE")"#).unwrap();
    s.tick(0.0); // the write only marks the box — the drain is what enables OKAY (1831)
    assert!(s.key_input("ENTER"));
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "Enter with OKAY enabled destroys and hides"
    );
    assert!(s.cursor_item().is_none());
    assert_eq!(s.take_container_destroys(), vec![(0, 1, 0)]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// NO on the typed variant is the plain one's cancel — `ClearCursor()`, no wire send — and the
/// entry's `OnHide` empties the box so the next raise opens blank (the ref's own reason: a word
/// left over from last time would be armed before the player read the dialog).
#[test]
fn no_on_the_typed_confirm_clears_and_leaves_the_box_empty_for_next_time() {
    let mut s = setup();
    drop_in_world(&mut s, 871, "Flurry Axe", 4);
    s.run(r#"StaticPopup1EditBox:SetText("DELETE")"#).unwrap();

    s.run("StaticPopup_OnClick(StaticPopup1, 2)").unwrap();
    assert!(!s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap());
    assert!(s.cursor_item().is_none(), "ClearCursor cleared it");
    assert!(s.take_container_destroys().is_empty(), "No never destroys");
    assert_eq!(
        s.eval::<String>("return StaticPopup1EditBox:GetText()")
            .unwrap(),
        "",
        "OnHide empties the box"
    );

    // Raise it again: blank box, OKAY disabled — the armed state did not survive.
    drop_in_world(&mut s, 871, "Flurry Axe", 4);
    assert_eq!(
        s.eval::<String>("return StaticPopup1EditBox:GetText()")
            .unwrap(),
        ""
    );
    assert_eq!(
        s.eval::<i64>("return StaticPopup1Button1:IsEnabled()")
            .unwrap(),
        0,
        "and OKAY is disabled again"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// ESC out of the focused box: 1743's named divergence. The reference's `DELETE_GOOD_ITEM` names
/// no `EditBoxOnEscapePressed`, so in 1.12 the focused box swallows the key and the destroy
/// confirm cannot be dismissed with ESC at all. benilla's engine falls back to the ordinary
/// hideOnEscape leg — the entry's `OnCancel` (`ClearCursor()`), then hide — so the item is
/// released rather than left held under a dialog that will not close.
#[test]
fn escape_out_of_the_typed_confirm_cancels_the_way_every_other_popup_does() {
    let mut s = setup();
    drop_in_world(&mut s, 871, "Flurry Axe", 4);
    assert!(s
        .eval::<bool>("return StaticPopup1EditBox:HasFocus()")
        .unwrap());

    assert!(s.key_input("ESCAPE"), "the focused box consumes ESCAPE");
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "ESC closes it"
    );
    assert!(s.cursor_item().is_none(), "and runs OnCancel's ClearCursor");
    assert!(s.take_container_destroys().is_empty(), "ESC never destroys");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The plain arm keeps its own edit box HIDDEN — the fork is not cosmetic, and a common-quality
/// destroy must not grow a field to type into. The control for every test above.
#[test]
fn the_plain_arm_shows_no_edit_box() {
    let mut s = setup();
    pick_up_and_drop_in_world(&mut s); // Tough Jerky, quality 1

    assert_eq!(
        s.eval::<String>("return StaticPopup1.which").unwrap(),
        "DELETE_ITEM"
    );
    assert!(
        !s.eval::<bool>("return StaticPopup1EditBox:IsShown()")
            .unwrap(),
        "no hasEditBox on the plain entry"
    );
    assert_eq!(
        s.eval::<i64>("return StaticPopup1Button1:IsEnabled()")
            .unwrap(),
        1,
        "and YES is live immediately"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
