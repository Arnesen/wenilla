//! The `ScrollingMessageFrame` method surface — the chat window's per-kind widget behavior
//! (`CSimpleMessageScrollFrame`, ctor `0x787670`).
//!
//! Grounded in wow-re's byte-verified runtime model (msgframe-runtime.md, §5 pair): a **true ring**
//! of `maxLines` (drop-oldest, `SetMaxLines` destructive), `AddMessage(text[,r,g,b[,id]])` with the
//! `trunc(x*255+0.5)` color quantization + forced-opaque alpha, per-line fade snapshots ticked only
//! while **AtBottom** (scrolled up freezes every alpha), 1-slot scrollback, and — the other half of
//! that fade, `msgframe-fade-rearm-law.md` — **every scroll entry re-arming the displayed lines**,
//! which is what brings a faded-out chat back. The heavy lifting
//! (the ring, the fade phases, the scroll clamps) lives on [`ScrollingMessageState`]
//! (`crate::widget`), unit-tested there; this module is the thin Lua binding over it, plus the
//! host-facing [`UiScript::add_chat_message`]/fade advance the app drives.
//!
//! The methods live in their own registry table, consulted by the frame `__index` dispatcher only
//! for ScrollingMessageFrame frames — so duck-typing addons (`if frame.AddMessage then …`) see `nil`
//! on every other kind, exactly as against the client's per-class method sets. **Its sibling class
//! `MessageFrame` has its own table next door** ([`super::plain`]) carrying a *different*
//! `AddMessage`: merging the two would hand a chat frame a `SetInsertMode` it does not have, and a
//! MessageFrame an id argument where its alpha belongs.

use mlua::{Lua, Table, Value};

use crate::script::object::frame_handle_of;
use crate::script::{Model, UiScript};
use crate::widget::{KindState, ScrollingMessageState};

/// Registry key of the ScrollingMessageFrame method table (the MAXCSTACK discipline: Lua-side root,
/// named key).
pub(crate) const REG_SCROLLINGMESSAGEFRAME_METHODS: &str =
    "__benilla_scrollingmessageframe_methods";

/// Run `f` over a frame's message-frame state under one short write borrow. Errors if `this` is not a
/// live ScrollingMessageFrame (unreachable through the kind dispatcher, but the method table is a
/// plain Lua value — a caller could fish it out and misapply it).
fn with_smf<T>(
    lua: &Lua,
    this: &Table,
    f: impl FnOnce(&mut ScrollingMessageState) -> T,
) -> mlua::Result<T> {
    let h = frame_handle_of(lua, this)?;
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let frame = model
        .arena
        .frame_mut(h)
        .ok_or_else(|| mlua::Error::runtime("stale frame handle"))?;
    match &mut frame.kind_state {
        KindState::ScrollingMessage(smf) => Ok(f(smf)),
        _ => Err(mlua::Error::runtime("not a ScrollingMessageFrame")),
    }
}

/// A Lua number-ish → f32 (nil/other → 0.0), for the color args.
fn num_f32(v: &Value) -> f32 {
    match v {
        Value::Number(n) => *n as f32,
        Value::Integer(i) => *i as f32,
        _ => 0.0,
    }
}

/// Run a scroll entry over a frame's state with the viewport's row budget in hand.
///
/// Every scroll method needs it, not just the pages: each one re-arms the **displayed** lines'
/// fade ([`ScrollingMessageState::reset_all_fade_times`]), and which lines those are depends on the
/// row budget. It comes from the frame's last resolved rect at the ring font's pitch (the
/// font-height line-step law, the emit pass's basis); an unresolved frame reads as one row.
fn scroll(
    lua: &Lua,
    this: &Table,
    op: impl FnOnce(&mut ScrollingMessageState, usize),
) -> mlua::Result<()> {
    let h = frame_handle_of(lua, this)?;
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let viewport_rows = UiScript::message_viewport_rows(&model, h);
    let frame = model
        .arena
        .frame_mut(h)
        .ok_or_else(|| mlua::Error::runtime("stale frame handle"))?;
    match &mut frame.kind_state {
        KindState::ScrollingMessage(smf) => {
            op(smf, viewport_rows);
            Ok(())
        }
        _ => Err(mlua::Error::runtime("not a ScrollingMessageFrame")),
    }
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let m = lua.create_table()?;

    // The justify quartet, shared with the sibling class — see [`super::install_justify`] for the
    // method-table bytes that put all four on BOTH tables.
    super::install_justify(lua, &m, "ScrollingMessageFrame")?;

    // AddMessage(text [, r, g, b [, id]]) — the id (5th arg) is accepted and ignored (v1 has no
    // per-line UpdateColorByID). rgb absent ⇒ white; the state quantizes round-half-up and forces
    // the line opaque, then the fade drives its alpha.
    m.set(
        "AddMessage",
        lua.create_function(
            |lua, (this, text, r, g, b): (Table, String, Value, Value, Value)| {
                let has_rgb = !matches!(
                    (&r, &g, &b),
                    (Value::Nil, _, _) | (_, Value::Nil, _) | (_, _, Value::Nil)
                );
                let (r, g, b) = if has_rgb {
                    (num_f32(&r), num_f32(&g), num_f32(&b))
                } else {
                    (1.0, 1.0, 1.0)
                };
                with_smf(lua, &this, |smf| smf.add(text, r, g, b))
            },
        )?,
    )?;

    m.set(
        "Clear",
        lua.create_function(|lua, this: Table| with_smf(lua, &this, ScrollingMessageState::clear))?,
    )?;
    m.set(
        "ScrollUp",
        lua.create_function(|lua, this: Table| {
            scroll(lua, &this, ScrollingMessageState::scroll_up)
        })?,
    )?;
    m.set(
        "ScrollDown",
        lua.create_function(|lua, this: Table| {
            scroll(lua, &this, ScrollingMessageState::scroll_down)
        })?,
    )?;
    m.set(
        "ScrollToTop",
        lua.create_function(|lua, this: Table| {
            scroll(lua, &this, ScrollingMessageState::scroll_to_top)
        })?,
    )?;
    m.set(
        "ScrollToBottom",
        lua.create_function(|lua, this: Table| {
            scroll(lua, &this, ScrollingMessageState::scroll_to_bottom)
        })?,
    )?;
    m.set(
        "PageUp",
        lua.create_function(|lua, this: Table| scroll(lua, &this, ScrollingMessageState::page_up))?,
    )?;
    m.set(
        "PageDown",
        lua.create_function(|lua, this: Table| {
            scroll(lua, &this, ScrollingMessageState::page_down)
        })?,
    )?;
    m.set(
        "AtTop",
        lua.create_function(|lua, this: Table| with_smf(lua, &this, |smf| smf.at_top()))?,
    )?;
    m.set(
        "AtBottom",
        lua.create_function(|lua, this: Table| with_smf(lua, &this, |smf| smf.at_bottom()))?,
    )?;

    // SetMaxLines is destructive (msgframe-runtime.md) — the state handles the wipe.
    m.set(
        "SetMaxLines",
        lua.create_function(|lua, (this, n): (Table, i64)| {
            with_smf(lua, &this, |smf| smf.set_max_lines(n.max(1) as usize))
        })?,
    )?;
    m.set(
        "GetMaxLines",
        lua.create_function(|lua, this: Table| with_smf(lua, &this, |smf| smf.max_lines as i64))?,
    )?;
    m.set(
        "GetNumMessages",
        lua.create_function(|lua, this: Table| with_smf(lua, &this, |smf| smf.lines.len() as i64))?,
    )?;

    m.set(
        "SetFading",
        lua.create_function(|lua, (this, on): (Table, Value)| {
            let on = !matches!(on, Value::Nil | Value::Boolean(false));
            with_smf(lua, &this, |smf| smf.fading_enabled = on)
        })?,
    )?;
    m.set(
        "GetFading",
        lua.create_function(|lua, this: Table| with_smf(lua, &this, |smf| smf.fading_enabled))?,
    )?;
    // The XML attr is `displayDuration`; the Lua accessors call the same field `TimeVisible`
    // (msgframe-runtime.md).
    m.set(
        "SetTimeVisible",
        lua.create_function(|lua, (this, s): (Table, f32)| {
            with_smf(lua, &this, |smf| smf.time_visible = s.max(0.0))
        })?,
    )?;
    m.set(
        "GetTimeVisible",
        lua.create_function(|lua, this: Table| with_smf(lua, &this, |smf| smf.time_visible))?,
    )?;
    m.set(
        "SetFadeDuration",
        lua.create_function(|lua, (this, s): (Table, f32)| {
            with_smf(lua, &this, |smf| smf.fade_duration = s.max(0.0))
        })?,
    )?;
    m.set(
        "GetFadeDuration",
        lua.create_function(|lua, this: Table| with_smf(lua, &this, |smf| smf.fade_duration))?,
    )?;

    // ── the shared font block ───────────────────────────────────────────────────────────────
    //
    // `Set/GetFontObject · Set/GetFont · Set/GetTextColor · Set/GetShadowColor ·
    // Set/GetShadowOffset` are real entries on this class's table, not a courtesy. wow-re's
    // registrar carve is explicit about the membership — *"Exposed on: FontString, Font object,
    // EditBox, MessageFrame, ScrollingMessageFrame, SimpleHTML. NOT on Button"* — and names this
    // class's own shims calling the shared implementations (`GetShadowColor 0x792240`). We shipped the block on two
    // of the six and this is the third and fourth; SimpleHTML is a widget kind we do not have at
    // all.
    //
    // Demand is observed, not counted: `BigWigs/Plugins/Messages.lua:212` is
    // `self.msgframe:SetFontObject(GameFontNormalLarge)` on a frame it has just given
    // `SetInsertMode("TOP")`, and BigWigs dies there every session.
    crate::script::font_block::install(
        lua,
        &m,
        |lua, this| {
            let h = frame_handle_of(lua, this)?;
            super::ensure_font_region(lua, h)
                .ok_or_else(|| mlua::Error::runtime("not a ScrollingMessageFrame"))
        },
        "ScrollingMessageFrame",
    )?;

    lua.set_named_registry_value(REG_SCROLLINGMESSAGEFRAME_METHODS, m)?;

    // SubmitChatInput(text) — the chat input EditBox's OnEnterPressed hands the typed line here; it
    // queues into the model for the app to parse into a chat command (the outbound Lua→app seam, the
    // twin of loot's LootSlot). Empty/whitespace lines are queued as-is; the app treats them as a
    // cancel. Not an EditBox method — a bare global, called with the line the handler read.
    // BenillaChatTabPressed() — the chat edit box's OnTabPressed queues a flag the app's
    // whisper-cycle system drains (UiScript::take_chat_tab), the SubmitChatInput pattern.
    lua.globals().set(
        "BenillaChatTabPressed",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.chat_tab = true;
            Ok(())
        })?,
    )?;
    lua.globals().set(
        "SubmitChatInput",
        lua.create_function(|lua, text: Option<String>| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.chat_input.push(text.unwrap_or_default());
            Ok(())
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::script::UiScript;
    use crate::widget::ScrollingMessageState;

    fn white(text: &str) -> (String, f32, f32, f32) {
        (text.to_string(), 1.0, 1.0, 1.0)
    }

    #[test]
    fn ring_drops_oldest_past_max_lines() {
        let mut s = ScrollingMessageState {
            max_lines: 3,
            ..Default::default()
        };
        for n in 0..5 {
            let (t, r, g, b) = white(&format!("line {n}"));
            s.add(t, r, g, b);
        }
        // Only the last 3 survive, oldest→newest.
        assert_eq!(s.lines.len(), 3);
        let texts: Vec<&str> = s.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["line 2", "line 3", "line 4"]);
    }

    #[test]
    fn set_max_lines_is_destructive() {
        let mut s = ScrollingMessageState::default();
        for n in 0..4 {
            s.add(format!("m{n}"), 1.0, 1.0, 1.0);
        }
        assert_eq!(s.lines.len(), 4);
        s.set_max_lines(128);
        assert!(s.lines.is_empty(), "SetMaxLines wipes the ring");
        assert_eq!(s.max_lines, 128);
        assert!(s.at_bottom());
    }

    #[test]
    fn color_is_quantized_round_half_up() {
        let mut s = ScrollingMessageState::default();
        // 0.5*255 = 127.5 → +0.5 → 128.0 → trunc 128 (round-half-up); 1.0 → 255; clamp past 1.0.
        s.add("q".into(), 0.5, 1.0, 2.0);
        assert_eq!(s.lines[0].color, [128, 255, 255]);
        // FF8040 (EMOTE) from 1.0, 0.5019.., 0.2509.. → 255,128,64 (spot the table's byte round-trip).
        s.add("e".into(), 1.0, 128.0 / 255.0, 64.0 / 255.0);
        assert_eq!(s.lines[1].color, [255, 128, 64]);
    }

    #[test]
    fn fade_holds_then_ramps_then_retires() {
        let mut s = ScrollingMessageState {
            time_visible: 1.0,
            fade_duration: 2.0,
            ..Default::default()
        };
        s.add("x".into(), 1.0, 1.0, 1.0);
        // Phase 1: full alpha while timeVisible remains.
        s.tick(0.5);
        assert_eq!(s.lines[0].alpha, 1.0);
        // Cross into phase 2: 0.5 more spends the last of timeVisible; next ticks ramp fadeDuration.
        s.tick(0.5);
        assert_eq!(s.lines[0].alpha, 1.0, "phase 1 just expired, ramp not yet");
        s.tick(1.0); // fade_left 2.0 → 1.0 → alpha = trunc(1.0/2.0*255)/255 = 127/255
        assert!((s.lines[0].alpha - 127.0 / 255.0).abs() < 1e-6);
        s.tick(1.0); // fade_left → 0 → retired
        assert_eq!(s.lines[0].alpha, 0.0);
    }

    #[test]
    fn fade_duration_zero_vanishes_instantly() {
        let mut s = ScrollingMessageState {
            time_visible: 0.5,
            fade_duration: 0.0,
            ..Default::default()
        };
        s.add("x".into(), 1.0, 1.0, 1.0);
        s.tick(0.6); // spends timeVisible (per-tick phase check: still full this tick)
        assert_eq!(s.lines[0].alpha, 1.0);
        s.tick(0.1); // now past phase 1, no ramp ⇒ straight to 0
        assert_eq!(s.lines[0].alpha, 0.0);
    }

    /// The tick's AtBottom gate, isolated from the scroll re-arm that now sits next to it: with a
    /// one-row viewport only the newest displayed line is re-armed, so the two lines either side of
    /// it are the ones that show the freeze.
    #[test]
    fn scrolled_up_freezes_the_fade() {
        let mut s = ScrollingMessageState {
            max_lines: 8,
            time_visible: 0.0, // straight into phase 2
            fade_duration: 4.0,
            ..Default::default()
        };
        for n in 0..3 {
            s.add(format!("l{n}"), 1.0, 1.0, 1.0);
        }
        s.tick(2.0); // half-way down the ramp: trunc(2.0/4.0*255) = 127
        let half = 127.0 / 255.0;
        assert_eq!(s.lines[0].alpha, half);
        // Scroll up one line — no longer AtBottom, so ticks must not advance any alpha. The line
        // that lands in the one-row viewport (l1) is re-armed by the scroll; l0 and l2 are not.
        s.scroll_up(1);
        assert!(!s.at_bottom());
        assert_eq!(s.lines[1].alpha, 1.0, "the displayed line was re-armed");
        s.tick(2.0);
        assert_eq!(s.lines[0].alpha, half, "frozen while scrolled up");
        assert_eq!(s.lines[2].alpha, half);
        assert_eq!(s.lines[1].alpha, 1.0);
        // Back to bottom → the fade resumes (from the re-arm this scroll performs).
        s.scroll_to_bottom(1);
        s.tick(2.0);
        assert!(s.lines[2].alpha < 1.0);
    }

    /// **The director's bug (2026-08-29): a fully-faded chat could never be brought back.** Every
    /// scroll entry re-arms the displayed lines — including the ones that move no cursor at all,
    /// which is what clicking the arrows on an already-at-bottom chat does.
    #[test]
    fn any_scroll_brings_faded_lines_back() {
        let armed = |op: fn(&mut ScrollingMessageState, usize)| {
            let mut s = ScrollingMessageState {
                max_lines: 8,
                time_visible: 1.0,
                fade_duration: 1.0,
                ..Default::default()
            };
            for n in 0..3 {
                s.add(format!("l{n}"), 1.0, 1.0, 1.0);
            }
            s.tick(1.5); // spend phase 1
            s.tick(1.5); // spend phase 2 — everything is gone
            assert!(s.lines.iter().all(|l| l.alpha == 0.0), "faded out first");
            op(&mut s, 8);
            s
        };
        for (name, op) in [
            (
                "ScrollUp",
                ScrollingMessageState::scroll_up as fn(&mut _, usize),
            ),
            ("ScrollDown", ScrollingMessageState::scroll_down),
            ("ScrollToTop", ScrollingMessageState::scroll_to_top),
            ("ScrollToBottom", ScrollingMessageState::scroll_to_bottom),
            ("PageUp", ScrollingMessageState::page_up),
            ("PageDown", ScrollingMessageState::page_down),
        ] {
            let s = armed(op);
            // Whatever the op left in view is what the engine re-arms — `ScrollUp` carries the
            // newest line off the bottom of the viewport, so the set is the op's, not the ring.
            let shown = s.displayed_range(8);
            assert!(!shown.is_empty(), "{name} displayed nothing");
            for i in shown {
                let line = &s.lines[i];
                assert_eq!(line.alpha, 1.0, "{name} left line {i} invisible");
                assert_eq!(line.time_left, 1.0, "{name} left line {i} un-armed");
                assert_eq!(line.fade_left, 1.0, "{name} left line {i} un-armed");
            }
        }
    }

    /// `0x788b80` walks the display vector, not the ring: a line scrolled out of view keeps
    /// whatever fade state it had.
    #[test]
    fn the_re_arm_reaches_only_the_displayed_lines() {
        let mut s = ScrollingMessageState {
            max_lines: 8,
            time_visible: 1.0,
            fade_duration: 1.0,
            ..Default::default()
        };
        for n in 0..3 {
            s.add(format!("l{n}"), 1.0, 1.0, 1.0);
        }
        s.tick(1.5);
        s.tick(1.5);
        s.scroll_down(1); // a no-op scroll at the bottom, one row of viewport
        assert_eq!(s.lines[2].alpha, 1.0, "the one displayed line came back");
        assert_eq!(s.lines[1].alpha, 0.0, "off-screen lines are untouched");
        assert_eq!(s.lines[0].alpha, 0.0);
    }

    #[test]
    fn scroll_clamps_at_both_ends() {
        let mut s = ScrollingMessageState {
            max_lines: 8,
            ..Default::default()
        };
        for n in 0..3 {
            s.add(format!("l{n}"), 1.0, 1.0, 1.0);
        }
        assert!(s.at_bottom());
        s.scroll_down(8); // already at bottom — no cursor move
        assert!(s.at_bottom());
        // 3 lines → max_scroll = 2; scroll past it clamps.
        for _ in 0..10 {
            s.scroll_up(8);
        }
        assert!(s.at_top());
        assert_eq!(s.scroll_offset, 2);
        s.scroll_to_bottom(8);
        assert!(s.at_bottom());
    }

    #[test]
    fn scrolled_view_stays_anchored_as_the_ring_grows() {
        let mut s = ScrollingMessageState {
            max_lines: 8,
            ..Default::default()
        };
        for n in 0..4 {
            s.add(format!("l{n}"), 1.0, 1.0, 1.0);
        }
        s.scroll_up(8); // viewing one line back (bottom row = l2)
        let off = s.scroll_offset;
        s.add("l4".into(), 1.0, 1.0, 1.0); // a new line arrives at the bottom
        assert_eq!(
            s.scroll_offset,
            off + 1,
            "offset tracks so the same lines stay in view"
        );
    }

    #[test]
    fn displayed_count_and_paging_respect_wrapped_rows() {
        let mut s = ScrollingMessageState {
            max_lines: 8,
            ..Default::default()
        };
        for n in 0..3 {
            s.add(format!("l{n}"), 1.0, 1.0, 1.0);
        }
        // Middle line wraps into 3 rows.
        s.lines[1].rows = 3;
        // Viewport of 4 rows from the bottom: newest (1 row) + middle (3 rows) fill it exactly —
        // the oldest is off the top. A partially-fitting message counts (it draws, clipped).
        assert_eq!(s.displayed_count(4), 2);
        assert_eq!(s.displayed_count(3), 2, "partial middle line still counts");
        assert_eq!(s.displayed_count(1), 1);
        // Page = displayed − 1 (one message of overlap), never 0.
        s.page_up(4);
        assert_eq!(s.scroll_offset, 1);
        s.page_up(1); // displayed 1 → page clamps to 1 message
        assert_eq!(s.scroll_offset, 2);
        s.page_down(4);
        assert_eq!(s.scroll_offset, 1);
        s.page_down(4);
        assert!(s.at_bottom());
    }

    #[test]
    fn emit_stacks_wrapped_rows_and_clips_partial_top() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        s.run(
            "local f = CreateFrame('ScrollingMessageFrame', 'CF')\n\
             f:SetPoint('BOTTOMLEFT', 32, 90)\n\
             f:SetWidth(430)\n\
             f:SetHeight(34)",
        )
        .unwrap();
        s.run("CF:AddMessage('old', 1, 1, 1)").unwrap();
        s.run("CF:AddMessage('wrapped', 1, 1, 1)").unwrap();
        s.run("CF:AddMessage('new', 1, 1, 1)").unwrap();
        s.resolve();
        // The measure round-trip: every fresh line requests its row count at the resolved width.
        let reqs = s.message_lines_needing_measure();
        assert_eq!(reqs.len(), 3);
        assert!(reqs.iter().all(|r| (r.wrap_width - 430.0).abs() < 0.5));
        // Host answers: the middle line wraps to 2 rows.
        let answers: Vec<(u32, u32, u16, u64)> = reqs
            .iter()
            .map(|r| (r.frame, r.index, if r.index == 1 { 2 } else { 1 }, r.key))
            .collect();
        s.set_message_line_rows(&answers);
        assert!(
            s.message_lines_needing_measure().is_empty(),
            "answered keys satisfy the cache"
        );
        // Default font (no FontString child) ⇒ pitch 14. Frame [90, 124): 'new' band [90,104),
        // 'wrapped' 2-row band [104,132) — starts inside, overflows the top, still emitted
        // (clipped); 'old' would start at 132 ≥ 124 — not emitted.
        let quads = s.extract();
        let texts: Vec<(String, f32, f32)> = quads
            .iter()
            .filter_map(|q| match (&q.content, q.rect) {
                (crate::script::QuadContent::Text { text: Some(t), .. }, Some(r)) => {
                    Some((t.clone(), r.bottom, r.top))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            texts.len(),
            2,
            "partial top line draws, off-top line doesn't"
        );
        assert_eq!(texts[0].0, "new");
        assert!((texts[0].1 - 90.0).abs() < 0.01 && (texts[0].2 - 104.0).abs() < 0.01);
        assert_eq!(texts[1].0, "wrapped");
        assert!((texts[1].1 - 104.0).abs() < 0.01 && (texts[1].2 - 132.0).abs() < 0.01);
        // Both clip to the frame rect, so the overflow never inks outside it.
        let clip = quads
            .iter()
            .find_map(|q| match &q.content {
                crate::script::QuadContent::Text { text: Some(t), .. } if t == "wrapped" => q.clip,
                _ => None,
            })
            .expect("wrapped line carries a clip");
        assert!((clip.top - 124.0).abs() < 0.01 && (clip.bottom - 90.0).abs() < 0.01);
    }

    /// The ring lines are the frame's ARTWORK content, not its bare draw slot: a BACKGROUND
    /// texture of the same frame draws BEHIND them, an OVERLAY one in front. The chat window's
    /// hover box is that background texture — at the bare slot it painted over the messages and
    /// dimmed them (director, 2026-07-26).
    #[test]
    fn ring_lines_draw_above_the_frames_background_and_below_its_overlay() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        s.run(
            "local f = CreateFrame('ScrollingMessageFrame', 'CF')\n\
             f:SetPoint('BOTTOMLEFT', 32, 90)\n\
             f:SetWidth(430)\n\
             f:SetHeight(60)\n\
             local bg = f:CreateTexture('CFBg', 'BACKGROUND')\n\
             bg:SetTexture('Interface\\\\Bg')\n\
             bg:SetAllPoints(f)\n\
             local over = f:CreateTexture('CFOver', 'OVERLAY')\n\
             over:SetTexture('Interface\\\\Over')\n\
             over:SetAllPoints(f)\n\
             f:AddMessage('hello', 1, 1, 1)",
        )
        .unwrap();
        s.resolve();
        let quads = s.extract();
        let z_of = |path: &str| {
            quads
                .iter()
                .find_map(|q| match &q.content {
                    crate::script::QuadContent::Texture { path: Some(p), .. } if p == path => {
                        Some(q.z)
                    }
                    _ => None,
                })
                .expect("texture drew")
        };
        let line_z = quads
            .iter()
            .find_map(|q| match &q.content {
                crate::script::QuadContent::Text { text: Some(t), .. } if t == "hello" => Some(q.z),
                _ => None,
            })
            .expect("the ring line drew");
        let slot_z = quads
            .iter()
            .find(|q| matches!(q.content, crate::script::QuadContent::Frame))
            .map(|q| q.z)
            .expect("the frame slot drew");
        let (bg_z, over_z) = (z_of("Interface\\Bg"), z_of("Interface\\Over"));
        assert!(slot_z < bg_z, "the frame's own slot still leads");
        assert!(
            bg_z < line_z,
            "a BACKGROUND texture draws behind the messages"
        );
        assert!(line_z < over_z, "an OVERLAY texture draws in front of them");
    }

    #[test]
    fn width_change_invalidates_row_measures() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        s.run(
            "local f = CreateFrame('ScrollingMessageFrame', 'CF')\n\
             f:SetPoint('BOTTOMLEFT', 0, 0)\n\
             f:SetWidth(430)\n\
             f:SetHeight(120)",
        )
        .unwrap();
        s.run("CF:AddMessage('x', 1, 1, 1)").unwrap();
        s.resolve();
        let reqs = s.message_lines_needing_measure();
        assert_eq!(reqs.len(), 1);
        let answers: Vec<(u32, u32, u16, u64)> =
            reqs.iter().map(|r| (r.frame, r.index, 1, r.key)).collect();
        s.set_message_line_rows(&answers);
        assert!(s.message_lines_needing_measure().is_empty());
        s.run("CF:SetWidth(300)").unwrap();
        s.resolve();
        assert_eq!(
            s.message_lines_needing_measure().len(),
            1,
            "a width change re-requests the row count"
        );
    }

    #[test]
    fn faded_line_holds_its_rows_without_drawing() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        s.run(
            "local f = CreateFrame('ScrollingMessageFrame', 'CF')\n\
             f:SetPoint('BOTTOMLEFT', 0, 0)\n\
             f:SetWidth(430)\n\
             f:SetHeight(120)\n\
             f:SetTimeVisible(0)\n\
             f:SetFadeDuration(0)",
        )
        .unwrap();
        s.run("CF:AddMessage('doomed', 1, 1, 1)").unwrap();
        // Two ticks retire 'doomed' (phase-1 spend, then the no-ramp snap), then fresh lines land.
        s.tick(0.1);
        s.tick(0.1);
        s.run("CF:SetTimeVisible(120)").unwrap();
        s.run("CF:AddMessage('a', 1, 1, 1)").unwrap();
        s.run("CF:AddMessage('b', 1, 1, 1)").unwrap();
        s.resolve();
        let quads = s.extract();
        let texts: Vec<(String, f32)> = quads
            .iter()
            .filter_map(|q| match (&q.content, q.rect) {
                (crate::script::QuadContent::Text { text: Some(t), .. }, Some(r)) => {
                    Some((t.clone(), r.bottom))
                }
                _ => None,
            })
            .collect();
        // 'doomed' draws nothing but its band (the oldest, topmost) still holds its place: the two
        // live lines sit at the two bottom bands, nothing re-packs into the faded slot.
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[0].0, "b");
        assert!((texts[0].1 - 0.0).abs() < 0.01);
        assert_eq!(texts[1].0, "a");
        assert!((texts[1].1 - 14.0).abs() < 0.01);
    }

    #[test]
    fn hyperlink_span_release_fires_on_hyperlink_click() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        s.run(
            "local f = CreateFrame('ScrollingMessageFrame', 'CF')\n\
             f:SetPoint('BOTTOMLEFT', 0, 0)\n\
             f:SetWidth(430)\n\
             f:SetHeight(120)\n\
             f:SetScript('OnHyperlinkClick', function(self, link, text, button)\n\
                 CLICKED = link .. '#' .. text .. '#' .. button\n\
             end)",
        )
        .unwrap();
        s.run("CF:AddMessage('x', 1, 1, 1)").unwrap();
        s.resolve();
        // Find the frame handle the extract loop would hand the app (the message line's target).
        let fh = s
            .extract()
            .iter()
            .find_map(|q| match (&q.content, q.target) {
                (
                    crate::script::QuadContent::Text { text: Some(t), .. },
                    crate::order::ZTarget::Frame(fh),
                ) if t == "x" => Some(fh),
                _ => None,
            })
            .expect("the ring line extracts");
        // The app feeds a span (y-up rect) covering [10..60]x[20..34]; a release inside fires the
        // handler with (link, markup, button); one outside does not.
        s.set_link_spans(vec![(
            fh,
            crate::layout::Rect::new(20.0, 10.0, 34.0, 60.0),
            "item:7073".to_string(),
            "|Hitem:7073|h[Broken Fang]|h".to_string(),
        )]);
        s.mouse_button(30.0, 25.0, "LeftButton", true);
        s.mouse_button(30.0, 25.0, "LeftButton", false);
        assert_eq!(
            s.eval::<String>("return CLICKED or ''").unwrap(),
            "item:7073#|Hitem:7073|h[Broken Fang]|h#LeftButton"
        );
        // Outside the span: no new fire.
        s.run("CLICKED = nil").unwrap();
        s.mouse_button(200.0, 100.0, "LeftButton", true);
        s.mouse_button(200.0, 100.0, "LeftButton", false);
        assert_eq!(
            s.eval::<String>("return CLICKED or 'none'").unwrap(),
            "none"
        );
    }

    #[test]
    fn add_chat_message_targets_the_named_scroll_frame() {
        let mut s = UiScript::new().unwrap();
        s.run("CreateFrame('ScrollingMessageFrame', 'ChatTest')")
            .unwrap();
        assert!(s.add_chat_message("ChatTest", "hello", 1.0, 1.0, 1.0));
        assert_eq!(
            s.eval::<i64>("return ChatTest:GetNumMessages()").unwrap(),
            1
        );
        // A missing frame or a non-message frame is a no-op.
        assert!(!s.add_chat_message("Nope", "x", 1.0, 1.0, 1.0));
        s.run("CreateFrame('Frame', 'PlainFrame')").unwrap();
        assert!(!s.add_chat_message("PlainFrame", "x", 1.0, 1.0, 1.0));
    }

    #[test]
    fn lua_addmessage_and_scroll_surface() {
        let s = UiScript::new().unwrap();
        s.run("CreateFrame('ScrollingMessageFrame', 'CF')").unwrap();
        s.run("CF:SetMaxLines(128)").unwrap();
        assert_eq!(s.eval::<i64>("return CF:GetMaxLines()").unwrap(), 128);
        s.run("CF:AddMessage('one', 1, 1, 1)").unwrap();
        s.run("CF:AddMessage('two')").unwrap(); // rgb-less form defaults white
        assert_eq!(s.eval::<i64>("return CF:GetNumMessages()").unwrap(), 2);
        assert!(s.eval::<bool>("return CF:AtBottom()").unwrap());
        s.run("CF:ScrollUp()").unwrap();
        assert!(!s.eval::<bool>("return CF:AtBottom()").unwrap());
        s.run("CF:ScrollToBottom()").unwrap();
        assert!(s.eval::<bool>("return CF:AtBottom()").unwrap());
        s.run("CF:Clear()").unwrap();
        assert_eq!(s.eval::<i64>("return CF:GetNumMessages()").unwrap(), 0);
    }

    /// The W4 skip token: a settled message frame's sweep hashes ZERO lines (the counter is the
    /// claim — 0735's counts rule), and each side of the token catches its own change class:
    /// text through the generation, the resolved width through the environment hash. The token
    /// is only stored by a zero-request sweep, so an UNANSWERED request keeps re-requesting —
    /// the region ledger's own rule (1410), inherited.
    #[test]
    fn a_settled_message_frame_hashes_no_lines_until_something_moves() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        s.run(
            "local f = CreateFrame('ScrollingMessageFrame', 'CF')\n\
             f:SetPoint('BOTTOMLEFT', 32, 90)\n\
             f:SetWidth(430)\n\
             f:SetHeight(34)",
        )
        .unwrap();
        s.run("CF:AddMessage('one', 1, 1, 1)").unwrap();
        s.run("CF:AddMessage('two', 1, 1, 1)").unwrap();
        s.resolve();
        // Unanswered requests re-request: no token may be stored while keys mismatch.
        let first = s.message_lines_needing_measure();
        assert_eq!(first.len(), 2);
        let again = s.message_lines_needing_measure();
        assert_eq!(again.len(), 2, "unanswered lines must keep re-requesting");
        let answers: Vec<(u32, u32, u16, u64)> =
            again.iter().map(|r| (r.frame, r.index, 1, r.key)).collect();
        s.set_message_line_rows(&answers);
        assert!(s.message_lines_needing_measure().is_empty());
        // Settled: the NEXT sweep must skip the frame whole — zero lines hashed.
        let hashed_before = s.model_mut().msg_lines_hashed;
        assert!(s.message_lines_needing_measure().is_empty());
        assert_eq!(
            s.model_mut().msg_lines_hashed,
            hashed_before,
            "a settled frame's sweep must hash no lines"
        );
        // Text change reopens through the GENERATION…
        s.run("CF:AddMessage('three', 1, 1, 1)").unwrap();
        let reqs = s.message_lines_needing_measure();
        assert_eq!(reqs.len(), 1, "the new line re-surfaces");
        assert_eq!(reqs[0].text, "three");
        let answers: Vec<(u32, u32, u16, u64)> =
            reqs.iter().map(|r| (r.frame, r.index, 1, r.key)).collect();
        s.set_message_line_rows(&answers);
        assert!(s.message_lines_needing_measure().is_empty());
        // …and a resolved-width change reopens through the ENVIRONMENT: every line re-keys.
        s.run("CF:SetWidth(300)").unwrap();
        s.resolve();
        let reqs = s.message_lines_needing_measure();
        assert_eq!(reqs.len(), 3, "a new wrap width re-measures every line");
        assert!(reqs.iter().all(|r| (r.wrap_width - 300.0).abs() < 0.5));
    }
}
