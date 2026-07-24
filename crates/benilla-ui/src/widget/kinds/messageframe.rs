use std::collections::VecDeque;

/// One line held in a [`ScrollingMessageState`] ring — its text, its already-quantized color, its
/// live fade state (a per-line snapshot of the frame's `timeVisible`/`fadeDuration` at insert,
/// msgframe-runtime.md: MessageData `+0xc`/`+0x10`), and its host-measured wrapped row count (the
/// message-line half of the measure round-trip — a long chat line occupies as many display rows as
/// it wraps into, so the bands above it shift up by real content height).
#[derive(Clone, Debug, PartialEq)]
pub struct MessageLine {
    /// The line text (already formatted app-side: `[Name]: text`, `Name yells: text`, …).
    pub text: String,
    /// The RGB the line draws at, **byte-quantized** at insert (`AddMessage` `trunc(x*255+0.5)`,
    /// round-half-up; alpha is never stored — it is forced opaque and then driven by the fade).
    pub color: [u8; 3],
    /// Remaining phase-1 countdown (the `timeVisible` snapshot ticking down); while `> 0` the line
    /// holds full alpha.
    pub time_left: f32,
    /// Remaining phase-2 countdown (the `fadeDuration` snapshot). Once `time_left` hits `0` this
    /// counts down and drives [`Self::alpha`].
    pub fade_left: f32,
    /// The current display alpha in `[0, 1]` (byte-quantized in the fade tick,
    /// `trunc(remaining/fadeDuration*255)`). `0` once fully faded — the line stays in the ring
    /// (a ring slot is freed only by drop-oldest / `SetMaxLines`), it just draws nothing (its rows
    /// still hold their place — the reference's chat never re-packs when old lines fade).
    pub alpha: f32,
    /// How many display rows the line wraps into at the frame's current width/font — host-measured
    /// through the message-line measure round-trip
    /// ([`crate::script::UiScript::message_lines_needing_measure`]). `1` until measured.
    pub rows: u16,
    /// Cache key of the [`Self::rows`] measurement — hash of (text, font, height, wrap width),
    /// computed engine-side. `0` = unmeasured (drawn as one row until the same-frame answer lands);
    /// a width/font change mismatches the key and re-requests.
    pub rows_key: u64,
}

/// A `CSimpleMessageScrollFrame`'s runtime state (msgframe-runtime.md, byte-verified §5 pair): a true
/// ring of `max_lines` (drop-oldest, independent of how many display), each line carrying its own
/// fade snapshot; a scrollback cursor counted **up from the bottom** (`0` = newest = AtBottom); and
/// the frame-level fade config the per-line snapshots copy at insert.
#[derive(Clone, Debug, PartialEq)]
pub struct ScrollingMessageState {
    /// The line ring, newest at the back. Capacity is enforced by drop-oldest in [`Self::add`].
    pub lines: VecDeque<MessageLine>,
    /// The ring capacity (`maxLines`; ctor default 8, ChatFrame.xml sets 128). `SetMaxLines` is
    /// **destructive** (msgframe-runtime.md).
    pub max_lines: usize,
    /// `timeVisible`/`displayDuration` — phase-1 duration a new line holds full alpha (ctor 10.0s;
    /// ChatFrame 120.0s).
    pub time_visible: f32,
    /// `fadeDuration` — phase-2 fade ramp length (ctor 3.0s). `0` ⇒ the line vanishes instantly at
    /// phase-1 expiry (no ramp).
    pub fade_duration: f32,
    /// `fadingEnabled` (ctor 1). While false, lines never fade.
    pub fading_enabled: bool,
    /// The scrollback offset, counted up from the newest line: `0` = pinned to the bottom (AtBottom,
    /// where fades tick); `n` = the view is `n` lines older. Clamped in [`Self::scroll_up`].
    pub scroll_offset: usize,
}

impl Default for ScrollingMessageState {
    fn default() -> ScrollingMessageState {
        // The shared CSimpleMessageScrollFrame ctor defaults (msgframe-runtime.md §"Shared ctor
        // defaults": fadingEnabled=1, timeVisible=10.0, fadeDuration=3.0; SMF maxLines=8).
        ScrollingMessageState {
            lines: VecDeque::new(),
            max_lines: 8,
            time_visible: 10.0,
            fade_duration: 3.0,
            fading_enabled: true,
            scroll_offset: 0,
        }
    }
}

/// Byte-quantize a `0..1` color/alpha component the way `AddMessage` does (`clamp[0,1]`, `*255`,
/// `+0.5`, truncate — round-half-up; msgframe-runtime.md AddMessage `0x788150`).
pub fn quantize_u8(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0 + 0.5).trunc() as u8
}

impl ScrollingMessageState {
    /// `AddMessage(text, r, g, b)` (`0x788150`): quantize the color, snapshot the current
    /// `timeVisible`/`fadeDuration` onto the new line, and push it at the ring's newest slot,
    /// dropping the oldest when over `max_lines`. A view scrolled up stays anchored on the same
    /// content (the ring cursor is a slot, not an offset — msgframe-runtime.md).
    pub fn add(&mut self, text: String, r: f32, g: f32, b: f32) {
        let line = MessageLine {
            text,
            color: [quantize_u8(r), quantize_u8(g), quantize_u8(b)],
            time_left: self.time_visible,
            fade_left: self.fade_duration,
            alpha: 1.0,
            rows: 1,
            rows_key: 0,
        };
        self.lines.push_back(line);
        // Scrolled up: keep the same lines in view as the ring grows below them.
        if self.scroll_offset > 0 {
            self.scroll_offset += 1;
        }
        while self.lines.len() > self.max_lines {
            self.lines.pop_front();
            // The dropped line was above the view — walk the anchor back down with it.
            self.scroll_offset = self.scroll_offset.saturating_sub(1);
        }
        self.clamp_scroll();
    }

    /// `SetMaxLines(n)` — **destructive** (`0x787dd0`): frees every line + resets the cursor, then
    /// sets the new capacity. Not a preserving resize.
    pub fn set_max_lines(&mut self, n: usize) {
        self.lines.clear();
        self.scroll_offset = 0;
        self.max_lines = n.max(1);
    }

    /// `Clear` (`0x7882b0`): retire every line immediately (no fade), reset to the bottom.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.scroll_offset = 0;
    }

    /// Whether the view is pinned to the newest line (`AtBottom` — the only state in which fades
    /// tick).
    pub fn at_bottom(&self) -> bool {
        self.scroll_offset == 0
    }

    /// Whether the view is scrolled as far back as the ring allows (`AtTop`).
    pub fn at_top(&self) -> bool {
        self.scroll_offset >= self.max_scroll()
    }

    /// The furthest the view can scroll up: enough to bring the oldest line to the bottom row.
    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(1)
    }

    fn clamp_scroll(&mut self) {
        self.scroll_offset = self.scroll_offset.min(self.max_scroll());
    }

    /// `ScrollUp` (`0x788610`) — one line older (no-op at the top).
    pub fn scroll_up(&mut self) {
        self.scroll_offset = (self.scroll_offset + 1).min(self.max_scroll());
    }

    /// `ScrollDown` (`0x788650`) — one line newer (no-op at the bottom).
    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    /// `ScrollToBottom` (`0x7886d0`) — jump to the newest line (re-arms the fade).
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// `ScrollToTop` (`0x788690`) — jump to the oldest line.
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = self.max_scroll();
    }

    /// How many messages the view shows from the current anchor, given the viewport's row budget
    /// (`floor(frame height / pitch)`): walk older from the anchor summing each message's wrapped
    /// [`MessageLine::rows`] until the budget is spent. A partially-fitting message counts (it draws,
    /// clipped), matching [`emit`](crate::script::UiScript::extract)'s band walk. At least 1 when
    /// any line exists.
    pub fn displayed_count(&self, viewport_rows: usize) -> usize {
        if self.lines.is_empty() || viewport_rows == 0 {
            return 0;
        }
        let top_index = self.lines.len().saturating_sub(1 + self.scroll_offset);
        let mut used = 0usize;
        let mut count = 0usize;
        for idx in (0..=top_index).rev() {
            if used >= viewport_rows {
                break;
            }
            used += usize::from(self.lines[idx].rows.max(1));
            count += 1;
        }
        count.max(1)
    }

    /// `PageUp` — the client pages by `numLinesDisplayed` scroll steps then one back
    /// (msgframe-runtime.md: net page = displayed − 1, one line of overlap).
    pub fn page_up(&mut self, viewport_rows: usize) {
        let page = self.displayed_count(viewport_rows).saturating_sub(1).max(1);
        self.scroll_offset = (self.scroll_offset + page).min(self.max_scroll());
    }

    /// `PageDown` — the same page size toward the newest line.
    pub fn page_down(&mut self, viewport_rows: usize) {
        let page = self.displayed_count(viewport_rows).saturating_sub(1).max(1);
        self.scroll_offset = self.scroll_offset.saturating_sub(page);
    }

    /// Advance the fade by `dt` (the OnUpdate tick, `0x788460`). The entry gate is **AtBottom** and
    /// `fading_enabled` — scrolled up, every line holds its current alpha and nothing un-fades. Each
    /// line runs phase 1 (`time_left` countdown at full alpha), then phase 2 (`fade_left` countdown,
    /// alpha = `trunc(fade_left/fade_duration*255)`); `fade_duration == 0` snaps straight to 0.
    pub fn tick(&mut self, dt: f32) {
        if !self.fading_enabled || !self.at_bottom() {
            return;
        }
        for line in &mut self.lines {
            if line.time_left > 0.0 {
                line.time_left -= dt;
                continue;
            }
            if self.fade_duration <= 0.0 {
                // No ramp — the line vanishes the instant phase 1 expires.
                line.alpha = 0.0;
                continue;
            }
            line.fade_left -= dt;
            if line.fade_left <= 0.0 {
                line.fade_left = 0.0;
                line.alpha = 0.0;
            } else {
                // Divisor is the LIVE frame fadeDuration (a mid-fade SetFadeDuration rescales the
                // ramp), byte-quantized like the client.
                let byte = quantize_fade(line.fade_left / self.fade_duration);
                line.alpha = f32::from(byte) / 255.0;
            }
        }
    }
}

/// Phase-2 alpha quantization: `trunc(x*255)` (no `+0.5` — the fade tick truncates, `0x788547`).
fn quantize_fade(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0).trunc() as u8
}
