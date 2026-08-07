//! The Lua scripting host — the engine-free VM that turns the arena/layout/order *model* into a
//! live, addon-facing runtime (decision 0068). No Bevy, no GPU: this module embeds mlua's Lua 5.1
//! (the Classic Era version) and wires it to [`crate::widget`]'s frame arena, [`crate::layout`]'s
//! anchor resolver, and [`crate::order`]'s draw-order traversal, exposing the WoW FrameScript object
//! model and the slice-of stdlib addons expect.
//!
//! ## Ground truth
//!
//! - **RF-0023** (`wow-5875-re/system/ui/ui.md` §"The Lua frame object model"): a frame's Lua value
//!   is a table `T` with `T[0] = lightuserdata(handle)`, one *shared* metatable
//!   `__framescript_meta` whose `__index` dispatches by method name; named frames auto-publish to
//!   `_G` non-overwriting. See [`object`].
//! - **RF-0025** (`ui.md` §"FrameScript handler-firing context"): a handler is fired via `pcall`
//!   with `this`/`event`/`arg1..argN` set as **globals**, saved-then-restored around the call
//!   (nesting-safe). We also pass the *modern* `(self, event, ...)` arguments the Era addons expect
//!   (the transition-era client did both). See [`event`].
//!
//! ## The `LUAI_MAXCSTACK` discipline (decision 0068, probe A)
//!
//! Probe A found the hard constraint that Rust must **never** hold thousands of persistent mlua
//! handles (each owned `Table`/`Function` occupies a slot on mlua's reference thread, capped by the
//! vendored build's `LUAI_MAXCSTACK = 8000`). So this host holds **zero** persistent Lua handles in
//! Rust: every piece of Lua-side state — the two shared metatables, the two method tables, the
//! wrapper cache (`id → wrapper table`), and every frame's script closures (`id → {name → fn}`) —
//! lives in the Lua **registry** under named keys and is fetched transiently per call. The Rust-side
//! [`Model`] is *plain data* (arena, layout inputs, id↔handle maps, region visuals, event
//! registrations, errors) stored in `lua.app_data`, reachable from callbacks via `RefCell`-style
//! dynamic borrows. Every callback takes a *short* borrow and drops it before re-entering Lua.
//!
//! ## Identity: ids, not handle bits
//!
//! RF-0023 stores the `CScriptObject*` in the lightuserdata. Benilla's [`crate::widget::FrameHandle`]
//! is an opaque generational handle with private fields (no bit accessor), so we mint a stable `u32`
//! **id** per handle and store *that* in the lightuserdata (`id as *mut c_void`); [`Model`] owns the
//! `id ↔ handle` bijection. The id doubles as the [`crate::layout::Handle`] used by the layout graph
//! (frames only; the screen root is the reserved id [`SCREEN`]). This is faithful to RF-0023's intent
//! (an opaque identity in a lightuserdata) — only the *encoding* differs.

mod action;
mod aura;
mod backdrop;
mod bank;
mod button;
mod char_stats;
mod clip;
mod container;
mod cooldown;
mod craft;
mod cursor;
mod death;
mod editbox;
pub(crate) use editbox::adopt_text_region;
mod cvars;
mod duel;
mod event;
mod extract;
mod follow;
mod gossip;
mod inspect;
mod item_stats;
mod item_text;
pub mod keybind;
mod layout;
mod loot;
mod loot_roll;
mod macros;
mod mail;
mod merchant;
mod messageframe;
mod minimap;
mod model;
mod net_stats;
mod object;
mod party;
mod pet;
mod pointer;
mod pvp;
mod quest;
mod quest_log;
mod region;
mod scrollframe;
mod session;
mod shapeshift;
mod skills;
mod slider;
mod social;
mod sound;
mod spellbook;
mod statusbar;
mod stdlib;
mod talent;
mod taxi;
mod tick;
mod tooltip;
mod tooltip_item;
mod tooltip_spell;
mod tooltip_unit;
pub use tooltip_unit::TooltipTint;
mod trade;
mod tradeskill;
mod trainer;
mod types;
mod unit;
mod worldmap;

pub use action::{ActionSlot, ActionState};
pub use aura::{AuraState, TrackingState};
pub use backdrop::{pieces, Backdrop, BackdropPiece, Insets};
pub use bank::BankState;
pub use char_stats::{
    weapon_subclass_skill, InvSlotView, InventorySlots, PlayerCombatStats, INVENTORY_SLOT_COUNT,
    SKILL_UNARMED,
};
pub use container::{ContainerMove, ContainerSlot, ContainerState, EnchantView, UiCursorMode};
pub use craft::{CraftReagent, CraftRecipe, CraftState};
pub use cursor::{
    CursorAction, CursorItem, CursorMacro, CursorPayload, CursorPetAction, CursorSpell,
    EnchantConfirm, WorldPick, EQUIPMENT_BAG,
};
pub use death::{DeathAction, DeathUiState};
pub use duel::DuelRequest;
pub use follow::FollowRequest;
pub use gossip::{GossipMenu, GossipOptionView, GossipQuestRow};
pub use inspect::InspectView;
pub use item_stats::{item_usable, ItemSetView, ItemTemplateView, PlayerReqState};
pub use item_text::ItemTextState;
pub use loot::{LootRow, LootState};
pub use loot_roll::{LootRollEntry, LootRollsState};
pub use macros::{MacroState, MacroView, MAX_MACROS, MAX_MACRO_BODY, MAX_MACRO_NAME};
pub use mail::{MailInboxRow, MailSendRequest, MailState};
pub use merchant::{ItemStatsHead, MerchantItem, MerchantState};
pub(crate) use model::Model;
pub use party::{PartyMemberInfo, PartyRequest, PartyState};
pub use pet::{PetActionView, PetStats};
pub use quest::{QuestAction, QuestItemView, QuestPanel, QuestSelect, QuestState};
pub use quest_log::{QuestLogDetail, QuestLogEntryView, QuestLogObjectiveView, QuestLogState};
pub use session::SessionRequest;
pub use shapeshift::ShapeshiftFormView;
pub use skills::{SkillEntry, SkillsState};
pub use social::{FriendInfo, SocialRequest, SocialState, WhoInfo};
pub use sound::SoundRequest;
pub use spellbook::{
    resolve_spell_by_name, PetBookState, SpellBookState, SpellSlotView, SpellTabView,
};
pub use talent::{TalentPrereqView, TalentTabView, TalentUiState, TalentView};
pub use taxi::{TaxiNodeType, TaxiUiNode, TaxiUiState};
pub use tooltip_spell::SpellTooltipView;
pub use trade::{TradeSideState, TradeSlotItem, TradeState, TRADE_SLOTS};
pub use tradeskill::{TradeSkillDifficulty, TradeSkillReagent, TradeSkillRecipe, TradeSkillState};
pub use trainer::{
    TrainerAbilityReq, TrainerGroup, TrainerService, TrainerServiceCategory, TrainerSkillReq,
    TrainerState,
};
pub use types::{
    EditAction, EditBoxTextUi, EditOutcome, EditUnit, ExtractedQuad, FontObject, FontShadow,
    JustifyH, JustifyV, LineMeasureRequest, MeasureRequest, Outline, QuadContent, ScriptValue,
    TexCoords,
};
pub(crate) use types::{MeasuredText, RegionData};
pub use unit::{grey_band, level_reads_unknown, power_token, unit_is_grey, UnitState};
pub use worldmap::{WorldMapContinentView, WorldMapOverlayView, WorldMapState, WorldMapZoneView};

use mlua::{Function, Lua, Table};

use crate::layout::Rect;
use crate::order::ZTarget;
use crate::widget::{FrameHandle, KindState};

// Registry key names — the only place Lua-side roots are kept alive (the MAXCSTACK discipline).
const REG_FRAME_META: &str = "__benilla_frame_meta";
const REG_REGION_META: &str = "__benilla_region_meta";
const REG_FRAME_METHODS: &str = "__benilla_frame_methods";
const REG_REGION_METHODS: &str = "__benilla_region_methods";
const REG_WRAPPERS: &str = "__benilla_wrappers";
const REG_SCRIPTS: &str = "__benilla_scripts";

/// The reserved layout [`crate::layout::Handle`] of the screen root (the client's `CSimpleTop` /
/// `UIParent`), whose rect is the physical screen. Top-level frames whose `SetPoint` omits a
/// `relativeTo` anchor to it. Real frame ids are minted from `1` upward so they never collide.
pub const SCREEN: crate::layout::Handle = 0;

/// The FrameScript handler kinds this host models. The first five are the lifecycle/event set; the
/// six mouse handlers are driven by the hit-testing API in [`pointer`] ([`UiScript::mouse_move`] /
/// [`mouse_button`](UiScript::mouse_button) / [`mouse_wheel`](UiScript::mouse_wheel)) — the app-side
/// event feed (net/window → these calls) is the Bevy side's job (decision 0068). `OnValueChanged`
/// is shared by the StatusBar (RF-28 `+0x32c`) and the Slider (`+0x330`; decision 0250) — one name,
/// each kind dispatching to its own value-changed slot. The eight
/// `On*Pressed`/text/focus slots are the EditBox's specialized scripts (RF-0082 §2): a focused EditBox
/// fires ONLY these, never generic `OnKeyDown`/`OnChar` (its C++ override replaces those slots).
/// `OnVerticalScroll`/`OnScrollRangeChanged` are the ScrollFrame's own slots (decision 0112).
/// `OnDragStart`/`OnDragStop`/`OnReceiveDrag` are the drag trio (decision 0216 §3) — driven by
/// `RegisterForDrag` + the same mouse path as the six mouse handlers above, not a separate one.
const SCRIPT_KINDS: [&str; 29] = [
    "OnLoad",
    "OnEvent",
    "OnUpdate",
    "OnShow",
    "OnHide",
    "OnClick",
    "OnEnter",
    "OnLeave",
    "OnMouseDown",
    "OnMouseUp",
    "OnMouseWheel",
    "OnValueChanged",
    "OnEnterPressed",
    "OnEscapePressed",
    "OnSpacePressed",
    "OnTabPressed",
    "OnTextChanged",
    "OnTextSet",
    "OnEditFocusGained",
    "OnEditFocusLost",
    "OnVerticalScroll",
    "OnScrollRangeChanged",
    "OnDragStart",
    "OnDragStop",
    "OnReceiveDrag",
    // A release over a message-frame hyperlink span (`OnHyperlinkClick(link, text, button)` —
    // the ChatFrameTemplate wires it to SetItemRef; decision 0288 P2).
    "OnHyperlinkClick",
    // The GameTooltip's engine-fired widget scripts (decision 0274; the real template wires all
    // three: money render, money clear, world-hover default placement).
    "OnTooltipAddMoney",
    "OnTooltipCleared",
    "OnTooltipSetDefaultAnchor",
];

// ─────────────────────────────────────────────────────────────────────────────────────────────
// UiScript — the public host
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The Lua scripting host. Owns the mlua VM; the frame arena, layout store, and event registry live
/// inside it (in `lua.app_data`, per the MAXCSTACK discipline) and are driven through this API.
///
/// Construction sandboxes the VM (removes `io`/`os`/`package`/`require`/`dofile`/`loadfile`/`debug`,
/// text-only chunk loading — see [`stdlib`]), installs the WoW stdlib layer (the global aliases, the
/// positional `format`, the `strsplit` family, `getglobal`/`setglobal`, `wipe`, …), and installs the
/// FrameScript object model (`CreateFrame` + the widget/region method surface, the shared metatables).
///
/// The host surface is split, `impl UiScript` blocks beside their concern (the `layout.rs` pattern):
/// the render-list builder in [`extract`], the per-frame runtime loop in [`tick`], the EditBox text
/// seam in [`editbox::seam`], the layout resolve in [`layout`], and the pointer/hit-test in
/// [`pointer`]. What stays here is construction and the small host-facing state pushes/queries.
pub struct UiScript {
    lua: Lua,
    /// The FrameXML **template registry**, persisted across [`crate::loader::load`] calls — the
    /// client's template table is global (`0x6ee500`, rf24), so a file may `inherits=` a template
    /// an *earlier file* registered (the real MerchantFrame.xml inherits
    /// CharacterFrameTemplates.xml's tab template). Register-before-use in load order, exactly
    /// the client's rule; a per-document registry silently dropped every cross-file inherit.
    pub(crate) framexml_templates:
        std::cell::RefCell<std::collections::HashMap<String, crate::framexml::Element>>,
    /// The FrameXML **font-element registry** (a separate namespace — a font inherits a font,
    /// never a frame template), persisted for the same cross-file reason.
    pub(crate) framexml_fonts:
        std::cell::RefCell<std::collections::HashMap<String, crate::framexml::Element>>,
}

impl UiScript {
    /// Build a fully sandboxed, stdlib- and object-model-equipped host.
    pub fn new() -> mlua::Result<UiScript> {
        let lua = Lua::new();
        lua.set_app_data(Model::new());

        stdlib::sandbox(&lua)?;
        stdlib::install(&lua)?;
        object::install(&lua)?;
        unit::install(&lua)?;
        party::install(&lua)?;
        social::install(&lua)?;
        duel::install(&lua)?;
        follow::install(&lua)?;
        session::install(&lua)?;
        pvp::install(&lua)?;
        death::install(&lua)?;
        aura::install(&lua)?;
        cvars::install(&lua)?;
        keybind::install(&lua)?;
        sound::install(&lua)?;
        pointer::install(&lua)?;
        action::install(&lua)?;
        container::install(&lua)?;
        cursor::install(&lua)?;
        spellbook::install(&lua)?;
        macros::install(&lua)?;
        talent::install(&lua)?;
        shapeshift::install(&lua)?;
        pet::install(&lua)?;
        gossip::install(&lua)?;
        merchant::install(&lua)?;
        bank::install(&lua)?;
        item_text::install(&lua)?;
        mail::install(&lua)?;
        trainer::install(&lua)?;
        taxi::install(&lua)?;
        trade::install(&lua)?;
        inspect::install(&lua)?;
        tradeskill::install(&lua)?;
        craft::install(&lua)?;
        skills::install(&lua)?;
        item_stats::install(&lua)?;
        char_stats::install(&lua)?;
        loot::install(&lua)?;
        loot_roll::install(&lua)?;
        quest::install(&lua)?;
        quest_log::install(&lua)?;
        messageframe::install(&lua)?;
        scrollframe::install(&lua)?;
        slider::install(&lua)?;
        minimap::install(&lua)?;
        cooldown::install(&lua)?;
        tooltip::install(&lua)?;
        worldmap::install(&lua)?;
        net_stats::install(&lua)?;

        Ok(UiScript {
            lua,
            framexml_templates: Default::default(),
            framexml_fonts: Default::default(),
        })
    }

    /// The embedded VM — for the Bevy plugin / TOC-XML loader to add the game-state API bindings
    /// (decision 0068 §1: "the Bevy side owns … the API bindings that touch ECS/net") on top of the
    /// object model this crate installs.
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Set the screen-root rect from a pixel size (`[0,0]` origin, y-up). Top-level frames anchor to
    /// it; changing it invalidates the next `resolve`. The app calls this every frame with an
    /// almost-always-identical size — compared before writing so the per-frame idiom doesn't
    /// dirty the layout gate's tier 1 (`Model::touch_layout`).
    pub fn set_screen_size(&mut self, width: f32, height: f32) {
        let new = Rect::new(0.0, 0.0, height, width);
        let mut model = self.model_mut();
        if model.screen != new {
            model.screen = new;
            model.touch_layout();
        }
    }

    /// Replace the Era atlas table (decision 0950) — pushed once at boot, before the XML loads.
    /// Push the modifier-key state (shift, ctrl, alt) behind `IsShiftKeyDown`/`IsControlKeyDown`/
    /// `IsAltKeyDown`. The app's input pass calls this BEFORE feeding the frame's mouse events, so
    /// a click handler's modifier fork (the reference's shift-split / ctrl-dressup /
    /// shift-pickup) reads the state as of the click.
    pub fn set_modifiers(&mut self, shift: bool, ctrl: bool, alt: bool) {
        let shift_was = {
            let mut model = self.model_mut();
            let was = model.modifiers.0;
            model.modifiers = (shift, ctrl, alt);
            was
        };
        // The shift EDGE drives the shopping-compare tooltips (0274 P4): press over a live
        // equippable item hover fires SHOW_COMPARE_TOOLTIP, release hides the pair.
        if shift_was != shift {
            tooltip_item::on_shift_edge(&self.lua, shift);
        }
    }

    /// Push the player's WMO-containment state onto every Minimap widget (the client's `0xceaa60`).
    /// It selects which of the two persisted zoom indices `GetZoom`/`SetZoom` act on, so the zoom
    /// buttons drive the indoor level while indoors and the outdoor level while outside. The app
    /// owns the containment test, so it owns this push — call it before the script tick.
    pub fn set_minimap_inside(&mut self, inside: bool) {
        for (_, frame) in self.model_mut().arena.iter_frames_mut() {
            if let KindState::Minimap(m) = &mut frame.kind_state {
                m.inside = inside;
            }
        }
    }

    /// Load and run a Lua chunk (text-only; the sandbox rejects bytecode). Errors propagate to the
    /// caller (a *load-time* error is the caller's to see; *handler* errors during events go to
    /// [`UiScript::errors`]).
    pub fn run(&self, chunk: &str) -> mlua::Result<()> {
        self.lua.load(chunk).set_mode(mlua::ChunkMode::Text).exec()
    }

    /// Load and evaluate a Lua chunk, returning its result. Primarily for tests / one-shot queries.
    pub fn eval<T: mlua::FromLuaMulti>(&self, chunk: &str) -> mlua::Result<T> {
        self.lua.load(chunk).set_mode(mlua::ChunkMode::Text).eval()
    }

    /// The owning frame's name for an [`ExtractedQuad`] target — a debugging affordance for
    /// capture/probe tooling, which sees quads but not widgets ("whose quad is this?").
    pub fn quad_owner_name(&self, target: ZTarget) -> Option<String> {
        let model = self.model_ref();
        let fh = match target {
            ZTarget::Frame(fh) => fh,
            ZTarget::Region(rh) => model.arena.region(rh)?.owner,
        };
        model.arena.frame(fh)?.name.clone()
    }

    /// Append a line to a named [`FrameKind::ScrollingMessageFrame`] (the app's chat feed → the seam,
    /// the analogue of the loot/merchant snapshot pushes). `r`/`g`/`b` are `0..1`; the engine
    /// byte-quantizes them round-half-up and drives the line's alpha via the fade. A missing frame,
    /// or one of another kind, is a no-op (returns `false`) — the app decides whether that's a bug.
    pub fn add_chat_message(&mut self, frame: &str, text: &str, r: f32, g: f32, b: f32) -> bool {
        let mut model = self.model_mut();
        let Some(h) = model.arena.lookup(frame) else {
            return false;
        };
        match model.arena.frame_mut(h).map(|f| &mut f.kind_state) {
            Some(crate::widget::KindState::ScrollingMessage(smf)) => {
                smf.add(text.to_string(), r, g, b);
                true
            }
            _ => false,
        }
    }

    /// Open the named EditBox for typing: show it and grab keyboard focus (so [`has_keyboard_focus`]
    /// gates the world's keys). The app's chat-open key (ENTER) drives this; the box's own
    /// `OnEnterPressed`/`OnEscapePressed` handlers hide + `ClearFocus` it on submit/cancel. A missing
    /// frame, or one that isn't an EditBox, is a no-op (`false`).
    pub fn focus_editbox(&mut self, name: &str) -> bool {
        let mut model = self.model_mut();
        let Some(h) = model.arena.lookup(name) else {
            return false;
        };
        if !matches!(
            model.arena.frame(h).map(|f| &f.kind_state),
            Some(crate::widget::KindState::EditBox(_))
        ) {
            return false;
        }
        model.arena.set_shown(h, true);
        model.focused_editbox = Some(h);
        true
    }

    // The EditBox text-UI seam (advance table, caret/selection geometry, clipboard) lives in
    // `editbox/seam.rs` — an `impl UiScript` block beside its concern, the layout.rs pattern.

    /// Drain the chat lines submitted through the input EditBox since the last call (the
    /// `SubmitChatInput` Lua binding queued them). The app parses each into an outbound chat command.
    pub fn take_chat_input(&mut self) -> Vec<String> {
        std::mem::take(&mut self.model_mut().chat_input)
    }

    /// Queue a line as if it had been typed into the chat EditBox and submitted — the headless
    /// twin of `SubmitChatInput`, for scripted probes (`WOW_PROBE_CHAT`). Going through this seam
    /// rather than straight to the wire is what lets a probe drive **client-side** slash commands
    /// (`/duel`, `/reaction`); a plain line or a `.gm`-style server command still reaches the wire
    /// exactly as before, because that is what the chat drain does with anything it doesn't own.
    pub fn push_chat_input(&mut self, line: String) {
        self.model_mut().chat_input.push(line);
    }

    /// Whether Tab was pressed in the chat edit box since the last call (`BenillaChatTabPressed`)
    /// — the whisper-target cycle's trigger (decision 0288 P5).
    pub fn take_chat_tab(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().chat_tab)
    }

    /// Replace the frame-keyed hyperlink spans (`(frame, rect, link, markup)`, rects in the
    /// engine's y-up screen space) — the app feeds these each frame after rasterizing message
    /// lines (it alone knows where the glyphs actually landed). A release inside a span fires
    /// `OnHyperlinkClick(link, markup, button)` on the owning frame ([`pointer`]).
    pub fn set_link_spans(&mut self, spans: Vec<(FrameHandle, Rect, String, String)>) {
        let mut model = self.model_mut();
        model.link_spans.clear();
        for (fh, rect, link, markup) in spans {
            model
                .link_spans
                .entry(fh)
                .or_default()
                .push((rect, link, markup));
        }
    }

    /// Invoke a compiled handler `func` under the RF-0025 frame-globals convention (`this`/`self` =
    /// `wrapper`), the same set/restore path registry handlers use. For the [`crate::loader`], which
    /// holds the `OnLoad` `Function` directly (to fire it bottom-up) rather than through the registry:
    /// this keeps the convention in one home instead of duplicating it. Errors are returned so the
    /// caller routes them (the loader records them in its own report).
    pub(crate) fn invoke_handler(&self, wrapper: &Table, func: &Function) -> mlua::Result<()> {
        event::invoke_with_globals(&self.lua, wrapper.clone(), func, None, Vec::new())
    }

    /// Resolve every frame's rect: sync each frame's effective scale from the arena into its layout
    /// input, run the [`crate::layout`] graph (screen root as the external base), and cache the
    /// resolved rects. `GetWidth`/`GetHeight`/`extract` read this cache.
    pub fn resolve(&mut self) {
        let mut model = self.model_mut();
        Self::resolve_layout(&mut model);
    }

    /// Store host measurements for [`MeasureRequest`]s (`(id, w, h, natural_w, key)` — id/key
    /// verbatim from the request). The next [`UiScript::resolve`] uses `w`/`h` as the FontStrings'
    /// implicit size.
    ///
    /// `w`/`h` are the text **as laid out** (wrapped inside a declared width); `natural_w` is what
    /// it would take **unwrapped**, and is what `GetStringWidth` reports — see [`MeasuredText`] for
    /// why the two must not be conflated. For a region with no declared width they are the same
    /// number, which is why [`Self::set_measured_text_unwrapped`] exists for tests.
    pub fn set_measured_text(&mut self, measures: &[(u32, f32, f32, f32, u64)]) {
        let mut model = self.model_mut();
        let mut changed = false;
        for &(id, w, h, natural_w, key) in measures {
            let Some(&rh) = model.id_to_region.get(&id) else {
                continue;
            };
            if let Some(d) = model.region_data.get_mut(&rh) {
                let new = MeasuredText {
                    w,
                    h,
                    natural_w,
                    key,
                };
                changed |= d.measured != Some(new);
                d.measured = Some(new);
            }
        }
        if changed {
            // Measured extents are the auto-size axes' inputs — the layout gate's read set.
            model.touch_layout();
        }
    }

    /// [`Self::set_measured_text`] for text with **no wrap constraint**, where the laid-out and
    /// natural widths are by definition the same number. Test-facing: a hand-fed measure for an
    /// unconstrained string should not have to say `w` twice, and a test that DOES care about the
    /// distinction (a declared-width region) should be forced to spell it out.
    pub fn set_measured_text_unwrapped(&mut self, measures: &[(u32, f32, f32, u64)]) {
        let widened: Vec<_> = measures
            .iter()
            .map(|&(id, w, h, key)| (id, w, h, w, key))
            .collect();
        self.set_measured_text(&widened);
    }

    /// Drop every cached host text metric — FontString measures, message-line row counts, editbox
    /// advance tables. The host calls this when its **raster environment** changes (a window
    /// resize / fullscreen toggle / uiScale move — anything that shifts the px-per-unit seam its
    /// answers were taken under): glyph advances step to whole pixels at the drawn raster size,
    /// so a metric measured under one environment does not rescale to another — the font-size
    /// snap alone moves a string's unit width by several percent, which is exactly enough for a
    /// boot-size measure to fail the post-resize ellipsis fit test and truncate text that fits
    /// (the director's fullscreen "Contr..." rows). The environment is the host's to watch — the
    /// engine deliberately never sees the seam scale — so staleness is the host's to declare; the
    /// round-trips re-answer on the frames that follow (the same one-frame convergence every
    /// measure already has).
    pub fn invalidate_text_measures(&mut self) {
        let mut model = self.model_mut();
        for d in model.region_data.values_mut() {
            d.measured = None;
        }
        // The frame-side caches key on content hashes; a bumped stored key can never equal the
        // recomputed one, so the next needing-measure sweep re-requests without this method
        // having to know what the keys hash.
        for (_, frame) in model.arena.iter_frames_mut() {
            match &mut frame.kind_state {
                KindState::ScrollingMessage(smf) => {
                    for line in &mut smf.lines {
                        line.rows_key = line.rows_key.wrapping_add(1);
                    }
                }
                KindState::EditBox(eb) => {
                    eb.advances_key = eb.advances_key.wrapping_add(1);
                }
                _ => {}
            }
        }
        // Measured extents are auto-size inputs — the layout gate's read set, same as
        // [`Self::set_measured_text`]'s.
        model.touch_layout();
    }

    /// Feed the number font's per-digit advance widths (logical px, `'0'..='9'` in order) as the
    /// Lua global table `BENILLA_DIGIT_W` (keyed by digit *string*, `["0"].."["9"]`). The
    /// synchronous half of the money layout's metrics (the real `SmallMoneyFrame` calls
    /// `GetTextWidth` mid-update — MoneyFrame.lua l.202 — while our `GetStringWidth` measure
    /// round-trip is a frame late): the app measures NumberFontNormal's digits once per atlas
    /// scale and `BenillaNumberWidth` (MerchantFrame.xml) sums them per price.
    pub fn set_digit_advances(&mut self, advances: &[f32; 10]) {
        let set = || -> mlua::Result<()> {
            let t = self.lua.create_table()?;
            for (i, w) in advances.iter().enumerate() {
                t.set(i.to_string(), *w)?;
            }
            self.lua.globals().set("BENILLA_DIGIT_W", t)
        };
        if let Err(e) = set() {
            self.model_mut()
                .errors
                .push(format!("set_digit_advances: {e}"));
        }
    }

    // ── Input: pointer-leaves-window cleanup (decision 0216 §3; the hit-test/mouse dispatch that
    // used to sit here now lives in [`pointer`]) ─────────────────────────────────────────────────

    /// The OS pointer left the window (decision 0216 §3's drag-gesture leak): clears
    /// [`Model::drag`] AND [`Model::mouse_down_on`] — the two press-tracked states nothing else
    /// clears when the pointer leaves the window mid-press, since the matching release is never
    /// fed. Left uncleared, a stale armed [`Model::drag`] would fire a spurious `OnDragStart` the
    /// instant the pointer re-enters and crosses the threshold against a press point that no
    /// longer means anything; a stale [`Model::mouse_down_on`] entry would fire a same-frame
    /// `OnClick` if the pointer re-enters and releases over the very frame it left from. The app
    /// calls this from the same branch that fires the synthetic `OnLeave` (`ui_script/input.rs`).
    pub fn pointer_left_window(&mut self) {
        let mut model = self.model_mut();
        model.drag = None;
        model.mouse_down_on.clear();
        // A thumb drag in progress when the pointer leaves is abandoned too (decision 0250 §5) —
        // the release that would end it is never fed, same leak as the drag gesture above.
        model.slider_drag = None;
    }

    // ── Keyboard entry (RF-0082 §1/§2: the EditBox focus + key/char routing) ─────────────────────
    //
    // benilla speaks *key names*, not scancodes — the host maps its window keycodes to these. The
    // routing is the client's exactly: if a box is focused it processes and CONSUMES every event; if
    // none is focused, the topmost effectively-visible `autoFocus` box self-acquires focus and
    // processes this same event; otherwise nothing is consumed. `autoFocus` never focuses on show —
    // only this self-acquire path, a click, or Lua `SetFocus` focuses a box.

    /// A typed character (may be multi-byte UTF-8) arriving from the host. Routes per §1/§2 and, on a
    /// focused box, inserts it (numeric/cap/password rules apply) or — for the Ctrl+A control code —
    /// selects all. Returns `true` if consumed (a focused box consumes every char).
    pub fn char_input(&mut self, text: &str) -> bool {
        editbox::char_input(&self.lua, text)
    }

    /// Paste text from the host OS clipboard into the focused EditBox. The engine-free runtime can't
    /// reach the clipboard itself, so the app reads it (Cmd+V on macOS / Ctrl+V elsewhere) and hands
    /// the string here; newlines survive only in a `multiLine` box, other control chars are dropped,
    /// and the remainder inserts as one edit. Returns `true` if a box consumed it.
    pub fn paste(&mut self, text: &str) -> bool {
        editbox::paste(&self.lua, text)
    }

    /// A non-character key press arriving from the host, by name — the three *box-event* keys
    /// (`"ENTER"`, `"ESCAPE"`, `"TAB"`) fire their FrameXML scripts; editing keys arrive as
    /// semantic [`EditAction`]s via [`Self::editbox_action`] instead (the host's per-OS keymap
    /// owns which chord means what). Routes per §1/§2; a focused box consumes the key even when
    /// it does nothing with it. Returns `true` if consumed.
    pub fn key_input(&mut self, key: &str) -> bool {
        editbox::key_input(&self.lua, key)
    }

    /// One semantic text-editing operation on the focused EditBox — the output of the host's
    /// per-OS keymap (decision 0301). Same routing/consumption law as [`Self::key_input`].
    /// Returns `true` if consumed.
    pub fn editbox_action(&mut self, action: EditAction) -> bool {
        editbox::action(&self.lua, action)
    }

    /// Whether an EditBox currently holds keyboard focus (and is effectively visible) — the app gates
    /// world/player key input on this, matching the client's `DAT_00cf4dc8 != 0` test (RF-0082 §1).
    pub fn has_keyboard_focus(&self) -> bool {
        let model = self.model_ref();
        model
            .focused_editbox
            .is_some_and(|h| model.arena.frame(h).is_some_and(|f| f.effective_visible))
    }

    /// How many resolves the layout change gate has let through (`Model::layout_solves`) — the
    /// gate's effectiveness, readable by tests and the app's cost meters (a per-frame delta of 0
    /// means the fingerprint judged the frame quiet).
    pub fn layout_solves(&self) -> u64 {
        self.model_ref().layout_solves
    }

    /// Total fixpoint ROUNDS across every solve ([`Model::layout_rounds`]) — a solve costs
    /// rounds × the whole graph, so the ratio against [`Self::layout_solves`] is the per-pass
    /// depth.
    pub fn layout_rounds(&self) -> u64 {
        self.model_ref().layout_rounds
    }

    /// A snapshot of the script errors collected so far (from `pcall`'d handlers).
    pub fn errors(&self) -> Vec<String> {
        self.model_ref().errors.clone()
    }

    /// Drain the collected script errors.
    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.model_mut().errors)
    }

    /// A snapshot of non-fatal host warnings (e.g. ignored `CreateFrame` templates).
    pub fn warnings(&self) -> Vec<String> {
        self.model_ref().warnings.clone()
    }

    /// Drain the accumulated non-fatal host warnings (an ignored `CreateFrame` template, a layout
    /// fixpoint that hit its round cap) — the host logs them; un-drained they pile up unseen.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.model_mut().warnings)
    }

    /// Register a named virtual [`FontObject`] (a resolved `<Font>`), overwriting any prior one of
    /// the same name — the loader's join for top-level `<Font>` nodes. FontString `inherits=` and Lua
    /// `SetFontObject` resolve against this registry.
    pub fn register_font_object(&self, name: &str, font: FontObject) {
        self.model_mut().font_objects.insert(name.to_string(), font);
    }

    /// Look up a registered [`FontObject`] by name (the resolved paint), if any. Used by tests and by
    /// the `SetFontObject` binding.
    pub fn font_object(&self, name: &str) -> Option<FontObject> {
        self.model_ref().font_objects.get(name).cloned()
    }

    /// Every registered [`FontObject`] (the resolved paints of all loaded `<Font>` nodes) — the
    /// host's bake census: the glyph atlas reads the distinct `(font, height, outline)` triples
    /// off this to know which outlined cell variants the shipped UI can actually request
    /// (the outlined-glyph bake, the fade-composite fold-back record).
    pub fn font_objects(&self) -> Vec<FontObject> {
        self.model_ref().font_objects.values().cloned().collect()
    }

    // ── internals ────────────────────────────────────────────────────────────────────────────

    fn model_ref(&self) -> mlua::AppDataRef<'_, Model> {
        self.lua
            .app_data_ref::<Model>()
            .expect("model app_data set")
    }

    fn model_mut(&self) -> mlua::AppDataRefMut<'_, Model> {
        self.lua
            .app_data_mut::<Model>()
            .expect("model app_data set")
    }

    fn push_error(&self, e: mlua::Error) {
        self.model_mut().errors.push(e.to_string());
    }
}

#[cfg(test)]
mod tests;
