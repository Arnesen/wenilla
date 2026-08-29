//! The GM help window (decision 1673, HelpFrame.xml): the category list the DBC feeds it, the two
//! faces of `UPDATE_TICKET`, the queue gate, the ticket toast, and the three dialogs.
//!
//! Written as the **falsification** pass over the transcription rather than a demonstration of it:
//! every test is named after one claim the window makes, and each was checked to fail when the
//! claim is broken. The load-bearing one is
//! [`clicking_a_category_files_a_ticket_under_that_categorys_dbc_id`] — the id travels from
//! `GMTicketCategory.dbc` through a button, a page, and the editor onto the wire, and a break
//! anywhere in that chain files every ticket under the wrong heading with nothing on screen to
//! show for it.

use benilla_ui::script::{GmTicketIntent, GmTicketWrite, ScriptValue, UiScript};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error (the binder tests'
/// loader, duplicated so this file is self-contained).
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

/// The window, its dependencies, and the catalog the app pushes — the real ten `GMTicketCategory`
/// rows, so a test that walks the list is walking the shipped data.
fn setup() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "BasicControls.xml",
        "UIPanelTemplates.xml",
        "ScrollTemplates.xml",
        "GameTooltip.xml",
        // Before UiPanels.xml: the shared StaticPopup carries a `SmallMoneyFrameTemplate` coin
        // row, whose OnLoad calls `SmallMoneyFrame_OnLoad` — the TOC's own order (1580's
        // talent-wipe fixture hit this first).
        "MoneyFrame.xml",
        "UiPanels.xml",
        "HelpFrame.xml",
    ] {
        load_xml(&s, file);
    }
    s.set_gm_ticket_categories(vec![
        (1, "Stuck".into()),
        (2, "Behavior/Harassment".into()),
        (3, "Guild".into()),
        (4, "Item".into()),
        (5, "Environmental".into()),
        (6, "Non-Quest/Creep".into()),
        (7, "Quest/Quest NPC".into()),
        (8, "Technical".into()),
        (9, "Account/Billing".into()),
        (10, "Character".into()),
    ]);
    s
}

/// The `UPDATE_TICKET` argument list the app's feed builds for an open ticket — category first,
/// text second, exactly as `ui_gm_ticket::update_ticket_args` orders it. Kept in sync by being
/// written the same way in both places; if they ever disagree, this file's tests are what notices.
fn open_ticket_args(
    category: i64,
    text: &str,
    age: f64,
    oldest: f64,
    update: f64,
) -> Vec<ScriptValue> {
    vec![
        ScriptValue::Int(category),
        ScriptValue::Str(text.into()),
        ScriptValue::Number(age),
        ScriptValue::Number(oldest),
        ScriptValue::Number(update),
        ScriptValue::Int(0),
        ScriptValue::Int(0),
    ]
}

/// **The whole point of the feature, end to end.** The category id travels
/// `GMTicketCategory.dbc` → `GetGMTicketCategories()` → a `HelpFrameButton*` → the page route →
/// `HelpFrameOpenTicket.ticketType` → `NewGMTicket`'s wire value. Break any link and every ticket
/// files under the wrong heading, silently — nothing on screen looks wrong.
#[test]
fn clicking_a_category_files_a_ticket_under_that_categorys_dbc_id() {
    let mut s = setup();
    s.run("ShowUIPanel(HelpFrame) HelpFrame_ShowFrame(\"GMHome\")")
        .unwrap();
    // Opening the window is itself traffic: its OnShow calls `GetGMStatus()`. Drained here so the
    // assertion below is about the CLICK and nothing else — the intents are one ordered queue, so
    // setup traffic and the thing under test share it (decision 1673).
    let _ = s.take_gm_ticket_intents();

    // Row 4 is Item, DBC id 4 — the row's LABEL and its stored id must be the same pair.
    assert_eq!(
        s.eval::<String>("return HelpFrameButton4Text:GetText()")
            .unwrap(),
        "Item"
    );
    assert_eq!(
        s.eval::<i64>("return HelpFrameButton4.ticketType").unwrap(),
        4,
        "the button carries the DBC id, not its row number"
    );

    // Click it, take the page's action button through to the editor, and submit.
    s.run("HelpFrameButton4:Click()").unwrap();
    s.run("HelpFrameGeneralButton_OnClick(HelpFrameGeneralButton)")
        .unwrap();
    s.run("HelpFrameOpenTicketText:SetText(\"My sword vanished.\")")
        .unwrap();
    s.run("HelpFrameOpenTicketSubmit_OnClick()").unwrap();

    assert_eq!(
        s.take_gm_ticket_intents(),
        vec![GmTicketIntent::Write(GmTicketWrite {
            category: 4,
            text: "My sword vanished.".into(),
            is_new: true,
        })],
        "the clicked category's DBC id is what goes on the wire"
    );
}

/// The ten shipped rows paint in DBC order, and row 2 routes to its OWN page rather than the
/// shared one — the single category in the table with a bespoke frame.
#[test]
fn the_ten_categories_paint_in_dbc_order_and_harassment_has_its_own_page() {
    let s = setup();
    s.run("ShowUIPanel(HelpFrame) HelpFrame_ShowFrame(\"GMHome\")")
        .unwrap();
    assert_eq!(
        s.eval::<String>("return HelpFrameButton1Text:GetText()")
            .unwrap(),
        "Stuck"
    );
    assert_eq!(
        s.eval::<String>("return HelpFrameButton10Text:GetText()")
            .unwrap(),
        "Character",
        "all ten rows fit without scrolling"
    );

    s.run("HelpFrameButton2:Click()").unwrap();
    assert!(
        s.eval::<bool>("return HelpFrameHarassment:IsVisible()")
            .unwrap(),
        "category 2 routes to HelpFrameHarassment"
    );
    s.run("HelpFrameButton3:Click()").unwrap();
    assert!(
        s.eval::<bool>("return HelpFrameGeneral:IsVisible()")
            .unwrap(),
        "category 3 routes to the shared general page"
    );
    assert!(
        !s.eval::<bool>("return HelpFrameHarassment:IsVisible()")
            .unwrap(),
        "and the previous page is hidden — one page at a time"
    );
}

/// **`UPDATE_TICKET`'s two faces.** With a ticket the editor becomes an editor (Save Changes /
/// Exit); with the bare `arg1 = 0` it goes back to being a form (Submit / Cancel). The zero leg is
/// the one that would silently rot: it is the ordinary answer, so a window stuck in edit mode
/// looks fine until you try to file a second ticket.
#[test]
fn an_open_ticket_turns_the_form_into_an_editor_and_a_zero_turns_it_back() {
    let mut s = setup();
    s.run("ShowUIPanel(HelpFrame) HelpFrame_ShowFrame(\"OpenTicket\")")
        .unwrap();

    s.fire_event(
        "UPDATE_TICKET",
        open_ticket_args(7, "Where is this NPC?", 0.25, 2.5, 0.01),
    );
    assert_eq!(
        s.eval::<String>("return HelpFrameOpenTicketText:GetText()")
            .unwrap(),
        "Where is this NPC?",
        "arg2 is the description"
    );
    assert_eq!(
        s.eval::<i64>("return HelpFrameOpenTicket.ticketType")
            .unwrap(),
        7,
        "arg1 is the category"
    );
    assert_eq!(
        s.eval::<i64>("return HelpFrameOpenTicket.hasTicket")
            .unwrap(),
        1
    );

    // And now the ordinary answer.
    s.fire_event("UPDATE_TICKET", vec![ScriptValue::Int(0)]);
    assert_eq!(
        s.eval::<String>("return HelpFrameOpenTicketText:GetText()")
            .unwrap(),
        "",
        "the editor empties"
    );
    assert!(
        s.eval::<bool>("return HelpFrameOpenTicket.hasTicket == nil")
            .unwrap(),
        "and stops believing it has a ticket"
    );
}

/// The Submit button picks its verb from the window's own `hasTicket`, which is what the app must
/// not second-guess: an edit after an answer is an UPDATE, a submit before one is a CREATE.
#[test]
fn submit_sends_create_before_an_answer_and_update_after_one() {
    let mut s = setup();
    s.run("ShowUIPanel(HelpFrame) HelpFrame_ShowFrame(\"OpenTicket\")")
        .unwrap();
    let _ = s.take_gm_ticket_intents(); // the OnShow GetGMStatus — see the category test
    s.run("HelpFrameOpenTicket.ticketType = 3 HelpFrameOpenTicketText:SetText(\"a\")")
        .unwrap();
    s.run("HelpFrameOpenTicketSubmit_OnClick()").unwrap();
    assert!(
        matches!(
            s.take_gm_ticket_intents().as_slice(),
            [GmTicketIntent::Write(w)] if w.is_new
        ),
        "no ticket known yet — this is a create"
    );

    s.fire_event("UPDATE_TICKET", open_ticket_args(3, "a", 0.1, 0.2, 0.01));
    s.run("HelpFrameOpenTicketText:SetText(\"a, still\")")
        .unwrap();
    s.run("HelpFrameOpenTicketSubmit_OnClick()").unwrap();
    let intents = s.take_gm_ticket_intents();
    let [GmTicketIntent::Write(write)] = intents.as_slice() else {
        panic!("expected exactly one write, got {intents:?}");
    };
    assert!(!write.is_new, "a ticket is known — this is an update");
    assert_eq!(write.text, "a, still");
}

/// **The queue gate.** `UPDATE_GM_STATUS(0)` takes the petition queue down, and asking for the
/// editor then closes the window and says why instead of showing a form that cannot submit.
/// The `1` leg must put it back — a one-way gate would lock the player out for the session.
#[test]
fn a_downed_queue_refuses_the_editor_and_says_so_and_comes_back_up() {
    let mut s = setup();
    s.fire_event("UPDATE_GM_STATUS", vec![ScriptValue::Int(0)]);
    s.run("ShowUIPanel(HelpFrame) HelpFrame_ShowFrame(\"OpenTicket\")")
        .unwrap();
    assert!(
        !s.eval::<bool>("return HelpFrame:IsVisible()").unwrap(),
        "the window closes"
    );
    assert!(
        s.eval::<bool>("return StaticPopup_Visible(\"HELP_TICKET_QUEUE_DISABLED\") ~= nil")
            .unwrap(),
        "and the dialog says why"
    );

    s.fire_event("UPDATE_GM_STATUS", vec![ScriptValue::Int(1)]);
    s.run("ShowUIPanel(HelpFrame) HelpFrame_ShowFrame(\"OpenTicket\")")
        .unwrap();
    assert!(
        s.eval::<bool>("return HelpFrameOpenTicket:IsVisible()")
            .unwrap(),
        "queue back up, editor opens"
    );
}

/// The toast follows the ticket: up while one is open, gone when it is not. It is the only thing
/// on screen that says a ticket exists at all once the window is closed.
#[test]
fn the_ticket_toast_follows_the_ticket() {
    let mut s = setup();
    s.fire_event(
        "UPDATE_TICKET",
        open_ticket_args(1, "Stuck.", 0.1, 0.2, 0.01),
    );
    assert!(
        s.eval::<bool>("return TicketStatusFrame:IsVisible()")
            .unwrap(),
        "a ticket raises the toast"
    );
    s.fire_event("UPDATE_TICKET", vec![ScriptValue::Int(0)]);
    assert!(
        !s.eval::<bool>("return TicketStatusFrame:IsVisible()")
            .unwrap(),
        "and abandoning it takes the toast away"
    );
}

/// The toast's own poll is what keeps a long wait honest: `TicketStatus_OnUpdate` re-asks the
/// server every `GMTICKET_CHECK_INTERVAL`, and not before. This is the reason the app counts
/// answers instead of diffing them, so it is worth a test on this side too.
#[test]
fn the_toast_repolls_the_server_only_after_the_full_interval() {
    let mut s = setup();
    s.fire_event(
        "UPDATE_TICKET",
        open_ticket_args(1, "Stuck.", 0.1, 0.2, 0.01),
    );
    let _ = s.take_gm_ticket_intents();

    s.run("TicketStatus_OnUpdate(599)").unwrap();
    assert!(s.take_gm_ticket_intents().is_empty(), "not yet");
    s.run("TicketStatus_OnUpdate(2)").unwrap();
    assert_eq!(
        s.take_gm_ticket_intents(),
        vec![GmTicketIntent::Ask],
        "600s elapsed — re-ask"
    );
    s.run("TicketStatus_OnUpdate(1)").unwrap();
    assert!(
        s.take_gm_ticket_intents().is_empty(),
        "and the clock restarts"
    );
}

/// Abandoning goes through a confirm, and only its Yes sends the delete. A dialog whose Yes did
/// nothing would look identical to one that worked, right up until the ticket reappeared.
#[test]
fn abandoning_a_ticket_confirms_first_and_only_yes_sends_it() {
    let mut s = setup();
    s.run("StaticPopup_Show(\"HELP_TICKET_ABANDON_CONFIRM\")")
        .unwrap();
    s.run("StaticPopup_OnClick(StaticPopup1, 2)").unwrap();
    assert!(s.take_gm_ticket_intents().is_empty(), "No sends nothing");

    s.run("StaticPopup_Show(\"HELP_TICKET_ABANDON_CONFIRM\")")
        .unwrap();
    s.run("StaticPopup_OnClick(StaticPopup1, 1)").unwrap();
    assert_eq!(s.take_gm_ticket_intents(), vec![GmTicketIntent::Delete]);
}

/// `ToggleHelpFrame` is the micro button's whole wiring, and opening the window asks the server
/// for the queue status — without that ask the gate above would run on a stale assumption for the
/// life of the session.
#[test]
fn toggling_the_window_opens_it_and_asks_for_the_queue_status() {
    let mut s = setup();
    s.run("ToggleHelpFrame()").unwrap();
    assert!(s.eval::<bool>("return HelpFrame:IsVisible()").unwrap());
    assert_eq!(
        s.take_gm_ticket_intents(),
        vec![GmTicketIntent::AskStatus],
        "OnShow calls GetGMStatus — the gate must not run on an assumption"
    );
    // This is the one test that asserts the OnShow traffic itself; the others drain it away first.
    assert!(
        s.eval::<bool>("return HelpFrameHome:IsVisible()").unwrap(),
        "and it opens on Home"
    );

    s.run("ToggleHelpFrame()").unwrap();
    assert!(!s.eval::<bool>("return HelpFrame:IsVisible()").unwrap());
}

/// The Auto-Unstuck button casts the Stuck spell and closes the window. It is the one control in
/// this window that does something other than talk about tickets.
#[test]
fn auto_unstuck_casts_and_closes() {
    let mut s = setup();
    s.run("ShowUIPanel(HelpFrame) HelpFrame_ShowFrame(1)")
        .unwrap();
    s.run("HelpFrameUnstick_OnClick()").unwrap();
    assert_eq!(s.take_stuck_casts(), 1);
    assert!(!s.eval::<bool>("return HelpFrame:IsVisible()").unwrap());
}

/// **The retail-only text is gone from the Home page** (director's call, 2026-08-29).
///
/// The page is entirely GlobalStrings off the player's own chain, so three of its strings still
/// pointed at `worldofwarcraft.com` — a dead PvP-policy link, and a closing paragraph directing the
/// player to Blizzard's forums and policy pages. The window overrides them before the markup
/// resolves.
///
/// This test exists because the failure mode is *invisible in a test suite*: nothing breaks if the
/// override is dropped, the page simply starts advertising dead links again. Asserted on the
/// rendered FontStrings rather than the globals, so it also catches the markup being re-pointed at
/// a different key.
#[test]
fn the_home_page_advertises_no_dead_retail_links() {
    let s = setup();
    s.run("ShowUIPanel(HelpFrame) HelpFrame_ShowFrame(\"Home\")")
        .unwrap();

    for frame in [
        "HelpFrameHomePvpPolicyUrl",
        "HelpFrameHomeText2",
        "HelpFrameHomeIssue3",
        "HelpFrameHomeText1",
    ] {
        let text = s
            .eval::<String>(&format!("return {frame}:GetText() or \"\""))
            .unwrap()
            .to_lowercase();
        for dead in [
            "worldofwarcraft.com",
            "http://",
            "www.",
            ".shtml",
            "the forums",
        ] {
            assert!(
                !text.contains(dead),
                "{frame} still carries retail-only text ({dead:?}): {text:?}"
            );
        }
    }

    // The PvP guidance itself is KEPT — only the sentence introducing the link went. A test that
    // let the whole bullet be emptied would pass on an over-correction, which is the other way to
    // get this wrong.
    let pvp = s
        .eval::<String>("return HelpFrameHomeIssue3:GetText()")
        .unwrap();
    assert!(
        pvp.contains("PVP game mechanics"),
        "the PvP guidance must survive the link's removal: {pvp:?}"
    );
}

/// **The geometry oracle** (decision 0675): every element the transcription shares with the
/// reference file carries the reference's own `<AbsDimension>` numbers.
///
/// **Verified to fail**, as 0675 requires: nudging `TicketStatusFrame`'s width 208 → 209 makes it
/// report `TicketStatusFrame: ours [(209.0, 52.0), …] != ref [(208.0, 52.0), …]` and fail. It is a
/// guard, not a comfort.
#[test]
fn the_windows_geometry_matches_the_reference_file() {
    let Some(reference) = super::framexml_diff::reference("HelpFrame.xml") else {
        return; // no install — this test is a no-op rather than a failure
    };
    /// Deliberate deviations, by REFERENCE name. Each earns its reason here; a tolerance would
    /// let a real difference hide, so this is a list and stays a list.
    const EXPECTED: &[&str] = &[
        // The GM category list is the house FAUX scroll kit, not the reference's ScrollFrame, and
        // the kit's shared trough replaces the three loose bar textures the reference hangs beside
        // it (HelpFrame.xml's header, divergence 1). Same for the ticket editor's own trough.
        "HelpFrameGMScrollFrame",
        "HelpFrameOpenTicketScrollFrame",
    ];
    super::framexml_diff::assert_geometry_matches("HelpFrame.xml", &reference, EXPECTED, 60);
}
