//! The item-text reader seam — the `ItemTextFrame` (letters, books, plaques) Lua surface.
//!
//! The app owns the read session (`crate::ui_item_text` on the benilla side): right-clicking a bag
//! item whose `ITEM_FIELD_ITEM_TEXT_ID` is set opens a read instead of a `CMSG_USE_ITEM`, fetches
//! the text over the ask-once `CMSG_ITEM_TEXT_QUERY` cache (the same wire mail letter bodies ride),
//! then pushes an [`ItemTextState`] here and fires the reference event pair `ITEM_TEXT_BEGIN` →
//! `ITEM_TEXT_READY` (ItemTextFrame.lua l.11/38). This module is only the Lua-facing mirror:
//!
//! - Getters over the pushed state: `ItemTextGetItem()` (the title), `ItemTextGetCreator()`,
//!   `ItemTextGetText()`, `ItemTextGetPage()`, `ItemTextHasNextPage()`, `ItemTextGetMaterial()`
//!   (`nil` → the Lua's own "Parchment" default, l.19-21).
//! - Intents the app drains: `CloseItemText()` (the frame's OnHide, l.349 — the app clears the
//!   session and fires `ITEM_TEXT_CLOSED`), `ItemTextPrevPage()`/`ItemTextNextPage()` (multi-page
//!   `PageText` books — a named follow-up; single-page letters never show the buttons).

use mlua::{Lua, Value};

use super::Model;

/// The open read session's Lua-visible snapshot, pushed whole by the app.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemTextState {
    /// The read item's name (`ItemTextGetItem()` — the window title, e.g. "Plain Letter").
    pub item: String,
    /// The letter's author (`ItemTextGetCreator()` — `ITEM_FIELD_CREATOR` resolved through the
    /// name cache); `None` for an authorless text (books) — the Lua skips the "From," tail.
    pub creator: Option<String>,
    /// The current page's text (`ItemTextGetText()`).
    pub text: String,
    /// The 1-based page (`ItemTextGetPage()`); letters are always page 1.
    pub page: u32,
    /// Whether a next page exists (`ItemTextHasNextPage()`); always `false` for letters.
    pub has_next: bool,
    /// The page material basename (`ItemTextGetMaterial()`, e.g. "Stone"); `None` = the default
    /// parchment (the Lua substitutes "Parchment" and hides the material corners).
    pub material: Option<String>,
}

impl super::UiScript {
    /// Push (or clear, with `None`) the open read session. The app pairs the push with the
    /// reference event flow: set → `ITEM_TEXT_BEGIN` → (text fetched) → `ITEM_TEXT_READY`;
    /// clear → `ITEM_TEXT_CLOSED`.
    pub fn set_item_text(&mut self, state: Option<ItemTextState>) {
        self.model_mut().item_text = state;
    }

    /// Whether `CloseItemText()` was called since the last drain (and clear the flag).
    pub fn take_item_text_close(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().item_text_close)
    }

    /// Drain the `ItemTextPrevPage()`/`ItemTextNextPage()` turns (`-1`/`+1`, in click order).
    pub fn take_item_text_page_turns(&mut self) -> Vec<i32> {
        std::mem::take(&mut self.model_mut().item_text_page_turns)
    }
}

/// Register the item-text globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    g.set(
        "ItemTextGetItem",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model
                .item_text
                .as_ref()
                .map(|s| s.item.clone())
                .unwrap_or_default())
        })?,
    )?;

    g.set(
        "ItemTextGetCreator",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(
                match model.item_text.as_ref().and_then(|s| s.creator.clone()) {
                    Some(c) => Value::String(lua.create_string(&c)?),
                    None => Value::Nil,
                },
            )
        })?,
    )?;

    g.set(
        "ItemTextGetText",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model
                .item_text
                .as_ref()
                .map(|s| s.text.clone())
                .unwrap_or_default())
        })?,
    )?;

    g.set(
        "ItemTextGetPage",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.item_text.as_ref().map_or(1, |s| i64::from(s.page)))
        })?,
    )?;

    g.set(
        "ItemTextHasNextPage",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(match model.item_text.as_ref().is_some_and(|s| s.has_next) {
                true => Value::Integer(1),
                false => Value::Nil,
            })
        })?,
    )?;

    g.set(
        "ItemTextGetMaterial",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(
                match model.item_text.as_ref().and_then(|s| s.material.clone()) {
                    Some(m) => Value::String(lua.create_string(&m)?),
                    None => Value::Nil,
                },
            )
        })?,
    )?;

    g.set(
        "CloseItemText",
        lua.create_function(|lua, ()| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .item_text_close = true;
            Ok(())
        })?,
    )?;

    for (name, delta) in [("ItemTextPrevPage", -1i32), ("ItemTextNextPage", 1)] {
        g.set(
            name,
            lua.create_function(move |lua, ()| {
                lua.app_data_mut::<Model>()
                    .expect("model app_data")
                    .item_text_page_turns
                    .push(delta);
                Ok(())
            })?,
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::UiScript;

    fn state() -> ItemTextState {
        ItemTextState {
            item: "Plain Letter".into(),
            creator: Some("One".into()),
            text: "asd".into(),
            page: 1,
            has_next: false,
            material: None,
        }
    }

    #[test]
    fn getters_mirror_the_pushed_state() {
        let mut s = UiScript::new().unwrap();
        // No session: benign defaults (the frame only reads these while shown anyway).
        assert_eq!(s.eval::<String>("return ItemTextGetItem()").unwrap(), "");
        assert!(s
            .eval::<bool>("return ItemTextGetCreator() == nil")
            .unwrap());

        s.set_item_text(Some(state()));
        assert_eq!(
            s.eval::<String>("return ItemTextGetItem()").unwrap(),
            "Plain Letter"
        );
        assert_eq!(
            s.eval::<String>("return ItemTextGetCreator()").unwrap(),
            "One"
        );
        assert_eq!(s.eval::<String>("return ItemTextGetText()").unwrap(), "asd");
        assert_eq!(s.eval::<i64>("return ItemTextGetPage()").unwrap(), 1);
        assert!(s
            .eval::<bool>("return ItemTextHasNextPage() == nil")
            .unwrap());
        assert!(s
            .eval::<bool>("return ItemTextGetMaterial() == nil")
            .unwrap());
    }

    #[test]
    fn close_and_page_intents_queue_and_drain() {
        let mut s = UiScript::new().unwrap();
        s.set_item_text(Some(state()));
        assert!(!s.take_item_text_close());
        s.run("CloseItemText()").unwrap();
        assert!(s.take_item_text_close());
        assert!(!s.take_item_text_close(), "drained");

        s.run("ItemTextNextPage() ItemTextPrevPage()").unwrap();
        assert_eq!(s.take_item_text_page_turns(), vec![1, -1]);
        assert!(s.take_item_text_page_turns().is_empty(), "drained");
    }
}
