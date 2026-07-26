//! The mail bindings (decision 0544 P1/P2) — the Era-shaped mailbox surface, the same two-way seam
//! as [`super::merchant`]/[`super::loot`]: the app pushes a **mail snapshot**
//! ([`UiScript::set_mail`] — the inbox rows already resolved from the wire to
//! name/icon/sender/body by the app's item-template + name caches) and the Lua
//! `CheckInbox`/`TakeInboxItem`/`SendMail`/… calls queue outbound **intents** the app drains. The
//! engine holds no mail knowledge — a row is a header (sender/subject/money/COD/expiry/flags), an
//! optional enclosed item (id/name/icon/quality), an ask-once letter body, and a stationery
//! basename.
//!
//! ## The 5875 API shape (VERIFIED against the extracted `MailFrame.lua`)
//!
//! `index` is **1-based** everywhere; an out-of-range index answers `nil`/`0`. The inbox getters:
//! `GetInboxNumItems()`, `GetInboxHeaderInfo(index)` → the reference 13-tuple
//! `packageIcon, stationeryIcon, sender, subject, money, CODAmount, daysLeft, hasItem(count|nil),
//! wasRead, wasReturned, textCreated, canReply, isGM` (MailFrame.lua l.105/265/279),
//! `GetInboxText(index)` → `body, stationeryTexture, isTakeable, isInvoice` (l.292 — a body cache
//! miss queues the `CMSG_ITEM_TEXT_QUERY` fetch and returns `""`), `GetInboxItem(index)` →
//! `name, texture, count, quality, canUse` (l.379), `InboxItemCanDelete(index)` → 1/nil (the
//! delete-vs-return law, l.417/450). `HasNewMail()` reads the app-pushed login-scoped flag
//! ([`UiScript::set_has_new_mail`], decision 0544 P3) — `nil` (false) until the app's first
//! `MSG_QUERY_NEXT_MAIL_TIME` answer lands.
//!
//! The send tab: `GetSendMailItem()` → `name, texture, stackCount, quality` off the attached cursor
//! item (l.511), `GetSendMailPrice()` → the flat **30c** postage (vmangos's fee), `SendMail(target,
//! subject, body)` fires the send, `SetSendMailMoney`/`SetSendMailCOD(copper)` push the money/COD
//! amounts the app reads at send time, `ClickSendMailItemButton()` attaches (or detaches) the
//! cursor's held bag item as the send attachment (l.719). Intents `CheckInbox`, `TakeInboxItem`,
//! `TakeInboxMoney`, `DeleteInboxItem`, `ReturnInboxItem`, `CloseMail` are 1-based row indices /
//! flags the app maps to the wire.

use mlua::{Lua, MultiValue, Value};

use super::cursor::{self, CursorPayload};
use super::Model;

/// One inbox row, resolved by the app from a wire `MailListEntry` (decision 0544). Plain data — its
/// 1-based order in the window is its position in [`MailState::inbox`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MailInboxRow {
    /// The enclosed item's icon (`GetInboxHeaderInfo`'s `packageIcon`); `None` when the mail carries
    /// no item, or while its template answer is in flight.
    pub package_icon: Option<String>,
    /// The letter icon (`stationeryIcon`) — the app derives it from the stationery id/GM flag; the
    /// row shows this when there's no package (or it's GM mail).
    pub stationery_icon: Option<String>,
    /// The sender name (`sender`), resolved through the name cache; `None` while in flight or for a
    /// non-player sender (the Lua shows `UNKNOWN`).
    pub sender: Option<String>,
    pub subject: String,
    pub money: u32,
    pub cod: u32,
    /// Days remaining before the mail is deleted/returned (`daysLeft`, the wire `expire_days`).
    pub days_left: f32,
    /// The enclosed item's stack count (`GetInboxHeaderInfo`'s `hasItem` returns this, or `nil` when
    /// `0`). `0` = no item.
    pub item_count: u32,
    /// `checked & READ(0x1)`.
    pub was_read: bool,
    /// `checked & RETURNED(0x2)`.
    pub was_returned: bool,
    /// `checked & COPIED(0x4)` — the letter body was materialized as a text item (`textCreated`).
    pub text_created: bool,
    /// `sender_guid.is_some() && !was_returned` — the Reply button gate (l.281).
    pub can_reply: bool,
    /// `stationery == 61` — a GM mail (l.108).
    pub is_gm: bool,
    /// The letter body (`GetInboxText`'s first return); `None` = not fetched yet (the getter returns
    /// `""` and queues the ask-once `CMSG_ITEM_TEXT_QUERY` when [`Self::has_body`]).
    pub body: Option<String>,
    /// The stationery texture basename (`GetInboxText`'s second return) — the Lua wraps it as
    /// `Interface\Stationery\<basename>1`/`2` for the open-letter backdrop.
    pub stationery_texture: String,
    /// `message_type == AUCTION(2)` — `GetInboxText`'s `isInvoice`.
    pub is_invoice: bool,
    /// The mail carries a fetchable letter body (`item_text_id != 0`) — gates the body ask.
    pub has_body: bool,
    /// The enclosed item's template entry (`0` = none) — the shared item-tooltip store's key
    /// (`GameTooltip:SetInboxItem`) and `GetInboxItem`'s identity.
    pub item_id: u32,
    /// The enclosed item's name (`GetInboxItem`'s first return); `None` while the template is in
    /// flight.
    pub item_name: Option<String>,
    /// The enclosed item's icon; `None` while in flight.
    pub item_texture: Option<String>,
    /// The enclosed item's quality (0..6); `None` while in flight.
    pub item_quality: Option<u32>,
    /// `InboxItemCanDelete` — the delete-vs-return law (`!can_reply`: a not-yet-returned mail from a
    /// player is returnable, everything else is deletable — l.417/450).
    pub can_delete: bool,
}

/// The open mailbox's inbox snapshot. Pushed whole by the app; `None` means no mailbox is open (the
/// window is closed).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MailState {
    pub inbox: Vec<MailInboxRow>,
}

/// A drained `SendMail` intent — the app resolves the attachment `(bag, slot)` to an item guid and
/// builds `CMSG_SEND_MAIL` from this (decision 0544 P2).
#[derive(Clone, Debug, PartialEq)]
pub struct MailSendRequest {
    pub target: String,
    pub subject: String,
    pub body: String,
    /// Money to enclose (copper) — `SetSendMailMoney`'s last value.
    pub money: u32,
    /// COD amount (copper) — `SetSendMailCOD`'s last value; `0` when not a COD send.
    pub cod: u32,
    /// The attached bag item's `(live-API bag, 1-based slot)`; `None` = no attachment. The app
    /// resolves it to the wire's `u64 itemGuid` at send time (the reference re-reads the bag slot
    /// when the send fires, so the resolve is lazy).
    pub item: Option<(i64, u32)>,
}

impl super::UiScript {
    /// Push (or clear, with `None`) the open mailbox's inbox snapshot.
    pub fn set_mail(&mut self, state: Option<MailState>) {
        self.model_mut().mail = state;
    }

    /// Whether `CheckInbox` was called since the last drain (and clear the flag) — the app maps it to
    /// a fresh `CMSG_GET_MAIL_LIST`.
    pub fn take_mail_check_inbox(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().mail_check_inbox)
    }

    /// Drain the 1-based indices of mails **opened** (`GetInboxText` was called on them) since the
    /// last drain — the app marks each read and fetches its body ask-once.
    pub fn take_mail_opens(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().mail_opens)
    }

    /// Drain the 1-based `TakeInboxItem` row picks.
    pub fn take_mail_take_items(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().mail_take_items)
    }

    /// Drain the 1-based `TakeInboxMoney` row picks.
    pub fn take_mail_take_money(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().mail_take_money)
    }

    /// Drain the 1-based `DeleteInboxItem` row picks.
    pub fn take_mail_deletes(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().mail_deletes)
    }

    /// Drain the 1-based `ReturnInboxItem` row picks.
    pub fn take_mail_returns(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().mail_returns)
    }

    /// Drain the 1-based `TakeInboxTextItem` row picks — the letter button's "permanent copy"
    /// verb; the app maps each to `CMSG_MAIL_CREATE_TEXT_ITEM`.
    pub fn take_mail_take_texts(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().mail_take_texts)
    }

    /// Whether `CloseMail` was called since the last drain (and clear the flag). vanilla's
    /// client-side close sends no packet — the app clears its local mail session.
    pub fn take_mail_close(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().mail_close)
    }

    /// Drain the `SendMail` intent, if one was queued since the last drain — folds in the last
    /// `SetSendMailMoney`/`SetSendMailCOD` amounts and the attached item's `(bag, slot)`. The
    /// attachment is left in place (a failed send keeps it; [`Self::clear_send_mail_item`] drops it
    /// on success).
    pub fn take_mail_send(&mut self) -> Option<MailSendRequest> {
        let mut model = self.model_mut();
        let (target, subject, body) = model.mail_send.take()?;
        let item = model.mail_send_item.as_ref().map(|it| (it.bag, it.slot));
        Some(MailSendRequest {
            target,
            subject,
            body,
            money: model.mail_send_money,
            cod: model.mail_send_cod,
            item,
        })
    }

    /// Drop the send tab's attached item + money/COD amounts — the app calls this on
    /// `MAIL_SEND_SUCCESS` (the reference `SendMailFrame_Reset`, MailFrame.lua l.49/552).
    pub fn clear_send_mail_item(&mut self) {
        let mut model = self.model_mut();
        model.mail_send_item = None;
        model.mail_send_money = 0;
        model.mail_send_cod = 0;
    }

    /// Push `HasNewMail()`'s answer (decision 0544 P3) — login-scoped, independent of whether a
    /// mailbox window is open. The app computes this off its `MSG_QUERY_NEXT_MAIL_TIME`/
    /// `SMSG_RECEIVED_MAIL`-fed countdown and calls this on every value change, right before firing
    /// `UPDATE_PENDING_MAIL` (the reference `MiniMapMailFrame`'s only listened event, Minimap.xml
    /// l.278-289 — its `OnEvent` just re-reads `HasNewMail()` and shows/hides).
    pub fn set_has_new_mail(&mut self, has: bool) {
        self.model_mut().has_new_mail = has;
    }
}

/// A `1`/`nil` boolean the way the client pushes flags (`pushnumber(1)` / `pushnil`).
fn flag(b: bool) -> Value {
    if b {
        Value::Integer(1)
    } else {
        Value::Nil
    }
}

/// An enclosed item's `canUse`, the shared item-usable gate over the row's template from the
/// merchant/tooltip store, keyed by the item id — usable while the template is still in flight (the
/// null-record skip). Spell knowledge comes from the engine's spellbook mirror, as the tooltip's
/// "Requires <spell>" red does.
fn is_usable(model: &Model, item_id: u32) -> bool {
    item_id == 0
        || model.item_templates.get(&item_id).is_none_or(|v| {
            super::item_stats::item_usable(v, &model.player_req, |id| {
                model.spellbook.slots.iter().any(|s| s.spell_id == id)
            })
        })
}

/// Fetch a cloned inbox row for a 1-based index, or `None` (out of range / no mailbox open).
fn row_at(model: &Model, index: usize) -> Option<MailInboxRow> {
    model
        .mail
        .as_ref()
        .and_then(|m| index.checked_sub(1).and_then(|n| m.inbox.get(n)))
        .cloned()
}

/// Register the mail globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // GetInboxNumItems() → the number of inbox rows (0 when no mailbox is open).
    g.set(
        "GetInboxNumItems",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.mail.as_ref().map_or(0, |m| m.inbox.len()) as i64)
        })?,
    )?;

    // GetInboxHeaderInfo(index) → the 13-tuple (see the module doc). An out-of-range index answers
    // nil (the reference callers only read within GetInboxNumItems, but the null shape is faithful).
    g.set(
        "GetInboxHeaderInfo",
        lua.create_function(|lua, index: usize| {
            let row = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                row_at(&model, index)
            };
            let Some(r) = row else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let opt_str = |s: &Option<String>| -> mlua::Result<Value> {
                Ok(match s {
                    Some(v) => Value::String(lua.create_string(v)?),
                    None => Value::Nil,
                })
            };
            // packageIcon shows only for a non-GM mail that carries an item (MailFrame.lua l.108).
            let package_icon = if r.item_id != 0 && !r.is_gm {
                opt_str(&r.package_icon)?
            } else {
                Value::Nil
            };
            Ok(MultiValue::from_vec(vec![
                package_icon,
                opt_str(&r.stationery_icon)?,
                opt_str(&r.sender)?,
                Value::String(lua.create_string(&r.subject)?),
                Value::Integer(i64::from(r.money)),
                Value::Integer(i64::from(r.cod)),
                Value::Number(f64::from(r.days_left)),
                // hasItem: the stack count, or nil when there's no item.
                if r.item_count > 0 {
                    Value::Integer(i64::from(r.item_count))
                } else {
                    Value::Nil
                },
                flag(r.was_read),
                flag(r.was_returned),
                flag(r.text_created),
                flag(r.can_reply),
                flag(r.is_gm),
            ]))
        })?,
    )?;

    // GetInboxText(index) → body, stationeryTexture, isTakeable, isInvoice (MailFrame.lua l.292).
    // Opening a mail is the ask-once trigger: queue the open intent (the app marks it read + fetches
    // the body when has_body). A cache miss returns "" until the body lands and a fresh snapshot
    // arrives. isTakeable = the mail carries a fetchable body (`item_text_id != 0`) — with
    // `not textCreated` it gates the letter button (the "permanent copy" verb, ref l.366; vmangos
    // `HandleMailCreateTextItem` requires the same pair: an `itemTextId` and no COPIED bit).
    g.set(
        "GetInboxText",
        lua.create_function(|lua, index: usize| {
            let row = {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                let row = row_at(&model, index);
                if row.is_some() {
                    // Ask-once dedup: don't re-queue an already-pending open this drain window.
                    let i = index as u32;
                    if !model.mail_opens.contains(&i) {
                        model.mail_opens.push(i);
                    }
                }
                row
            };
            let Some(r) = row else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(r.body.as_deref().unwrap_or(""))?),
                Value::String(lua.create_string(&r.stationery_texture)?),
                flag(r.has_body),
                flag(r.is_invoice),
            ]))
        })?,
    )?;

    // GetInboxItem(index) → name, texture, count, quality, canUse (MailFrame.lua l.379). name/quality
    // are nil while the item-template answer is in flight; a mail with no item answers all-nil name.
    // canUse is the shared item-usable gate over the enclosed item's template (merchant's isUsable
    // path) — usable while the template is still in flight (the null-record skip).
    g.set(
        "GetInboxItem",
        lua.create_function(|lua, index: usize| {
            let (row, usable) = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                let row = row_at(&model, index);
                let usable = row.as_ref().is_none_or(|r| is_usable(&model, r.item_id));
                (row, usable)
            };
            let Some(r) = row.filter(|r| r.item_id != 0) else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let name = match &r.item_name {
                Some(n) => Value::String(lua.create_string(n)?),
                None => Value::Nil,
            };
            let texture = match &r.item_texture {
                Some(t) => Value::String(lua.create_string(t)?),
                None => Value::Nil,
            };
            let quality = match r.item_quality {
                Some(q) => Value::Integer(i64::from(q)),
                None => Value::Nil,
            };
            Ok(MultiValue::from_vec(vec![
                name,
                texture,
                Value::Integer(i64::from(r.item_count.max(1))),
                quality,
                flag(usable),
            ]))
        })?,
    )?;

    // InboxItemCanDelete(index) → 1/nil (MailFrame.lua l.417/450): the delete-vs-return law.
    g.set(
        "InboxItemCanDelete",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(flag(row_at(&model, index).is_some_and(|r| r.can_delete)))
        })?,
    )?;

    // HasNewMail() → 1/nil off the app's login-scoped countdown flag (decision 0544 P3; see
    // `UiScript::set_has_new_mail`'s doc for the wire behind it).
    g.set(
        "HasNewMail",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(flag(model.has_new_mail))
        })?,
    )?;

    // CheckInbox() — flag the inbox-refresh intent (the app sends CMSG_GET_MAIL_LIST).
    g.set(
        "CheckInbox",
        lua.create_function(|lua, ()| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .mail_check_inbox = true;
            Ok(())
        })?,
    )?;

    // The five 1-based row-pick intents (take item / take money / delete / return / copy letter).
    for (name, field) in [
        ("TakeInboxItem", 0u8),
        ("TakeInboxMoney", 1),
        ("DeleteInboxItem", 2),
        ("ReturnInboxItem", 3),
        ("TakeInboxTextItem", 4),
    ] {
        g.set(
            name,
            lua.create_function(move |lua, index: u32| {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                match field {
                    0 => model.mail_take_items.push(index),
                    1 => model.mail_take_money.push(index),
                    2 => model.mail_deletes.push(index),
                    3 => model.mail_returns.push(index),
                    _ => model.mail_take_texts.push(index),
                }
                Ok(())
            })?,
        )?;
    }

    // CloseMail() — client-side close (no packet): flag it so the app clears its mail session.
    g.set(
        "CloseMail",
        lua.create_function(|lua, ()| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .mail_close = true;
            Ok(())
        })?,
    )?;

    // SendMail(target, subject, body) — queue the send; the app folds in money/COD + the attachment.
    g.set(
        "SendMail",
        lua.create_function(|lua, (target, subject, body): (String, String, String)| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .mail_send = Some((target, subject, body));
            Ok(())
        })?,
    )?;

    // SetSendMailMoney(copper) → 1 (the SEND_MONEY popup's OnAccept gates SendMail on the truthy
    // return, StaticPopup.lua l.252). Stores the enclose-money amount the app reads at send time.
    g.set(
        "SetSendMailMoney",
        lua.create_function(|lua, copper: u32| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .mail_send_money = copper;
            Ok(Value::Integer(1))
        })?,
    )?;

    // SetSendMailCOD(copper) — store the COD amount the app reads at send time (MailFrame.lua l.497).
    g.set(
        "SetSendMailCOD",
        lua.create_function(|lua, copper: u32| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .mail_send_cod = copper;
            Ok(())
        })?,
    )?;

    // GetSendMailItem() → name, texture, stackCount, quality off the attached cursor item
    // (MailFrame.lua l.511). All-nil/1 when nothing is attached.
    g.set(
        "GetSendMailItem",
        lua.create_function(|lua, ()| {
            let item = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model.mail_send_item.clone()
            };
            let Some(it) = item else {
                return Ok(MultiValue::from_vec(vec![
                    Value::Nil,
                    Value::Nil,
                    Value::Integer(1),
                    Value::Nil,
                ]));
            };
            let name = cursor::item_link_name(it.link.as_deref());
            let name = if name.is_empty() {
                Value::Nil
            } else {
                Value::String(lua.create_string(&name)?)
            };
            let texture = match &it.texture {
                Some(t) => Value::String(lua.create_string(t)?),
                None => Value::Nil,
            };
            let quality = match it.quality {
                Some(q) => Value::Integer(i64::from(q)),
                None => Value::Nil,
            };
            Ok(MultiValue::from_vec(vec![
                name,
                texture,
                Value::Integer(i64::from(it.count.unwrap_or(1).max(1))),
                quality,
            ]))
        })?,
    )?;

    // GetSendMailPrice() → the flat 30c postage (vmangos's fee; the reference reads a C constant).
    g.set(
        "GetSendMailPrice",
        lua.create_function(|_, ()| Ok(Value::Integer(30)))?,
    )?;

    // ClickSendMailItemButton() — attach the cursor's held bag item as the send attachment, or, with
    // an empty cursor and a filled slot, pick the attachment back onto the cursor (MailFrame.lua
    // l.719, mirroring pickup_container_item's swap). A spell/action cursor is refused (no-op).
    g.set(
        "ClickSendMailItemButton",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            click_send_mail_item(&mut model);
            Ok(())
        })?,
    )?;

    Ok(())
}

/// The attach/detach swap of `ClickSendMailItemButton` — the mail send slot as a cursor drop
/// target (decision 0216's payload rails). Every `model.cursor` mutation is followed by
/// [`cursor::queue_cursor_update`]/[`cursor::queue_lock_changed`] so `CURSOR_UPDATE`/
/// `ITEM_LOCK_CHANGED` fire, exactly like a bag slot.
fn click_send_mail_item(model: &mut Model) {
    match model.cursor.take() {
        // Cursor holds a bag item → attach it (the item leaves nothing in the bag until send).
        Some(CursorPayload::Item(item)) => {
            let (bag, slot) = (item.bag, item.slot);
            // If a different item was already attached, it returns to nowhere client-side (the
            // reference simply overwrites); its slot was never locked, so nothing to unlock.
            model.mail_send_item = Some(item);
            cursor::queue_cursor_update(model);
            cursor::queue_lock_changed(model, bag, slot);
        }
        // Empty cursor, a filled slot → pick the attachment back onto the cursor (detach).
        None => {
            if let Some(item) = model.mail_send_item.take() {
                let (bag, slot) = (item.bag, item.slot);
                model.cursor = Some(CursorPayload::Item(item));
                cursor::queue_cursor_update(model);
                cursor::queue_lock_changed(model, bag, slot);
            }
        }
        // A spell/action payload is refused — put it back untouched (container.rs's refuse arm).
        Some(other) => {
            model.cursor = Some(other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::UiScript;

    fn row(item_id: u32) -> MailInboxRow {
        MailInboxRow {
            package_icon: (item_id != 0).then(|| "Interface\\Icons\\INV_Misc_Bag_08".to_string()),
            stationery_icon: Some("Interface\\Icons\\INV_Letter_15".to_string()),
            sender: Some("Thrall".into()),
            subject: "Warchief's orders".into(),
            money: 5000,
            cod: 0,
            days_left: 29.5,
            item_count: if item_id != 0 { 3 } else { 0 },
            was_read: false,
            was_returned: false,
            text_created: false,
            can_reply: true,
            is_gm: false,
            body: Some("Lok'tar.".into()),
            stationery_texture: "STATIONERYTEST".into(),
            is_invoice: false,
            has_body: true,
            item_id,
            item_name: (item_id != 0).then(|| "Linen Cloth".to_string()),
            item_texture: (item_id != 0)
                .then(|| "Interface\\Icons\\INV_Fabric_Linen_01".to_string()),
            item_quality: (item_id != 0).then_some(1),
            can_delete: false, // from a player, not returned → returnable (not deletable)
        }
    }

    fn state() -> MailState {
        MailState {
            inbox: vec![row(2589), row(0)],
        }
    }

    #[test]
    fn inbox_header_reads_the_reference_tuple() {
        let mut s = UiScript::new().unwrap();
        assert_eq!(s.eval::<i64>("return GetInboxNumItems()").unwrap(), 0);
        assert!(s
            .eval::<bool>("return GetInboxHeaderInfo(1) == nil")
            .unwrap());

        s.set_mail(Some(state()));
        assert_eq!(s.eval::<i64>("return GetInboxNumItems()").unwrap(), 2);

        // Row 1 (has item): the leading strings + money/COD/days, then the flags — asserted in two
        // smaller evals so the harness never destructures the whole 13-tuple in one Rust type.
        let (pkg, sta, sender, subject, money, cod, days): (
            String,
            String,
            String,
            String,
            i64,
            i64,
            f64,
        ) = s
            .eval("local a,b,c,d,e,f,g = GetInboxHeaderInfo(1)\nreturn a,b,c,d,e,f,g")
            .unwrap();
        assert_eq!(pkg, "Interface\\Icons\\INV_Misc_Bag_08");
        assert_eq!(sta, "Interface\\Icons\\INV_Letter_15");
        assert_eq!(
            (sender.as_str(), subject.as_str()),
            ("Thrall", "Warchief's orders")
        );
        assert_eq!((money, cod), (5000, 0));
        assert!((days - 29.5).abs() < 1e-3);
        // hasItem = count 3, wasRead nil, wasReturned nil, textCreated nil, canReply 1, isGM nil.
        assert!(s
            .eval::<bool>(
                "local a,b,c,d,e,f,g, has, read, ret, tc, reply, gm = GetInboxHeaderInfo(1)\n\
                 return has == 3 and read == nil and ret == nil and tc == nil and reply == 1 and gm == nil",
            )
            .unwrap());

        // Row 2 (no item): hasItem nil, packageIcon nil.
        assert!(s
            .eval::<bool>(
                "local pkg, sta, s, subj, m, c, d, has = GetInboxHeaderInfo(2)\n\
                 return pkg == nil and has == nil",
            )
            .unwrap());
    }

    #[test]
    fn get_inbox_text_returns_body_and_queues_open() {
        let mut s = UiScript::new().unwrap();
        s.set_mail(Some(state()));
        let (body, tex, takeable, invoice): (String, String, Value, Value) =
            s.eval("return GetInboxText(1)").unwrap();
        assert_eq!(body, "Lok'tar.");
        assert_eq!(tex, "STATIONERYTEST");
        assert_eq!(takeable, Value::Integer(1)); // has a fetchable body
        assert!(matches!(invoice, Value::Nil));
        // Opening queued the row for the mark-read/body ask; deduped within the drain window.
        s.run("GetInboxText(1)").unwrap();
        assert_eq!(s.take_mail_opens(), vec![1]);
        assert!(s.take_mail_opens().is_empty(), "drained");
    }

    #[test]
    fn get_inbox_item_and_can_delete() {
        let mut s = UiScript::new().unwrap();
        s.set_mail(Some(state()));
        let (name, _tex, count, quality, canuse): (String, String, i64, i64, i64) =
            s.eval("return GetInboxItem(1)").unwrap();
        assert_eq!(
            (name.as_str(), count, quality, canuse),
            ("Linen Cloth", 3, 1, 1)
        );
        // Row 2 has no item → nil.
        assert!(s.eval::<bool>("return GetInboxItem(2) == nil").unwrap());
        // Row 1 is from a player, not returned → returnable, NOT deletable.
        assert!(s
            .eval::<bool>("return InboxItemCanDelete(1) == nil")
            .unwrap());
    }

    #[test]
    fn intents_queue_and_drain() {
        let mut s = UiScript::new().unwrap();
        s.set_mail(Some(state()));
        s.run("CheckInbox()").unwrap();
        assert!(s.take_mail_check_inbox());
        assert!(!s.take_mail_check_inbox(), "drained");

        s.run("TakeInboxItem(1) TakeInboxMoney(1) DeleteInboxItem(2) ReturnInboxItem(1) TakeInboxTextItem(1)")
            .unwrap();
        assert_eq!(s.take_mail_take_items(), vec![1]);
        assert_eq!(s.take_mail_take_money(), vec![1]);
        assert_eq!(s.take_mail_deletes(), vec![2]);
        assert_eq!(s.take_mail_returns(), vec![1]);
        assert_eq!(s.take_mail_take_texts(), vec![1]);
        assert!(s.take_mail_take_texts().is_empty(), "drained");

        s.run("CloseMail()").unwrap();
        assert!(s.take_mail_close());
    }

    #[test]
    fn send_folds_money_cod_and_returns_true_from_setmoney() {
        let mut s = UiScript::new().unwrap();
        assert!(s.take_mail_send().is_none());
        assert!(s
            .eval::<bool>("return SetSendMailMoney(1234) == 1")
            .unwrap());
        s.run("SetSendMailCOD(50)").unwrap();
        s.run("SendMail('Jaina', 'Hi', 'body text')").unwrap();
        let req = s.take_mail_send().expect("a send was queued");
        assert_eq!(req.target, "Jaina");
        assert_eq!(req.subject, "Hi");
        assert_eq!(req.body, "body text");
        assert_eq!((req.money, req.cod), (1234, 50));
        assert_eq!(req.item, None);
        assert!(s.take_mail_send().is_none(), "drained");
        // GetSendMailPrice is the flat 30c.
        assert_eq!(s.eval::<i64>("return GetSendMailPrice()").unwrap(), 30);
    }

    #[test]
    fn attach_from_cursor_and_get_send_item() {
        use crate::script::cursor::{CursorItem, CursorPayload};
        let mut s = UiScript::new().unwrap();
        // Nothing attached → nil name, stack 1.
        assert!(s
            .eval::<bool>("local n = GetSendMailItem()\nreturn n == nil")
            .unwrap());

        // Put a bag item on the cursor, then click the send slot: it attaches.
        s.model_mut().cursor = Some(CursorPayload::Item(CursorItem {
            bar_placeable: true,
            bag: 0,
            slot: 5,
            item_id: 2589,
            texture: Some("Interface\\Icons\\INV_Fabric_Linen_01".into()),
            link: Some("|cff...|Hitem:2589|h[Linen Cloth]|h|r".into()),
            count: Some(7),
            quality: Some(1),
            equip_slots: Vec::new(),
        }));
        s.run("ClickSendMailItemButton()").unwrap();
        // Cursor is now empty; the send item reads the attachment.
        assert!(s.eval::<bool>("return not CursorHasItem()").unwrap());
        let (name, _tex, count, quality): (String, String, i64, i64) =
            s.eval("return GetSendMailItem()").unwrap();
        assert_eq!((name.as_str(), count, quality), ("Linen Cloth", 7, 1));

        // A send folds in the attachment's (bag, slot).
        s.run("SendMail('Alt', 'stuff', '')").unwrap();
        assert_eq!(s.take_mail_send().unwrap().item, Some((0, 5)));

        // Clicking again with an empty cursor detaches (back onto the cursor).
        s.run("ClickSendMailItemButton()").unwrap();
        assert!(s.eval::<bool>("return CursorHasItem()").unwrap());
        assert!(s
            .eval::<bool>("local n = GetSendMailItem()\nreturn n == nil")
            .unwrap());
    }

    #[test]
    fn clear_send_item_resets_the_form() {
        use crate::script::cursor::{CursorItem, CursorPayload};
        let mut s = UiScript::new().unwrap();
        s.model_mut().cursor = Some(CursorPayload::Item(CursorItem {
            bar_placeable: true,
            bag: 0,
            slot: 1,
            item_id: 1,
            texture: None,
            link: None,
            count: None,
            quality: None,
            equip_slots: Vec::new(),
        }));
        s.run("ClickSendMailItemButton()").unwrap();
        s.run("SetSendMailMoney(99) SetSendMailCOD(5)").unwrap();
        s.clear_send_mail_item();
        s.run("SendMail('x','y','z')").unwrap();
        let req = s.take_mail_send().unwrap();
        assert_eq!((req.money, req.cod, req.item), (0, 0, None));
    }

    #[test]
    fn clearing_the_mail_empties_it() {
        let mut s = UiScript::new().unwrap();
        s.set_mail(Some(state()));
        s.set_mail(None);
        assert_eq!(s.eval::<i64>("return GetInboxNumItems()").unwrap(), 0);
        assert!(s.eval::<bool>("return HasNewMail() == nil").unwrap());
    }

    #[test]
    fn has_new_mail_reads_the_pushed_flag() {
        let mut s = UiScript::new().unwrap();
        assert!(s.eval::<bool>("return HasNewMail() == nil").unwrap());
        s.set_has_new_mail(true);
        assert!(s.eval::<bool>("return HasNewMail() == 1").unwrap());
        s.set_has_new_mail(false);
        assert!(s.eval::<bool>("return HasNewMail() == nil").unwrap());
    }
}
