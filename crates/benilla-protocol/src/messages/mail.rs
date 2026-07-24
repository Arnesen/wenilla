//! The mail arc's wire layer (decision 0544 phase P0): the mailbox window's send/inbox/take/
//! item-text/arrival opcodes (vmangos `Handlers/MailHandler.cpp`, `Mail/Mail.{h,cpp}`,
//! `Server/Packets/Mail.{h,cpp}`, `Misc.{h,cpp}`, `Opcodes_1_12_1.h`, VERIFIED at decision time).
//! There is no `SMSG_SHOW_MAILBOX` on 5875 — the window open is entirely client-side (0544's
//! recorded fact); every mail CMSG below carries the mailbox guid at its head and the server
//! re-validates the 5 yd interact distance (`CheckMailBox`) on each one independently.

use std::io;

use crate::wire::{read_cstring, read_f32_le, read_u32_le, read_u64_le, read_u8};

/// `MAIL_ITEM (5)` on [`MailListEntry::message_type`] carries no sender bytes at all — the only
/// branch of the four that reads nothing before the subject cstring (VERIFIED vmangos
/// `WorldSession::HandleGetMailList`, `Handlers/MailHandler.cpp`).
pub mod mail_message_type {
    /// A player sender — the sender field is a plain 8-byte guid.
    pub const NORMAL: u8 = 0;
    /// An auction-house notice — the sender field is a u32 auction id.
    pub const AUCTION: u8 = 2;
    /// A creature/NPC sender — the sender field is a u32 creature entry.
    pub const CREATURE: u8 = 3;
    /// A GameObject sender — the sender field is a u32 GameObject entry.
    pub const GAMEOBJECT: u8 = 4;
    /// A pre-made "item" mail (e.g. a GM/starter mail) — NO sender field rides the wire at all.
    pub const ITEM: u8 = 5;
}

/// `SMSG_SEND_MAIL_RESULT`'s `action` field (VERIFIED vmangos `Mail.h` `MailResponseType`).
pub mod mail_action {
    pub const SEND: u32 = 0;
    pub const MONEY_TAKEN: u32 = 1;
    pub const ITEM_TAKEN: u32 = 2;
    pub const RETURNED: u32 = 3;
    pub const DELETED: u32 = 4;
    pub const MADE_PERMANENT: u32 = 5;
}

/// `SMSG_SEND_MAIL_RESULT`'s `error` field (VERIFIED vmangos `Mail.h` `MailResponseResult`).
pub mod mail_error {
    pub const OK: u32 = 0;
    /// The trailing `equipError` tail rides only this one.
    pub const EQUIP_ERROR: u32 = 1;
    pub const CANNOT_SEND_TO_SELF: u32 = 2;
    pub const NOT_ENOUGH_MONEY: u32 = 3;
    pub const RECIPIENT_NOT_FOUND: u32 = 4;
    pub const NOT_YOUR_TEAM: u32 = 5;
    pub const INTERNAL_ERROR: u32 = 6;
    pub const TRIAL_ACCOUNT: u32 = 14;
    pub const TOO_MANY_ATTACHMENTS: u32 = 15;
}

/// The single attachment slot a mail can carry (`MAX_MAIL_ITEMS == 1` on 5875) — the fixed-width
/// item block that always rides `SMSG_MAIL_LIST_RESULT`'s per-record tail, zeroed when the mail
/// carries no item. `entry == 0` on the wire is the "no item" signal; [`read_mail_list_result`]
/// folds that into `None` rather than a zeroed `Some`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailAttachment {
    pub entry: u32,
    pub perm_enchant: u32,
    pub random_prop_id: u32,
    pub suffix_factor: u32,
    pub count: u8,
    pub charges: u32,
    pub durability_max: u32,
    pub durability: u32,
}

/// One `SMSG_MAIL_LIST_RESULT` row (VERIFIED vmangos `WorldSession::HandleGetMailList`'s
/// hand-serialization, `Handlers/MailHandler.cpp`). `sender_guid`/`sender_id` are
/// mutually exclusive branches of one wire field, keyed by `message_type`
/// ([`mail_message_type`]): a player sender is a guid, an auction/creature/GameObject sender is a
/// u32 id, and a pre-made item mail (`ITEM`) sends neither.
#[derive(Debug, Clone, PartialEq)]
pub struct MailListEntry {
    pub message_id: u32,
    pub message_type: u8,
    /// `Some` only for [`mail_message_type::NORMAL`].
    pub sender_guid: Option<u64>,
    /// `Some` for [`mail_message_type::AUCTION`]/`CREATURE`/`GAMEOBJECT`.
    pub sender_id: Option<u32>,
    pub subject: String,
    /// `0` = no letter body to fetch; nonzero = a [`super::item_text_query`] key.
    pub item_text_id: u32,
    pub stationery: u32,
    /// `None` iff the wire's item-block `entry` is `0`.
    pub item: Option<MailAttachment>,
    pub money: u32,
    pub cod: u32,
    /// `checked` bit masks (VERIFIED vmangos `Mail.h`): READ `0x1`, RETURNED `0x2`, COPIED `0x4`,
    /// COD_PAYMENT `0x8`, HAS_BODY `0x10`.
    pub checked: u32,
    pub expire_days: f32,
    pub mail_template_id: u32,
}

/// Body of `CMSG_GET_MAIL_LIST` (VERIFIED vmangos `Server/Packets/Mail.cpp`,
/// `GetMailList::ReadFromWorldPacket`): one full 8-byte mailbox guid. Answered by
/// `SMSG_MAIL_LIST_RESULT` ([`read_mail_list_result`]).
pub fn get_mail_list(mailbox: u64) -> Vec<u8> {
    mailbox.to_le_bytes().to_vec()
}

/// Body of `CMSG_SEND_MAIL` (VERIFIED vmangos `Server/Packets/Mail.cpp`,
/// `SendMail::ReadFromWorldPacket`): `u64 mailbox, cstr receiver, cstr subject, cstr body, u32
/// stationery, u32 package, u64 itemGuid, u32 money, u32 COD`, then **9 zero bytes** (a `u64 0` +
/// a `u8 0`) — the 1.12.1 tail vmangos read-skips without interpreting. vmangos *discards* the
/// client's `stationery`/`package` choice: player mail is always stored `MAIL_STATIONERY_DEFAULT`
/// (41), so both fields exist only to keep the reader aligned.
#[allow(clippy::too_many_arguments)]
pub fn send_mail(
    mailbox: u64,
    receiver: &str,
    subject: &str,
    body: &str,
    stationery: u32,
    package: u32,
    item_guid: u64,
    money: u32,
    cod: u32,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        8 + receiver.len() + 1 + subject.len() + 1 + body.len() + 1 + 4 + 4 + 8 + 4 + 4 + 9,
    );
    out.extend_from_slice(&mailbox.to_le_bytes());
    out.extend_from_slice(receiver.as_bytes());
    out.push(0);
    out.extend_from_slice(subject.as_bytes());
    out.push(0);
    out.extend_from_slice(body.as_bytes());
    out.push(0);
    out.extend_from_slice(&stationery.to_le_bytes());
    out.extend_from_slice(&package.to_le_bytes());
    out.extend_from_slice(&item_guid.to_le_bytes());
    out.extend_from_slice(&money.to_le_bytes());
    out.extend_from_slice(&cod.to_le_bytes());
    // The 9-byte read-skipped tail: a u64 then a u8, both zero, both discarded server-side.
    out.extend_from_slice(&0u64.to_le_bytes());
    out.push(0);
    out
}

/// The shared `u64 mailbox, u32 mailId` shape of the five take/mark/return/delete CMSGs below.
fn mailbox_and_mail_id(mailbox: u64, mail_id: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&mailbox.to_le_bytes());
    body.extend_from_slice(&mail_id.to_le_bytes());
    body
}

/// Body of `CMSG_MAIL_TAKE_MONEY`: `u64 mailbox, u32 mailId`. Answered by `SMSG_SEND_MAIL_RESULT`
/// (action [`mail_action::MONEY_TAKEN`]).
pub fn mail_take_money(mailbox: u64, mail_id: u32) -> Vec<u8> {
    mailbox_and_mail_id(mailbox, mail_id)
}

/// Body of `CMSG_MAIL_TAKE_ITEM`: `u64 mailbox, u32 mailId`. Answered by `SMSG_SEND_MAIL_RESULT`
/// (action [`mail_action::ITEM_TAKEN`], carrying the taken item's entry + count on success).
pub fn mail_take_item(mailbox: u64, mail_id: u32) -> Vec<u8> {
    mailbox_and_mail_id(mailbox, mail_id)
}

/// Body of `CMSG_MAIL_MARK_AS_READ`: `u64 mailbox, u32 mailId`. **No response packet** — the app
/// flips the local `checked` READ bit and repaints.
pub fn mail_mark_as_read(mailbox: u64, mail_id: u32) -> Vec<u8> {
    mailbox_and_mail_id(mailbox, mail_id)
}

/// Body of `CMSG_MAIL_RETURN_TO_SENDER`: `u64 mailbox, u32 mailId`. Answered by
/// `SMSG_SEND_MAIL_RESULT` (action [`mail_action::RETURNED`]).
pub fn mail_return_to_sender(mailbox: u64, mail_id: u32) -> Vec<u8> {
    mailbox_and_mail_id(mailbox, mail_id)
}

/// Body of `CMSG_MAIL_DELETE`: `u64 mailbox, u32 mailId`. Answered by `SMSG_SEND_MAIL_RESULT`
/// (action [`mail_action::DELETED`]).
pub fn mail_delete(mailbox: u64, mail_id: u32) -> Vec<u8> {
    mailbox_and_mail_id(mailbox, mail_id)
}

/// Body of `CMSG_MAIL_CREATE_TEXT_ITEM` (VERIFIED vmangos `Server/Packets/Mail.cpp`,
/// `MailCreateTextItem::ReadFromWorldPacket`): `u64 mailbox, u32 mailId, u32 mailTemplateId` —
/// the letter button's "permanent copy" verb; `mailTemplateId` is always `0` for player mail (no
/// mail templates in 1.12 player mail; vmangos only reads it on builds > 1.9.4). Answered by
/// `SMSG_SEND_MAIL_RESULT` (action [`mail_action::MADE_PERMANENT`]); on OK the server also stamps
/// the mail's `checked` COPIED bit — the next list re-sync flips `textCreated`.
pub fn mail_create_text_item(mailbox: u64, mail_id: u32) -> Vec<u8> {
    let mut body = mailbox_and_mail_id(mailbox, mail_id);
    body.extend_from_slice(&0u32.to_le_bytes()); // mailTemplateId
    body
}

/// Body of `CMSG_ITEM_TEXT_QUERY` (VERIFIED vmangos `Server/Packets/Mail.cpp`,
/// `ItemTextQuery::ReadFromWorldPacket`): `u32 textId, u32 mailId, u32 unk` — vmangos resolves the
/// text from `textId` alone and never reads `mailId`/`unk` back out, but the real client writes
/// all three. Answered by `SMSG_ITEM_TEXT_QUERY_RESPONSE` ([`read_item_text_query_response`]).
pub fn item_text_query(text_id: u32, mail_id: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&text_id.to_le_bytes());
    body.extend_from_slice(&mail_id.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes()); // the unk vmangos never reads back
    body
}

/// Read `SMSG_MAIL_LIST_RESULT` (VERIFIED vmangos `WorldSession::HandleGetMailList`,
/// hand-serialized in `Handlers/MailHandler.cpp`): `u8 count` (cap 254; ≤100 by business rule), then `count`
/// × [`MailListEntry`] rows. Each row: `u32 messageId, u8 messageType`, the sender branch keyed by
/// `messageType` ([`mail_message_type`]), `cstr subject, u32 itemTextId, u32 package(dropped —
/// always 0, no consumer), u32 stationery`, the fixed-width item block (ALWAYS present; `entry ==
/// 0` folds to [`MailListEntry::item`] being `None`), `u32 money, u32 COD, u32 checked, f32
/// expireDays, u32 mailTemplateId`.
pub(super) fn read_mail_list_result(r: &mut &[u8]) -> io::Result<Vec<MailListEntry>> {
    let count = read_u8(r)?;
    let mut mails = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let message_id = read_u32_le(r)?;
        let message_type = read_u8(r)?;
        let (sender_guid, sender_id) = match message_type {
            mail_message_type::NORMAL => (Some(read_u64_le(r)?), None),
            mail_message_type::AUCTION
            | mail_message_type::CREATURE
            | mail_message_type::GAMEOBJECT => (None, Some(read_u32_le(r)?)),
            // MAIL_ITEM (5) and anything else unrecognized: no sender bytes at all.
            _ => (None, None),
        };
        let subject = read_cstring(r)?;
        let item_text_id = read_u32_le(r)?;
        let _package = read_u32_le(r)?; // always 0 on 5875 (vmangos hardcodes it); no consumer
        let stationery = read_u32_le(r)?;
        let entry = read_u32_le(r)?;
        let perm_enchant = read_u32_le(r)?;
        let random_prop_id = read_u32_le(r)?;
        let suffix_factor = read_u32_le(r)?;
        let count_stack = read_u8(r)?;
        let charges = read_u32_le(r)?;
        let durability_max = read_u32_le(r)?;
        let durability = read_u32_le(r)?;
        let item = (entry != 0).then_some(MailAttachment {
            entry,
            perm_enchant,
            random_prop_id,
            suffix_factor,
            count: count_stack,
            charges,
            durability_max,
            durability,
        });
        let money = read_u32_le(r)?;
        let cod = read_u32_le(r)?;
        let checked = read_u32_le(r)?;
        let expire_days = read_f32_le(r)?;
        let mail_template_id = read_u32_le(r)?;
        mails.push(MailListEntry {
            message_id,
            message_type,
            sender_guid,
            sender_id,
            subject,
            item_text_id,
            stationery,
            item,
            money,
            cod,
            checked,
            expire_days,
            mail_template_id,
        });
    }
    Ok(mails)
}

/// Read `SMSG_SEND_MAIL_RESULT` (VERIFIED vmangos `Server/Packets/Mail.cpp`,
/// `SendMailResult::AppendBodyTo`): `u32 mailId, u32 action ([`mail_action`]), u32 error
/// ([`mail_error`])`, then a conditional tail — `u32 equipError` iff `error ==
/// `[`mail_error::EQUIP_ERROR`]`, else `u32 itemEntry, u32 itemCount` iff `action ==
/// `[`mail_action::ITEM_TAKEN`]` and `error == `[`mail_error::OK`]``. Neither tail rides any other
/// action/error combination; the remaining-length guard mirrors how the codec's other
/// conditional tails (e.g. `SMSG_TRANSFER_PENDING`'s transport block) stay robust to a body that
/// doesn't carry the tail its fields would otherwise predict.
#[allow(clippy::type_complexity)]
pub(super) fn read_send_mail_result(
    r: &mut &[u8],
) -> io::Result<(u32, u32, u32, Option<u32>, Option<(u32, u32)>)> {
    let mail_id = read_u32_le(r)?;
    let action = read_u32_le(r)?;
    let error = read_u32_le(r)?;
    let mut equip_error = None;
    let mut item = None;
    if error == mail_error::EQUIP_ERROR && !r.is_empty() {
        equip_error = Some(read_u32_le(r)?);
    } else if action == mail_action::ITEM_TAKEN && error == mail_error::OK && !r.is_empty() {
        item = Some((read_u32_le(r)?, read_u32_le(r)?));
    }
    Ok((mail_id, action, error, equip_error, item))
}

/// Read `SMSG_ITEM_TEXT_QUERY_RESPONSE` (VERIFIED vmangos `Server/Packets/Mail.cpp`,
/// `ItemTextQueryResponse::AppendBodyTo`): `u32 textId, cstr text` — the letter body's ask-once
/// fetch, answering [`item_text_query`].
pub(super) fn read_item_text_query_response(r: &mut &[u8]) -> io::Result<(u32, String)> {
    Ok((read_u32_le(r)?, read_cstring(r)?))
}

/// Read `SMSG_RECEIVED_MAIL` (VERIFIED vmangos `Server/Packets/Mail.cpp`,
/// `ReceivedMail::AppendBodyTo`): one `u32`, always `0` on vmangos — kept for fidelity though no
/// consumer reads it back. Sent when a mail *arrives* (instant for text-only, on the delivery
/// timer's expiry otherwise).
pub(super) fn read_received_mail(r: &mut &[u8]) -> io::Result<u32> {
    read_u32_le(r)
}

/// Read `MSG_QUERY_NEXT_MAIL_TIME`'s reply (same opcode both directions — our request is an EMPTY
/// body, [`super::opcode::MSG_QUERY_NEXT_MAIL_TIME`]): one `f32` — `0.0` = unread mail waiting,
/// `-86400.0` = none.
pub(super) fn read_query_next_mail_time(r: &mut &[u8]) -> io::Result<f32> {
    read_f32_le(r)
}
