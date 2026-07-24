//! The widget-kind vocabulary + per-kind state (the "later layer" over the frame arena): the
//! 13 client widget classes ([`FrameKind`]), the two region leaves ([`RegionKind`]), and the
//! modeled per-kind behavior ([`KindState`]) that a `CSimple*` subtype adds over `CSimpleFrame`
//! (RF-28 tables; decision 0068). Split from the arena so each grows independently.

use std::collections::HashSet;

use super::{FrameHandle, RegionHandle};

mod editbox;
mod messageframe;
pub use editbox::*;
pub use messageframe::*;

/// The widget subtype of a [`Frame`]. Each corresponds to a client `CSimple*` class
/// (`frame-model.md`, the 13 widget factories, decision 0068). Kinds with modeled behavior carry it
/// in [`Frame::kind_state`] (StatusBar today); the rest are tags whose per-kind behavior (button
/// states, editbox text, …) is a later layer over this arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameKind {
    /// Plain `CSimpleFrame` — the base container.
    Frame,
    Button,
    CheckButton,
    EditBox,
    StatusBar,
    Slider,
    ScrollFrame,
    Model,
    MessageFrame,
    /// `CSimpleMessageScrollFrame` — the scrolling, ring-buffered message frame (the chat window's
    /// class). Its behavior (the line ring, per-line fade, wheel scrollback) is modeled in
    /// [`KindState::ScrollingMessage`]. Sibling of [`FrameKind::MessageFrame`] (different C++ ctor),
    /// not a subclass — offsets/semantics do not transfer (msgframe-runtime.md).
    ScrollingMessageFrame,
    ColorSelect,
    SimpleHtml,
    MovieFrame,
    /// The `GameTooltip` widget family (decision 0274). Like [`FrameKind::Minimap`]/
    /// [`FrameKind::Cooldown`] a *game-layer* factory over `CSimpleFrame`; its modeled behavior —
    /// the line stack, owner/anchor law, auto-size, fade — lives in [`KindState::Tooltip`] and
    /// `script::tooltip`. The real class's Lua surface is the 38-binding family wow-re pinned
    /// (`ui/scratch/bindings.md` 0x530c40–0x5364a0); the line/color primitives are byte-diffed
    /// (`luabind_530`), the content builders land per 0274's phases.
    GameTooltip,
    /// The `<Minimap>` widget (Minimap.xml's circular HUD map). Like [`FrameKind::GameTooltip`], a
    /// *game-layer* factory, not one of the 13 base FrameXML types (`RegisterFrameFactories`
    /// `0x495940` registers the game UI's own widget set at `CGGameUI::Initialize`; ui node). The
    /// widget is a sized hole the game engine draws into — tiles, blips, and the player arrow are
    /// the app renderer's job (decision 0203); the engine core carries only the rect and the zoom
    /// state ([`KindState::Minimap`]).
    Minimap,
    /// The cooldown sweep widget (decision 0137 phase 4). The 1.12 reference builds it as a
    /// `Model` playing `UI-Cooldown-Indicator.mdx` (`CooldownFrameTemplate` + `Cooldown.lua`'s
    /// scrub/flash/hide machine); benilla models the *mechanism* as a first-class widget — the
    /// Era API's own `Cooldown` frame type — whose state machine lives engine-side
    /// ([`KindState::Cooldown`]) and whose pie-wipe/flash pixels are the app renderer's job.
    Cooldown,
}

/// Whether a [`Region`] leaf is a texture or a text string. These are the client's two non-frame
/// region types (`CScriptRegion`-derived leaves, `frame-model.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegionKind {
    /// A `Texture` (BLP quad).
    Texture,
    /// A `FontString` (text run).
    FontString,
}

/// Per-kind widget state — the "later layer" over the kind tag, for the kinds whose behavior is
/// modeled. Lives on the [`Frame`] node (the client's `CSimpleStatusBar` etc. extend `CSimpleFrame`
/// with exactly such members).
#[derive(Clone, Debug, PartialEq)]
pub enum KindState {
    /// No modeled per-kind behavior (a plain frame, or a kind whose behavior is a later layer).
    None,
    /// `CSimpleStatusBar` (factory `0x6eef20`; LoadXML table RF-28): a value in `[min, max]` filling
    /// a bar texture along the orientation axis.
    StatusBar(StatusBarState),
    /// `CSimpleButton`/`CSimpleCheckbox` (factories `0x6eeab0`/`0x6eeb30`; LoadXML tables RF-28) —
    /// the state-texture array + ButtonText. [`FrameKind::CheckButton`] shares this state (the
    /// client's checkbox extends the button class), using the two `checked` members.
    Button(ButtonState),
    /// `CSimpleEditBox` (factory `0x6eec70`; runtime model RF-0082): the text buffer, cursor,
    /// selection, config flags, and the FontString the text renders through.
    EditBox(EditBoxState),
    /// `CSimpleMessageScrollFrame` (ctor `0x787670`; runtime model msgframe-runtime.md): the line
    /// ring, the per-line fade snapshots + phases, and the scrollback cursor.
    ScrollingMessage(ScrollingMessageState),
    /// `CSimpleScrollFrame` (decision 0112 — the ScrollFrame mechanism, the engine's last structural
    /// gap: the quest log's detail pane, chat history, and every long-content window need it): the
    /// scroll child + the vertical scroll offset. The mechanism is spec-faithful (the documented
    /// `SetScrollChild`/`SetVerticalScroll` contract, same posture as StatusBar's fill), not
    /// byte-pinned. Horizontal scroll is out of scope (no 1.12 template drives it).
    Scroll(ScrollFrameState),
    /// `CSimpleSlider` (factory `0x6eee40`; LoadXML table `0x789580`, RF-28): a value in `[min, max]`
    /// with a step and orientation, positioning a thumb texture along the track. The mechanism is
    /// spec-faithful to the documented Slider widget contract (same posture as StatusBar's fill /
    /// ScrollFrame's scroll), not byte-pinned. Every scrollbar is one (decision 0250).
    Slider(SliderState),
    /// The `<Minimap>` widget's zoom state (decision 0203). The engine core carries only what the
    /// Lua API reads/writes (`GetZoom`/`SetZoom`/`GetZoomLevels`); the tile/blip render is app-side.
    Minimap(MinimapState),
    /// The cooldown widget's timer ([`CooldownState`]) — the reference `Cooldown.lua` machine's
    /// inputs; the sweep/flash phases derive from them at extract time.
    Cooldown(CooldownState),
    /// The GameTooltip widget's line stack + owner/fade state ([`TooltipState`], decision 0274).
    Tooltip(TooltipState),
}

/// A `GameTooltip`'s runtime state (decision 0274). The line *text/color/wrap* is not duplicated
/// here — each line pair is a real named FontString region (`<name>TextLeftN`/`TextRightN`,
/// engine-created on demand, published as Lua globals exactly like the real template's 30
/// pre-declared pairs, which reference Lua addresses by name: `GameTooltipTextLeft1:SetTextColor`).
/// The state carries what the C++ class carries beside its strings: the live line count, the
/// owner + the region pool, the minimum width, and the fade clock.
///
/// Layout is engine-side (`script::tooltip::layout_tooltips`, a `resolve` pre-pass): frame size =
/// max measured line width + 2·[`TOOLTIP_PAD`] × summed line heights + gaps, right columns
/// re-pointed flush to the text inset — the job the real client's C++ line layout does over the
/// template's static anchors (today's Lua `OnUpdate` measure loop, retired).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TooltipState {
    /// Lines currently filled (`NumLines`). Regions past this index exist but are hidden.
    pub num_lines: usize,
    /// The left-column FontString pool, index i = line i+1. Grown on demand — no line cap (the
    /// real template ships 30 pairs and the class grows via `AddFontStrings`; one mechanism here).
    pub left_lines: Vec<RegionHandle>,
    /// The right-column pool (the `AddDoubleLine`/`TextRightN` half), parallel to `left_lines`.
    pub right_lines: Vec<RegionHandle>,
    /// `SetOwner`'s frame — dropped on hide (`IsOwned` is the hover re-enter loop's gate,
    /// ref `ContainerFrame.lua` OnUpdate).
    pub owner: Option<FrameHandle>,
    /// `SetMinimumWidth(w)` — a floor on the auto-sized width (the ref's money-row floor).
    /// Cleared (0.0) by `ClearLines`/hide, like the content.
    pub min_width: f32,
    /// `FadeOut()`'s start on the `GetTime` clock; `None` = not fading. Any fresh content
    /// (SetOwner/SetText/AddLine/Show) cancels the fade and restores full alpha.
    pub fade_start: Option<f64>,
    /// The unit token this tooltip currently shows (`SetUnit`/the world mouseover) — the health
    /// watcher's key: a `set_unit` push for this token re-drives the status bar (decision 0276's
    /// verified refresh law). Dropped with the content.
    pub unit_token: Option<String>,
    /// This tooltip currently shows WORLD-hover content (a mouseover unit, GameObject, or
    /// corpse) — the fade-on-loss gate (`world_tooltip_fade`); a window hover never fades.
    /// Dropped with the content.
    pub world_owned: bool,
    /// ARMED for a shopping-compare render: the next `SetInventoryItem` on this frame renders in
    /// the byte law's compare mode (`[arg+0x14]≠0` compact + `[arg+0x18]≠0` "Currently Equipped"
    /// — wow-re tooltip-content-law.md). Set by the engine right before it fires
    /// `SHOW_COMPARE_TOOLTIP` for this frame's index and consumed by that render. Survives
    /// `SetOwner`'s content clear (FrameXML SetOwners between the arm and the render — ref
    /// PaperDollFrame.lua:621-640); how the real engine plumbs the flag to `0x52b650` is
    /// unrecorded, so this seam is the INTERIM model of it.
    pub compare_armed: bool,
    /// The paperdoll slot ids the item currently shown could equip into (empty = not
    /// equippable / not an item tooltip) — set by the item render on the main GameTooltip, read
    /// by the shift-edge compare drive to (re)fire `SHOW_COMPARE_TOOLTIP`. Dropped with the
    /// content.
    pub compare_slots: Vec<u32>,
    /// `SetPadding(w)` — extra width beyond the measured content (ref ItemRefTooltip's
    /// OnLoad `SetPadding(16)`: room for the corner close button). 0 for ordinary tooltips.
    pub padding: f32,
    /// Line 1 was ADOPTED from XML-declared regions (`<name>TextLeft1` in the instance/template
    /// — ShoppingTooltipTemplate's small-font ladder): lines the engine creates past the
    /// declared set clone the previous line's faces instead of the header/text defaults, the
    /// real class's grow-past-the-template behavior.
    pub xml_declared_lines: bool,
}

/// The tooltip plate's text inset — the real template seats `TextLeft1` at TOPLEFT (10,−10).
pub const TOOLTIP_PAD: f32 = 10.0;
/// Inter-line gap — each `TextLeftN` hangs at the previous line's BOTTOMLEFT (0,−2).
pub const TOOLTIP_LINE_GAP: f32 = 2.0;
/// The minimum gap between a double line's columns. INFERRED from the ref template's static
/// `TextRightN` offset (+40 off its partner); the C++ layout that owns the real gap isn't RE'd —
/// this is the eyeball knob if a double line reads too wide/tight (carried over from the
/// pre-0274 Lua tooltip).
pub const TOOLTIP_DOUBLE_GAP: f32 = 40.0;
/// `FadeOut`'s ramp length, seconds. INTERIM pending the 0274 §5 lifecycle verdict (the world-
/// mouseover tooltip's fade constant lives in the untraced SetUnit/mouseover path).
pub const TOOLTIP_FADE_SECS: f64 = 0.5;
/// The width a wrap-flagged line wraps at, logical px — pinned onto the line region at APPEND
/// time (`append_line`), so its first measure comes back wrapped. INTERIM pending the 0274 §5
/// (the real wrap column lives in the untraced C++ line layout); sized against the reference's
/// description/trigger-line wrap by eye.
pub const TOOLTIP_WRAP_WIDTH: f32 = 260.0;

/// The cooldown widget's timer, in the engine's `GetTime` clock (seconds): the reference
/// `CooldownFrame_SetTimer(start, duration, enable)` stores exactly this pair (its `enable == 0`
/// / non-positive gate hides instead of storing — kept in the Lua helper, ref-verbatim). The
/// three phases the reference machine derives (`Cooldown.lua`, byte-authored):
/// - `t < start + duration` — the sweep: sequence 0 scrubbed to `(now-start)/duration`.
/// - the next [`COOLDOWN_FLASH_SECS`] — the finish flash: sequence 1 played realtime.
/// - after that — hidden (`OnAnimFinished` → `Hide`), done engine-side in `tick`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CooldownState {
    /// `GetTime`-clock start seconds (0 = no timer set).
    pub start: f64,
    /// Duration seconds (0 = no timer set).
    pub duration: f64,
}

/// The finish flash's length: the model's sequence 1 is authored at exactly 1.000 s
/// (`UI-Cooldown-Indicator.m2` m2seq; played realtime by `CooldownFrame_OnUpdateModel`'s
/// `AdvanceTime`).
pub const COOLDOWN_FLASH_SECS: f64 = 1.0;

impl CooldownState {
    /// The end of the sweep (= the flash's start).
    pub fn sweep_end(&self) -> f64 {
        self.start + self.duration
    }

    /// The moment the whole display is over (flash finished → hide).
    pub fn finished_at(&self) -> f64 {
        self.sweep_end() + COOLDOWN_FLASH_SECS
    }
}

/// The Minimap widget's modeled state — the client keeps **two independent zoom indices**, chosen by
/// whether the player is inside a WMO (wow-re minimap node, `wmo-interior-minimap.md` finding 2 Q7
/// CORRECTION): the inside flag `0xceaa60` routes the outdoor index `0x86f698` (CVar `minimapZoom`,
/// chunk table `0x8116d0`) or the indoor index `0x86f69c` (CVar `minimapInsideZoom`, radius table
/// `0x8116e8` = `{150,120,90,60,40,25}` yd). `GetZoom`/`SetZoom` read/write whichever is active, and
/// each persists across transitions — so zooming in indoors does not disturb the outdoor zoom, and
/// vice versa. (An earlier reading held the second index to be a rotate-minimap CVar and the interior
/// scale to be a constant; both were wrong — superseded in wow-re.) `set_zoom` clamps to 5.
#[derive(Clone, Debug, PartialEq)]
pub struct MinimapState {
    /// The **outdoor** zoom index, `0..MINIMAP_ZOOM_LEVELS` (0 = widest, 5 = tightest). `SetZoom`
    /// clamps like the client's `0x6daa10` (clamp at 5, mark dirty).
    pub zoom: u8,
    /// The **indoor** zoom index (same range), persisted separately — the client's `0x86f69c`.
    pub inside_zoom: u8,
    /// Is the player inside a WMO interior (the client's `0xceaa60`)? Selects which index the Lua
    /// zoom API reads and writes. Pushed down by the app, which owns the WMO containment test.
    pub inside: bool,
}

/// Both minimap zoom CVars register with the default `"3"` — **not** 0 (wow-re, VERIFIED at the
/// `RegisterCVar 0x63db90` argument slot: `minimapZoom` @`0x48fc5a` and `minimapInsideZoom`
/// @`0x48fc76` each push the string `"3"` at `0x82e960`). The minimap reset path copies the
/// persisted CVar int into the live index rather than zeroing it, so 3 is what a fresh client runs.
/// **Higher index = more zoomed in**: index 3 ⇒ a 60 yd indoor radius (of the `{150…25}` table) and a
/// 133.3 yd outdoor half-extent (of the `{14…4}` chunk table). Seeding 0 here made both maps far too
/// wide — the director's "way too zoomed out" (2026-07-09).
pub const MINIMAP_DEFAULT_ZOOM: u8 = 3;

impl Default for MinimapState {
    fn default() -> Self {
        Self {
            zoom: MINIMAP_DEFAULT_ZOOM,
            inside_zoom: MINIMAP_DEFAULT_ZOOM,
            inside: false,
        }
    }
}

impl MinimapState {
    /// The zoom index the client's `get_zoom_index`/`set_zoom` operate on right now.
    pub fn active_zoom(&self) -> u8 {
        if self.inside {
            self.inside_zoom
        } else {
            self.zoom
        }
    }

    /// Write the active index (the `set_zoom` half of the same routing).
    pub fn set_active_zoom(&mut self, zoom: u8) {
        if self.inside {
            self.inside_zoom = zoom;
        } else {
            self.zoom = zoom;
        }
    }
}

/// The client's minimap zoom-level count (`get_zoom_levels` `0x6da9a0` returns the constant 6).
pub const MINIMAP_ZOOM_LEVELS: u8 = 6;

/// A `CSimpleScrollFrame`'s runtime state: the frame whose anchors are overridden to track the
/// scroll offset ([`crate::script::UiScript::resolve`]'s scroll-child override), and the current
/// vertical scroll position. `SetVerticalScroll` clamps into `[0, GetVerticalScrollRange()]`, where
/// the range is always computed live from the resolved rects (never cached here) — so this struct
/// carries only the two members the client's `SetScrollChild`/`SetVerticalScroll` actually set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScrollFrameState {
    /// The scroll child (`SetScrollChild`) — the one frame whose content pans within this frame's
    /// rect. `None` = no child (nothing to clip or offset).
    pub child: Option<FrameHandle>,
    /// The vertical scroll offset in px (`SetVerticalScroll`), always in `[0, range]`. XML
    /// y-positive-up: a positive offset lifts the child (`child.top = scrollframe.top + vertical`),
    /// bringing content below the fold into view.
    pub vertical: f32,
}

/// A Button's state model: which of the state textures draws is a *function of interaction state*
/// (the client's texture array `+0x4b8` with a current-shown pointer `+0x4c4`), so the regions all
/// exist in the arena and [`Self::region_visible`] picks at extract time.
#[derive(Clone, Debug, PartialEq)]
pub struct ButtonState {
    /// `Enable`/`Disable`. A disabled button shows its DisabledTexture and fires no clicks.
    pub enabled: bool,
    /// The scripted PUSHED state — `SetButtonState("PUSHED"/"NORMAL") 0x780270` /
    /// `GetButtonState 0x780180`, the keybind visual's engine half (ref `ActionButtonDown/Up`,
    /// `ActionButton.lua:15-28`). ORs with the mouse-derived held+hovered press in
    /// [`Self::region_visible`]; the mouse press itself stays outside the widget (the app's
    /// capture), so `GetButtonState` reads only this flag — INTERIM: a mouse-held button
    /// answers "NORMAL" here where the real engine's one state variable would say "PUSHED".
    /// Nothing in the transcribed FrameXML reads the state mid-mouse-press.
    pub pushed_state: bool,
    /// CheckButton's checked flag (`+0x4dc`; XML `checked`). Unused on a plain Button.
    pub checked: bool,
    /// `<NormalTexture>`/`SetNormalTexture` (`+0x4bc`).
    pub normal: Option<RegionHandle>,
    /// `<PushedTexture>` (`+0x4c0`) — shown while the mouse is held down over the button.
    pub pushed: Option<RegionHandle>,
    /// `<DisabledTexture>` (`+0x4b8`).
    pub disabled: Option<RegionHandle>,
    /// `<HighlightTexture>` (`+0x4c8`) — additive over the current state texture while hovered
    /// (it lives in the HIGHLIGHT draw layer, above the others, not instead of them).
    pub highlight: Option<RegionHandle>,
    /// CheckButton `<CheckedTexture>` (`+0x4e0`) — additive while checked.
    pub checked_tex: Option<RegionHandle>,
    /// CheckButton `<DisabledCheckedTexture>` (`+0x4e4`) — replaces CheckedTexture when disabled.
    pub disabled_checked: Option<RegionHandle>,
    /// The `<ButtonText>` fontstring (`+0x338`; `SetText`). Always drawn.
    pub text: Option<RegionHandle>,
    /// `RegisterForClicks`' set (the exact 1.12 API strings, e.g. `"LeftButtonUp"`,
    /// `"RightButtonDown"`) — which press/release transitions reach `OnClick`. Plain strings, not
    /// an enum: the API is an open list of `"<Button>Button<Up|Down>"` names and the input path
    /// only ever needs membership, never to enumerate it. Defaults to the client's own default
    /// (`{"LeftButtonUp"}` — a left click, on release).
    pub registered_clicks: HashSet<String>,
    /// Per-state label font-object NAMES (`<NormalFont inherits=>`/`SetTextFontObject` and the
    /// Highlight/Disabled pair): at extract, the ButtonText re-points to the current state's font
    /// object — the client's per-state CFontString font swap (UIPanelButtonTemplate's gold
    /// normal / white highlight / gray disabled label). `None` = keep the label's own paint.
    pub normal_font: Option<String>,
    /// See [`ButtonState::normal_font`] — the hovered state (falls back to `normal_font`).
    pub highlight_font: Option<String>,
    /// See [`ButtonState::normal_font`] — the disabled state.
    pub disabled_font: Option<String>,
    /// Per-state label COLOR overrides (`Button:SetTextColor` and the Highlight/Disabled pair):
    /// when the matching state is current, extract repaints the ButtonText with this color over
    /// the state font object's own paint — the dropdown kit's rows lean on all three
    /// (`info.textR/G/B`, isTitle's NORMAL-yellow and notClickable's HIGHLIGHT-white recolors of
    /// a disabled row). `None` = the state font's paint.
    pub normal_color: Option<[f32; 4]>,
    /// See [`ButtonState::normal_color`] — the hovered state (falls back to `normal_color`).
    pub highlight_color: Option<[f32; 4]>,
    /// See [`ButtonState::normal_color`] — the disabled state.
    pub disabled_color: Option<[f32; 4]>,
    /// `LockHighlight()` — the HighlightTexture draws regardless of hover until
    /// `UnlockHighlight()` (ref `CButton::LockHighlight`; the dropdown kit keeps a checked row's
    /// highlight lit).
    pub locked_highlight: bool,
}

impl Default for ButtonState {
    fn default() -> Self {
        ButtonState {
            enabled: true,
            pushed_state: false,
            checked: false,
            normal: None,
            pushed: None,
            disabled: None,
            highlight: None,
            checked_tex: None,
            disabled_checked: None,
            text: None,
            registered_clicks: HashSet::from(["LeftButtonUp".to_string()]),
            normal_font: None,
            highlight_font: None,
            disabled_font: None,
            normal_color: None,
            highlight_color: None,
            disabled_color: None,
            locked_highlight: false,
        }
    }
}

impl ButtonState {
    /// Whether a region of this button draws, given the interaction inputs (`hovered` = the cursor
    /// is over the button; `held` = a mouse press captured it and hasn't released). The exclusive
    /// state textures resolve to one "current": disabled → Disabled, with **no** Normal fallback.
    /// This is the byte-verified rule (decision 0227; wow-re
    /// `system/ui/scratch/button-check-and-state-texture.md`, `SetState 0x779790`): the shown
    /// pointer `+0x4c4` always holds the *current state's own* slot (`[this+state*4+0x4b8]`), so a
    /// null new-state slot draws nothing — the reference's empty spellbook slots are exactly this
    /// (born-disabled SpellButtons whose UI-Quickslot2 NormalTexture never shows). Held+hovered →
    /// Pushed, falling back to Normal when unset (a pressed button without pushed art keeps its
    /// normal art in the reference). Else Normal. Highlight/Checked draw additively per their own
    /// conditions. Any region that is not one of the state textures (ButtonText, user regions)
    /// always draws.
    ///
    /// One known divergence from the byte-exact `0x779790` (INTERIM, not load-bearing): the client
    /// only *updates* `+0x4c4` when the new state HAS a texture — hiding the old is gated on that,
    /// so disabling an already-shown button with a null Disabled slot leaves the OLD texture
    /// sticky-visible. This is a pure function of the current state instead, so it hides that
    /// texture. The visible case that matters — a born-disabled slot — agrees either way (never in
    /// the normal state, so nothing was ever shown). Reproducing the sticky path needs a stateful
    /// shown-pointer this model deliberately doesn't carry yet.
    pub fn region_visible(&self, rh: RegionHandle, hovered: bool, held: bool) -> bool {
        let some = Some(rh);
        if some == self.normal || some == self.pushed || some == self.disabled {
            let current = if !self.enabled {
                self.disabled
            } else if (held && hovered) || self.pushed_state {
                self.pushed.or(self.normal)
            } else {
                self.normal
            };
            return some == current;
        }
        if some == self.highlight {
            return self.enabled && (hovered || self.locked_highlight);
        }
        if some == self.checked_tex {
            return self.checked && (self.enabled || self.disabled_checked.is_none());
        }
        if some == self.disabled_checked {
            return self.checked && !self.enabled;
        }
        true
    }
}

/// A StatusBar's value model + its bar-fill region. Zero-initialized like the client's members —
/// a degenerate range (`max <= min`) draws an empty bar until the data configures it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusBarState {
    /// `SetMinMaxValues` low bound (XML `minValue`; the 1.12 loader swaps a reversed pair, RF-28).
    pub min: f32,
    /// `SetMinMaxValues` high bound (XML `maxValue`).
    pub max: f32,
    /// The current value (`SetValue`/XML `defaultValue`), clamped into `[min, max]`.
    pub value: f32,
    /// `true` = VERTICAL (fills bottom-up); `false` = HORIZONTAL (fills left-to-right), the default
    /// (shared enum table `0x811b00`: HORIZONTAL=0/VERTICAL=1).
    pub vertical: bool,
    /// The bar-fill texture region (`SetStatusBarTexture`/`<BarTexture>`), created on first set. A
    /// renderer scales this region's rect by [`Self::fraction`] along the orientation axis.
    pub bar: Option<RegionHandle>,
}

impl StatusBarState {
    /// The fill fraction `(value − min) / (max − min)`, clamped to `[0, 1]`; a degenerate range
    /// (`max <= min`) is `0.0` (an unconfigured bar draws empty, matching zero-init members).
    pub fn fraction(&self) -> f32 {
        if self.max > self.min {
            ((self.value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// A `CSimpleSlider`'s runtime state (RF-28 LoadXML `0x789580`): the value in `[min, max]`, the
/// value step, orientation, an enabled flag, and the thumb texture the engine positions by the value
/// fraction along the track. Only the members the documented `SetMinMaxValues`/`SetValue`/
/// `SetValueStep`/`SetOrientation`/`SetThumbTexture` contract sets — the thumb's *position* is
/// derived live from the fraction (never cached here), like StatusBar's fill.
///
/// **Default orientation is VERTICAL** — the opposite of [`StatusBarState`]'s HORIZONTAL default,
/// and verified against the real templates, not assumed: `UIPanelScrollBarTemplate` (every
/// scrollbar) declares no `orientation` and is vertical, while a horizontal slider
/// (`OptionsSliderTemplate`) must declare `orientation="HORIZONTAL"` (decision 0250). Sliders in the
/// UI are overwhelmingly scrollbars, so VERTICAL is the ctor default.
#[derive(Clone, Debug, PartialEq)]
pub struct SliderState {
    /// `SetMinMaxValues` low bound (XML `minValue`). Unlike StatusBar, a reversed pair is **not**
    /// swapped (the Slider LoadXML stores `min` + `max−min` as a range and does no swap, RF-28); the
    /// value clamp guards a degenerate range instead ([`Self::clamp`]).
    pub min: f32,
    /// `SetMinMaxValues` high bound (XML `maxValue`).
    pub max: f32,
    /// The current value (`SetValue`/XML `defaultValue`), clamped into `[min, max]`.
    pub value: f32,
    /// `SetValueStep` (XML `valueStep`) — the step the arrow keys / step buttons move by. Stored and
    /// returned; `SetValue` does **not** snap to it (the client's SetValue sets the raw value).
    pub step: f32,
    /// `true` = VERTICAL (the ctor default; value maps along the track's height, min at the top),
    /// `false` = HORIZONTAL (`orientation`; shared enum `0x811b00` HORIZONTAL=0/VERTICAL=1).
    pub vertical: bool,
    /// `Enable`/`Disable` (`IsEnabled`). A disabled slider does not respond to thumb drag; the ctor
    /// enables it (the interactive-widget ctors take mouse — Button/EditBox/ScrollFrame do too).
    pub enabled: bool,
    /// The thumb texture region (`SetThumbTexture`/`<ThumbTexture>`), created on first set. A
    /// renderer positions this region's rect at [`Self::fraction`] along the orientation axis.
    pub thumb: Option<RegionHandle>,
}

impl Default for SliderState {
    fn default() -> SliderState {
        SliderState {
            min: 0.0,
            max: 0.0,
            value: 0.0,
            step: 0.0,
            vertical: true,
            enabled: true,
            thumb: None,
        }
    }
}

impl SliderState {
    /// The value fraction `(value − min) / (max − min)`, clamped to `[0, 1]`; a degenerate range
    /// (`max <= min`, an unscrollable slider) is `0.0` — the thumb sits at the track's start.
    pub fn fraction(&self) -> f32 {
        if self.max > self.min {
            ((self.value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Clamp `v` into the live range and store it; returns `Some(new_value)` iff it actually changed
    /// (the caller fires `OnValueChanged` — firing only on a real change is what keeps the reference
    /// scrollbar wiring `OnValueChanged → SetVerticalScroll → scrollbar:SetValue` from recursing
    /// forever). A degenerate range (`max <= min`) pins to `min`.
    pub fn store_value(&mut self, v: f32) -> Option<f32> {
        let clamped = v.clamp(self.min, self.max.max(self.min));
        (clamped != self.value).then(|| {
            self.value = clamped;
            clamped
        })
    }
}
