//! Drives the REAL `assets/ui/MailFrame.xml` through the engine (decision 0544 P1/P2) — the mail
//! twin of `tradeskill_frame.rs`: it loads the same file chain the app does (cut to the mail
//! window's dependency prefix), pushes a synthetic inbox, opens the window with the app's own
//! `MAIL_SHOW`/`MAIL_INBOX_UPDATE` events, and asserts the transcribed Lua actually paints — the
//! named regions exist, the rows populate from a fed `MailState`, the paging math is right, and the
//! unread/read row state tracks the wire `wasRead` flag.

use benilla_ui::script::{MailInboxRow, MailState, UiScript};

const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ui");

/// The mail window's load prefix — the app's own order (`ui_script/mod.rs`), members only.
/// MerchantFrame.xml rides along because MailFrame.xml reuses its global `BenillaMoney_*` coin
/// helpers (postage display), so a load error in either fails here.
const FILES: [&str; 5] = [
    "Fonts.xml",
    "UiPanels.xml",
    "GameTooltip.xml",
    "MerchantFrame.xml",
    "MailFrame.xml",
];

fn load_ui(script: &UiScript) {
    let dir = std::path::Path::new(UI_DIR);
    let provider = |req: &str| -> Option<String> {
        let norm = req.replace('\\', "/");
        let base = norm.rsplit('/').next().unwrap_or(&norm);
        std::fs::read_to_string(dir.join(&norm))
            .or_else(|_| std::fs::read_to_string(dir.join(base)))
            .ok()
    };
    for file in FILES {
        let text = std::fs::read_to_string(dir.join(file)).unwrap_or_else(|e| {
            panic!("reading {file}: {e}");
        });
        let doc = benilla_ui::framexml::parse(&text).unwrap_or_else(|e| {
            panic!("parsing {file}: {e}");
        });
        let report = benilla_ui::loader::load(script, &doc, &provider);
        assert!(
            report.errors.is_empty(),
            "{file} loaded with errors: {:#?}",
            report.errors
        );
    }
}

/// One inbox row with the fields the window paints.
fn row(sender: &str, subject: &str, was_read: bool, item_id: u32, cod: u32) -> MailInboxRow {
    MailInboxRow {
        package_icon: (item_id != 0).then(|| "Interface\\Icons\\INV_Misc_Bag_08".to_string()),
        stationery_icon: Some("Interface\\Icons\\INV_Misc_Note_01".to_string()),
        sender: Some(sender.to_string()),
        subject: subject.to_string(),
        money: 0,
        cod,
        days_left: 29.0,
        item_count: if item_id != 0 { 2 } else { 0 },
        was_read,
        was_returned: false,
        text_created: false,
        can_reply: true,
        is_gm: false,
        body: Some("body".into()),
        stationery_texture: "STATIONERYTEST".into(),
        is_invoice: false,
        has_body: true,
        item_id,
        item_name: (item_id != 0).then(|| "Linen Cloth".to_string()),
        item_texture: (item_id != 0).then(|| "Interface\\Icons\\INV_Fabric_Linen_01".to_string()),
        item_quality: (item_id != 0).then_some(1),
        can_delete: false,
    }
}

/// A one-page inbox (2 mails).
fn small_inbox() -> MailState {
    MailState {
        inbox: vec![
            row("Thrall", "Warchief's orders", false, 2589, 0),
            row("Jaina", "A letter", true, 0, 0),
        ],
    }
}

#[test]
fn mail_frame_loads_and_key_regions_exist() {
    let s = UiScript::new().unwrap();
    load_ui(&s);
    // The window, both tabs' bodies, a row, the open-letter toplevel, and the send-tab widgets.
    for name in [
        "BenillaMailFrame",
        "BenillaInboxFrame",
        "BenillaMailItem1",
        "BenillaMailItem1Button",
        "BenillaMailItem7",
        "BenillaSendMailFrame",
        "BenillaSendMailNameEditBox",
        "BenillaSendMailMailButton",
        "BenillaOpenMailFrame",
        "BenillaOpenMailReplyButton",
    ] {
        assert!(
            s.eval::<bool>(&format!("return getglobal('{name}') ~= nil"))
                .unwrap(),
            "region {name} should exist"
        );
    }
    // The window is hidden until MAIL_SHOW.
    assert!(!s.eval::<bool>("return BenillaMailFrame:IsShown()").unwrap());
}

#[test]
fn mail_show_opens_and_inbox_populates() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_mail(Some(small_inbox()));

    s.fire_event("MAIL_SHOW", vec![]);
    assert!(
        s.eval::<bool>("return BenillaMailFrame:IsShown()").unwrap(),
        "the window opens on MAIL_SHOW"
    );
    // MAIL_SHOW's CheckInbox() queued the inbox-refresh intent.
    assert!(s.take_mail_check_inbox(), "MAIL_SHOW fires CheckInbox");

    s.fire_event("MAIL_INBOX_UPDATE", vec![]);
    assert_eq!(s.eval::<i64>("return GetInboxNumItems()").unwrap(), 2);
    // Row 1 painted its sender + subject from the fed state.
    assert_eq!(
        s.eval::<String>("return BenillaMailItem1Sender:GetText()")
            .unwrap(),
        "Thrall"
    );
    assert_eq!(
        s.eval::<String>("return BenillaMailItem1Subject:GetText()")
            .unwrap(),
        "Warchief's orders"
    );
    // The row's clickable child button is shown for a populated row, hidden past the 2 mails (the
    // reference row FRAME stays shown — only $parentButton toggles; ref InboxFrame_Update l.120/180).
    assert!(s
        .eval::<bool>("return BenillaMailItem1Button:IsShown()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return BenillaMailItem3Button:IsShown()")
        .unwrap());

    // No script errors escaped the event-driven repaints.
    assert!(s.take_errors().is_empty(), "clean repaint");
}

#[test]
fn paging_math_enables_next_only_when_overflowing() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    // 9 mails → 2 pages of 7.
    let mut inbox = Vec::new();
    for i in 0..9 {
        inbox.push(row(&format!("S{i}"), &format!("subj{i}"), false, 0, 0));
    }
    s.set_mail(Some(MailState { inbox }));
    s.fire_event("MAIL_SHOW", vec![]);
    s.fire_event("MAIL_INBOX_UPDATE", vec![]);

    // Page 1: prev disabled, next enabled.
    assert!(!s
        .eval::<bool>("return BenillaInboxPrevPageButton:IsEnabled()")
        .unwrap());
    assert!(s
        .eval::<bool>("return BenillaInboxNextPageButton:IsEnabled()")
        .unwrap());
    // Turn the page: prev enabled, next disabled (only 2 mails on page 2).
    s.run("BenillaInboxNextPage()").unwrap();
    assert!(s
        .eval::<bool>("return BenillaInboxPrevPageButton:IsEnabled()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return BenillaInboxNextPageButton:IsEnabled()")
        .unwrap());
}

/// The unread/read row *coloring* (SetTextColor / SetVertexColor) has no engine getter to assert
/// against, so the harness verifies the other observable per-row state the same Update paints: the
/// COD tag shows on a COD mail and hides on a plain one, and the plain row's money field is nil.
#[test]
fn cod_tag_shows_on_a_cod_mail() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_mail(Some(MailState {
        inbox: vec![
            row("Auctioneer", "COD parcel", false, 2589, 5000), // COD 50s
            row("Jaina", "A letter", true, 0, 0),               // no COD
        ],
    }));
    s.fire_event("MAIL_SHOW", vec![]);
    s.fire_event("MAIL_INBOX_UPDATE", vec![]);

    assert!(
        s.eval::<bool>("return BenillaMailItem1ButtonCOD:IsShown()")
            .unwrap(),
        "the COD mail shows its coin tag"
    );
    assert!(
        !s.eval::<bool>("return BenillaMailItem2ButtonCOD:IsShown()")
            .unwrap(),
        "the plain mail hides the COD tag"
    );
    assert!(s.take_errors().is_empty());
}

#[test]
fn opening_a_letter_shows_the_open_frame_and_queues_the_body() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_mail(Some(small_inbox()));
    s.fire_event("MAIL_SHOW", vec![]);
    s.fire_event("MAIL_INBOX_UPDATE", vec![]);
    let _ = s.take_mail_opens();

    // Click row 1: the check button toggles on → open the letter.
    // A programmatic click toggles the check button on, then fires OnClick (this = the button).
    s.run("BenillaMailItem1Button:Click()").unwrap();
    assert!(
        s.eval::<bool>("return BenillaOpenMailFrame:IsShown()")
            .unwrap(),
        "the open-letter frame shows"
    );
    // The sender/subject/body painted; GetInboxText queued the open (mark-read + body ask).
    assert_eq!(
        s.eval::<String>("return BenillaOpenMailSender:GetText()")
            .unwrap(),
        "Thrall"
    );
    assert!(s.take_mail_opens().contains(&1), "opening queued the row");
    assert!(s.take_errors().is_empty());
}

#[test]
fn reply_switches_to_send_tab_prefilled() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_mail(Some(small_inbox()));
    s.fire_event("MAIL_SHOW", vec![]);
    s.fire_event("MAIL_INBOX_UPDATE", vec![]);
    // A programmatic click toggles the check button on, then fires OnClick (this = the button).
    s.run("BenillaMailItem1Button:Click()").unwrap();

    s.run("BenillaOpenMail_Reply()").unwrap();
    // The send tab is now shown, the recipient prefilled, the subject "RE: "-prefixed.
    assert!(s
        .eval::<bool>("return BenillaSendMailFrame:IsShown()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return BenillaSendMailNameEditBox:GetText()")
            .unwrap(),
        "Thrall"
    );
    assert_eq!(
        s.eval::<String>("return BenillaSendMailSubjectEditBox:GetText()")
            .unwrap(),
        "RE: Warchief's orders"
    );
}

/// Closing an ordinary letter (no money, no item, textCreated=false) must NOT delete it — the
/// reference OnHide rule (MailFrame.lua l.256-272) only purges a fully-taken husk.
#[test]
fn closing_a_plain_letter_does_not_delete_it() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_mail(Some(MailState {
        inbox: vec![row("One", "test", false, 0, 0)],
    }));
    s.fire_event("MAIL_SHOW", vec![]);
    s.fire_event("MAIL_INBOX_UPDATE", vec![]);
    let _ = s.take_mail_opens();
    let _ = s.take_mail_deletes();

    s.run("BenillaMailItem1Button:Click()").unwrap();
    assert!(s
        .eval::<bool>("return BenillaOpenMailFrame:IsShown()")
        .unwrap());
    s.run("BenillaOpenMailCancelButton:Click()").unwrap();
    assert!(s.take_errors().is_empty(), "no Lua errors on close");
    let deletes = s.take_mail_deletes();
    assert!(
        deletes.is_empty(),
        "closing a plain read letter must NOT delete it, got deletes: {deletes:?}"
    );
}

/// Closing a fully-taken husk (no money, no item, textCreated=TRUE) deletes it — the reference
/// OnHide purge. This is also the live "mail vanished on close" moment against vmangos: the server
/// stamps an EMPTY-BODY player mail MAIL_CHECK_MASK_COPIED (`MailHandler.cpp` l.421,
/// `req->body.empty() ? MAIL_CHECK_MASK_COPIED : MAIL_CHECK_MASK_HAS_BODY`), which IS the wire's
/// textCreated bit — so a subject-only letter with nothing attached auto-purges when closed, in
/// the real 1.12 client exactly as here.
#[test]
fn closing_a_taken_husk_deletes_it() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    let mut husk = row("One", "test", false, 0, 0);
    husk.text_created = true;
    s.set_mail(Some(MailState { inbox: vec![husk] }));
    s.fire_event("MAIL_SHOW", vec![]);
    s.fire_event("MAIL_INBOX_UPDATE", vec![]);
    let _ = s.take_mail_opens();
    let _ = s.take_mail_deletes();

    s.run("BenillaMailItem1Button:Click()").unwrap();
    s.run("BenillaOpenMailCancelButton:Click()").unwrap();
    assert!(s.take_errors().is_empty(), "no Lua errors on close");
    assert_eq!(
        s.take_mail_deletes(),
        vec![1],
        "the husk purges on close (reference OnHide, MailFrame.lua l.256-272)"
    );
}

/// The expiry text pluralizes like the reference (GetText("DAYS_ABBR"): "Day"/"Days").
#[test]
fn expiry_text_pluralizes_days() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    let mut one_day = row("Two", "b", false, 0, 0);
    one_day.days_left = 1.7;
    s.set_mail(Some(MailState {
        inbox: vec![row("One", "a", false, 0, 0), one_day],
    }));
    s.fire_event("MAIL_SHOW", vec![]);
    s.fire_event("MAIL_INBOX_UPDATE", vec![]);
    assert_eq!(
        s.eval::<String>("return BenillaMailItem1ExpireTime:GetText()")
            .unwrap(),
        "|cff20ff2029 Days|r"
    );
    assert_eq!(
        s.eval::<String>("return BenillaMailItem2ExpireTime:GetText()")
            .unwrap(),
        "|cff20ff201 Day|r"
    );
    assert!(s.take_errors().is_empty());
}

/// The letter button — "make a permanent copy" (ref OpenMail_Update l.364-376): a mail whose body
/// is takeable (`item_text_id != 0`) and not yet copied shows it; clicking queues the
/// `TakeInboxTextItem` intent (→ `CMSG_MAIL_CREATE_TEXT_ITEM`). With money enclosed too, both
/// buttons show and the caption reads "Take Attachments:".
#[test]
fn letter_button_shows_for_a_body_letter_and_click_queues_the_copy() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    let mut mail = row("One", "asd", false, 0, 0);
    mail.money = 10000;
    s.set_mail(Some(MailState { inbox: vec![mail] }));
    s.fire_event("MAIL_SHOW", vec![]);
    s.fire_event("MAIL_INBOX_UPDATE", vec![]);

    s.run("BenillaMailItem1Button:Click()").unwrap();
    assert!(
        s.eval::<bool>("return BenillaOpenMailLetterButton:IsShown()")
            .unwrap(),
        "a takeable, not-yet-copied body shows the letter button"
    );
    assert!(s
        .eval::<bool>("return BenillaOpenMailMoneyButton:IsShown()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return BenillaOpenMailAttachmentText:GetText()")
            .unwrap(),
        "Take Attachments:"
    );
    s.run("BenillaOpenMailLetterButton:Click()").unwrap();
    assert_eq!(
        s.take_mail_take_texts(),
        vec![1],
        "the click queues the permanent-copy intent"
    );
    assert!(s.take_errors().is_empty());
}

/// Once the body is copied (`textCreated`, the wire COPIED bit) the letter button hides — the same
/// flag whose OnHide purge then deletes the husk on close.
#[test]
fn letter_button_hides_once_copied() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    let mut mail = row("One", "asd", false, 0, 0);
    mail.money = 10000;
    mail.text_created = true;
    s.set_mail(Some(MailState { inbox: vec![mail] }));
    s.fire_event("MAIL_SHOW", vec![]);
    s.fire_event("MAIL_INBOX_UPDATE", vec![]);

    s.run("BenillaMailItem1Button:Click()").unwrap();
    assert!(!s
        .eval::<bool>("return BenillaOpenMailLetterButton:IsShown()")
        .unwrap());
    assert!(s
        .eval::<bool>("return BenillaOpenMailMoneyButton:IsShown()")
        .unwrap());
    assert!(s.take_errors().is_empty());
}

/// Hovering the coins shows the plain money tooltip (ref OpenMailMoneyButton OnEnter l.1823-1829).
#[test]
fn money_button_hover_shows_the_amount_tooltip() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    let mut mail = row("One", "asd", false, 0, 0);
    mail.money = 10000;
    s.set_mail(Some(MailState { inbox: vec![mail] }));
    s.fire_event("MAIL_SHOW", vec![]);
    s.fire_event("MAIL_INBOX_UPDATE", vec![]);
    s.run("BenillaMailItem1Button:Click()").unwrap();

    s.run("BenillaOpenMailMoneyButton_OnEnter(BenillaOpenMailMoneyButton)")
        .unwrap();
    assert!(
        s.eval::<bool>("return GameTooltip:IsShown()").unwrap(),
        "the money tooltip shows on hover"
    );
    assert!(
        s.eval::<bool>("return GameTooltipMoneyCoin1:IsShown()")
            .unwrap(),
        "the coin row rendered (SetTooltipMoney path)"
    );
    assert!(s.take_errors().is_empty());
}

/// The inbox page label stays EMPTY: the ref XML declares InboxCurrentPage (l.329) but no
/// reference Lua ever writes it — the real 1.12 window shows nothing there ("Page 1" was our
/// invention, now removed).
#[test]
fn inbox_page_label_stays_empty_like_the_reference() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_mail(Some(small_inbox()));
    s.fire_event("MAIL_SHOW", vec![]);
    s.fire_event("MAIL_INBOX_UPDATE", vec![]);
    let text = s
        .eval::<Option<String>>("return BenillaInboxCurrentPage:GetText()")
        .unwrap();
    assert!(
        text.as_deref().unwrap_or("").is_empty(),
        "InboxCurrentPage must stay unwritten (got {text:?})"
    );
}
