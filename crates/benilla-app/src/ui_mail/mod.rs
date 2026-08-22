//! The app-side **mail feed** (decision 0544 P1/P2) — the inward half of the mail seam around
//! [`benilla_ui::script`]'s `mail` module, the mailbox twin of [`crate::ui_merchant`]'s vendor seam.
//!
//! The mailbox window is entirely client-side (there is no `SMSG_SHOW_MAILBOX` on 5875): a
//! right-click on a MAILBOX GameObject ([`crate::target`]) opens the session here — it stores the
//! mailbox guid and requests the window's show; it sends **no** packet (the wow-re §5 confirms the
//! MAILBOX use handler overrides the shared use-sender to a local open — never `CMSG_GAMEOBJ_USE`).
//! The window's own `MAIL_SHOW` handler then calls `CheckInbox()`, whose intent this module drains
//! into the first `CMSG_GET_MAIL_LIST` — one request, Lua-driven, exactly the reference flow
//! (MailFrame.lua l.42).
//!
//! The net bridge ([`crate::net::apply::mail`]) fills [`MailOpen`] from the wire
//! (`SMSG_MAIL_LIST_RESULT` → the inbox rows; `SMSG_ITEM_TEXT_QUERY_RESPONSE` → the body cache;
//! `SMSG_SEND_MAIL_RESULT` → the send/take result queues). Each frame [`feed_mail`] resolves each
//! wire [`MailListEntry`] to a Lua-facing [`MailInboxRow`] (sender via the ask-once name cache, item
//! name/quality/icon via the ask-once item-template cache + `ItemDisplayInfo.dbc`, body from the
//! cache, stationery basename from `Stationery.dbc`), pushes the snapshot
//! ([`benilla_ui::script::UiScript::set_mail`]), and fires the events the reference Lua drives —
//! `MAIL_SHOW` on open, `MAIL_INBOX_UPDATE` on a list/body change, `MAIL_CLOSED` on clear, and the
//! send-result trio (see [`feed_mail`]). [`drain_mail`] pulls the Lua intents back out into the mail
//! `CMSG`s. The standardized NPC-session range guard ([`crate::ui_session`]) client-side-closes the
//! window when the player leaves the mailbox's interaction range (wow-re §5: the client's generic
//! interaction-target range gate — past it every mail `CMSG` silently fails its `CheckMailBox`).
//!
//! **The arrival layer ([`MailPending`], decisions 0544 P3 / 0904 / 0913, wow-re
//! `ui/scratch/mail-pending-countdown.md`).** `HasNewMail()` mirrors the real client's own model: a
//! countdown float (`0x845eac`) read as **`|countdown| < ε`** — *near zero*, not `<= 0` (the sign
//! matters, decision 0904: vmangos's "no mail" reply is `-86400.0`, which a `<= 0` predicate reads
//! as "you have mail" at every login). vmangos only ever sends `0.0` ("waiting now") or `-86400.0`
//! ("none") — never a positive value — but the countdown is implemented in full since that's the
//! client's real model, not a two-value special case.
//!
//! **The icon's whole life is that float**, because `MiniMapMailFrame` is the *only* listener of
//! `UPDATE_PENDING_MAIL` in all of 1.12 FrameXML and its handler is a bare idempotent
//! `if HasNewMail() then Show() else Hide()` (`Minimap.xml` l.278-289) — it never listens for
//! `MAIL_INBOX_UPDATE`/`MAIL_CLOSED` despite the icon logically depending on the inbox state. So
//! **checking your mail clears the icon by a route that never touches the inbox** (decision 0913):
//! opening a letter arms the deferred-refresh flag ([`MailPending::arm_refresh`], the reference's
//! `[0xb6efcc]`), and the mailbox *close* re-asks the server — the sender stamping the countdown to
//! [`MAIL_TIME_NO_MAIL`] and the reply settling it. The same stamp-then-ask runs at every
//! world-enter ([`send_query_next_mail_time_on_enter`]), which is why the reference icon also blinks
//! off across a loading screen.
//!
//! [`feed_mail`] steps the float every frame ([`MailPending::step`]), keeps
//! [`benilla_ui::script::UiScript::set_has_new_mail`] current, and fires `UPDATE_PENDING_MAIL` at
//! the three sites the reference fires it (see [`MailPending::notify`]).

use std::collections::{HashMap, HashSet};

use benilla_protocol::messages::{mail_error, mail_message_type, MailListEntry};
use bevy::prelude::*;

use benilla_ui::script::{MailInboxRow, MailState, ScriptValue, UiScript};

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::names::NameCache;
use crate::net::{ClientCommand, EnteredWorldMessage, NetCommands, ObjectStore, SelfPlayer};
use crate::ui_script::UiInput;
use crate::ui_session::{close_npc_session_out_of_range, NpcSession};

mod pending;

pub(crate) use pending::MailPending;

/// `checked` mask bits (VERIFIED vmangos `Mail.h`, decision 0544): READ, RETURNED, COPIED.
const CHECKED_READ: u32 = 0x1;
const CHECKED_RETURNED: u32 = 0x2;
const CHECKED_COPIED: u32 = 0x4;

/// `MAIL_STATIONERY_GM` (`Stationery.dbc` row 61) — `isGM` is `stationery == 61` (MailFrame.lua
/// l.108: a GM row shows its stationery icon even when it carries a package).
const STATIONERY_GM: u32 = 61;

/// The `CheckInbox` → `CMSG_GET_MAIL_LIST` client-side rate limit, in seconds (wow-re §5
/// `ui/scratch/mail-interaction.md`: the real client drops repeat inbox queries inside this window).
const CHECK_INBOX_THROTTLE_SECS: f64 = 60.0;

/// The letter-row icon by kind (the wire carries only a stationery id; the reference derives the
/// row's `stationeryIcon` from it). VERIFIED via the data chain `Stationery.dbc` → stationery
/// item (41 → 9311 "Default Stationery", 61 → 18154 "Blizzard Stationery") → the item's
/// `ItemDisplayInfo` icon (displays 7798 / 30658) — and the default's pixels match the reference
/// screenshot's red-stamped letter (`INV_Letter_15`, our old guess, is the tan open envelope).
const STATIONERY_ICON_NORMAL: &str = "Interface\\Icons\\INV_Misc_Note_01";
const STATIONERY_ICON_GM: &str = "Interface\\Icons\\Mail_GMIcon";

/// The `Stationery.dbc` id → texture-basename catalog, loaded with the entity catalogs
/// ([`crate::entities`]). Optional resource — absent, every mail falls to the default backdrop
/// ([`benilla_formats::StationeryCatalog::DEFAULT_TEXTURE`]) and the window still works.
#[derive(Resource)]
pub(crate) struct Stationery(pub(crate) benilla_formats::StationeryCatalog);

/// One `SMSG_SEND_MAIL_RESULT` for `action == SEND`, queued by the net bridge for [`feed_mail`] to
/// fire (the feed owns the `UiScript`, the apply site doesn't). `MAIL_FAILED` fires on **every**
/// SEND result — success included — because the real client overloads it as "send resolved, unblock
/// the UI" (wow-re §5, byte-verified; MailFrame.lua's `MAIL_FAILED` handler only re-enables the
/// button). `MAIL_SEND_SUCCESS` fires additionally when `error == OK`.
pub(crate) struct MailSendAck {
    pub(crate) ok: bool,
    /// A pre-formatted red error line for a non-OK, non-equip send failure (else `None`).
    pub(crate) error_text: Option<String>,
}

/// The open mailbox, filled by the net bridge ([`crate::net::apply::mail`]) and read by
/// [`feed_mail`]. Holds the mailbox guid and the inbox rows exactly as the wire delivered them
/// (`SMSG_MAIL_LIST_RESULT`), the ask-once letter-body cache, and the send-result queues; the feed
/// resolves each row to a display row and the drain maps a clicked 1-based row to its wire mail id.
/// Cleared on a client-side close and on disconnect.
#[derive(Resource, Default)]
pub(crate) struct MailOpen {
    /// The mailbox whose window is open; `None` = no mailbox open.
    pub(crate) mailbox: Option<u64>,
    /// The inbox rows (wire order = 1-based display order).
    pub(crate) mails: Vec<MailListEntry>,
    /// Ask-once letter-body cache: `item_text_id` → body text.
    pub(crate) bodies: HashMap<u32, String>,
    /// `item_text_id`s with a `CMSG_ITEM_TEXT_QUERY` in flight (ask-once dedup).
    pub(crate) pending_bodies: HashSet<u32>,
    /// Fire `MAIL_SHOW` next feed — set on every mailbox click (the reference opens the window per
    /// use; wow-re §5). Consumed by [`feed_mail`].
    show_requested: bool,
    /// Session-scoped throttle: the `Time::elapsed_secs_f64` of the last `CMSG_GET_MAIL_LIST`, or
    /// `None` when never queried this session (a fresh open queries immediately).
    last_list_query: Option<f64>,
    /// SEND-action results the net bridge queued for the feed to fire ([`MailSendAck`]).
    pub(crate) send_acks: Vec<MailSendAck>,
    /// Pre-formatted red error lines (a take/return/delete failure) the feed fires via
    /// `UI_ERROR_MESSAGE`.
    pub(crate) errors: Vec<String>,
}

impl MailOpen {
    /// A mailbox right-click (decision 0544): open (or re-show) the window's session and request its
    /// `MAIL_SHOW`. A *different* mailbox resets the session (rows + throttle); the same one keeps
    /// them and just re-shows (the reference re-fires `MAIL_SHOW` on every use — `CheckInbox`'s own
    /// throttle stops the refresh from spamming the wire).
    pub(crate) fn click(&mut self, mailbox: u64) {
        if self.mailbox != Some(mailbox) {
            self.mailbox = Some(mailbox);
            self.mails.clear();
            self.last_list_query = None;
        }
        self.show_requested = true;
    }

    /// The wire mail id at a 1-based display row — what the mail `CMSG`s address.
    fn mail_id_at(&self, index_1based: u32) -> Option<u32> {
        index_1based
            .checked_sub(1)
            .and_then(|i| self.mails.get(i as usize))
            .map(|e| e.message_id)
    }

    /// Flip a row's local `checked` READ bit — the no-reply `CMSG_MAIL_MARK_AS_READ` has no server
    /// answer (decision 0544 P1), so the read state is client-authoritative until the next list.
    fn mark_read(&mut self, index_1based: u32) {
        if let Some(e) = index_1based
            .checked_sub(1)
            .and_then(|i| self.mails.get_mut(i as usize))
        {
            e.checked |= CHECKED_READ;
        }
    }

    /// Close the open window (a client-side close, no packet — vanilla). Keeps nothing; a re-open
    /// re-lists.
    pub(crate) fn clear(&mut self) {
        self.mailbox = None;
        self.mails.clear();
        self.bodies.clear();
        self.pending_bodies.clear();
        self.show_requested = false;
        self.last_list_query = None;
        self.send_acks.clear();
        self.errors.clear();
    }

    /// Disconnect: drop the open window (mirrors the merchant/gossip session clears).
    pub(crate) fn clear_session(&mut self) {
        self.clear();
    }
}

/// The mailbox window is an NPC-less interaction session: the standardized range guard
/// ([`crate::ui_session`]) client-side-closes it — the exact `CloseMail` clear — when the player
/// leaves the mailbox's interaction range (wow-re §5: the client's generic interaction-target range
/// gate, the same one the mail `CMSG`s' server-side `CheckMailBox` enforces at 5 yd).
impl NpcSession for MailOpen {
    fn npc(&self) -> Option<u64> {
        self.mailbox
    }

    fn close(&mut self) {
        self.clear();
    }
}

pub(crate) struct UiMailPlugin;

impl Plugin for UiMailPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MailOpen>()
            .init_resource::<MailPending>()
            .add_systems(
                Update,
                (
                    // Range-close before the feed so the clear turns into MAIL_CLOSED the same
                    // frame; feed before the input pass so an open/close is on screen the same
                    // frame; drain after it so a click's intent (CheckInbox, a take, a send) goes
                    // out the same frame (the ui_merchant ordering exactly). After the UnitFeed set
                    // so the SetInboxItem tooltip reads a landed item-template store.
                    close_npc_session_out_of_range::<MailOpen>.before(feed_mail),
                    feed_mail.after(crate::ui_unit::UnitFeed).before(UiInput),
                    drain_mail.after(UiInput),
                    // The world-enter one-shot (decision 0548 §7: "once at UI/login load") — its
                    // own system, ordering-independent of the feed/drain pair above.
                    send_query_next_mail_time_on_enter,
                ),
            );
    }
}

/// `MSG_QUERY_NEXT_MAIL_TIME`, sent once per **world-enter** — VERIFIED faithful (decision 0913):
/// the query's two callers are the mail module's init tail and the close-with-pending path, and
/// that init hangs off the world-enter cascade `0x4908c0` (`PLAYER_ENTERING_WORLD` + 37 subsystem
/// inits), whose guard is re-armed by the paired teardown — so it runs per world-enter (login,
/// worldport, instance transfer, `ReloadUI`), not once per process as an earlier note had it.
///
/// The init stamps the countdown to [`MAIL_TIME_NO_MAIL`] before asking, which is why the reference
/// icon blinks off across a loading screen and comes back with the reply.
fn send_query_next_mail_time_on_enter(
    mut entered: MessageReader<EnteredWorldMessage>,
    commands: Res<NetCommands>,
    mut pending: ResMut<MailPending>,
) {
    if entered.read().next().is_some() {
        pending.on_query_sent();
        let _ = commands.0.send(ClientCommand::QueryNextMailTime);
    }
}

/// The client's message string for a `SMSG_SEND_MAIL_RESULT` error (vmangos `MailResponseResult`);
/// texts quoted verbatim from the reference `GlobalStrings.lua` where one exists (decision 0544).
/// `NOT_YOUR_TEAM` has no 1.12 GlobalString — a best-effort English literal stands in (flagged).
pub(crate) fn mail_error_text(error: u32) -> String {
    match error {
        mail_error::CANNOT_SEND_TO_SELF => "You can't send mail to yourself.".into(), // ERR_MAIL_TO_SELF
        mail_error::NOT_ENOUGH_MONEY => "You don't have enough money.".into(), // ERR_NOT_ENOUGH_MONEY
        mail_error::RECIPIENT_NOT_FOUND => "Cannot find mail recipient.".into(), // ERR_MAIL_TARGET_NOT_FOUND
        mail_error::NOT_YOUR_TEAM => "That player is not part of your alliance.".into(), // no 1.12 GlobalString
        mail_error::INTERNAL_ERROR => "Internal mail database error.".into(), // ERR_MAIL_DATABASE_ERROR
        mail_error::TRIAL_ACCOUNT => "Trial accounts cannot perform that action.".into(),
        mail_error::TOO_MANY_ATTACHMENTS => {
            "You have reached the in-game cap of unique mail recipients".into() // ERR_MAIL_REACHED_CAP
        }
        other => format!("Mail failed ({other})."),
    }
}

mod invoice;

use invoice::auction_mail;

/// Resolve one wire [`MailListEntry`] into the Lua-facing [`MailInboxRow`]: sender through the ask-
/// once name cache (player guids only), the enclosed item's name/quality/icon through the ask-once
/// item-template cache + `ItemDisplayInfo.dbc`, the body from the cache, the stationery basename
/// from `Stationery.dbc`. `None`s stay `None` while a query is in flight — the row shows a
/// placeholder and fills in when the answer lands (the merchant/loot pattern).
#[allow(clippy::too_many_arguments)] // one resolve per ask-once cache the row reads
fn resolve_row(
    entry: &MailListEntry,
    bodies: &HashMap<u32, String>,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    stationery: Option<&Stationery>,
    names: &mut NameCache,
    commands: &NetCommands,
    rolls: crate::items::RollCatalogs,
    macros: &crate::npc_text::MacroContext,
    // The VM's own GlobalStrings, for the auction notice's subject template. A resolver rather than
    // a table because the strings are the player's install's, and Rust must never carry a copy of
    // them (decision 0669's shape).
    get_text: &dyn Fn(&str) -> Option<String>,
) -> MailInboxRow {
    let is_gm = entry.stationery == STATIONERY_GM;
    let returned = entry.checked & CHECKED_RETURNED != 0;
    // Reply is allowed only to a not-yet-returned mail from a player (MailFrame.lua l.281).
    let can_reply = entry.sender_guid.is_some() && !returned;
    // Delete-vs-return (INTERIM, decision 0548): a mail RETURNS on delete/expiry only when it still
    // carries attachments (item or money) from a not-yet-returned player sender — that is the
    // server's own expiry behavior (vmangos `ReturnOrDeleteOldMails`). Everything else — plain
    // letters, system mail, already-returned mail — deletes. The exact client predicate behind
    // `InboxItemCanDelete` is un-RE'd; this mirrors the observable law.
    let can_delete =
        returned || entry.sender_guid.is_none() || (entry.item.is_none() && entry.money == 0);

    // The enclosed item, resolved ask-once from its template.
    let (item_id, item_roll, item_count, item_name, item_texture, item_quality) = match &entry.item
    {
        Some(att) => {
            let template = items.template(att.entry, 0, commands);
            // The rolled name (1547) — the same join the loot window and the auction rows make.
            let name = template.map(|t| rolls.name(&t.name, att.random_prop_id));
            let quality = template.map(|t| t.quality);
            let display_id = template.map(|t| t.display_info_id).unwrap_or(0);
            let texture = icons
                .and_then(|i| i.catalog.get(display_id))
                .and_then(|d| d.icon.clone());
            (
                att.entry,
                att.random_prop_id,
                u32::from(att.count),
                name,
                texture,
                quality,
            )
        }
        None => (0, 0, 0, None, None, None),
    };

    let sender = entry
        .sender_guid
        .and_then(|g| names.resolve(g, commands).map(str::to_string));

    let stationery_texture = stationery
        .map(|s| s.0.texture(entry.stationery).to_string())
        .unwrap_or_else(|| benilla_formats::StationeryCatalog::DEFAULT_TEXTURE.to_string());

    // The letter body, raw. Hoisted because the auction invoice parses THIS and not the
    // `$`-substituted text below: the invoice is machine-written colon fields, and running a macro
    // expander over them would be expanding the auction house's own bookkeeping.
    let raw_body = (entry.item_text_id != 0)
        .then(|| bodies.get(&entry.item_text_id))
        .flatten();

    // ── The auction house's mail (`ui_mail::invoice`, wow-re §11.1a) ─────────────────────────
    // Two independent resolves off one subject: the DISPLAYED subject, which every auction notice
    // gets, and the INVOICE, which only a won/sold notice can answer and only once its body has
    // been fetched.
    let auction = (entry.message_type == mail_message_type::AUCTION)
        .then(|| invoice::parse_subject(&entry.subject))
        .flatten();
    // The item the notice is ABOUT — resolved from the subject's own entry, not from the
    // attachment: a "sold" notice encloses the money, and names an item it does not carry.
    let notice_item_name = auction
        .and_then(|a| items.template(a.entry, 0, commands))
        .map(|t| t.name.clone());
    // `<key>: %s` + the name. Until the item template lands the raw triplet stands — a subject that
    // reads `4428:0:1` for one frame is better than one that reads `Auction won: ` forever if the
    // query never answers, and the row re-resolves (and fires MAIL_INBOX_UPDATE) when it does.
    let subject = match (auction, &notice_item_name) {
        (Some(a), Some(name)) => invoice::subject_key(a.result)
            .and_then(get_text)
            .map(|t| t.replace("%s", name))
            .unwrap_or_else(|| entry.subject.clone()),
        _ => entry.subject.clone(),
    };
    let auction_invoice = auction
        .filter(|a| matches!(a.result, auction_mail::WON | auction_mail::SOLD))
        .and_then(|a| {
            let seller = a.result == auction_mail::SOLD;
            let n = invoice::parse_body(raw_body?, seller)?;
            // An unresolved counterparty takes the miss tail this pass and fills in when the name
            // query lands — the reference does exactly this, and fires MAIL_INBOX_UPDATE on the
            // late arrival, which our snapshot diff does for free.
            let player_name = names.resolve(n.player_guid, commands)?.to_string();
            Some(benilla_ui::script::MailInvoice {
                seller,
                item_name: notice_item_name.clone()?,
                player_name,
                bid: n.bid,
                buyout: n.buyout,
                deposit: n.deposit,
                consignment: n.consignment,
            })
        });

    MailInboxRow {
        package_icon: item_texture.clone(),
        stationery_icon: Some(
            if is_gm {
                STATIONERY_ICON_GM
            } else {
                STATIONERY_ICON_NORMAL
            }
            .to_string(),
        ),
        sender,
        subject,
        money: entry.money,
        cod: entry.cod,
        days_left: entry.expire_days,
        item_count,
        was_read: entry.checked & CHECKED_READ != 0,
        was_returned: returned,
        text_created: entry.checked & CHECKED_COPIED != 0,
        can_reply,
        is_gm,
        // The letter body is server-authored text, so it runs the `$`-macro expander — the
        // reference does it from two sites in `GetInboxText` (decision 0754). Subject is the
        // local player, as it is for every panel seam.
        body: raw_body.map(|b| crate::npc_text::substitute(b, macros)),
        stationery_texture,
        // NOT "is an auction mail": the reference's `isInvoice` reads the fields its subject
        // parser persisted, and it persists them only for won(1)/sold(2) — so an outbid or expiry
        // notice answers false here (decision 1527). This is also what nils the body: the two
        // answers come off the same record state, which is exactly why MailFrame can lay the
        // invoice pane over the letter page.
        is_invoice: auction
            .is_some_and(|a| matches!(a.result, auction_mail::WON | auction_mail::SOLD)),
        invoice: auction_invoice,
        has_body: entry.item_text_id != 0,
        item_id,
        item_random_property_id: item_roll,
        item_name,
        item_texture,
        item_quality,
        can_delete,
    }
}

/// Build the Lua-facing snapshot from [`MailOpen`] — `None` when no mailbox is open.
#[allow(clippy::too_many_arguments)] // mirrors `resolve_row`'s cache set
fn snapshot(
    mail: &MailOpen,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    stationery: Option<&Stationery>,
    names: &mut NameCache,
    commands: &NetCommands,
    rolls: crate::items::RollCatalogs,
    macros: &crate::npc_text::MacroContext,
    get_text: &dyn Fn(&str) -> Option<String>,
) -> Option<MailState> {
    mail.mailbox?;
    Some(MailState {
        inbox: mail
            .mails
            .iter()
            .map(|e| {
                resolve_row(
                    e,
                    &mail.bodies,
                    items,
                    icons,
                    stationery,
                    names,
                    commands,
                    rolls,
                    macros,
                    get_text,
                )
            })
            .collect(),
    })
}

/// Push the current mail into the VM and fire the show/update/close + send-result events on a
/// transition or content change (an async sender/item/body landing, a mark-read flip, a fresh
/// list). Diffed against a `Local` memory, exactly like the merchant/gossip feeds.
#[allow(clippy::too_many_arguments)]
fn feed_mail(
    script: Option<NonSendMut<UiScript>>,
    mut mail: ResMut<MailOpen>,
    mut items: ResMut<Items>,
    icons: Option<Res<ItemDisplays>>,
    stationery: Option<Res<Stationery>>,
    mut names: ResMut<NameCache>,
    commands: Res<NetCommands>,
    mut pending: ResMut<MailPending>,
    time: Res<Time>,
    mut last: Local<crate::ui_script::VmMemo<Option<MailState>>>,
    mut last_mailbox: Local<crate::ui_script::VmMemo<Option<u64>>>,
    mut last_has_new_mail: Local<crate::ui_script::VmMemo<bool>>,
    // The `$`-macro subject for the letter body: the local player, as at every panel seam.
    self_q: Query<(&crate::net::ObjectStore, &crate::net::Guid), With<crate::net::SelfPlayer>>,
    states: Res<crate::world_state::WorldStates>,
    // The random-suffix roll's catalogs (1547): an enclosed item's rolled name.
    props: Option<Res<crate::items::RandomProperties>>,
    enchants: Option<Res<crate::items::Enchants>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let rolls = crate::items::RollCatalogs {
        props: props.as_deref(),
        enchants: enchants.as_deref(),
    };
    let last = last.get(&script);
    let last_mailbox = last_mailbox.get(&script);
    let last_has_new_mail = last_has_new_mail.get(&script);

    // Take/return/delete failures surface as the client's red error line (the equip/cast path shape).
    for text in std::mem::take(&mut mail.errors) {
        script.fire_event("UI_ERROR_MESSAGE", vec![ScriptValue::Str(text)]);
    }
    // SEND acks: MAIL_FAILED on EVERY send result (unblocks the button — wow-re §5), plus
    // MAIL_SEND_SUCCESS on OK (resets the form) or the red error line on a failure.
    for ack in std::mem::take(&mut mail.send_acks) {
        script.fire_event("MAIL_FAILED", vec![]);
        if ack.ok {
            script.fire_event("MAIL_SEND_SUCCESS", vec![]);
            script.clear_send_mail_item();
        } else if let Some(text) = ack.error_text {
            script.fire_event("UI_ERROR_MESSAGE", vec![ScriptValue::Str(text)]);
        }
    }

    // The macro subject, resolved before the row walk borrows the name cache again. `None` until
    // the player is streamed and named — the feed diffs on the substituted text, so a letter opened
    // that early re-substitutes as soon as it lands.
    let subject = crate::npc_text::player_identity(&self_q, &mut names, &commands);
    let macros = crate::npc_text::MacroContext {
        subject: subject.as_ref(),
        states: &states,
    };
    let fresh = {
        // Scoped: the resolver borrows the VM, and `set_mail` below needs it back.
        let get_text = |key: &str| script.lua().globals().get::<String>(key).ok();
        snapshot(
            &mail,
            &mut items,
            icons.as_deref(),
            stationery.as_deref(),
            &mut names,
            &commands,
            rolls,
            &macros,
            &get_text,
        )
    };
    let opened = last_mailbox.is_none() && mail.mailbox.is_some();
    let closed = last_mailbox.is_some() && mail.mailbox.is_none();
    let show_requested = std::mem::take(&mut mail.show_requested);
    let changed = fresh != *last;
    if changed {
        script.set_mail(fresh.clone());
    }
    if opened {
        script.fire_event("MAIL_SHOW", vec![]);
        // The compose tab resets on open — a fresh window carries no stale attachment/money.
        script.clear_send_mail_item();
        // Byte-verified open side-effect (wow-re §5): the client's compose-tab reset fires
        // MAIL_SEND_SUCCESS on open too, so the page-turn sound on open is faithful, not a bug.
        script.fire_event("MAIL_SEND_SUCCESS", vec![]);
    } else if closed {
        script.fire_event("MAIL_CLOSED", vec![]);
        // The close core's tail (wow-re `0x4acdad`/`0x4acdb1`, decision 0913): a session where a
        // mail was read — or one arrived while the window was open — re-asks the server, and the
        // sender stamps the countdown to "no mail" on the way out. This is the whole mechanism by
        // which checking your mail clears the minimap icon; nothing on the inbox path touches it.
        // Both close paths converge here (explicit `CloseMail` and the walk-away range close), as
        // they do in the reference through the same generic dispatcher.
        if pending.take_refresh() {
            pending.on_query_sent();
            let _ = commands.0.send(ClientCommand::QueryNextMailTime);
        }
    } else if mail.mailbox.is_some() {
        // A re-click re-shows the (already open) window; MAIL_SHOW's CheckInbox refreshes the list.
        if show_requested {
            script.fire_event("MAIL_SHOW", vec![]);
        }
        // A content change (async name/body landed, list replaced, mark-read flip) → repaint.
        if changed {
            script.fire_event("MAIL_INBOX_UPDATE", vec![]);
        }
    }
    *last = fresh;
    *last_mailbox = mail.mailbox;

    // The arrival layer (decisions 0544 P3 / 0904 / 0913): step the client-local countdown, keep
    // the VM's `HasNewMail()` answer current, and fire `UPDATE_PENDING_MAIL` only where the
    // reference fires it. The two are deliberately separate: the *push* keeps the Lua getter
    // truthful whenever it is asked, while the *event* is edge-triggered at three sites only (the
    // query reply, a near-zero arrival, the step crossing ε) — so, exactly as in the reference, the
    // query sender's "no mail" stamp does not move the icon until the reply lands.
    // `MiniMapMailFrame` is the sole listener in all of 1.12 FrameXML and its handler is an
    // idempotent Show/Hide (`Minimap.xml` l.278-289, quoted in the module doc).
    pending.step(time.delta_secs());
    let has_new_mail = pending.has_new_mail();
    if has_new_mail != *last_has_new_mail {
        script.set_has_new_mail(has_new_mail);
        *last_has_new_mail = has_new_mail;
    }
    if pending.take_notify() {
        script.fire_event("UPDATE_PENDING_MAIL", vec![]);
    }
}

/// Drain the Lua intents into the mail `CMSG`s (decision 0544): `CheckInbox` → a throttled
/// `CMSG_GET_MAIL_LIST`; an opened mail → `CMSG_MAIL_MARK_AS_READ` (once, unread) + the ask-once
/// `CMSG_ITEM_TEXT_QUERY` for its body; take/return/delete → their per-mail `CMSG`s; `SendMail` →
/// `CMSG_SEND_MAIL` (the attachment's `(bag, slot)` resolved to its item guid here); `CloseMail` →
/// a local clear (no packet, vanilla).
fn drain_mail(
    script: Option<NonSendMut<UiScript>>,
    mut mail: ResMut<MailOpen>,
    commands: Res<NetCommands>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    items: Res<Items>,
    time: Res<Time>,
    mut pending: ResMut<MailPending>,
) {
    let Some(mut script) = script else {
        return;
    };
    let Some(mailbox) = mail.mailbox else {
        // No session: still honor a stray close (idempotent) and drop everything else.
        if script.take_mail_close() {
            mail.clear();
        }
        let _ = script.take_mail_check_inbox();
        let _ = script.take_mail_opens();
        let _ = script.take_mail_send();
        return;
    };

    // CheckInbox → a session-scoped, 60s-throttled inbox query (wow-re §5).
    if script.take_mail_check_inbox() {
        let now = time.elapsed_secs_f64();
        if mail
            .last_list_query
            .is_none_or(|t| now - t >= CHECK_INBOX_THROTTLE_SECS)
        {
            mail.last_list_query = Some(now);
            let _ = commands.0.send(ClientCommand::GetMailList { mailbox });
        }
    }

    // Opened mails: mark read (once) then ask-once the body (wow-re §5: read FIRST, body second).
    for index in script.take_mail_opens() {
        let Some((mail_id, text_id, read)) = index
            .checked_sub(1)
            .and_then(|i| mail.mails.get(i as usize))
            .map(|e| (e.message_id, e.item_text_id, e.checked & CHECKED_READ != 0))
        else {
            continue;
        };
        // Arm the deferred refresh on **every** open, read or not: the reference's `GetInboxText`
        // calls the mark-as-read sender unconditionally, and the `[0xb6efcc] = 1` inside it is
        // unconditional too (`0x4adda6`) — so opening any letter is what makes the close re-ask the
        // server, and that is what clears the minimap icon (decision 0913). Arming it on an
        // already-read mail costs at most one redundant query on close; failing to arm it would
        // strand the icon lit, so the eager side is the safe side.
        pending.arm_refresh();
        if !read {
            mail.mark_read(index);
            let _ = commands
                .0
                .send(ClientCommand::MailMarkAsRead { mailbox, mail_id });
        }
        if text_id != 0
            && !mail.bodies.contains_key(&text_id)
            && mail.pending_bodies.insert(text_id)
        {
            let _ = commands
                .0
                .send(ClientCommand::ItemTextQuery { text_id, mail_id });
        }
    }

    // The per-mail row picks addressing the wire mail id.
    for index in script.take_mail_take_items() {
        if let Some(mail_id) = mail.mail_id_at(index) {
            let _ = commands
                .0
                .send(ClientCommand::MailTakeItem { mailbox, mail_id });
        }
    }
    for index in script.take_mail_take_money() {
        if let Some(mail_id) = mail.mail_id_at(index) {
            let _ = commands
                .0
                .send(ClientCommand::MailTakeMoney { mailbox, mail_id });
        }
    }
    for index in script.take_mail_deletes() {
        if let Some(mail_id) = mail.mail_id_at(index) {
            let _ = commands
                .0
                .send(ClientCommand::MailDelete { mailbox, mail_id });
        }
    }
    for index in script.take_mail_returns() {
        if let Some(mail_id) = mail.mail_id_at(index) {
            let _ = commands
                .0
                .send(ClientCommand::MailReturnToSender { mailbox, mail_id });
        }
    }
    for index in script.take_mail_take_texts() {
        if let Some(mail_id) = mail.mail_id_at(index) {
            let _ = commands
                .0
                .send(ClientCommand::MailCreateTextItem { mailbox, mail_id });
        }
    }

    // SendMail: resolve the attachment's (bag, slot) to the wire item guid at send time (the
    // reference re-reads the slot when the send fires — a lazy resolve, decision 0216/0544).
    if let Some(req) = script.take_mail_send() {
        let item_guid = req
            .item
            .and_then(|(bag, slot)| {
                self_q.iter().next().and_then(|s| {
                    crate::ui_items::slot_guid(&s.0, bag, (slot.max(1) - 1) as u8, &items)
                })
            })
            .unwrap_or(0);
        let _ = commands.0.send(ClientCommand::SendMail {
            mailbox,
            receiver: req.target,
            subject: req.subject,
            body: req.body,
            item_guid,
            money: req.money,
            cod: req.cod,
        });
    }

    if script.take_mail_close() {
        mail.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_protocol::messages::MailAttachment;

    /// The no-subject macro context these row tests run under: none of them exercises a `$` token,
    /// and a subject-less context leaves plain text untouched (the expander only rewrites at a `$`).
    /// The macro path's own behaviour is covered in [`crate::npc_text`]'s tests.
    fn no_macros(states: &crate::world_state::WorldStates) -> crate::npc_text::MacroContext<'_> {
        crate::npc_text::MacroContext {
            subject: None,
            states,
        }
    }

    fn entry(
        message_id: u32,
        sender_guid: Option<u64>,
        checked: u32,
        expire_days: f32,
    ) -> MailListEntry {
        MailListEntry {
            message_id,
            message_type: mail_message_type::NORMAL,
            sender_guid,
            sender_id: None,
            subject: "s".into(),
            item_text_id: 0,
            stationery: 41,
            item: None,
            money: 0,
            cod: 0,
            checked,
            expire_days,
            mail_template_id: 0,
        }
    }

    #[test]
    fn click_opens_and_reclick_keeps_rows() {
        let mut m = MailOpen::default();
        m.click(0x100);
        assert_eq!(m.mailbox, Some(0x100));
        assert!(m.show_requested);
        m.mails.push(entry(1, Some(0xA), 0, 30.0));
        m.last_list_query = Some(5.0);
        m.show_requested = false;
        // Re-click the SAME mailbox keeps the rows + throttle, just re-requests the show.
        m.click(0x100);
        assert_eq!(m.mails.len(), 1);
        assert_eq!(m.last_list_query, Some(5.0));
        assert!(m.show_requested);
        // A DIFFERENT mailbox resets the session.
        m.click(0x200);
        assert!(m.mails.is_empty());
        assert_eq!(m.last_list_query, None);
    }

    #[test]
    fn mail_id_and_mark_read() {
        let mut m = MailOpen::default();
        m.click(0x1);
        m.mails.push(entry(42, Some(0xA), 0, 30.0));
        m.mails.push(entry(43, None, CHECKED_READ, 10.0));
        assert_eq!(m.mail_id_at(1), Some(42));
        assert_eq!(m.mail_id_at(2), Some(43));
        assert_eq!(m.mail_id_at(3), None);
        assert_eq!(m.mail_id_at(0), None);
        // Mark row 1 read: its READ bit flips locally.
        assert_eq!(m.mails[0].checked & CHECKED_READ, 0);
        m.mark_read(1);
        assert_eq!(m.mails[0].checked & CHECKED_READ, CHECKED_READ);
    }

    #[test]
    fn clear_closes_the_window() {
        let mut m = MailOpen::default();
        m.click(0x1);
        m.mails.push(entry(1, Some(0xA), 0, 30.0));
        m.bodies.insert(7, "hi".into());
        m.clear();
        assert!(m.mailbox.is_none());
        assert!(m.mails.is_empty());
        assert!(m.bodies.is_empty());
    }

    /// No GlobalStrings in a unit test — the auction-subject rewrite is exercised where the
    /// strings are (`tests/mail_frame.rs`), and here every key simply misses.
    fn no_strings(_key: &str) -> Option<String> {
        None
    }

    #[test]
    fn resolve_row_derives_the_reply_and_delete_law() {
        let states = crate::world_state::WorldStates::default();
        let mut items = Items::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);
        let mut names = NameCache::default();
        let bodies = HashMap::new();

        // A plain letter from a player (no attachments) → replyable AND deletable.
        let from_player = entry(1, Some(0xA), 0, 30.0);
        let row = resolve_row(
            &from_player,
            &bodies,
            &mut items,
            None,
            None,
            &mut names,
            &commands,
            crate::items::RollCatalogs::NONE,
            &no_macros(&states),
            &no_strings,
        );
        assert!(row.can_reply);
        assert!(row.can_delete);
        assert_eq!(row.stationery_texture, "STATIONERYTEST");

        // A player mail still carrying money → returnable, NOT deletable (the 0548 INTERIM law:
        // attachments return; the same holds for an enclosed item).
        let mut with_money = entry(4, Some(0xA), 0, 30.0);
        with_money.money = 500;
        let row = resolve_row(
            &with_money,
            &bodies,
            &mut items,
            None,
            None,
            &mut names,
            &commands,
            crate::items::RollCatalogs::NONE,
            &no_macros(&states),
            &no_strings,
        );
        assert!(row.can_reply);
        assert!(!row.can_delete);

        // A returned mail → not replyable, deletable.
        let returned = entry(2, Some(0xA), CHECKED_RETURNED, 30.0);
        let row = resolve_row(
            &returned,
            &bodies,
            &mut items,
            None,
            None,
            &mut names,
            &commands,
            crate::items::RollCatalogs::NONE,
            &no_macros(&states),
            &no_strings,
        );
        assert!(!row.can_reply);
        assert!(row.can_delete);

        // A system mail (no sender guid) → deletable.
        let system = entry(3, None, 0, 30.0);
        let row = resolve_row(
            &system,
            &bodies,
            &mut items,
            None,
            None,
            &mut names,
            &commands,
            crate::items::RollCatalogs::NONE,
            &no_macros(&states),
            &no_strings,
        );
        assert!(row.can_delete);
    }

    #[test]
    fn resolve_row_reads_the_item_and_body_caches() {
        let states = crate::world_state::WorldStates::default();
        let mut items = Items::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);
        let mut names = NameCache::default();
        let mut bodies = HashMap::new();
        bodies.insert(99, "the letter body".into());

        let mut e = entry(1, Some(0xA), 0, 30.0);
        e.item_text_id = 99;
        e.item = Some(MailAttachment {
            entry: 2589,
            perm_enchant: 0,
            random_prop_id: 0,
            suffix_factor: 0,
            count: 5,
            charges: 0,
            durability_max: 0,
            durability: 0,
        });
        let row = resolve_row(
            &e,
            &bodies,
            &mut items,
            None,
            None,
            &mut names,
            &commands,
            crate::items::RollCatalogs::NONE,
            &no_macros(&states),
            &no_strings,
        );
        assert_eq!(row.item_id, 2589);
        assert_eq!(row.item_count, 5);
        assert_eq!(row.body.as_deref(), Some("the letter body"));
        assert!(row.has_body);
        // The item name/texture are in flight (no template answer yet).
        assert!(row.item_name.is_none());
    }
}
