//! The mail arc's drain-side arm bodies for [`super::apply_net_updates`]'s dispatch match
//! (decision 0544 P1/P2). Each `pub(super)` fn here is one `SessionEvent` arm's body, filling the
//! [`crate::ui_mail::MailOpen`] session the feed reads; the match at the call site stays the
//! dispatcher. The `UiScript` events these ultimately drive are fired by [`crate::ui_mail::feed_mail`]
//! (the feed owns the VM), so these arms only mutate resources + send the wire re-syncs.

use benilla_protocol::messages::{mail_action, mail_error, MailListEntry};

use crate::net::{ClientCommand, NetCommands};
use crate::ui_items::EquipErrors;
use crate::ui_mail::{mail_error_text, MailOpen, MailPending, MailSendAck, CHECKED_READ};

/// `SessionEvent::MailList` (`SMSG_MAIL_LIST_RESULT`) — replace the session's rows + fire the inbox
/// repaint (via the feed's diff). The inbox handler auto-purges expired mail: any row whose timer ran
/// out (`expire_days <= 0`) is deleted server-side (`CMSG_MAIL_DELETE`) and dropped here (wow-re §5,
/// `ui/scratch/mail-interaction.md`).
///
/// Also clears [`MailPending`] (decision 0544 P3) when the surviving list has nothing left unread:
/// vmangos recomputes its own `unReadMails` count whenever it serves the list, but never re-notifies
/// the client's countdown float directly — the float is purely client-local. Mirroring "every row
/// read (or no rows at all) ⇒ HasNewMail() goes false" is the observable law the reference itself
/// exhibits (opening/checking the inbox clears the minimap icon); a list that still has an unread row
/// leaves the countdown alone (a fresh unrelated mail may be independently pending).
pub(super) fn mail_list(
    mails: Vec<MailListEntry>,
    mail: &mut MailOpen,
    commands: &NetCommands,
    pending: &mut MailPending,
) {
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
    if mail.mails.iter().all(|e| e.checked & CHECKED_READ != 0) {
        pending.0 = None;
    }
}

/// `SessionEvent::SendMailResult` (`SMSG_SEND_MAIL_RESULT`) — route per action/error (decision 0544
/// P2). action == SEND queues a [`MailSendAck`] for the feed (MAIL_FAILED always, MAIL_SEND_SUCCESS
/// on OK, else the red error line). A successful take/return/delete re-syncs the inbox with a fresh
/// `CMSG_GET_MAIL_LIST` (the reference client's inbox-refresh moment); an EQUIP_ERROR routes to the
/// existing inventory-error surface; any other failure surfaces the red error line.
#[allow(clippy::too_many_arguments)]
pub(super) fn send_mail_result(
    _mail_id: u32,
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
            equip_errors.0.push((equip_error.unwrap_or(0) as u8, None));
        }
        mail.send_acks.push(MailSendAck {
            ok: error == mail_error::OK,
            error_text: (error != mail_error::OK && error != mail_error::EQUIP_ERROR)
                .then(|| mail_error_text(error)),
        });
        return;
    }
    // A take/return/delete result.
    match error {
        mail_error::OK => {
            // Re-sync the inbox: the taken money/item or removed row is gone server-side.
            if let Some(mailbox) = mail.mailbox {
                let _ = commands.0.send(ClientCommand::GetMailList { mailbox });
            }
        }
        mail_error::EQUIP_ERROR => equip_errors.0.push((equip_error.unwrap_or(0) as u8, None)),
        other => mail.errors.push(mail_error_text(other)),
    }
}

/// `SessionEvent::MailItemText` (`SMSG_ITEM_TEXT_QUERY_RESPONSE`) — land the letter body in the
/// ask-once cache + clear its pending flag; the feed repaints (MAIL_INBOX_UPDATE) on the change.
pub(super) fn mail_item_text(text_id: u32, text: String, mail: &mut MailOpen) {
    mail.bodies.insert(text_id, text);
    mail.pending_bodies.remove(&text_id);
}

/// `SessionEvent::ReceivedMail` (`SMSG_RECEIVED_MAIL`) — mail just arrived: the wire body carries no
/// payload (always `u32 0`, decision 0544), so the *event itself* is the notification. Set the
/// countdown to `0.0` (`HasNewMail()` flips true — wow-re §5 §7) and, if a mailbox session is open,
/// re-sync its list: a server push bypasses `CheckInbox`'s 60 s client-side throttle (decision 0544
/// P3) — the reference's own case for "the inbox visibly updates while you're standing at it".
pub(super) fn received_mail(pending: &mut MailPending, mail: &MailOpen, commands: &NetCommands) {
    pending.0 = Some(0.0);
    if let Some(mailbox) = mail.mailbox {
        let _ = commands.0.send(ClientCommand::GetMailList { mailbox });
    }
}

/// `SessionEvent::NextMailTime` (`MSG_QUERY_NEXT_MAIL_TIME`'s reply, one `f32`) — seed the
/// countdown (decision 0544 P3, wow-re §5 §7): `0.0` = mail waiting now, negative (vmangos always
/// sends `-86400.0`) = none, a hypothetical positive value counts down per frame in `crate::ui_mail`'s
/// `feed_mail` and flips `HasNewMail()` true at `0`.
pub(super) fn next_mail_time(seconds: f32, pending: &mut MailPending) {
    pending.0 = Some(seconds);
}
