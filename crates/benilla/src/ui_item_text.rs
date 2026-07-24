//! The item-text reader session (`ItemTextFrame.xml`) — reading a bag letter (a mail-made
//! permanent copy, `ITEM_FIELD_ITEM_TEXT_ID != 0`) in the reference reader window.
//!
//! The right-click routing lives in `ui_items::drain::drain_container_uses`: a bag item whose
//! *instance* carries an item-text id opens a read here instead of sending `CMSG_USE_ITEM` (the
//! real client's fork — vmangos would refuse a `CMSG_READ_ITEM` for it anyway, since that opcode's
//! handler gates on the *template*'s `PageText`, and the Plain Letter template's is 0). The text
//! rides the same ask-once `CMSG_ITEM_TEXT_QUERY` cache mail letter bodies use
//! ([`crate::ui_mail::MailOpen::bodies`] — that map is the client's item-text cache, mail is just
//! its first tenant); the creator line resolves `ITEM_FIELD_CREATOR` through the name cache.
//!
//! The feed mirrors the reference event flow (ItemTextFrame.lua l.10-96): open →
//! `ITEM_TEXT_BEGIN` (title known), everything fetched → `ITEM_TEXT_READY`, `CloseItemText()` →
//! session cleared + `ITEM_TEXT_CLOSED`. `ITEM_TEXT_TRANSLATION` (the foreign-language progress
//! bar) never fires — no translation mechanic on a private server's letters. Multi-page
//! `PageText` books (`CMSG_READ_ITEM` / `SMSG_READ_ITEM_OK` + the page chain) are a named
//! follow-up — the page-turn intents drain here and are dropped.

use bevy::prelude::*;

use benilla_ui::script::{ItemTextState, UiScript};

use crate::items::Items;
use crate::names::NameCache;
use crate::net::{ClientCommand, NetCommands};
use crate::ui_mail::MailOpen;
use crate::ui_script::UiInput;

/// The open read session; `None` in [`ItemTextOpen::pending`] = no reader open.
pub(crate) struct ReadSession {
    /// The read item's object guid — title/creator resolve off it each frame until ready.
    pub(crate) item_guid: u64,
    /// `ITEM_FIELD_ITEM_TEXT_ID` — the ask-once cache key.
    pub(crate) text_id: u32,
    /// `ITEM_TEXT_BEGIN` fired (the reference fires it once per open, before the text lands).
    begun: bool,
    /// `ITEM_TEXT_READY` fired — the session is fully painted.
    ready: bool,
}

/// The reader-session resource ([`ReadSession`]).
#[derive(Resource, Default)]
pub(crate) struct ItemTextOpen {
    pub(crate) pending: Option<ReadSession>,
}

impl ItemTextOpen {
    /// Open a read for a bag item (the right-click route in `drain_container_uses`). Re-opening
    /// (same or another letter) restarts the reference event flow from `ITEM_TEXT_BEGIN`.
    pub(crate) fn open(&mut self, item_guid: u64, text_id: u32) {
        self.pending = Some(ReadSession {
            item_guid,
            text_id,
            begun: false,
            ready: false,
        });
    }
}

pub(crate) struct UiItemTextPlugin;

impl Plugin for UiItemTextPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ItemTextOpen>().add_systems(
            Update,
            (
                // Feed before the input pass so an open paints the same frame; drain after it so
                // a Close click clears the same frame (the ui_mail ordering).
                feed_item_text.before(UiInput),
                drain_item_text.after(UiInput),
            ),
        );
    }
}

/// Drive the open session to `ITEM_TEXT_BEGIN`/`ITEM_TEXT_READY` as its pieces land: the title
/// (item template name), the body (the ask-once item-text cache), and the creator name (name
/// cache). BEGIN holds until the title resolves (the template is all but always cached — the bag
/// needed it for the icon); READY holds until the body and the creator line are both in.
#[allow(clippy::too_many_arguments)]
fn feed_item_text(
    script: Option<NonSendMut<UiScript>>,
    mut open: ResMut<ItemTextOpen>,
    mut mail: ResMut<MailOpen>,
    mut items: ResMut<Items>,
    mut names: ResMut<NameCache>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    let Some(sess) = open.pending.as_mut() else {
        return;
    };
    if sess.ready {
        return;
    }

    // The title: the item object's entry → template name.
    let title = items
        .object(sess.item_guid)
        .and_then(|o| o.object_entry())
        .and_then(|entry| {
            items
                .template(entry, sess.item_guid, &commands)
                .map(|t| t.name.clone())
        });
    let Some(title) = title else {
        return; // template in flight — BEGIN next frame
    };

    // The creator line: `ITEM_FIELD_CREATOR` → name cache (ask-once). `None` guid = authorless.
    let creator_guid = items.object(sess.item_guid).and_then(|o| o.item_creator());
    let creator = creator_guid.map(|g| names.resolve(g, &commands).map(str::to_string));

    // The body: the shared ask-once item-text cache (fetch if this is the first asker).
    let body = mail.bodies.get(&sess.text_id).cloned();
    if body.is_none() && mail.pending_bodies.insert(sess.text_id) {
        let _ = commands.0.send(ClientCommand::ItemTextQuery {
            text_id: sess.text_id,
            mail_id: 0,
        });
    }

    if !sess.begun {
        script.set_item_text(Some(ItemTextState {
            item: title.clone(),
            creator: None,
            text: String::new(),
            page: 1,
            has_next: false,
            material: None,
        }));
        script.fire_event("ITEM_TEXT_BEGIN", vec![]);
        sess.begun = true;
    }

    // READY once the body is in and the creator (if any) resolved.
    let creator = match creator {
        None => None,                   // authorless
        Some(Some(name)) => Some(name), // resolved
        Some(None) => return,           // name query in flight
    };
    let Some(text) = body else {
        return; // body query in flight
    };
    script.set_item_text(Some(ItemTextState {
        item: title,
        creator,
        text,
        page: 1,
        has_next: false,
        material: None,
    }));
    script.fire_event("ITEM_TEXT_READY", vec![]);
    sess.ready = true;
}

/// Drain the reader's intents: `CloseItemText()` clears the session (+ `ITEM_TEXT_CLOSED`, the
/// reference C-side answer the frame's OnHide relies on); page turns are dropped (single-page
/// letters — the buttons never show; books are the named follow-up).
fn drain_item_text(script: Option<NonSendMut<UiScript>>, mut open: ResMut<ItemTextOpen>) {
    let Some(mut script) = script else {
        return;
    };
    let _ = script.take_item_text_page_turns();
    if script.take_item_text_close() && open.pending.take().is_some() {
        script.set_item_text(None);
        script.fire_event("ITEM_TEXT_CLOSED", vec![]);
    }
}
