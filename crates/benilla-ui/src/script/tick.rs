//! The host runtime loop — an `impl UiScript` block beside its concern (the `layout.rs` pattern):
//! the event fan-out to registered frames ([`UiScript::fire_event`]), the per-frame advance
//! ([`UiScript::tick`]: OnUpdate + the engine-side fades/cooldowns), and the FrameXML session clock
//! ([`UiScript::now`]). The low-level handler-firing these drive lives in [`super::event`].

use crate::widget::FrameHandle;

use super::{editbox, event, tooltip, ScriptValue, UiScript};

impl UiScript {
    /// Queue an event to fire at the **start of the next tick**, into the same
    /// [`super::Model::pending_events`] list the engine's own bindings use — so it lands *after*
    /// everything queued earlier in the frame and before that tick's `OnUpdate` pass.
    ///
    /// **Ordering, not laziness, is the whole reason this is public** (decision 1750). A benilla
    /// drain runs a step behind the input pass that fed it, so an event it fires *immediately*
    /// arrives BEFORE events the same input already queued — the reverse of the reference, where
    /// the deferral happens inside the very call that queued them. That inversion is not
    /// hypothetical: the soulbind confirm raised by placing an item into a worn slot was cancelled
    /// one tick later by that same place's own `CURSOR_UPDATE`, whose `StaticPopup_Hide` is
    /// `UIParent.lua:356-360` transcribed correctly. Queueing restores the reference's relative
    /// order — cursor-change first, question second — while a *later* cursor change still retires
    /// the question, which is exactly what that arm is for.
    ///
    /// Use [`Self::fire_event`] for anything whose ordering against in-flight queued events does
    /// not matter, which is nearly everything.
    pub fn queue_event(&mut self, event: &str, args: Vec<ScriptValue>) {
        self.model_mut()
            .pending_events
            .push((event.to_string(), args));
    }

    /// Fire an event to every frame registered for it, invoking their `OnEvent` handlers (RF-0025):
    /// both the legacy `this`/`event`/`arg1..argN` globals *and* the modern `(self, event, ...)`
    /// arguments. Events reach registered frames regardless of visibility, **in registration
    /// order** — the client's `SignalEvent 0x703e50` walks the per-event listener list head-first
    /// (tail-append insert `0x7052d0`, wow-re `event-dispatch-order.md`: FIFO, first-registered
    /// fires first). Cross-frame order is a LAW consumers depend on: both ZoneText frames write
    /// `PVPInfoTextString` on one event — the last writer decides. Handler errors are collected
    /// into [`UiScript::errors`], never panicking.
    ///
    /// **The walk steps by a next-listener SAVED BEFORE the handler runs** (`0x703ee8: mov
    /// edi,[eax+0x4]` — "save the next node before firing"; decision 1324), never by list index.
    /// That is what makes a handler that UNREGISTERS ITSELF mid-dispatch unable to rob its
    /// successor — and it is load-bearing in the wild: AceEvent-2.0 treats `PLAYER_LOGIN` /
    /// `VARIABLES_LOADED` as fire-once events and unregisters its frame *inside* the handler, so
    /// an index walk skipped whichever addon registered right after AceEvent (Bagnon_Forever's
    /// saved-bag DB never initialized — the director's `SaveBagData:76` error dialogs). A frame
    /// registered mid-dispatch tail-appends and is still visited; a handler that unregisters the
    /// walk's SAVED next stops the dispatch there (the reference frees the node and walks into
    /// its zeroed links — memory-accident territory we render as a deterministic stop, a knowing
    /// divergence from an accident, not from a mechanism).
    pub fn fire_event(&mut self, event: &str, args: Vec<ScriptValue>) {
        let mut at = {
            let model = self.model_mut();
            model
                .event_to_frames
                .get(event)
                .and_then(|l| l.first().copied())
        };
        while let Some(h) = at {
            let mut model = self.model_mut();
            // The saved handle must still be registered — its removal (by the previous handler)
            // ends the walk, per the doc above.
            let Some(pos) = model
                .event_to_frames
                .get(event)
                .and_then(|l| l.iter().position(|&x| x == h))
            else {
                break;
            };
            let next = model
                .event_to_frames
                .get(event)
                .and_then(|l| l.get(pos + 1).copied());
            let id = model.frame_id(h);
            drop(model);
            if let Err(e) = event::fire_event_handler(&self.lua, id, event, &args) {
                self.push_error(e);
            }
            at = next;
        }
    }

    /// Advance time: run `OnUpdate(self, elapsed)` on every *effectively-visible* frame that has one
    /// (RF-0025: OnUpdate → `this` + `arg1 = elapsed`). Errors collected, never panicking.
    /// Also advances the `GetTime()` clock — the FrameXML session clock (seconds, monotonic,
    /// arbitrary epoch like the real client's), kept as the `__benilla_now` global so the stdlib
    /// binding reads it without a host round-trip. Reference FrameXML (CastingBarFrame & co.)
    /// anchors durations on GetTime; the clock advancing in the same call that fires OnUpdate
    /// keeps the two views of time consistent within a frame.
    /// The current `GetTime()` value (`__benilla_now`, seconds) — the FrameXML session clock. The app
    /// reads it to stamp an absolute expiry into the clock a Lua countdown reads (the aura feed's
    /// `expirationTime`, decision 0257); it is the same value `GetTime()` returns inside the VM.
    pub fn now(&self) -> f64 {
        self.lua.globals().get("__benilla_now").unwrap_or(0.0)
    }

    pub fn tick(&mut self, elapsed: f32) {
        let clock = {
            let g = self.lua.globals();
            let now: f64 = g.get("__benilla_now").unwrap_or(0.0);
            g.set("__benilla_now", now + f64::from(elapsed))
        };
        if let Err(e) = clock {
            self.push_error(e);
        }
        // The focused edit box's caret blink (`0x77a790` runs on the client's frame tick).
        editbox::tick_blink(&self.lua, elapsed);
        // Events queued by Lua bindings last tick (`Model::pending_events` — e.g. `SetMapZoom` →
        // `WORLD_MAP_UPDATE`; the cursor arc's `CURSOR_UPDATE`/`ITEM_LOCK_CHANGED`/
        // `DELETE_ITEM_CONFIRM`, decision 0216) fire first, so handlers see them before this
        // frame's OnUpdate runs.
        let pending = std::mem::take(&mut self.model_mut().pending_events);
        for (event, args) in pending {
            self.fire_event(&event, args);
        }
        let ids: Vec<u32> = {
            let mut model = self.model_mut();
            // The OnUpdate population is its own list (decision 1446) — maintained by
            // `SetScript`, `scripts`' one writer — so this reads a few hundred handles instead
            // of re-filtering the whole scripts map. A destroyed frame's handle stays until its
            // liveness check fails once, then compacts here.
            let frames: Vec<FrameHandle> = model
                .on_update_frames
                .iter()
                .copied()
                .filter(|&h| model.arena.frame(h).is_some_and(|f| f.effective_visible))
                .collect();
            if model
                .on_update_frames
                .iter()
                .any(|&h| model.arena.frame(h).is_none())
            {
                let arena = &model.arena;
                let live: Vec<FrameHandle> = model
                    .on_update_frames
                    .iter()
                    .copied()
                    .filter(|&h| arena.frame(h).is_some())
                    .collect();
                model.on_update_frames = live;
            }
            let mut ids: Vec<u32> = frames.into_iter().map(|h| model.frame_id(h)).collect();
            // **Deterministic order, by frame id — i.e. creation order.**
            //
            // `model.scripts` is a hash map, so this sweep used to fire OnUpdate in an order that
            // varied per process. That was invisible for as long as every geometry getter answered
            // from the PREVIOUS frame's resolve: each handler saw the same stale world regardless
            // of when it ran. The moment the getters began settling on demand (so a handler can
            // observe work an earlier handler did this same sweep), the order became load-bearing
            // and the macro window's tab width started flipping 164/167 run to run.
            //
            // Random order was always wrong — the reference sweeps a defined order — the staleness
            // was merely hiding it. Creation order is stable, matches FrameXML declaration order,
            // and is what a handler reading a sibling's geometry should see.
            ids.sort_unstable();
            ids
        };
        for id in ids {
            if let Err(e) = event::fire_update_handler(&self.lua, id, elapsed) {
                self.push_error(e);
            }
        }
        // Advance every ScrollingMessageFrame's per-line fade (the client's OnUpdate `0x788460`).
        // Independent of the frame's own OnUpdate script — the fade is C++ behavior, not Lua. The
        // AtBottom freeze gate lives inside `ScrollingMessageState::tick`.
        let now = self.now();
        let mut model = self.model_mut();
        // The sibling class's OnUpdate (`0x786200`): the same two-phase fade with no scroll gate,
        // plus the capacity law that is this class's stand-in for `maxLines` — the cap is what fits
        // vertically, so it needs the frame's resolved rect and is collected first (the arena walk
        // below holds a mutable borrow that cannot also read `model.resolved`).
        // Both per-kind walks below ride the arena's ticked-kind registry (decision 1446):
        // dozens of handles instead of two full-arena sweeps per tick.
        let ticked: Vec<FrameHandle> = model.arena.ticked_kinds().to_vec();
        let message_frames: Vec<(FrameHandle, usize)> = ticked
            .iter()
            .copied()
            .filter(|&h| {
                model
                    .arena
                    .frame(h)
                    .is_some_and(|f| matches!(f.kind_state, crate::widget::KindState::Message(_)))
            })
            .map(|h| (h, Self::message_viewport_rows(&model, h)))
            .collect();
        for (h, viewport_rows) in message_frames {
            if let Some(crate::widget::KindState::Message(mf)) =
                model.arena.frame_mut(h).map(|f| &mut f.kind_state)
            {
                mf.tick(elapsed);
                mf.trim_to_viewport(viewport_rows);
            }
        }
        let mut finished_cooldowns: Vec<FrameHandle> = Vec::new();
        for &h in &ticked {
            let Some(frame) = model.arena.frame_mut(h) else {
                continue;
            };
            if let crate::widget::KindState::ScrollingMessage(smf) = &mut frame.kind_state {
                smf.tick(elapsed);
            }
            // A Cooldown whose flash has finished hides itself — the reference machine's
            // `OnAnimFinished` → `Hide()` edge (`Cooldown.lua`), modeled engine-side like the
            // message fade above (C++-equivalent behavior, not Lua).
            if let crate::widget::KindState::Cooldown(cd) = &frame.kind_state {
                if frame.shown && cd.duration > 0.0 && now >= cd.finished_at() {
                    finished_cooldowns.push(h);
                }
            }
        }
        for h in finished_cooldowns {
            model.arena.set_shown(h, false);
        }
        drop(model);
        // Advance fading tooltips (FadeOut's ramp + end-of-ramp hide) — engine behavior like the
        // message fade above, decision 0274.
        tooltip::tick_fades(&self.lua);
        // The hover RE-PICK ([`Model::hover_repick`]): the world under a stationary cursor
        // changed this tick (the hovered frame hid, or a frame was shown over the cursor), so
        // re-run the hover walk at the SAVED cursor position — the reference's own pump tail
        // (`0x765650` → `0x7660d0` with the saved event, didn't-move coalesce bypassed).
        // `mouse_move` at the unchanged point is exactly that walk: the hidden frame's OnLeave
        // already fired at hide time and `mouseover` is clear, so only the new winner's
        // boundary fires — no second OnLeave, `OnEnter` with no physical mouse move.
        let repick = {
            let mut model = self.model_mut();
            let due = model.hover_repick;
            model.hover_repick = false;
            due.then_some(model.cursor_pos)
        };
        if let Some((x, y)) = repick {
            self.mouse_move(x, y);
        }
        // `WOW_UI_HANDLERS=<secs>` — who spent the frame (decision 1395). Last, so a report covers
        // everything this tick fired; a no-op unless the instrument is armed.
        self.report_handler_profile(elapsed);
    }
}
