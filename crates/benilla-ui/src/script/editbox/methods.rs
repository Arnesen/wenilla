//! The EditBox Lua method surface — `install` builds the method table
//! (`REG_EDITBOX_METHODS`) the dispatcher consults before the shared frame table for EditBox
//! frames: text/cursor/selection accessors, focus, history, the config setters, and the blink
//! dial. Every method routes through the mother module's primitives (`set_text`,
//! `highlight_text`, …), so the byte-verified law lives exactly once.

use mlua::{Lua, Table, Value};

use crate::script::object::frame_handle_of;
use crate::script::Model;
use crate::widget::EditBoxState;

use super::{
    clear_focus_handle, highlight_text, insert, set_focus_handle, set_text, set_text_insets,
    with_eb, REG_EDITBOX_METHODS,
};

/// Run `f` over a frame's EditBox state for a Lua method call; errors if `this` is not a live EditBox.
fn with_editbox<T>(
    lua: &Lua,
    this: &Table,
    f: impl FnOnce(&mut EditBoxState) -> T,
) -> mlua::Result<T> {
    let h = frame_handle_of(lua, this)?;
    with_eb(lua, h, f).ok_or_else(|| mlua::Error::runtime("not an EditBox"))
}

pub(in crate::script) fn install(lua: &Lua) -> mlua::Result<()> {
    let m = lua.create_table()?;

    m.set(
        "SetText",
        lua.create_function(|lua, (this, s): (Table, Option<String>)| {
            let h = frame_handle_of(lua, &this)?;
            // Programmatic SetText KEEPS a history browse in progress: the chat live parse
            // rewrites the box on every recalled slash line ("/s hi" → Say + "hi"), and ending
            // the browse there reset every UP to the newest entry — "history only goes back 1"
            // (director's report, 2026-07-26). The browse ends on typed edits, AddHistoryLine,
            // and focus gain (`set_focus_handle`) — the fresh-session reset.
            set_text(lua, h, &s.unwrap_or_default());
            Ok(())
        })?,
    )?;
    m.set(
        "GetText",
        lua.create_function(|lua, this: Table| with_editbox(lua, &this, |eb| eb.text.clone()))?,
    )?;
    // GetNumber: atof of the real text (0 on failure), matching `0x798790`.
    m.set(
        "GetNumber",
        lua.create_function(|lua, this: Table| {
            let text = with_editbox(lua, &this, |eb| eb.text.clone())?;
            Ok(text.trim().parse::<f64>().unwrap_or(0.0))
        })?,
    )?;
    m.set(
        "Insert",
        lua.create_function(|lua, (this, s): (Table, String)| {
            let h = frame_handle_of(lua, &this)?;
            insert(lua, h, &s, true);
            Ok(())
        })?,
    )?;

    m.set(
        "SetFocus",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            set_focus_handle(lua, h);
            Ok(())
        })?,
    )?;
    m.set(
        "ClearFocus",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            clear_focus_handle(lua, h);
            Ok(())
        })?,
    )?;
    m.set(
        "HasFocus",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.focused_editbox == Some(h))
        })?,
    )?;

    // HighlightText([start [, end]]) — defaults (0, -1) = select-all.
    m.set(
        "HighlightText",
        lua.create_function(
            |lua, (this, start, end): (Table, Option<i64>, Option<i64>)| {
                let h = frame_handle_of(lua, &this)?;
                highlight_text(lua, h, start.unwrap_or(0), end.unwrap_or(-1));
                Ok(())
            },
        )?,
    )?;

    m.set(
        "SetMaxLetters",
        lua.create_function(|lua, (this, n): (Table, i64)| {
            with_editbox(lua, &this, |eb| eb.max_letters = n.max(0) as usize)
        })?,
    )?;
    // The submitted-line history (`historyLines`): FrameXML pushes each sent line
    // (ChatEdit_AddHistory), UP/DOWN recall it (see `key_input`).
    m.set(
        "AddHistoryLine",
        lua.create_function(|lua, (this, line): (Table, Option<String>)| {
            with_editbox(lua, &this, |eb| {
                eb.add_history_line(line.as_deref().unwrap_or(""));
            })
        })?,
    )?;
    m.set(
        "SetHistoryLines",
        lua.create_function(|lua, (this, n): (Table, i64)| {
            with_editbox(lua, &this, |eb| {
                eb.history_max = n.max(0) as usize;
                let over = eb.history.len().saturating_sub(eb.history_max);
                if over > 0 {
                    eb.history.drain(..over);
                }
            })
        })?,
    )?;
    m.set(
        "GetHistoryLines",
        lua.create_function(|lua, this: Table| {
            with_editbox(lua, &this, |eb| eb.history_max as i64)
        })?,
    )?;
    // SetBlinkSpeed/GetBlinkSpeed — the caret half-period (`E+0x370`, XML `blinkSpeed`; ctor 0.5).
    m.set(
        "SetBlinkSpeed",
        lua.create_function(|lua, (this, s): (Table, f32)| {
            with_editbox(lua, &this, |eb| eb.blink_period = s)
        })?,
    )?;
    m.set(
        "GetBlinkSpeed",
        lua.create_function(|lua, this: Table| with_editbox(lua, &this, |eb| eb.blink_period))?,
    )?;
    // SetTextColor(r, g, b [, a]) — the FontInstance half an EditBox inherits in the client
    // (`ChatEdit_UpdateHeader` colors the typed text this way): tint the box's text region,
    // creating it lazily like every text-touching path.
    m.set(
        "SetTextColor",
        lua.create_function(
            |lua, (this, r, g, b, a): (Table, f32, f32, f32, Option<f32>)| {
                let h = frame_handle_of(lua, &this)?;
                let Some(rh) = super::ensure_text_region(lua, h) else {
                    return Err(mlua::Error::runtime("not an EditBox"));
                };
                lua.app_data_mut::<Model>()
                    .expect("model app_data")
                    .region_data
                    .entry(rh)
                    .or_default()
                    .vertex_color = Some([r, g, b, a.unwrap_or(1.0)]);
                Ok(())
            },
        )?,
    )?;
    m.set(
        "SetTextInsets",
        lua.create_function(
            |lua,
             (this, l, r, t, b): (
                Table,
                Option<f32>,
                Option<f32>,
                Option<f32>,
                Option<f32>,
            )| {
                let h = frame_handle_of(lua, &this)?;
                set_text_insets(
                    lua,
                    h,
                    l.unwrap_or(0.0),
                    r.unwrap_or(0.0),
                    t.unwrap_or(0.0),
                    b.unwrap_or(0.0),
                );
                Ok(())
            },
        )?,
    )?;
    m.set(
        "GetTextInsets",
        lua.create_function(|lua, this: Table| {
            let ins = with_editbox(lua, &this, |eb| eb.text_insets)?;
            Ok((ins[0], ins[1], ins[2], ins[3]))
        })?,
    )?;
    // GetNumLetters: the LETTER count — `0x7992c0` walks the class array (`0x77bc80`), which
    // counts classes 2, 3 and 6 only, so escapes are free and a 43-byte item link reports 9
    // (decision 1077). Not bytes, and not chars either.
    m.set(
        "GetNumLetters",
        lua.create_function(|lua, this: Table| {
            with_editbox(lua, &this, |eb| {
                crate::markup::ClassMap::new(&eb.text).num_letters() as i64
            })
        })?,
    )?;

    // The config flags. SetAutoFocus/SetNumeric/SetPassword/SetMultiLine mirror the live API;
    // SetIgnoreArrows has no client method (the flag is XML-only) — a benilla convenience so the
    // loader can drive every flag through one method surface.
    for (name, set) in flag_setters() {
        let refresh_justify = name == "SetMultiLine";
        m.set(
            name,
            lua.create_function(move |lua, (this, v): (Table, Value)| {
                let on = !matches!(v, Value::Nil | Value::Boolean(false));
                with_editbox(lua, &this, |eb| set(eb, on))?;
                if refresh_justify {
                    // The text-anchoring law reads multiLine (TOP vs MIDDLE), and the loader
                    // wires the declared `<FontString>` BEFORE the editbox flags (LoadXML order
                    // 5·b vs 5b) — re-seat an already-wired region so `multiLine="true"` lands.
                    super::refresh_text_region_justify(lua, &this)?;
                }
                Ok(())
            })?,
        )?;
    }

    lua.set_named_registry_value(REG_EDITBOX_METHODS, m)?;
    Ok(())
}

/// A `Set<Flag>` name paired with the EditBoxState mutator it drives (Lua truthiness).
type FlagSetter = (&'static str, fn(&mut EditBoxState, bool));

/// The config-flag setters, in one place so the loader and the Lua surface share the list.
fn flag_setters() -> [FlagSetter; 5] {
    [
        ("SetAutoFocus", |eb, on| eb.auto_focus = on),
        ("SetNumeric", |eb, on| eb.numeric = on),
        ("SetPassword", |eb, on| eb.password = on),
        ("SetMultiLine", |eb, on| eb.multi_line = on),
        ("SetIgnoreArrows", |eb, on| eb.ignore_arrows = on),
    ]
}
