//! **Frame keyboard delivery** — the walk, the existence gate, and the key-name argument.
//!
//! Until this module, keys went straight to the focused EditBox and a plain frame could not receive
//! a key at all: `OnChar`/`OnKeyDown`/`OnKeyUp` were the three script names
//! [`super::object::events_regions`] deliberately *raised* on, because accepting a name this engine
//! never fires is the silent-drop class 1203/1205/1211 each recorded. This is the machinery that
//! note was waiting on, so those three names are now accepted there.
//!
//! ## The law (wow-re `system/ui/scratch/frame-key-script-delivery.md`, §5 trio — VERIFIED)
//!
//! An OS key event reaches **one dispatcher per channel**, and the dispatcher — not the frame — is
//! what walks: `0x765f10` (key-down, kind-1 buckets, vtable `+0x60`) and `0x765df0` (char, kind-0
//! buckets, `+0x5c`). Each walks **9 strata records, TOOLTIP (8) down to WORLD (0)**, and within a
//! record the frame array **index 0 upward**, which the inserter `0x764aa0` keeps ordered by
//! **level descending, ties oldest-registration-first**. Every frame in that order is *called*
//! until one returns nonzero; that one **consumes** and both loops stop. So several frames may be
//! called, at most one consumes.
//!
//! **The consumption gate is EXISTENCE, not handling** (`0x76b7d0`/`0x76bba0`/`0x76b760`, §3):
//!
//! | slot state | key-down returns | fires |
//! |---|---|---|
//! | `OnKeyDown` set | **consume** | `OnKeyDown` |
//! | only `OnKeyUp` set | **consume** | *nothing* |
//! | neither | decline | nothing |
//!
//! That asymmetry is real and is transcribed below: a frame carrying only an `OnKeyUp` swallows
//! every key-down and runs no script. `OnChar` gates on its own slot alone; `OnKeyUp` on `+0x190`
//! alone (no `OnKeyDown` fallback). **A 1.12 handler cannot signal "handled"** — the fire's return
//! is discarded at all three call sites (§3.1) — so a Lua handler never influences consumption.
//!
//! **Bucket membership is the keyboard-enabled flag**, never the presence of a script (§3.2):
//! `EnableKeyboard(true)` on a script-less frame puts it *in* the walk, where it is called and
//! declines. XML `enableKeyboard` enables both kinds; an XML `<Scripts>` block auto-enables per
//! handler; **Lua `SetScript` auto-enables nothing**, so a runtime-created frame needs an explicit
//! `EnableKeyboard(true)` exactly as in the real client.
//!
//! ## Where the EditBox sits — it is a participant, not a separate stage
//!
//! The reference has no "editbox first" rule: `CSimpleEditBox` is a frame in the same buckets whose
//! own vtable (`0x77b160` key-down / `0x77a900` char) consumes while focused and never chains to
//! the base. Modelling it as a stage before or after the walk would get the order wrong whenever a
//! keyboard frame and the focused box are in different strata — a TOOLTIP-strata frame really does
//! pre-empt a focused HIGH-strata box, and a WORLD-strata one really does not. So [`walk`] visits
//! the focused box **at its own strata/level**, handing it to the RF-0082 routing this crate
//! already had ([`super::editbox`]) and taking a `true` as consumption.
//!
//! What is deliberately NOT re-derived here: the box's own focus acquisition (`autoFocus`
//! self-acquire, click-to-focus, `SetFocus`) stays entirely in `editbox`, and an event no frame
//! consumed still falls through to it — so every existing routing test holds unchanged.
//!
//! ## Not modelled, named
//!
//! The **sticky per-key-code key-up slot** (§2.1: `0x765fd0` routes key-up to `[root+code*4+0x84]`,
//! the last frame that consumed *some* down for that code, and does **not** clear it on delivery)
//! is not built: this engine's host feeds no key-up at all today. `OnKeyUp` is therefore accepted,
//! stored, gated in the table above — and never fired. That is stated rather than hidden, because
//! its one observable consequence is live: a frame with only an `OnKeyUp` still consumes key-downs.

use mlua::Lua;

use super::{event, Model};
use crate::widget::FrameHandle;

/// Which channel a dispatch is on — the two walks the reference registers separately.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Channel {
    /// `0x765df0` — kind-0 buckets, vtable `+0x5c`, fires `OnChar` with the literal character.
    Char,
    /// `0x765f10` — kind-1 buckets, vtable `+0x60`, fires `OnKeyDown` with the key NAME.
    KeyDown,
}

/// The walk order (§2): every keyboard-enabled, effectively-visible frame, **strata descending,
/// then level descending, then oldest registration first**.
///
/// Built as a sorted `Vec` per dispatch rather than kept as nine live buckets. The reference
/// maintains the buckets incrementally because it is walking them on every key at 1.12 frame
/// budgets; here a key press is a human-rate event and the candidate set is the *keyboard-enabled*
/// frames only — single digits in any real interface — so an index that must be kept coherent with
/// every Show/Hide/SetFrameLevel/SetFrameStrata would be pure drift surface for no measurable win.
/// The ORDER is the law; the storage is ours.
fn walk_order(model: &Model) -> Vec<FrameHandle> {
    let mut candidates: Vec<(FrameHandle, u8, u16, u32)> = model
        .arena
        .iter_frames()
        .filter(|(_, f)| f.effective_visible && f.keyboard_enabled)
        .map(|(h, f)| (h, f.strata as u8, f.level, f.insertion_seq))
        .collect();
    // strata DESC, level DESC, insertion ASC — the inserter's own ordering (`0x764aa0`), where a
    // frame goes before the first entry of strictly lower level and equal levels keep registration
    // order.
    candidates.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.3.cmp(&b.3)));
    candidates.into_iter().map(|(h, ..)| h).collect()
}

/// Does `frame` have a script bound under `name`?
///
/// The existence test the C++ gate performs on `+0x180`/`+0x188`/`+0x190` — asked of the script
/// registry, which is where this engine keeps those slots.
fn has_script(lua: &Lua, id: u32, name: &str) -> bool {
    event::has_widget_handler(lua, id, name)
}

/// The dispatcher for one channel — [`Channel`]'s walk, gate and fire, in the reference's order.
///
/// Returns whether the event was **consumed**, which is what the caller must use to suppress the
/// keybinding system: consumption is decided by the C++ existence gate alone, never by what the
/// Lua handler did or returned (§3.1).
fn walk(lua: &Lua, channel: Channel, arg: &str) -> bool {
    let order = {
        let model = lua.app_data_ref::<Model>().expect("model app_data");
        walk_order(&model)
    };
    for h in order {
        // The focused EditBox consumes through its OWN vtable, never the base one — so it is asked
        // in its walk position and its answer is final for this event.
        let is_focused_box = {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            model.focused_editbox == Some(h)
        };
        if is_focused_box {
            let consumed = match channel {
                Channel::Char => super::editbox::char_input(lua, arg),
                Channel::KeyDown => super::editbox::key_input(lua, arg),
            };
            if consumed {
                return true;
            }
            continue;
        }
        let id = {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.frame_id(h)
        };
        match channel {
            // `0x76b760`: gates on the OnChar slot alone.
            Channel::Char => {
                if has_script(lua, id, "OnChar") {
                    fire(lua, id, "OnChar", arg);
                    return true;
                }
            }
            // `0x76b7d0`: the OR gate — either key slot consumes, only `OnKeyDown` fires.
            Channel::KeyDown => {
                let down = has_script(lua, id, "OnKeyDown");
                if down || has_script(lua, id, "OnKeyUp") {
                    if down {
                        fire(lua, id, "OnKeyDown", arg);
                    }
                    return true;
                }
            }
        }
    }
    false
}

/// Fire one key handler with its single string argument (`0x7026f0(frame, &slot, "%s", str)` — the
/// argument is a string on all three channels). Errors land in [`Model::errors`] like every other
/// widget handler; a raising handler must not eat the key press.
fn fire(lua: &Lua, id: u32, script: &str, arg: &str) {
    let val = mlua::Value::String(match lua.create_string(arg) {
        Ok(s) => s,
        Err(e) => {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .errors
                .push(e.to_string());
            return;
        }
    });
    if let Err(e) = event::fire_widget_handler(lua, id, script, vec![val]) {
        lua.app_data_mut::<Model>()
            .expect("model app_data")
            .errors
            .push(e.to_string());
    }
}

/// The char channel (`0x765df0`) — `arg1` is the literal character, UTF-8 (§4.1: the code is
/// UTF-8-encoded with no range guard, so a multi-byte codepoint arrives whole).
pub(super) fn char_input(lua: &Lua, text: &str) -> bool {
    walk(lua, Channel::Char, text)
}

/// The key-down channel (`0x765f10`) — `arg1` is the key NAME, with no modifier prefix, from the
/// same table the keybinding chord uses (§4.2). The host speaks those names already
/// (`crate::script::UiScript::key_input`'s contract), so no decode happens here.
pub(super) fn key_input(lua: &Lua, key: &str) -> bool {
    walk(lua, Channel::KeyDown, key)
}

/// The same key-down walk, for the keys this engine delivers to a **focused EditBox as a semantic
/// [`crate::script::EditAction`] chord** rather than by name — BACKSPACE, DELETE, the arrows, HOME,
/// END (decision 0301: the host's per-OS keymap owns which chord means what, so the box never sees
/// these as names).
///
/// Those keys still have to reach a keyboard frame — a dialog you type into needs its BACKSPACE —
/// and the ordering question that creates is real: whoever comes FIRST in the walk owns the key.
/// So this runs the identical order and **declines at the focused box** rather than skipping past
/// it: a frame above the box in walk order consumes and fires; a box above the frame ends the walk
/// with `false`, and the caller then dispatches its chord exactly as before. The one thing this
/// must never do is let a frame *below* the focused box steal the box's editing keys.
pub(super) fn frame_key_input(lua: &Lua, key: &str) -> bool {
    let order = {
        let model = lua.app_data_ref::<Model>().expect("model app_data");
        walk_order(&model)
    };
    for h in order {
        {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            if model.focused_editbox == Some(h) {
                return false; // the box owns this key; its chord path handles it
            }
        }
        let id = {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.frame_id(h)
        };
        let down = has_script(lua, id, "OnKeyDown");
        if down || has_script(lua, id, "OnKeyUp") {
            if down {
                fire(lua, id, "OnKeyDown", key);
            }
            return true;
        }
    }
    false
}
