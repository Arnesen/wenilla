//! The mail arc's drain-side arm bodies for [`super::apply_net_updates`]'s dispatch match
//! (decision 0544 P1/P2). Each `pub(super)` fn here is one `SessionEvent` arm's body, filling the
//! [`crate::ui_mail::MailOpen`] session the feed reads; the match at the call site stays the
//! dispatcher. The `UiScript` events these ultimately drive are fired by [`crate::ui_mail::feed_mail`]
//! (the feed owns the VM), so these arms only mutate resources + send the wire re-syncs.

use benilla_protocol::messages::{mail_action, mail_error, mail_message_type, MailListEntry};

use crate::net::{ClientCommand, NetCommands};
use crate::ui_items::{EquipError, EquipErrors};
use crate::ui_mail::{mail_refusal, MailOpen, MailPending, MailSendAck};

/// `SessionEvent::MailList` (`SMSG_MAIL_LIST_RESULT`) — replace the session's rows + fire the inbox
/// repaint (via the feed's diff). The inbox handler auto-purges expired mail: any row whose timer ran
/// out (`expire_days <= 0`) is deleted server-side (`CMSG_MAIL_DELETE`) and dropped here (wow-re §5,
/// `ui/scratch/mail-interaction.md`).
///
/// **It does not touch [`crate::ui_mail::MailPending`]** — and that is a positive fact, not an
/// omission (decision 0913). This arm used to clear the countdown when the surviving list had
/// nothing unread, on the inferred grounds that "checking your mail clears the icon" had to be the
/// list's doing. A full write-xref of the countdown float in wow-re says otherwise: nothing on the
/// inbox path writes it. The icon clears because **opening a letter arms the deferred-refresh flag
/// and the mailbox *close* re-asks the server** — modelled in [`crate::ui_mail`], where the close
/// edge lives.
pub(super) fn mail_list(mails: Vec<MailListEntry>, mail: &mut MailOpen, commands: &NetCommands) {
    let mailbox = mail.mailbox;
    mail.mails = mails
        .into_iter()
        .filter(|e| {
            if e.expire_days <= 0.0 {
                if let Some(mailbox) = mailbox {
                    let _ = commands.0.send(ClientCommand::MailDelete {
                        mailbox,
                        mail_id: e.message_id,
                    });
                }
                false
            } else {
                true
            }
        })
        .collect();
}

/// `MAIL_CHECK_MASK_COD_PAYMENT` — the `checked` bit (`byte[rec+0x148] & 8`) that marks a mail as
/// the money a COD taker paid; taking that money empties the mail (wow-re, 1970).
const CHECKED_COD_PAYMENT: u32 = 8;

/// The two take legs' "this mail is now empty" decision, off the client's own bytes (wow-re
/// `stationery-bindings.md` §8, 1970): a take that empties the mail sends `CMSG_MAIL_DELETE`
/// itself, then `CLOSE_INBOX_ITEM(index)`, then `MAIL_INBOX_UPDATE`. The money leg (`0x4ad6b0`)
/// purges iff the mail is a COD payment, or an auction notice with no item; the item leg
/// (`0x4ad7b0`) purges iff the money is gone and the mail is an auction notice or carries no
/// letter text. A plain letter you took the money from stays open, husk and all — that is the
/// reference too (the stock `OpenMailFrame_OnHide` deletes a copied, emptied letter on close).
///
/// The item leg's second conjunct reads `[rec+0x114] == 0 && [rec+0x25c] == 0`; `+0x114` is the
/// letter's text id and `+0x25c` a second no-text field the carve did not name (INFERRED to be the
/// fetched body). This takes the text id alone, which can only purge a mail the client also would
/// when that second field is zero whenever the first is.
fn take_empties(entry: &MailListEntry, action: u32) -> bool {
    let auction = entry.message_type == mail_message_type::AUCTION;
    match action {
        mail_action::MONEY_TAKEN => {
            entry.checked & CHECKED_COD_PAYMENT != 0 || (auction && entry.item.is_none())
        }
        mail_action::ITEM_TAKEN => entry.money == 0 && (auction || entry.item_text_id == 0),
        _ => false,
    }
}

/// `SessionEvent::SendMailResult` (`SMSG_SEND_MAIL_RESULT`) — route per action/error (decision 0544
/// P2). action == SEND queues a [`MailSendAck`] for the feed (MAIL_FAILED always, MAIL_SEND_SUCCESS
/// on OK, else the red error line). A successful take applies to the local row and, when it
/// empties the mail, deletes it the way the client does ([`take_empties`]); every successful
/// take/return/delete then re-syncs the inbox with a fresh `CMSG_GET_MAIL_LIST` (the reference
/// client's inbox-refresh moment); an EQUIP_ERROR routes to the existing inventory-error surface;
/// any other failure surfaces the red error line.
#[allow(clippy::too_many_arguments)]
pub(super) fn send_mail_result(
    mail_id: u32,
    action: u32,
    error: u32,
    equip_error: Option<u32>,
    _item: Option<(u32, u32)>,
    mail: &mut MailOpen,
    commands: &NetCommands,
    equip_errors: &mut EquipErrors,
) {
    // ITEM_TAKEN's OK tail carries (entry, count) for a "received" line; vmangos does NOT also send
    // SMSG_ITEM_PUSH_RESULT for a mail take, and no mail-received-line precedent exists yet, so we do
    // nothing extra with it here (the GetMailList re-sync below reflects the emptied row). [_item]
    if action == mail_action::SEND {
        if error == mail_error::EQUIP_ERROR {
            equip_errors.0.push(EquipError {
                reason: equip_error.unwrap_or(0) as u8,
                required_level: None,
                // SMSG_SEND_MAIL_RESULT carries only the code, never a bag slot. 255 is the
                // wire's own "the player's own array" sentinel — reason 16's substitution
                // correctly declines to name a container it was never told about.
                bag_slot: 255,
            });
        }
        mail.send_acks.push(MailSendAck {
            ok: error == mail_error::OK,
            refusal: (error != mail_error::OK && error != mail_error::EQUIP_ERROR)
                .then(|| mail_refusal(error)),
        });
        return;
    }
    // A take/return/delete result.
    match error {
        mail_error::OK => {
            // The take landed on the local row first (the client clears `[rec+0x140]` /
            // `[rec+0x120]` before deciding), and an emptied mail is purged client-side:
            // `CMSG_MAIL_DELETE`, then `CLOSE_INBOX_ITEM(index)` from the feed (1970).
            if let Some(pos) = mail.mails.iter().position(|e| e.message_id == mail_id) {
                match action {
                    mail_action::MONEY_TAKEN => mail.mails[pos].money = 0,
                    mail_action::ITEM_TAKEN => mail.mails[pos].item = None,
                    _ => {}
                }
                if take_empties(&mail.mails[pos], action) {
                    if let Some(mailbox) = mail.mailbox {
                        let _ = commands
                            .0
                            .send(ClientCommand::MailDelete { mailbox, mail_id });
                    }
                    mail.close_inbox.push(pos as u32 + 1);
                    mail.mails.remove(pos);
                }
            }
            // Re-sync the inbox: the taken money/item or removed row is gone server-side.
            if let Some(mailbox) = mail.mailbox {
                let _ = commands.0.send(ClientCommand::GetMailList { mailbox });
            }
        }
        mail_error::EQUIP_ERROR => equip_errors.0.push(EquipError {
            reason: equip_error.unwrap_or(0) as u8,
            required_level: None,
            bag_slot: 255,
        }),
        other => mail.errors.push(mail_refusal(other)),
    }
}

/// `SessionEvent::MailItemText` (`SMSG_ITEM_TEXT_QUERY_RESPONSE`) — land the letter body in the
/// ask-once cache + clear its pending flag; the feed repaints (MAIL_INBOX_UPDATE) on the change.
pub(super) fn mail_item_text(text_id: u32, text: String, mail: &mut MailOpen) {
    mail.bodies.insert(text_id, text);
    mail.pending_bodies.remove(&text_id);
}

/// `SessionEvent::ReceivedMail` (`SMSG_RECEIVED_MAIL`) — mail just arrived. `seconds` is the wire's
/// delay float (vmangos always sends `0.0` = "now"); it runs the countdown's set-value ladder,
/// which takes the **busy** branch when a mailbox window is open — arming the deferred refresh
/// instead of moving the icon under the player's nose (wow-re `0x4ad620`, decision 0913).
///
/// The list re-sync is ours, not the reference's, and stays: a server push bypasses `CheckInbox`'s
/// 60 s client-side throttle (decision 0544 P3), so a mail arriving while you stand at the mailbox
/// shows up. The reference reaches the same place by its close-time re-query; leaving a mail you
/// were just told about invisible for up to a minute is the worse client, and this costs one packet.
pub(super) fn received_mail(
    seconds: f32,
    pending: &mut MailPending,
    mail: &MailOpen,
    commands: &NetCommands,
) {
    pending.apply_received_mail(seconds, mail.mailbox.is_some());
    if let Some(mailbox) = mail.mailbox {
        let _ = commands.0.send(ClientCommand::GetMailList { mailbox });
    }
}

/// `SessionEvent::NextMailTime` (`MSG_QUERY_NEXT_MAIL_TIME`'s reply, one `f32`) — store the
/// server's float verbatim and signal `UPDATE_PENDING_MAIL` **unconditionally** (wow-re `0x4ad5f0`,
/// signal site `0x4ad605`; decision 0913). `0.0` = mail waiting now, negative (vmangos always sends
/// `-86400.0`) = none, a positive value counts down per frame in `crate::ui_mail`'s `feed_mail` and
/// flips `HasNewMail()` true as it lands inside ε.
pub(super) fn next_mail_time(seconds: f32, pending: &mut MailPending) {
    pending.apply_query_reply(seconds);
}
