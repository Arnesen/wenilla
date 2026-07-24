//! The mail arc's `WorldWriter` sends (decision 0544 P0): the inbox open/refresh, send, take-money/
//! take-item/mark-read/return/delete/copy-letter verbs, the letter-body ask-once fetch, and the login
//! unread-mail query. Bodies in [`crate::messages`]'s `mail` builders (layout VERIFIED against
//! vmangos `Handlers/MailHandler.cpp` + `Server/Packets/Mail.cpp`). Split out of `writer/mod.rs`
//! the same way `channel`/`group` are — one clearly separable concern among the writer's domains.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Ask the mailbox's inbox page (`CMSG_GET_MAIL_LIST`, layout in [`messages::get_mail_list`]) —
    /// the window's open verb and its refresh: one mailbox guid. Answered by
    /// `SMSG_MAIL_LIST_RESULT` (a `MailList` event).
    pub fn get_mail_list(&mut self, mailbox: u64) -> Result<()> {
        self.send(
            opcode::CMSG_GET_MAIL_LIST,
            &messages::get_mail_list(mailbox),
        )
    }

    /// Send a mail (`CMSG_SEND_MAIL`, layout in [`messages::send_mail`]) — the Send tab:
    /// recipient/subject/body, an optional item attachment (`item_guid == 0` = none), money, and
    /// COD. `stationery`/`package` ride the wire but are discarded server-side (player mail is
    /// always stored `MAIL_STATIONERY_DEFAULT`). Answered by `SMSG_SEND_MAIL_RESULT` (a
    /// `SendMailResult` event).
    #[allow(clippy::too_many_arguments)]
    pub fn send_mail(
        &mut self,
        mailbox: u64,
        receiver: &str,
        subject: &str,
        body: &str,
        stationery: u32,
        package: u32,
        item_guid: u64,
        money: u32,
        cod: u32,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_SEND_MAIL,
            &messages::send_mail(
                mailbox, receiver, subject, body, stationery, package, item_guid, money, cod,
            ),
        )
    }

    /// Take a mail's attached money (`CMSG_MAIL_TAKE_MONEY`, layout in
    /// [`messages::mail_take_money`]). Answered by `SMSG_SEND_MAIL_RESULT`
    /// (action [`messages::mail_action::MONEY_TAKEN`]).
    pub fn mail_take_money(&mut self, mailbox: u64, mail_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_MAIL_TAKE_MONEY,
            &messages::mail_take_money(mailbox, mail_id),
        )
    }

    /// Take a mail's attached item (`CMSG_MAIL_TAKE_ITEM`, layout in
    /// [`messages::mail_take_item`]). Answered by `SMSG_SEND_MAIL_RESULT`
    /// (action [`messages::mail_action::ITEM_TAKEN`]).
    pub fn mail_take_item(&mut self, mailbox: u64, mail_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_MAIL_TAKE_ITEM,
            &messages::mail_take_item(mailbox, mail_id),
        )
    }

    /// Mark a mail read (`CMSG_MAIL_MARK_AS_READ`, layout in [`messages::mail_mark_as_read`]) —
    /// sent when a letter opens. **No response packet**: the app flips the local `checked` READ
    /// bit and repaints.
    pub fn mail_mark_as_read(&mut self, mailbox: u64, mail_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_MAIL_MARK_AS_READ,
            &messages::mail_mark_as_read(mailbox, mail_id),
        )
    }

    /// Return a mail to its sender (`CMSG_MAIL_RETURN_TO_SENDER`, layout in
    /// [`messages::mail_return_to_sender`]). Answered by `SMSG_SEND_MAIL_RESULT`
    /// (action [`messages::mail_action::RETURNED`]).
    pub fn mail_return_to_sender(&mut self, mailbox: u64, mail_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_MAIL_RETURN_TO_SENDER,
            &messages::mail_return_to_sender(mailbox, mail_id),
        )
    }

    /// Delete a mail (`CMSG_MAIL_DELETE`, layout in [`messages::mail_delete`]) — the taken-letter
    /// close and the explicit Delete button alike. Answered by `SMSG_SEND_MAIL_RESULT`
    /// (action [`messages::mail_action::DELETED`]).
    pub fn mail_delete(&mut self, mailbox: u64, mail_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_MAIL_DELETE,
            &messages::mail_delete(mailbox, mail_id),
        )
    }

    /// Make a permanent copy of a letter's body (`CMSG_MAIL_CREATE_TEXT_ITEM`, layout in
    /// [`messages::mail_create_text_item`]) — the open letter's "Take Attachments" letter button.
    /// Answered by `SMSG_SEND_MAIL_RESULT` (action [`messages::mail_action::MADE_PERMANENT`]).
    pub fn mail_create_text_item(&mut self, mailbox: u64, mail_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_MAIL_CREATE_TEXT_ITEM,
            &messages::mail_create_text_item(mailbox, mail_id),
        )
    }

    /// Fetch a letter's body text (`CMSG_ITEM_TEXT_QUERY`, layout in [`messages::item_text_query`])
    /// — sent once per mail whose `item_text_id != 0` (ask-once cacheable). Answered by
    /// `SMSG_ITEM_TEXT_QUERY_RESPONSE` (a `MailItemText` event).
    pub fn item_text_query(&mut self, text_id: u32, mail_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_ITEM_TEXT_QUERY,
            &messages::item_text_query(text_id, mail_id),
        )
    }

    /// Ask whether unread mail is waiting (`MSG_QUERY_NEXT_MAIL_TIME`, EMPTY body — same opcode
    /// answers with one `f32`: `0.0` unread waiting, `-86400.0` none). Sent once at login to seed
    /// `HasNewMail()`/the minimap letter icon.
    pub fn query_next_mail_time(&mut self) -> Result<()> {
        self.send(opcode::MSG_QUERY_NEXT_MAIL_TIME, &[])
    }
}
