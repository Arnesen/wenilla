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

use super::test_ui::load_ui as load_xml;

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

/// **The whole point of decision 1687: one click from Home to the text box.**
///
/// The category list and the per-category blurb page are gone, so this walks what is left — open
/// the window, press the one button, type, submit — and asserts the ticket goes out **uncategorised
/// (0)**. That last part is the assertion that actually pins 1687: 0 is what makes a GM's queue read
/// "Unknown" instead of a heading the player never chose.
#[test]
fn one_click_from_home_files_an_uncategorised_ticket() {
    let mut s = setup();
    s.run("ToggleHelpFrame()").unwrap();
    let _ = s.take_gm_ticket_intents(); // the OnShow GetGMStatus

    assert!(
        s.eval::<bool>("return HelpFrameHome:IsVisible()").unwrap(),
        "the window opens on Home"
    );
    s.run("HelpFrameHomeIssues:Click()").unwrap();
    assert!(
        s.eval::<bool>("return HelpFrameOpenTicket:IsVisible()")
            .unwrap(),
        "one click lands on the editor — no category page in between"
    );

    s.run("HelpFrameOpenTicketText:SetText(\"My sword vanished.\")")
        .unwrap();
    s.run("HelpFrameOpenTicketSubmit_OnClick()").unwrap();

    assert_eq!(
        s.take_gm_ticket_intents(),
        vec![GmTicketIntent::Write(GmTicketWrite {
            category: 0,
            text: "My sword vanished.".into(),
            is_new: true,
        })],
        "a create files under 0 — \"Unknown\" in the GM's queue"
    );
}

/// **Auto-Unstuck sits on Home now**, beside the ticket button — it used to be the Stuck category's
/// action button, three pages down, which is nowhere.
///
/// Clicked through the real button rather than by calling the handler, because the button's
/// existence on Home *is* the change.
#[test]
fn auto_unstuck_is_a_button_on_home() {
    let mut s = setup();
    s.run("ToggleHelpFrame()").unwrap();
    s.run("BenillaHelpFrameHomeUnstick:Click()").unwrap();
    assert_eq!(s.take_stuck_casts(), 1);
    assert!(
        !s.eval::<bool>("return HelpFrame:IsVisible()").unwrap(),
        "and it closes the window"
    );
}

/// **The two Home buttons do not overlap, and both fit their labels.**
///
/// The layout is ours (the reference had one button here), and its whole trick is that the left
/// button hangs by its TOPRIGHT and the right one off that fixed edge, so fitting them to their
/// text grows them *away* from the gutter. Nothing else in the suite would notice them landing on
/// top of each other: a script error inside `OnShow` is swallowed into the VM's error record, so a
/// fit that never ran still leaves every other test green.
#[test]
fn the_two_home_buttons_sit_side_by_side_without_overlapping() {
    let mut s = setup();
    s.run("ToggleHelpFrame()").unwrap();
    s.resolve();

    let edge = |frame: &str, edge: &str| {
        s.eval::<f32>(&format!("return {frame}:Get{edge}()"))
            .unwrap_or_else(|e| panic!("{frame}:Get{edge}(): {e}"))
    };
    let (ticket_l, ticket_r) = (
        edge("HelpFrameHomeIssues", "Left"),
        edge("HelpFrameHomeIssues", "Right"),
    );
    let (stuck_l, stuck_r) = (
        edge("BenillaHelpFrameHomeUnstick", "Left"),
        edge("BenillaHelpFrameHomeUnstick", "Right"),
    );

    assert!(
        ticket_r <= stuck_l,
        "the buttons overlap: ticket ends at {ticket_r}, unstick starts at {stuck_l}"
    );
    assert!(
        ticket_r > ticket_l && stuck_r > stuck_l,
        "both buttons must have width: {ticket_l}..{ticket_r}, {stuck_l}..{stuck_r}"
    );
    // The fit ran: an unfitted button keeps its authored 250px, and "Auto-Unstuck" is nowhere near
    // that wide. This is what catches an OnShow that silently errored out.
    assert!(
        stuck_r - stuck_l < 200.0,
        "the unstick button was never fitted to its label (width {})",
        stuck_r - stuck_l
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

/// The Submit button picks its verb from the window's own `hasTicket` — an edit after an answer is
/// an UPDATE, a submit before one is a CREATE — **and the two legs send different categories**.
///
/// That asymmetry is decision 1687's real content, and it is the half a suite would otherwise miss.
/// A create has nothing to go on and sends 0. An edit sends the server's own category straight
/// back, because `HandleGMTicketUpdateTextOpcode` overwrites the field with whatever arrives: send
/// 0 there and a GM who re-filed the ticket onto a real heading watches it drop to "Unknown" the
/// moment the player fixes a typo. Nothing on screen would ever show that.
#[test]
fn a_create_files_under_zero_and_an_edit_gives_the_server_its_category_back() {
    let mut s = setup();
    s.run("ShowUIPanel(HelpFrame) HelpFrame_ShowFrame(\"OpenTicket\")")
        .unwrap();
    let _ = s.take_gm_ticket_intents(); // the OnShow GetGMStatus

    // A stale `ticketType` must NOT leak into a create — the window has no picker, so a create is
    // uncategorised no matter what is lying around on the frame.
    s.run("HelpFrameOpenTicket.ticketType = 3 HelpFrameOpenTicketText:SetText(\"a\")")
        .unwrap();
    s.run("HelpFrameOpenTicketSubmit_OnClick()").unwrap();
    let intents = s.take_gm_ticket_intents();
    let [GmTicketIntent::Write(create)] = intents.as_slice() else {
        panic!("expected exactly one write, got {intents:?}");
    };
    assert!(create.is_new, "no ticket known yet — this is a create");
    assert_eq!(create.category, 0, "a create is uncategorised");

    // The server answers: this ticket is filed under 4 (Item) — a GM moved it there.
    s.fire_event("UPDATE_TICKET", open_ticket_args(4, "a", 0.1, 0.2, 0.01));
    s.run("HelpFrameOpenTicketText:SetText(\"a, still\")")
        .unwrap();
    s.run("HelpFrameOpenTicketSubmit_OnClick()").unwrap();
    let intents = s.take_gm_ticket_intents();
    let [GmTicketIntent::Write(edit)] = intents.as_slice() else {
        panic!("expected exactly one write, got {intents:?}");
    };
    assert!(!edit.is_new, "a ticket is known — this is an update");
    assert_eq!(edit.text, "a, still");
    assert_eq!(
        edit.category, 4,
        "the edit must hand the GM's own category back, not overwrite it with 0"
    );
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
/// **Verified to fail**, as 0675 requires, and re-verified after 1687 lowered `min_compared` from
/// 60 to 28: nudging `TicketStatusFrame`'s width 208 → 209 reports
/// `TicketStatusFrame: ours [(209.0, 52.0), …] != ref [(208.0, 52.0), …]` and fails. Re-checking
/// that after shrinking the floor is the point — a guard whose floor drops below what it actually
/// compares stops guarding and never says so.
#[test]
fn the_windows_geometry_matches_the_reference_file() {
    let Some(reference) = super::framexml_diff::reference("HelpFrame.xml") else {
        return; // no install — this test is a no-op rather than a failure
    };
    /// Deliberate deviations, by REFERENCE name. Each earns its reason here; a tolerance would
    /// let a real difference hide, so this is a list and stays a list.
    const EXPECTED: &[&str] = &[
        // The ticket editor's scroll frame carries the house kit's shared trough in place of the
        // three loose bar textures the reference hangs beside it (HelpFrame.xml's header).
        "HelpFrameOpenTicketScrollFrame",
        // Home's button row is ours: the reference has one button centred under the text, we have
        // two, so the left one hangs by its TOPRIGHT at -6 rather than centred at 0. Its SIZE still
        // matches, which is why only the offset differs.
        "HelpFrameHomeIssues",
    ];
    // 1687 deleted the category list and the per-category page, and roughly seventy named elements
    // went with them — hence 28 rather than 60. The floor still has to bite: it is what stops this
    // guard quietly comparing nothing after the next window-shrinking change.
    super::framexml_diff::assert_geometry_matches("HelpFrame.xml", &reference, EXPECTED, 28);
}
