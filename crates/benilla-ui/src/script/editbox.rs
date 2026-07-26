//! The `EditBox` runtime — keyboard focus, the text buffer + cursor + selection, editing, and the
//! key/char dispatch (`CSimpleEditBox`, factory `0x6eec70`).
//!
//! Grounded in wow-5875-re's byte-verified RF-0082 (`rf82-editbox-runtime.md`):
//!
//! - **Focus (§1):** a single class-owned focus owner ([`Model::focused_editbox`], the client's
//!   `DAT_00cf4dc8`). `SetFocus` gates on effective-visibility, is a no-op if already focused, and
//!   fires `OnEditFocusLost` on the old box then `OnEditFocusGained` on the new. `autoFocus` does NOT
//!   focus on show — it only lets a box self-acquire on the first keyboard/char event while nothing is
//!   focused (verified by absence: no OnShow/OnLoad `SetFocus` caller). A LeftButton click focuses
//!   UNCONDITIONALLY.
//! - **Routing (§2):** a focused box processes and CONSUMES every key/char (`return 1` past the
//!   guard); an unfocused non-autoFocus box ignores input. The override fires ONLY the specialized
//!   scripts (Enter/Escape/Space/Tab/TextChanged/TextSet/focus), never generic `OnKeyDown`/`OnChar`.
//! - **Text/editing (§3/§4):** every insert replaces the selection first; `numeric` aborts an insert
//!   wholesale on any non-digit; caps trim from the end (`maxBytes` then `maxLetters`); `SetText`
//!   short-circuits when unchanged; `HighlightText(0,-1)` selects all with the client's clamp.
//!
//! The mouse/selection/caret half (click→index `0x77d0d0`, drag `0x77a860`, the clipboard pair,
//! the 0.5 s blink, the char-granular scroll window) lives in [`interact`]/[`seam`] over the
//! host-answered advance table — decision 0298. The OS clipboard itself stays host-side (this
//! crate is engine-free): paste text arrives via [`paste`], copy/cut strings return through
//! `UiScript::editbox_copy`/`editbox_cut`; Ctrl+A arrives as the SOH control char.
//!
//! Editing keys reach the box as semantic [`EditAction`]s ([`action`]) — the *host's* per-OS
//! keymap decides which physical chord means which action (decision 0301), while the effect of
//! each action stays this module's byte-verified law. [`key_input`] keeps only the three
//! box-event keys (ENTER/ESCAPE/TAB).
//!
//! ## Stated divergences / gaps
//! - **`OnCursorChanged`** (the 4-float caret-pos fire) is not fired — no shipped XML consumes it.
//! - Small INFERRED corners (click rounding, word classes, no shift+click, the password-copy
//!   placeholder literal) are flagged where they live — decision 0298's list.
//! - **Word/edge deletes** (`Delete{Word,Edge}`) have no 1.12 counterpart at all — they exist for
//!   the host's OS-native keymaps (Option/Cmd+Backspace on macOS, Ctrl+Backspace/Delete on
//!   Windows/Linux; decision 0301) and reuse the byte-verified word classes + selection-first
//!   delete law.

use mlua::{Lua, Table};

use super::object::frame_handle_of;
use super::types::{EditAction, EditUnit};
use super::{event, Model, RegionData};
use crate::layout::{Anchor, Point};
use crate::order::{self, DrawLayer, ZTarget};
use crate::widget::{EditBoxState, FrameHandle, FrameKind, KindState, RegionHandle, RegionKind};

/// Registry key of the EditBox method table (the MAXCSTACK discipline: Lua-side root, named key).
pub(super) const REG_EDITBOX_METHODS: &str = "__benilla_editbox_methods";

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Public entry points (called from UiScript::char_input / key_input / mouse_button)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A typed character. Routes per §1/§2; on a focused box, Ctrl+A (delivered as the SOH control char)
/// selects all, other C0 control chars are consumed-but-inert, and printable text is inserted. Always
/// consumes when a box is (or becomes) focused.
pub(super) fn char_input(lua: &Lua, text: &str) -> bool {
    let Some(h) = route(lua) else {
        return false;
    };
    if text == "\u{1}" {
        // Ctrl+A → select-all (the client's keydown case 0x41/0x61; here via char_input since a
        // Ctrl-modified letter arrives as its control code).
        highlight_text(lua, h, 0, -1);
    } else if text.chars().any(|c| (c as u32) >= 0x20) {
        // A printable char (or string); pure control input is consumed but not inserted.
        insert(lua, h, text, true);
    }
    true
}

/// Paste an OS-clipboard string into the focused box. The engine-free runtime can't reach the OS
/// clipboard itself (RF-0082's stated gap), so the host reads it and hands the text here. Sanitized
/// the same way a typed char is gated — every C0 control char is dropped, except a newline in a
/// `multiLine` box — then inserted as one edit (selection-replace + numeric/caps rules via [`insert`],
/// no `OnSpacePressed` fire: a paste is not a typed space). Always consumes when a box is focused.
pub(super) fn paste(lua: &Lua, text: &str) -> bool {
    let Some(h) = route(lua) else {
        return false;
    };
    let multi_line = with_eb(lua, h, |eb| eb.multi_line).unwrap_or(false);
    let cleaned: String = text
        .chars()
        .filter(|&c| c as u32 >= 0x20 || (multi_line && c == '\n'))
        .collect();
    if !cleaned.is_empty() {
        insert(lua, h, &cleaned, false);
    }
    true
}

/// A non-character key by name — only the three *box-event* keys act (their FrameXML scripts);
/// every editing key reaches the box as a semantic [`EditAction`] via [`action`] instead. Routes
/// per §1/§2; a focused box consumes the key even when it does nothing with it.
pub(super) fn key_input(lua: &Lua, key: &str) -> bool {
    let Some(h) = route(lua) else {
        return false;
    };
    let id = frame_id_of(lua, h);
    match key.to_ascii_uppercase().as_str() {
        // multiLine: Enter inserts a newline (no OnSpacePressed); else fire OnEnterPressed.
        "ENTER" => {
            if with_eb(lua, h, |eb| eb.multi_line).unwrap_or(false) {
                insert(lua, h, "\n", false);
            } else {
                fire_script(lua, id, "OnEnterPressed");
            }
        }
        // Escape fires the script but does NOT release focus (FrameXML calls ClearFocus itself).
        "ESCAPE" => fire_script(lua, id, "OnEscapePressed"),
        "TAB" => fire_script(lua, id, "OnTabPressed"),
        // Any other named key: a focused box still consumes it (past the guard), doing nothing.
        _ => {}
    }
    true
}

/// One semantic editing operation — the host's per-OS keymap output (decision 0301). Same
/// routing/consumption law as [`key_input`]: the focused (or self-acquiring per §2) box processes
/// it; no box → not consumed.
pub(super) fn action(lua: &Lua, a: EditAction) -> bool {
    let Some(h) = route(lua) else {
        return false;
    };
    match a {
        EditAction::Move { unit, back, extend } => match unit {
            // ignoreArrows: consumed but inert (guard `0x77b18e`) — arrows are the only chord
            // source of Char moves, and the ref's Ctrl bypass maps to Word, which passes.
            EditUnit::Char => {
                if !with_eb(lua, h, |eb| eb.ignore_arrows).unwrap_or(false) {
                    move_horizontal(lua, h, !back, extend);
                }
            }
            // Ctrl/Option picks the word-granular cursor helper (RF-0082 §4: "char- vs
            // word-granular by the Ctrl check").
            EditUnit::Word => interact::move_word(lua, h, !back, extend),
            EditUnit::Edge => move_to_edge(lua, h, !back, extend),
        },
        EditAction::Delete { unit, back } => match unit {
            EditUnit::Char => delete_dir(lua, h, !back),
            EditUnit::Word => delete_span(lua, h, |eb| eb.word_boundary(!back)),
            EditUnit::Edge => delete_span(lua, h, |eb| if back { 0 } else { eb.text.len() }),
        },
        EditAction::SelectAll => highlight_text(lua, h, 0, -1),
        // History recall (the chat box's `historyLines`): prev = older, next = newer, live draft
        // restored past the newest. Single-line only (benilla's multiLine box has no vertical
        // caret nav — survey gap — a multiLine box consumes the step inert). The recall chords
        // are the host keymap's plain Up/Down (rf82's history controller is untraced; decision
        // 0301) — `ignoreArrows` does not gate them: the ref chat box ships BOTH ignoreArrows and
        // historyLines, so arrows-as-history is exactly what that combination leaves.
        EditAction::HistoryPrev | EditAction::HistoryNext => {
            if !with_eb(lua, h, |eb| eb.multi_line).unwrap_or(false) {
                history_step_key(lua, h, a == EditAction::HistoryPrev);
            }
        }
    }
    true
}

// The mouse/selection interaction law (click→index, drag-select, clipboard, blink) — a child
// module over this file's focus/editing primitives (RF-0082 §1/§4 + the diffed mouse leaves) —
// and the host-facing seam (`impl UiScript`: the advance round trip + text-UI geometry).
mod interact;
mod seam;
pub(super) use interact::{
    click, copy_selection, cut_selection, drag_end, drag_update, tick_blink,
};

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Focus model (RF-0082 §1)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Resolve the box that should process this key/char event, self-acquiring per §2 if needed.
///
/// - A live, effectively-visible focused box → process it.
/// - A focused box that is hidden (still alive) → `None`: the client's guard returns 0 and the focus
///   global stays set, so no other box self-acquires while it "holds" focus.
/// - A focused handle that went stale → cleared, then fall through to self-acquire.
/// - No focus → the topmost effectively-visible `autoFocus` EditBox self-acquires (firing
///   `OnEditFocusGained`) and processes this same event; otherwise `None` (not consumed).
fn route(lua: &Lua) -> Option<FrameHandle> {
    {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        if let Some(h) = model.focused_editbox {
            match model.arena.frame(h) {
                Some(f) if f.effective_visible && f.kind == FrameKind::EditBox => return Some(h),
                Some(_) => return None, // alive but hidden: block self-acquire, don't consume
                None => model.focused_editbox = None, // stale: drop and self-acquire below
            }
        }
    }
    let h = topmost_autofocus(lua)?;
    set_focus_handle(lua, h);
    Some(h)
}

/// The topmost-drawn effectively-visible `autoFocus` EditBox (the keyboard self-acquire target), or
/// `None`. Walks the draw order top-down like [`crate::order::hit_test`].
fn topmost_autofocus(lua: &Lua) -> Option<FrameHandle> {
    let model = lua.app_data_ref::<Model>().expect("model app_data");
    let sorted = order::traversal(&model.arena);
    for (target, _) in sorted.iter().rev() {
        if let ZTarget::Frame(fh) = *target {
            if let Some(KindState::EditBox(eb)) = model.arena.frame(fh).map(|f| &f.kind_state) {
                if eb.auto_focus {
                    return Some(fh);
                }
            }
        }
    }
    None
}

/// `SetFocus` (`0x77e3d0`): gate on effective-visibility, no-op if already focused; else fire
/// `OnEditFocusLost` on the old box, move focus, fire `OnEditFocusGained` on the new.
fn set_focus_handle(lua: &Lua, h: FrameHandle) {
    let (old_id, new_id) = {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        let focusable = matches!(
            model.arena.frame(h),
            Some(f) if f.effective_visible && f.kind == FrameKind::EditBox
        );
        if !focusable || model.focused_editbox == Some(h) {
            return;
        }
        let old = model.focused_editbox;
        model.focused_editbox = Some(h);
        // A fresh focus starts a fresh history session: the browse position must not leak from
        // the box's previous open (programmatic SetText deliberately preserves it — the chat
        // live parse's recall rewrite — so this is where a stale walk gets dropped).
        if let Some(KindState::EditBox(eb)) = model.arena.frame_mut(h).map(|f| &mut f.kind_state) {
            eb.end_history_browse();
        }
        (old.map(|o| model.frame_id(o)), model.frame_id(h))
    };
    if let Some(oid) = old_id {
        fire_script(lua, oid, "OnEditFocusLost");
    }
    fire_script(lua, new_id, "OnEditFocusGained");
}

/// `ClearFocus` (`0x77e410`): only if this box holds focus; fire `OnEditFocusLost`.
fn clear_focus_handle(lua: &Lua, h: FrameHandle) {
    let id = {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        if model.focused_editbox != Some(h) {
            return;
        }
        model.focused_editbox = None;
        model.frame_id(h)
    };
    fire_script(lua, id, "OnEditFocusLost");
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Editing primitives (RF-0082 §3/§4) — mutate state under one borrow, then sync + fire
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Insert `ins` at the cursor (`0x77bee0`): replace any selection first; `numeric` aborts wholesale on
/// a non-digit; splice + advance; enforce caps; fire `OnTextChanged`, and (when `fire_space`) one
/// `OnSpacePressed` per space in `ins`.
fn insert(lua: &Lua, h: FrameHandle, ins: &str, fire_space: bool) {
    let spaces = with_eb(lua, h, |eb| {
        if eb.numeric && !ins.chars().all(|c| c.is_ascii_digit()) {
            return None; // numeric: any non-digit aborts the whole insert
        }
        eb.end_history_browse(); // a typed edit turns the recalled line into an ordinary draft
        delete_selection(eb);
        eb.text.insert_str(eb.cursor, ins);
        eb.cursor += ins.len();
        collapse(eb);
        enforce_caps(eb);
        eb.reset_blink();
        Some(if fire_space {
            ins.matches(' ').count()
        } else {
            0
        })
    });
    // Some(Some(n)) inserted (n spaces); Some(None) numeric-aborted; None not an EditBox.
    if let Some(Some(spaces)) = spaces {
        sync_text_region(lua, h);
        let id = frame_id_of(lua, h);
        fire_script(lua, id, "OnTextChanged");
        for _ in 0..spaces {
            fire_script(lua, id, "OnSpacePressed");
        }
    }
}

/// `SetText` (`0x77be00`): short-circuit when unchanged (no events); else clear selection, replace,
/// cursor to end, enforce caps, fire `OnTextSet` then `OnTextChanged`.
fn set_text(lua: &Lua, h: FrameHandle, s: &str) {
    let changed = with_eb(lua, h, |eb| {
        if eb.text == s {
            return false;
        }
        eb.text = s.to_string();
        eb.cursor = eb.text.len();
        collapse(eb);
        enforce_caps(eb);
        eb.reset_blink();
        true
    });
    if changed == Some(true) {
        sync_text_region(lua, h);
        let id = frame_id_of(lua, h);
        fire_script(lua, id, "OnTextSet");
        fire_script(lua, id, "OnTextChanged");
    }
}

/// Word/edge deletes (`Delete{Word,Edge}` — no 1.12 counterpart; the OS-native keymap family):
/// the selection first when one exists, else the span from the cursor to `target_of(eb)`; sync +
/// `OnTextChanged` when anything was removed.
fn delete_span(lua: &Lua, h: FrameHandle, target_of: impl FnOnce(&EditBoxState) -> usize) {
    let changed = with_eb(lua, h, |eb| {
        eb.end_history_browse(); // an edit ends any recall browse
        let did = if eb.sel_start != eb.sel_end {
            delete_selection(eb);
            true
        } else {
            let t = target_of(eb);
            let (a, b) = (t.min(eb.cursor), t.max(eb.cursor));
            if a == b {
                false
            } else {
                eb.text.replace_range(a..b, "");
                eb.cursor = a;
                collapse(eb);
                true
            }
        };
        if did {
            eb.reset_blink();
        }
        did
    });
    if changed == Some(true) {
        sync_text_region(lua, h);
        fire_script(lua, frame_id_of(lua, h), "OnTextChanged");
    }
}

/// BACKSPACE / DELETE: delete the selection if non-empty, else the char before (`forward=false`) or
/// after (`forward=true`) the cursor; fire `OnTextChanged` if anything was removed.
fn delete_dir(lua: &Lua, h: FrameHandle, forward: bool) {
    let changed = with_eb(lua, h, |eb| {
        eb.end_history_browse(); // a typed edit ends any recall browse
        let did = 'del: {
            if eb.sel_start != eb.sel_end {
                delete_selection(eb);
                break 'del true;
            }
            if forward {
                if eb.cursor < eb.text.len() {
                    let end = next_boundary(&eb.text, eb.cursor);
                    eb.text.replace_range(eb.cursor..end, "");
                    break 'del true;
                }
            } else if eb.cursor > 0 {
                let start = prev_boundary(&eb.text, eb.cursor);
                eb.text.replace_range(start..eb.cursor, "");
                eb.cursor = start;
                collapse(eb);
                break 'del true;
            }
            false
        };
        if did {
            eb.reset_blink();
        }
        did
    });
    if changed == Some(true) {
        sync_text_region(lua, h);
        fire_script(lua, frame_id_of(lua, h), "OnTextChanged");
    }
}

/// One UP/DOWN history-recall step: ask the state for the line to show and route it through the
/// internal `set_text` path (region sync + `OnTextSet`/`OnTextChanged` — a recall resets FrameXML
/// tab-complete state exactly like any other text change). `set_text` (internal AND Lua) leaves
/// the browse position intact; typed edits, `AddHistoryLine`, and focus gain end browsing.
fn history_step_key(lua: &Lua, h: FrameHandle, older: bool) {
    let recalled = with_eb(lua, h, |eb| eb.history_step(older)).flatten();
    if let Some(text) = recalled {
        set_text(lua, h, &text);
    }
}

/// LEFT/RIGHT one char: `shift` extends the selection from the fixed anchor; otherwise the caret
/// collapses to the selection edge (if any) or moves one char. Fires no script (a cursor/selection
/// move; `OnCursorChanged` geometry is out of scope).
fn move_horizontal(lua: &Lua, h: FrameHandle, right: bool, shift: bool) {
    with_eb(lua, h, |eb| {
        if shift {
            let anchor = selection_anchor(eb);
            eb.cursor = if right {
                next_boundary(&eb.text, eb.cursor)
            } else {
                prev_boundary(&eb.text, eb.cursor)
            };
            set_span(eb, anchor, eb.cursor);
        } else if eb.sel_start != eb.sel_end {
            eb.cursor = if right { eb.sel_end } else { eb.sel_start };
            collapse(eb);
        } else {
            eb.cursor = if right {
                next_boundary(&eb.text, eb.cursor)
            } else {
                prev_boundary(&eb.text, eb.cursor)
            };
            collapse(eb);
        }
        eb.reset_blink();
    });
}

/// HOME/END: move the caret to 0 / `len`; `shift` extends from the fixed anchor.
fn move_to_edge(lua: &Lua, h: FrameHandle, end: bool, shift: bool) {
    with_eb(lua, h, |eb| {
        let target = if end { eb.text.len() } else { 0 };
        if shift {
            let anchor = selection_anchor(eb);
            eb.cursor = target;
            set_span(eb, anchor, target);
        } else {
            eb.cursor = target;
            collapse(eb);
        }
        eb.reset_blink();
    });
}

/// `HighlightText` (`0x77cca0`), the client's exact clamp: `start = clamp(start, 0..=len)`;
/// `end = (end < 0 || end > len) ? len : end`; then `if end < start { end = len }` — so `(0, -1)`
/// selects all. Offsets are bytes, snapped to char boundaries.
fn highlight_text(lua: &Lua, h: FrameHandle, start: i64, end: i64) {
    with_eb(lua, h, |eb| {
        let len = eb.text.len() as i64;
        let s = start.clamp(0, len);
        let mut e = if end < 0 || end > len { len } else { end };
        if e < s {
            e = len;
        }
        let s = snap_down(&eb.text, s as usize);
        let e = snap_down(&eb.text, e as usize);
        eb.sel_start = s;
        eb.sel_end = e;
        eb.cursor = e;
        eb.reset_blink();
    });
}

// ── state helpers (pure over EditBoxState) ───────────────────────────────────────────────────

/// Delete the current selection (if any): remove `[min, max)`, collapse the caret to the left edge.
fn delete_selection(eb: &mut EditBoxState) {
    if eb.sel_start == eb.sel_end {
        return;
    }
    let (a, b) = (eb.sel_start.min(eb.sel_end), eb.sel_start.max(eb.sel_end));
    eb.text.replace_range(a..b, "");
    eb.cursor = a;
    collapse(eb);
}

/// Collapse the selection onto the caret (no selection).
fn collapse(eb: &mut EditBoxState) {
    eb.sel_start = eb.cursor;
    eb.sel_end = eb.cursor;
}

/// The fixed end of the current selection while extending (the endpoint that is *not* the caret; the
/// caret itself when there is no selection).
fn selection_anchor(eb: &EditBoxState) -> usize {
    if eb.sel_start == eb.cursor {
        eb.sel_end
    } else {
        eb.sel_start
    }
}

/// Set the selection to span `[min(a,b), max(a,b)]`.
fn set_span(eb: &mut EditBoxState, a: usize, b: usize) {
    eb.sel_start = a.min(b);
    eb.sel_end = a.max(b);
}

/// Enforce the length caps after an edit (`0x77c02d`): trim whole chars from the end while over
/// `maxBytes`, then while over `maxLetters` (char count); clamp the caret/selection into the buffer.
fn enforce_caps(eb: &mut EditBoxState) {
    if let Some(mb) = eb.max_bytes {
        while eb.text.len() > mb {
            eb.text.pop();
        }
    }
    if eb.max_letters > 0 {
        while eb.text.chars().count() > eb.max_letters {
            eb.text.pop();
        }
    }
    let len = eb.text.len();
    eb.cursor = snap_down(&eb.text, eb.cursor.min(len));
    eb.sel_start = snap_down(&eb.text, eb.sel_start.min(len));
    eb.sel_end = snap_down(&eb.text, eb.sel_end.min(len));
}

/// The byte offset of the char boundary at or below `i` (identity if `i` already sits on one).
fn snap_down(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The next char boundary strictly after `i` (clamped to `len`).
fn next_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut j = i + 1;
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

/// The char boundary strictly before `i` (clamped to `0`).
fn prev_boundary(s: &str, i: usize) -> usize {
    if i == 0 {
        return 0;
    }
    let mut j = i - 1;
    while j > 0 && !s.is_char_boundary(j) {
        j -= 1;
    }
    j
}

// ── text-region sync (the '*' mask lives here, RF-0082 §3) ───────────────────────────────────

/// Write the *display* string into the text region's [`RegionData::text`], creating the region lazily
/// (the implicit FontString) if none exists. `password` shows one `'*'` per **character** (the mask,
/// never the real text).
fn sync_text_region(lua: &Lua, h: FrameHandle) {
    let Some(rh) = ensure_text_region(lua, h) else {
        return;
    };
    let display = with_eb(lua, h, |eb| {
        if eb.password {
            "*".repeat(eb.text.chars().count())
        } else {
            eb.text.clone()
        }
    });
    if let Some(display) = display {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        model.region_data.entry(rh).or_default().text = Some(display);
    }
}

/// `SetTextInsets(l, r, t, b)`: store the insets and re-anchor the text region inside the box by
/// them (y-up: the top inset pulls the region's top DOWN, the bottom inset pulls its bottom UP).
/// The chat edit box calls this per header change (`ChatEdit_UpdateHeader`'s
/// `SetTextInsets(15 + headerWidth, 13, 0, 0)`), so typed text starts past the "Say:" header.
fn set_text_insets(lua: &Lua, h: FrameHandle, l: f32, r: f32, t: f32, b: f32) {
    if with_eb(lua, h, |eb| eb.text_insets = [l, r, t, b]).is_none() {
        return;
    }
    let Some(rh) = ensure_text_region(lua, h) else {
        return;
    };
    write_inset_anchors(lua, h, rh, [l, r, t, b]);
}

/// Anchor the text region inside its box by the insets — the two corner anchors that pin its rect
/// (y-up: the top inset pulls the region's top DOWN, the bottom inset pulls its bottom UP).
fn write_inset_anchors(lua: &Lua, h: FrameHandle, rh: RegionHandle, [l, r, t, b]: [f32; 4]) {
    let owner = frame_id_of(lua, h);
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let data = model.region_data.entry(rh).or_default();
    data.anchors = vec![
        Anchor::new(Point::TopLeft, owner, Point::TopLeft, l, -t),
        Anchor::new(Point::BottomRight, owner, Point::BottomRight, -r, b),
    ];
}

/// The loader's special-`<FontString>` slot assignment: adopt `region` as `frame`'s text region,
/// anchored by the current insets. The engine ASSIGNS an EditBox's font string at LoadXML — it
/// never searches the region list (a find-first here once grabbed a `<Layers>` FontString, the
/// chat header, so typing overwrote "Say:" and the insets clobbered the header's anchor).
pub(crate) fn adopt_text_region(lua: &Lua, frame: &Table, region: &Table) -> mlua::Result<()> {
    let h = frame_handle_of(lua, frame)?;
    let rh = super::region::region_handle_of(lua, region)?;
    let Some((insets, multi_line)) = with_eb(lua, h, |eb| {
        eb.text_region = Some(rh);
        (eb.text_insets, eb.multi_line)
    }) else {
        return Ok(()); // not an EditBox — the loader guards on the tag, but stay safe
    };
    write_inset_anchors(lua, h, rh, insets);
    // A live text region always carries Some(text) — an empty box still emits its Text quad, so
    // the host's caret has a styled quad to ride before the first keystroke.
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let data = model.region_data.entry(rh).or_default();
    data.text.get_or_insert_with(String::new);
    apply_text_region_justify(data, multi_line);
    Ok(())
}

/// Get-or-create the implicit text FontString (the EditBox's ButtonText analogue). Returns `None` if
/// `h` is not a live EditBox. An XML-declared `<FontString>` is wired explicitly by the loader's
/// special pass ([`adopt_text_region`]); the lazy create covers CreateFrame'd boxes.
pub(super) fn ensure_text_region(lua: &Lua, h: FrameHandle) -> Option<RegionHandle> {
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let (existing, multi_line) = match model.arena.frame(h).map(|f| &f.kind_state) {
        Some(KindState::EditBox(eb)) => (eb.text_region, eb.multi_line),
        _ => return None,
    };
    if existing.is_some() {
        return existing;
    }
    let rh = model
        .arena
        .create_region(h, RegionKind::FontString, DrawLayer::Overlay, 0)?;
    let mut data = RegionData {
        // Some("") from birth — the empty box still emits its Text quad for the host caret.
        text: Some(String::new()),
        ..RegionData::default()
    };
    apply_text_region_justify(&mut data, multi_line);
    model.region_data.insert(rh, data);
    if let Some(KindState::EditBox(eb)) = model.arena.frame_mut(h).map(|f| &mut f.kind_state) {
        eb.text_region = Some(rh);
    }
    Some(rh)
}

/// Re-seat an already-wired text region's justify by the box's CURRENT multiLine flag — the
/// `SetMultiLine` rider: the loader adopts the declared `<FontString>` (LoadXML step 5·b) before
/// it applies the editbox flags (5b), so `multiLine="true"` must re-run the law or the body box
/// keeps the single-line MIDDLE seat.
pub(super) fn refresh_text_region_justify(lua: &Lua, this: &Table) -> mlua::Result<()> {
    let h = frame_handle_of(lua, this)?;
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let Some(KindState::EditBox(eb)) = model.arena.frame(h).map(|f| &f.kind_state) else {
        return Ok(());
    };
    let (Some(rh), multi_line) = (eb.text_region, eb.multi_line) else {
        return Ok(());
    };
    apply_text_region_justify(model.region_data.entry(rh).or_default(), multi_line);
    Ok(())
}

/// The EditBox text-anchoring law: the box's text region lays out LEFT-justified (the client's
/// editbox draw `0x77da80` is left-anchored at the insets rect regardless of the font string's
/// declared justification — RF-0082's windowed draw, focused or not), from the TOP for a
/// multiline box, vertically centered for a single-line one. Without this, a bare
/// `<FontString inherits="ChatFontNormal"/>` inherits the FontString CENTER/MIDDLE defaults and
/// an empty focused box parks its caret mid-box (the mail send tab's original sin).
fn apply_text_region_justify(data: &mut RegionData, multi_line: bool) {
    data.justify_h = crate::script::JustifyH::Left;
    data.justify_v = if multi_line {
        crate::script::JustifyV::Top
    } else {
        crate::script::JustifyV::Middle
    };
}

// ── small shared helpers ─────────────────────────────────────────────────────────────────────

/// Run `f` over a frame's EditBox state under one short write borrow; `None` if not a live EditBox.
fn with_eb<T>(lua: &Lua, h: FrameHandle, f: impl FnOnce(&mut EditBoxState) -> T) -> Option<T> {
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    match model.arena.frame_mut(h).map(|fr| &mut fr.kind_state) {
        Some(KindState::EditBox(eb)) => Some(f(eb)),
        _ => None,
    }
}

/// The stable id of a frame handle (minting one if needed).
fn frame_id_of(lua: &Lua, h: FrameHandle) -> u32 {
    lua.app_data_mut::<Model>()
        .expect("model app_data")
        .frame_id(h)
}

/// Fire one specialized EditBox script (no args beyond `self`); errors go to [`Model::errors`].
fn fire_script(lua: &Lua, id: u32, name: &str) {
    if let Err(e) = event::fire_widget_handler(lua, id, name, Vec::new()) {
        lua.app_data_mut::<Model>()
            .expect("model app_data")
            .errors
            .push(e.to_string());
    }
}

// The Lua method surface (SetText/GetText/HighlightText/SetFocus/…, consulted before the shared
// frame table only for EditBox frames) — a child module over this file's focus/editing
// primitives, like `interact`/`seam`.
mod methods;
pub(super) use methods::install;

#[cfg(test)]
mod tests;
