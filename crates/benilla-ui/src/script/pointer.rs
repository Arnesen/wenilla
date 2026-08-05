//! Pointer input dispatch: hit-testing and the mouse-move/-button/-wheel entry points that turn
//! host pointer events into FrameScript handler calls (decision 0068). The pipeline is
//! **hit-test → hover → click/drag**:
//!
//! - [`UiScript::hit_test`] answers "what's under `(x, y)`" — the topmost-drawn, mouse-enabled,
//!   effective-visible frame whose resolved rect (and effective ScrollFrame clip, decision 0112
//!   §5) contains the point. Pure query; fires nothing, mutates nothing.
//! - [`UiScript::mouse_move`] hit-tests every move and, on a captured-frame **change**, fires the
//!   `OnLeave`/`OnEnter` pair (RF-0025's `motion=true` hover boundary) — and, before that,
//!   advances an armed drag gesture and an in-flight **Slider thumb drag** (both move within one
//!   frame, crossing no hover boundary, so they must run before the boundary early-return).
//! - [`UiScript::mouse_button`] resolves `OnMouseDown`/`OnMouseUp`, the `OnClick`
//!   press/release-registration rules ([`button::wants_click`]), the **drag trio** + **world
//!   drop** below, the **Slider thumb-drag** capture (a left press on a thumb, decision 0250 §5),
//!   and EditBox click-to-focus — all keyed off the same hit-test.
//! - [`UiScript::mouse_wheel`] fires `OnMouseWheel`, bubbling to the nearest ancestor with a
//!   handler when the hit frame has none (a ScrollFrame under a non-scrolling child).
//!
//! ## The drag trio + world drop (decisions 0216 §3, corrected by 0218)
//!
//! A press on a [`super::Model::drag_registered`] frame **arms** a gesture ([`cursor::arm_drag`]); once
//! the cursor crosses the drag-start threshold it **starts** (firing `OnDragStart`) — that
//! threshold check is [`cursor::maybe_start_drag`], run from `mouse_move` on every move. A
//! release **resolves** the gesture ([`cursor::take_drag`]): a STARTED gesture fires
//! `OnDragStop` on the source and `OnReceiveDrag` on whatever the release hit (a no-op if that
//! frame carries no such script), and SUPPRESSES `OnClick` for that release — an un-started
//! gesture leaves the ordinary click path untouched. The **world drop**
//! ([`cursor::world_drop_click`]) fires only on a completed left CLICK over the game world:
//! press AND release both landed over no frame, no drag started (the byte-verified trigger — the
//! client's `0x495300` runs on the WorldFrame click release; a drag release routes as a drag,
//! never a click), routed by the app-fed pick ([`super::Model::world_pick`], decisions 0571 +
//! 0574 — an object pick keeps everything, terrain drops items only, nothing drops any arm).
//!
//! ## Division of labor with [`cursor`]
//!
//! [`cursor`] owns the payload/gesture **STATE**: [`super::Model::cursor`] (what the cursor
//! carries), [`super::Model::drag`] (the in-flight gesture), and the pure functions that
//! arm/advance/resolve them. This file owns the input **DISPATCH** — turning raw
//! `(x, y, button, down)` host events into a hit-test plus the right sequence of Lua handler
//! calls, calling into `cursor`'s state machine at the right points rather than touching
//! [`super::Model::drag`]/[`super::Model::cursor`] directly.

use mlua::Value;

use crate::layout::Rect;
use crate::order;

use super::clip::{effective_clip, scroll_clip_sources};
use super::{button, cursor, editbox, event, UiScript};

// ── Input / hit-testing (decision 0068; spec-faithful, not byte-pinned) ─────────────────────
//
// The mouse-focus model (`propagation.md`): focus walks frames top-down in reverse draw order;
// the first mouse-enabled, effective-visible frame whose resolved rect contains the cursor
// captures. This is faithful to the *documented model*, **not** bit-exact — the real hit-test
// leaf is a flagged RE-time item, not byte-pinned (`anchors.md`; see [`crate::order::hit_test`]).
// Coordinates are WoW UI space (y-up, pixels) — the same space [`resolve`]/[`extract`] use.
// Call [`resolve`] first so rects are populated; a frame with no resolved rect never captures.

/// Install the pointer-adjacent Lua globals: `GetCursorPosition()` → the model's live cursor in
/// the engine's y-up UI space (fed by the host's mouse_move/mouse_button) — what the ref's
/// `MouseIsOver`/FCF hover logic reads (1.12 returns UI-scaled coords; benilla's UI space is
/// 1:1 logical px, so no scale factor applies).
pub(super) fn install(lua: &mlua::Lua) -> mlua::Result<()> {
    lua.globals().set(
        "GetCursorPosition",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<super::Model>().expect("model app_data");
            Ok(model.cursor_pos)
        })?,
    )?;
    Ok(())
}

impl UiScript {
    /// Hit-test the cursor at `(x, y)` and return the **id** of the captured frame (the topmost-drawn
    /// mouse-enabled, effective-visible frame whose rect contains the point **and** whose effective
    /// ScrollFrame clip, if any, also contains it — decision 0112 §5: a button scrolled out of its
    /// ScrollFrame's rect must not hit), or `None`. Pure query — fires nothing, mutates nothing. The
    /// id is the same one used across this host (e.g. the layout [`crate::layout::Handle`]); the app
    /// can hand it back or map it as it likes.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<u32> {
        let model = self.model_ref();
        let sorted = order::traversal(&model.arena);
        let scroll_sources = scroll_clip_sources(&model);
        let hit = order::hit_test(&sorted, |fh| {
            model.arena.is_mouse_enabled(fh)
                && model.resolved.get(&fh).is_some_and(|r| {
                    point_in_rect(inset_rect(*r, model.arena.hit_rect_insets(fh)), x, y)
                })
                && effective_clip(&model, &scroll_sources, fh)
                    .is_none_or(|c| point_in_rect(c, x, y))
        });
        hit.and_then(|h| model.frame_to_id.get(&h).copied())
    }

    /// The NAME of the frame [`Self::hit_test`] captures at `(x, y)` — the pointer twin of
    /// [`super::UiScript::quad_owner_name`] ("who eats a click here?"), for tests and probe
    /// tooling. `None` when nothing captures the point or the captured frame is anonymous.
    pub fn hit_test_name(&self, x: f32, y: f32) -> Option<String> {
        let id = self.hit_test(x, y)?;
        let model = self.model_ref();
        let fh = model.id_to_frame.get(&id).copied()?;
        model.arena.frame(fh)?.name.clone()
    }

    /// Move the cursor to `(x, y)`: hit-test, and if the captured frame **changed** since the last
    /// move, fire `OnLeave(self, motion=true)` on the frame being left (if any and still live) and
    /// `OnEnter(self, motion=true)` on the newly-captured frame (if any). Tracks the current
    /// mouseover in the model. Also advances an armed [`super::Model::drag`] gesture (decision 0216 §3):
    /// this runs BEFORE the no-boundary-crossed early return below, since dragging within one
    /// frame crosses no hover boundary at all. Returns the captured frame id (so the app can drive
    /// `PointerOverUi`). Handler errors are collected into [`UiScript::errors`], never panicking.
    pub fn mouse_move(&mut self, x: f32, y: f32) -> Option<u32> {
        // A drag-active EditBox tracks every move (`0x77a860`): extend the selection to the
        // hovered char, cursor following — before any hover-boundary early return.
        editbox::drag_update(&self.lua, x, y);
        let new_id = self.hit_test(x, y);
        #[allow(clippy::type_complexity)]
        let (old_id, drag_start, boundary_crossed, slider_change): (
            Option<u32>,
            Option<(u32, String)>,
            bool,
            Option<(u32, f32)>,
        ) = {
            let mut model = self.model_mut();
            model.cursor_pos = (x, y);
            // Advance an in-flight Slider thumb drag first — dragging within one frame crosses no
            // hover boundary, so this must run before the early return below (decision 0250 §5).
            let slider_change = super::slider::drag_move(&mut model, x, y);
            let drag_start = cursor::maybe_start_drag(&mut model, (x, y));
            let new_handle = new_id.and_then(|id| model.id_to_frame.get(&id).copied());
            if new_handle == model.mouseover {
                (None, drag_start, false, slider_change) // no boundary crossed
            } else {
                // Fire OnLeave only if the frame we're leaving is still live.
                let old_id = model.mouseover.and_then(|h| {
                    model
                        .arena
                        .frame(h)
                        .is_some()
                        .then(|| model.frame_to_id.get(&h).copied())
                        .flatten()
                });
                model.mouseover = new_handle;
                (old_id, drag_start, true, slider_change)
            }
        };
        // The thumb-drag value change fires OnValueChanged (outside the borrow) — the scrollbar's
        // OnValueChanged→SetVerticalScroll wiring runs here, exactly as the arrow buttons drive it.
        if let Some((id, value)) = slider_change {
            if let Err(e) = event::fire_widget_handler(
                &self.lua,
                id,
                "OnValueChanged",
                vec![Value::Number(f64::from(value))],
            ) {
                self.push_error(e);
            }
        }
        if let Some((id, button)) = drag_start {
            let btn = match self.lua.create_string(&button) {
                Ok(s) => Value::String(s),
                Err(e) => {
                    self.push_error(e);
                    return new_id;
                }
            };
            if let Err(e) = event::fire_widget_handler(&self.lua, id, "OnDragStart", vec![btn]) {
                self.push_error(e);
            }
        }
        if !boundary_crossed {
            return new_id;
        }
        if let Some(oid) = old_id {
            if let Err(e) =
                event::fire_widget_handler(&self.lua, oid, "OnLeave", vec![Value::Boolean(true)])
            {
                self.push_error(e);
            }
        }
        if let Some(nid) = new_id {
            if let Err(e) =
                event::fire_widget_handler(&self.lua, nid, "OnEnter", vec![Value::Boolean(true)])
            {
                self.push_error(e);
            }
        }
        new_id
    }

    /// A mouse button transition at `(x, y)`: `button` is the WoW name (`"LeftButton"`,
    /// `"RightButton"`, …), `down` is press vs release. Fires `OnMouseDown(self, button)` on press or
    /// `OnMouseUp(self, button)` on release, on the captured frame.
    ///
    /// Separately, `OnClick` fires per the hit frame's `RegisterForClicks` set
    /// ([`button::wants_click`]): a **press** fires immediately when the frame registered
    /// `"<Button>ButtonDown"` (`OnClick(self, button, down=true)`); a **release** whose matching
    /// **press** captured the *same* frame fires when the frame registered `"<Button>ButtonUp"`
    /// (`OnClick(self, button, down=false)` — the Era signature,
    /// `warcraft.wiki.gg/wiki/UIHANDLER_OnClick`). The default set is `{"LeftButtonUp"}`, so
    /// unregistered buttons (a bare right-click, say) reach neither. Press targets are tracked per
    /// button.
    ///
    /// The drag trio + world drop (decision 0216 §3, corrected by 0218) ride the same
    /// transition: a press ARMS a [`super::Model::drag`] gesture on a [`super::Model::drag_registered`] frame
    /// ([`cursor::arm_drag`]); a release RESOLVES it ([`cursor::take_drag`]) — a STARTED gesture
    /// fires `OnDragStop` on the source and `OnReceiveDrag` on whatever the release hit (a no-op
    /// if that frame carries no such script), and SUPPRESSES `OnClick` for this release (an
    /// un-started gesture leaves the ordinary click path untouched). The **world drop**
    /// ([`cursor::world_drop_click`]) fires ONLY on a completed left CLICK over the world —
    /// press AND release both over no frame, no started drag (the byte-verified trigger: the
    /// client's `0x495300` runs on the WorldFrame click release; a drag release routes as a
    /// drag, never a click — wow-re cursor-dragdrop-payload.md, and the director's report it
    /// confirmed) — routed by the app-fed pick (decisions 0571 + 0574): over an object nothing
    /// drops (the reference keeps an item payload and selects); over terrain an item fires
    /// `DELETE_ITEM_CONFIRM` and stays held while a spell/action survives; over nothing the
    /// item pops and any other payload clears silently. A drag released over nothing keeps its
    /// payload untouched.
    ///
    /// Handler errors are collected into [`UiScript::errors`]. Returns whether the UI consumed
    /// the event (`hit_id.is_some()` or a world drop was handled) — a new public return; existing
    /// statement-position calls (`script.mouse_button(...);`) are unaffected.
    pub fn mouse_button(&mut self, x: f32, y: f32, button: &str, down: bool) -> bool {
        let hit_id = self.hit_test(x, y);
        self.model_mut().cursor_pos = (x, y);
        let btn = match self.lua.create_string(button) {
            Ok(s) => Value::String(s),
            Err(e) => {
                self.push_error(e);
                return hit_id.is_some();
            }
        };
        // Resolve the OnClick target (if any), arm/resolve the drag gesture, and handle a world
        // drop — all under one short borrow. A press fires OnClick when the hit frame registered
        // "<Button>ButtonDown"; a release fires it when press+release landed on the same frame AND
        // it registered "<Button>ButtonUp" — UNLESS a started drag is being resolved instead.
        #[allow(clippy::type_complexity)]
        let (click_id, drag_release, world_dropped, slider_jump): (
            Option<u32>,
            Option<cursor::DragRelease>,
            bool,
            Option<(u32, f32)>,
        ) = {
            let mut model = self.model_mut();
            let hit_handle = hit_id.and_then(|id| model.id_to_frame.get(&id).copied());
            if down {
                match hit_handle {
                    Some(h) => {
                        model.mouse_down_on.insert(button.to_string(), h);
                    }
                    None => {
                        model.mouse_down_on.remove(button);
                    }
                }
                cursor::arm_drag(&mut model, hit_handle, button, (x, y));
                // A left press on a Slider begins the engine drag (0250 §5): a thumb press grabs
                // it in place; a track press seats the thumb under the cursor first (the returned
                // value jump — 0989), independent of the click/drag-trio path above (a Slider
                // isn't drag_registered).
                let jump = if button == "LeftButton" {
                    super::slider::begin_drag(&mut model, hit_handle, x, y)
                } else {
                    None
                };
                let wants = format!("{button}Down");
                let click = hit_handle
                    .filter(|&h| button::wants_click(&model, h, &wants))
                    .and(hit_id);
                (click, None, false, jump)
            } else {
                let pressed = model.mouse_down_on.remove(button);
                // A left release ends any in-flight thumb drag (decision 0250 §5).
                if button == "LeftButton" {
                    super::slider::end_drag(&mut model);
                }
                let same_frame = matches!(
                    (pressed, hit_handle),
                    (Some(p), Some(released)) if p == released
                );
                let release = cursor::take_drag(&mut model, button);
                let started = release.as_ref().is_some_and(|r| r.started);
                // The world drop: press AND release both over no frame, no drag started —
                // a completed left click on the game world (never a drag release, which just
                // keeps carrying; decision 0218's byte-verified trigger). What actually drops
                // is routed inside `world_drop_click` by the app-fed world pick (decisions
                // 0571 + 0574): an object pick keeps everything, terrain drops items only,
                // nothing drops any arm.
                let dropped = button == "LeftButton"
                    && !started
                    && hit_id.is_none()
                    && pressed.is_none()
                    && cursor::world_drop_click(&mut model);
                let wants = format!("{button}Up");
                let click = if started {
                    None
                } else {
                    hit_handle
                        .filter(|&h| same_frame && button::wants_click(&model, h, &wants))
                        .and(hit_id)
                };
                (click, release, dropped, None)
            }
        };
        // A track press's value jump fires OnValueChanged first (outside the borrow), the same
        // seam as `mouse_move`'s in-drag changes — the press IS the first move of the gesture.
        if let Some((id, value)) = slider_jump {
            if let Err(e) = event::fire_widget_handler(
                &self.lua,
                id,
                "OnValueChanged",
                vec![Value::Number(f64::from(value))],
            ) {
                self.push_error(e);
            }
        }
        let script = if down { "OnMouseDown" } else { "OnMouseUp" };
        if let Some(id) = hit_id {
            if let Err(e) = event::fire_widget_handler(&self.lua, id, script, vec![btn.clone()]) {
                self.push_error(e);
            }
        }
        // A release inside a hyperlink span fires `OnHyperlinkClick(link, markup, button)` on the
        // owning frame — the ChatFrameTemplate wires it to `SetItemRef` (decision 0288 P2). Never
        // on a drag resolution (same suppression as OnClick); spans were fed by the app after the
        // last rasterize (`UiScript::set_link_spans`), so they match what's on screen.
        if !down && !drag_release.as_ref().is_some_and(|r| r.started) {
            if let Some(id) = hit_id {
                let span = {
                    let model = self.model_ref();
                    model
                        .id_to_frame
                        .get(&id)
                        .and_then(|fh| model.link_spans.get(fh))
                        .and_then(|spans| {
                            spans
                                .iter()
                                .find(|(r, _, _)| point_in_rect(*r, x, y))
                                .map(|(_, l, m)| (l.clone(), m.clone()))
                        })
                };
                if let Some((link, markup)) = span {
                    let args = match (
                        self.lua.create_string(&link),
                        self.lua.create_string(&markup),
                    ) {
                        (Ok(l), Ok(m)) => {
                            vec![Value::String(l), Value::String(m), btn.clone()]
                        }
                        _ => Vec::new(),
                    };
                    if !args.is_empty() {
                        if let Err(e) =
                            event::fire_widget_handler(&self.lua, id, "OnHyperlinkClick", args)
                        {
                            self.push_error(e);
                        }
                    }
                }
            }
        }
        // The EditBox click law (`0x77b800`): a LeftButton press inside an EditBox places the
        // cursor at the clicked char, starts the drag, and focuses it UNCONDITIONALLY
        // (independent of autoFocus), before any OnClick fires. A release ends any drag.
        if button == "LeftButton" {
            if down {
                if let Some(id) = hit_id {
                    editbox::click(&self.lua, id, x, y);
                }
            } else {
                editbox::drag_end(&self.lua);
            }
        }
        // The drag trio's stop/receive pair, fired outside the model borrow above (only for a
        // gesture that actually started — see `cursor::DragRelease::started`).
        if let Some(release) = drag_release.filter(|r| r.started) {
            let source_id = {
                let model = self.model_ref();
                model
                    .arena
                    .frame(release.source)
                    .is_some()
                    .then(|| model.frame_to_id.get(&release.source).copied())
                    .flatten()
            };
            if let Some(id) = source_id {
                if let Err(e) = event::fire_widget_handler(&self.lua, id, "OnDragStop", Vec::new())
                {
                    self.push_error(e);
                }
            }
            if let Some(id) = hit_id {
                if let Err(e) =
                    event::fire_widget_handler(&self.lua, id, "OnReceiveDrag", Vec::new())
                {
                    self.push_error(e);
                }
            }
        }
        if let Some(id) = click_id {
            // One click home for the physical and programmatic (`Click()`) paths: gated on a
            // Button's enabled flag, a CheckButton toggles before OnClick fires (button.rs).
            button::click_button(&self.lua, id, button, down);
        }
        hit_id.is_some() || world_dropped
    }

    /// A mouse-wheel spin at `(x, y)`: fires `OnMouseWheel(self, delta)` on the hit frame — or,
    /// when the hit frame carries no OnMouseWheel handler, on its NEAREST ANCESTOR that does (the
    /// client's bubbling: a reward-row Button inside a ScrollFrame doesn't eat the wheel, the
    /// pane keeps scrolling under the cursor). WoW's convention is `+1` per notch up / `-1` down
    /// (`warcraft.wiki.gg/wiki/UIHANDLER_OnMouseWheel`); `delta` is passed through as given, so
    /// the app-side feed picks the magnitude/sign. Handler errors are collected into
    /// [`UiScript::errors`].
    pub fn mouse_wheel(&mut self, x: f32, y: f32, delta: f32) {
        let target: Option<u32> = {
            let hit = self.hit_test(x, y);
            let model = self.model_ref();
            hit.and_then(|id| {
                let mut h = *model.id_to_frame.get(&id)?;
                loop {
                    if model
                        .scripts
                        .get(&h)
                        .is_some_and(|s| s.contains("OnMouseWheel"))
                    {
                        // A frame carrying a handler was registered through Lua, so it has an id.
                        return model.frame_to_id.get(&h).copied();
                    }
                    h = model.arena.frame(h)?.parent?;
                }
            })
        };
        if let Some(id) = target {
            if let Err(e) = event::fire_widget_handler(
                &self.lua,
                id,
                "OnMouseWheel",
                vec![Value::Number(f64::from(delta))],
            ) {
                self.push_error(e);
            }
        }
    }
}

/// Point-in-rect for hit-testing, in WoW UI coords (**y-up**): the cursor `(x, y)` is inside `r` when
/// it lies within `[left, right] × [bottom, top]` (edges inclusive). This is the same space
/// [`UiScript::resolve`]/[`UiScript::extract`] produce rects in — the verified `point_in_rect
/// 0x76b020` primitive (`propagation.md`), modeled on our [`Rect`] convention.
pub(super) fn point_in_rect(r: Rect, x: f32, y: f32) -> bool {
    x >= r.left && x <= r.right && y >= r.bottom && y <= r.top
}

/// A frame's **mouse** rect: its resolved rect shrunk by `[left, right, top, bottom]`
/// ([`crate::widget::Frame::hit_rect_insets`]). y-up, so a `top` inset lowers `top` and a `bottom`
/// inset raises `bottom`. Insets that over-shrink collapse the rect rather than inverting it — an
/// empty rect simply captures nothing, which is what the API's own "inset past the far edge" case
/// means.
fn inset_rect(r: Rect, [left, right, top, bottom]: [f32; 4]) -> Rect {
    Rect {
        left: r.left + left,
        right: (r.right - right).max(r.left + left),
        top: r.top - top,
        bottom: (r.bottom + bottom).min(r.top - top),
    }
}
