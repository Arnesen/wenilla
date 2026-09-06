//! The widget-kind vocabulary + per-kind state (the "later layer" over the frame arena): the
//! 13 client widget classes ([`FrameKind`]), the two region leaves ([`RegionKind`]), and the
//! modeled per-kind behavior ([`KindState`]) that a `CSimple*` subtype adds over `CSimpleFrame`
//! (RF-28 tables; decision 0068). Split from the arena so each grows independently.

use std::collections::{HashSet, VecDeque};

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
    /// The reference's `CGWorldFrame` (decisions 1983/1984; wow-re `worldframe-widget.md`): the
    /// singleton the 3D world renders behind. Registered as its own frame type (`"WorldFrame"`
    /// @`0x843450`, factory `0x4959d0`) whose registry record is **destroyed on the first
    /// instantiation** — a second `<WorldFrame>` or `CreateFrame("WorldFrame")` is an unknown type;
    /// **a `Frame` to Lua** (`GetObjectType()` answers `"Frame"`, `IsObjectType("WorldFrame")` is
    /// nil — its vtable inherits the base's type slots, the TaxiRouteFrame precedent); born with
    /// key, mouse and wheel enabled (`0xE`) in **stratum 0, `WORLD`, below `BACKGROUND`**, which
    /// no XML or Lua can name. benilla draws the world through Bevy, so the kind is a full-screen
    /// frame whose hits are the WORLD's: the app's pointer arbiter treats a hovered WorldFrame as
    /// not-over-UI while its own scripts still fire, as the reference's do before the click's
    /// binding runs.
    WorldFrame,
    Button,
    /// `CLootButton` (`Ui\\LootFrame.h:21`, factory `0x495a30`, size `0x4e0`) — a
    /// **registered `CreateFrame` type**, one of the eight the client registers through
    /// `0x495940` (pair `("LootButton" @0x843414, 0x495a30)` at site `0x4959a6`), and the reason
    /// the stock `LootFrame.xml` cannot load against a plain `<Button>`.
    ///
    /// It is a `CSimpleButton` **plus one dword and one overridden virtual**, and that is the
    /// whole class. The dword at `+0x4dc` is the 0-based loot slot; the virtual is primary slot 37
    /// `OnClick 0x4c1820`. Everything else it overrides (slots 0, 2, 4, 6, 7 and the secondary
    /// adjustor thunk) is compiler-mandated identity and lifetime plumbing. A binary-wide census
    /// of `+0x4dc` finds exactly three sites in its code: the ctor's zero, `SetSlot`'s write, and
    /// `OnClick`'s read.
    ///
    /// **What it does NOT have**, each proven by the vtable dwords being *identical* to
    /// `CSimpleButton`'s: no C-side `OnEnter`/`OnLeave` (slots 19/20), so the hover tooltip is
    /// purely the FrameXML `<OnEnter>`; no drag (29/30/31); no `OnLoad`/`OnShow`/`OnHide`/
    /// `OnUpdate` (9/12/13/14). And no button-code test anywhere in its 79-byte click body — it
    /// reads `[ebp+8]` once and immediately forwards it — so **right-click loots exactly like
    /// left-click**, and with no `<OnDoubleClick>` a fast double-click takes two slots.
    ///
    /// The click, in order (`0x4c1820`): a scripted click returns immediately and does **nothing,
    /// not even run the Lua `<OnClick>`** (`0x4c182b`); otherwise the base fires the Lua handler
    /// unconditionally, and then — only with **no shift, ctrl or alt** held — the take runs. That
    /// gate is what keeps the C take and `LootFrameItem_OnClick`'s ctrl/shift arms from firing
    /// together, and it is why the shipped Lua handler deliberately never calls a take itself.
    ///
    /// Its whole Lua surface of its own is **one method, `SetSlot(index)`** (table `0x847ce4`,
    /// count 1 read off the registrar's own `mov edx,1`) — 1-based in, 0-based stored, no returns.
    /// `LootFrame.lua:94` is its only caller. Decision 1799; wow-re `e5338caf`.
    LootButton,
    CheckButton,
    EditBox,
    StatusBar,
    Slider,
    ScrollFrame,
    Model,
    /// `CGCharacterModelBase` (`0x505680`, size `0x3f8`) — the **unit-showing** model pane, and a
    /// registered `CreateFrame` type in its own right: pair `("PlayerModel" @0x84343c, factory
    /// 0x495bd0)` at registration site `0x49597f`, one of the eight the game client registers
    /// through `0x495940` (wow-re `ui/scratch/model-pane-method-tables.md` §1).
    ///
    /// **It extends [`FrameKind::Model`] and adds exactly three verbs** — `SetUnit`, `RefreshUnit`,
    /// `SetRotation` (table `0x84f1fc`, 3 entries). Its Lua surface is those three *plus* all 23 of
    /// `Model`'s, and it gets them by **chaining, not repetition**: `CGCharacterModelBase`'s method
    /// lookup `0x506260` probes its own 3-entry map and, on a miss, tail-calls `0x506290 call
    /// 0x76f870` — `CSimpleModel`'s. Our `__index` already walks a *slice* of registry keys, so the
    /// chain is `&[PLAYERMODEL, MODEL]` and nothing is duplicated.
    ///
    /// **The direction is derived → base only.** A plain `<Model>` does *not* acquire `SetUnit`/
    /// `RefreshUnit`/`SetRotation`: nothing chains from `0x76f870` down into `0x506260`. benilla
    /// published all three on `Model` until 2026-08-30 — a `strings WoW.exe` hit read as ownership,
    /// which it cannot be (`SetUnit`'s pooled string `0x84f22c` is referenced by **two** table
    /// entries, `PlayerModel 0x84f1fc` and `GameTooltip 0x854290`, and by no `Model` entry).
    ///
    /// Same posture as its base: the engine core holds the scene the Lua API reads and writes
    /// ([`KindState::Model`], shared with [`FrameKind::Model`]); the pixels are the app renderer's.
    /// `CGCharacterModelBase` adds `0x1c` bytes of members over `CSimpleModel` — the turn-animation
    /// flag/expiry at `+0x3e8`/`+0x3ec` that `SetRotation` arms — and those are **not modeled**:
    /// no getter reads them, and the app's `<Model>` renderer draws no shuffle animation.
    /// `script::modelframe`'s `SetRotation` carries the addresses.
    PlayerModel,
    /// `DressUpModel` — `CGDressUpModelFrame` (`Ui\DressUpModelFrame.cpp`, factory `0x495c00`,
    /// ctor `0x5041d0` chaining `CGCharacterModelBase`'s `0x505680`): the dressing room's pane.
    /// It EXTENDS [`FrameKind::PlayerModel`] — the same members, two behavioural vtable overrides
    /// (idx36 `0x504350`: clone the unit's CharacterComponents and seed the two hand lanes; idx38
    /// `0x504470`) — and adds exactly three verbs of its own, table `0x84f190`: `Undress 0x504c00`,
    /// `Dress 0x504cd0`, `TryOn 0x504d90`. Everything else it answers is `PlayerModel`'s and then
    /// `Model`'s, by the same chaining. Its state is [`KindState::Model`] like both of them; what a
    /// try-on DOES lives app-side as an ordered intent queue (`script::dressup`), because the
    /// substitution set is a look composed against the player's live equipment, which the VM
    /// never holds (decisions 1060, 1969; wow-re `ui/scratch/dressup-model-equipment.md` §0).
    DressUpModel,
    /// `TabardModel` (`0x503bd0`) — the guild tabard designer's pane: `CGCharacterModelBase`'s
    /// other subclass (the dress-up model's sibling), with ten verbs of its own in table
    /// `0x84ee40` — the emblem/colour cycling, the two emblem-texture setters, the save gate and
    /// the save. Its state is [`KindState::Model`] like its siblings; the design it carries lives
    /// app-side (`script::tabard`, decision 1977).
    TabardModel,
    /// `CSimpleMessageFrame` — the non-scrolling message frame (`UIErrorsFrame`'s class, and the
    /// one `CreateFrame("MessageFrame")` makes). Its behaviour (the display lines, the per-line
    /// fade, `insertMode`) is modeled in [`KindState::Message`]. Sibling of
    /// [`FrameKind::ScrollingMessageFrame`], not a base or a subset of it.
    MessageFrame,
    /// `CSimpleMessageScrollFrame` — the scrolling, ring-buffered message frame (the chat window's
    /// class). Its behavior (the line ring, per-line fade, wheel scrollback) is modeled in
    /// [`KindState::ScrollingMessage`]. Sibling of [`FrameKind::MessageFrame`] (different C++ ctor),
    /// not a subclass — offsets/semantics do not transfer (msgframe-runtime.md).
    ScrollingMessageFrame,
    ColorSelect,
    SimpleHtml,
    MovieFrame,
    /// The `GameTooltip` widget family (decision 0274). Like [`FrameKind::Minimap`], a
    /// *game-layer* factory over `CSimpleFrame`; its modeled behavior —
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
}

/// Whether a [`Region`] leaf is a texture or a text string. These are the client's two non-frame
/// region types (`CScriptRegion`-derived leaves, `frame-model.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegionKind {
    /// A `Texture` (BLP quad).
    Texture,
    /// A `FontString` (text run).
    FontString,
    /// A frame's **title region** — the drag handle `Frame:CreateTitleRegion()` makes.
    ///
    /// A third leaf rather than a texture that happens to draw nothing, because the difference is
    /// OBSERVABLE: wow-re carves the object as a plain Region (`widget-api-batch-benilla.md` Q6,
    /// `0x773910`) whose `GetObjectType()` answers `"Region"` and which exposes exactly the 19
    /// Region methods — **no Show/Hide, no scripts, no textures**. A `Texture` in disguise would
    /// answer `"Texture"` to any addon that asked, and would emit a quad.
    Title,
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
    /// `CSimpleMessageFrame` (ctor `0x785640`; same runtime model) — the `UIErrorsFrame` class:
    /// display lines with no ring and no scrollback, plus `insertMode`. A **sibling** of
    /// [`KindState::ScrollingMessage`], not a subset of it ([`MessageFrameState`]'s doc has the
    /// contract table).
    Message(MessageFrameState),
    /// `CSimpleScrollFrame` (decision 0112 — the ScrollFrame mechanism, the engine's last structural
    /// gap: the quest log's detail pane, chat history, and every long-content window need it): the
    /// scroll child + the vertical scroll offset. The offset setter and the range are byte-pinned
    /// (2017: `0x786db0` stores the offset as given, no clamp; 1338: `0x786e30` measures the
    /// child's subtree); the rest is spec-faithful to the documented contract, same posture as
    /// StatusBar's fill. Horizontal scroll is out of scope (no 1.12 template drives it).
    Scroll(ScrollFrameState),
    /// `CSimpleSlider` (factory `0x6eee40`; LoadXML table `0x789580`, RF-28): a value in `[min, max]`
    /// with a step and orientation, positioning a thumb texture along the track. The mechanism is
    /// spec-faithful to the documented Slider widget contract (same posture as StatusBar's fill /
    /// ScrollFrame's scroll), not byte-pinned. Every scrollbar is one (decision 0250).
    Slider(SliderState),
    /// `CSimpleColorSelect` (ctor `0x78b220`, factory `0x6eef90`; LoadXML `0x78b3f0`, script-map
    /// `0x78b4f0`, RF-28): the colour the picker window holds, as the client holds it — **HSV
    /// floats**, not RGB ([`ColorSelectState`], whose docs carry the byte-verified law). The colour
    /// *wheel* and *value strip* the real widget draws are engine art with no BLP behind them (their
    /// `<ColorWheelTexture>`/`<ColorValueTexture>` elements carry no `file=`), so this state is the
    /// widget's whole modeled behavior here; see [`crate::script::colorselect`].
    ColorSelect(ColorSelectState),
    /// The `Model` widget's scene state — the 3D pane an addon or a FrameXML frame parks a model
    /// in. **The same split as [`KindState::Minimap`]**: the engine core carries exactly what
    /// the Lua API reads and writes, and the pixels are the app renderer's job. See
    /// [`ModelState`].
    Model(ModelState),
    /// The `<Minimap>` widget's zoom state (decision 0203). The engine core carries only what the
    /// Lua API reads/writes (`GetZoom`/`SetZoom`/`GetZoomLevels`); the tile/blip render is app-side.
    Minimap(MinimapState),
    /// The GameTooltip widget's line stack + owner/fade state ([`TooltipState`], decision 0274).
    Tooltip(TooltipState),
}

impl KindState {
    /// The display lines of **either** message-frame class, or `None` for every other kind.
    ///
    /// The two classes are siblings with different stores, different `AddMessage` tails and
    /// different scrollback (see [`MessageFrameState`]), but the *line record* they display is the
    /// same [`MessageLine`] — text, quantized colour, fade phases, wrapped row count. This pair of
    /// accessors is the only place that likeness is spent: the wrapped-row measure round-trip and
    /// the band emit are one code path for both, while every behaviour that actually differs stays
    /// on its own state. Matching on the two variants at those sites instead would have been two
    /// near-copies of the round-trip, which is how the second one silently rots.
    pub fn message_lines(&self) -> Option<&VecDeque<MessageLine>> {
        match self {
            KindState::ScrollingMessage(smf) => Some(&smf.lines),
            KindState::Message(mf) => Some(&mf.lines),
            _ => None,
        }
    }

    /// The message kinds' sweep-skip generation ([`ScrollingMessageState::lines_gen`]);
    /// `None` for every other kind.
    pub fn lines_gen(&self) -> Option<u64> {
        match self {
            KindState::ScrollingMessage(smf) => Some(smf.lines_gen),
            KindState::Message(mf) => Some(mf.lines_gen),
            _ => None,
        }
    }

    /// [`Self::message_lines`], mutably — the measure round-trip's write-back half.
    pub fn message_lines_mut(&mut self) -> Option<&mut VecDeque<MessageLine>> {
        match self {
            // Any mut borrow through this door counts as a text change for the measure sweep's
            // skip token — conservative, and the door every write-back path uses.
            KindState::ScrollingMessage(smf) => {
                smf.lines_gen = smf.lines_gen.wrapping_add(1);
                Some(&mut smf.lines)
            }
            KindState::Message(mf) => {
                mf.lines_gen = mf.lines_gen.wrapping_add(1);
                Some(&mut mf.lines)
            }
            _ => None,
        }
    }
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

/// The model-pane scene state — **shared by [`FrameKind::Model`] and [`FrameKind::PlayerModel`]**,
/// because the client's `CGCharacterModelBase` (`0x505680`) *extends* `CSimpleModel` (`0x76c8e0`)
/// rather than replacing it: every field below is a `CSimpleModel` member both classes carry.
///
/// **What this is and is not.** The 1.12 `Model` widget is a viewport that draws one M2 in a
/// little scene of its own — the character pane, the tabard designer, the minimap ping, the pet
/// bar's autocast shine, and every addon that wants a 3D thing in a frame. The engine core holds
/// exactly the scene an addon can read back or write; **the render is the app's**, the same
/// contract [`MinimapState`] already runs under, and the reason both exist as state-only kinds
/// here.
///
/// **Every field is a 1.12 binding's storage, and which binding is read off the registrar, not off
/// a string scan.** wow-re enumerated the whole family at the pair bytes on 2026-08-30
/// (`ui/scratch/model-pane-method-tables.md`): `Model`'s table is `0x878948` with **23** entries,
/// `PlayerModel`'s is `0x84f1fc` with **3**, and the derived table never repeats its base's. The
/// earlier justification for these fields — "the names are in the binary as isolated strings" — is
/// the mistake that round corrected: a `strings` hit answers *whether a name exists*, never *which
/// table owns it*, and `SetUnit`'s single pooled string `0x84f22c` is referenced by two entries in
/// two different tables. Ownership now comes from a dword-reference count over the name's VA.
///
/// **All 23 of `Model`'s are published** (decision 2027 carved the last seven — `AdvanceTime`,
/// `ReplaceIconTexture` and the fog near/far/clear set). `script::tests::modelframe`'s `UNBUILT`
/// array is the live count and is empty; the wall around it is what notices a name arriving or
/// leaving (1134 §4's naming rule, with nothing left to name).
#[derive(Clone, Debug, PartialEq)]
pub struct ModelState {
    /// The M2/MDX path last given to `SetModel`, or `None` after `ClearModel` / before any set.
    /// Stored **as written** — the client's own path space is `Interface\...\Foo.mdx`, and the
    /// app's loader is what maps `.mdx` to the `.m2` that actually ships.
    pub path: Option<String>,
    /// `PlayerModel:SetUnit(unit)` — the *other* way a pane gets its content, used by the dress-up
    /// and paper-doll frames. Mutually exclusive with [`Self::path`] in practice: whichever was set
    /// last is what the pane shows, so each setter clears the other.
    ///
    /// The field lives on every pane because it is a `CSimpleModel` member; the **setter** does
    /// not — `SetUnit` is [`FrameKind::PlayerModel`]'s (`0x84f1fc[0]` → `0x505d70`), and a plain
    /// `<Model>` has no way to reach it.
    pub unit: Option<String>,
    /// `SetSequence(n)` — the last animation id asked for (the raw id lands in
    /// `[bone0 block + 0xf8]`, which `PlayerModel`'s per-paint stomp reads). What actually
    /// PLAYS is [`Self::armed`].
    pub sequence: i32,
    /// **The widget's private scene clock**, milliseconds — `[scene+0xc]` of the `CM2Scene` the
    /// widget owns (`0x76cfc0`, cached at `+0x314`; never the world's `[0xc7b298]`). Advanced by
    /// the widget's own `OnUpdate` (`0x76d7f0`: `trunc(elapsed · 1000)`, no `+0.5`), which the UI
    /// pump walks for **visible** frames only — so a hidden pane's clock stands still and a
    /// re-shown one resumes where it stopped (the minimap ping's "ping N resumes where ping N−1
    /// left off"). Nothing else advances it: `AdvanceTime` is inert. The scene outlives the
    /// model, so `SetModel` does not reset it. Decision 2007.
    pub clock_ms: u64,
    /// What is armed on bone slot 0 — the sequence the pane plays and the anchor its cursor is
    /// read against. `None` while nothing plays: before any file, after `ClearModel`, or after
    /// a `SetSequence` naming an id the file does not own (which stops what was playing and
    /// arms nothing — `0x7121a0`'s interrupt runs before its bounds check).
    pub armed: Option<ArmedSequence>,
    /// `SetModel` ran but the loader's own arm has not — the file's facts ([`ModelFileFacts`])
    /// were not known at the call. The reference links the instance as a waiter for the
    /// streaming drain and runs the completion (`0x70ebd0`: arm Stand, variation 0) when the
    /// asset lands; [`ModelState::seed_from_facts`] is that completion, run when the host hands
    /// the facts over.
    pub pending_seed: bool,
    /// `ReplaceIconTexture(path)` — the type-14 texture override (`0x76cfe0(0xe, path)` →
    /// `0x710ec0`), which lives on the model instance and dies with it: `SetModel` and
    /// `ClearModel` clear it. `None` = the file's own textures.
    pub icon: Option<String>,
    /// The frame's size is the **implicit rect** — the file's bounding-box extent in layout units
    /// (`bboxExtent · 768·√(a²+1)` FrameXML units; render law §3, `implicit-size-law.md` §1),
    /// written by the engine because the pane authored no size (decision 2015). The geometry
    /// getters `0x76d080`/`0x76d0d0` answer it whenever no size is authored; here it is written
    /// into the layout input when the file's facts land and re-derived when the screen's aspect
    /// moves, and an authored `SetWidth`/`SetHeight`/`SetSize` clears it for good.
    pub implicit_size: bool,
    /// The pane's yaw in radians — `CSimpleModel+0x39c`. **One slot, written by two verbs on two
    /// different classes**: `Model:SetFacing` (`0x76dce0`) and `PlayerModel:SetRotation`
    /// (`0x505f00` → `0x505bb0`, whose last act is `0x505c44 mov [esi+0x39c], eax` — literally the
    /// same field). That is why the reference's rotate buttons can drive a paper-doll pane through
    /// `SetRotation` and `GetFacing` reads it back.
    pub facing: f32,
    /// `SetModelScale` — the model's own scale within the pane, default 1.
    pub scale: f32,
    /// The **pending** camera index (`CSimpleModel+0x320`) — `Some(n)` while the camera question
    /// is open, `None` once it is settled. The ctor writes `Some(0)`, a standing request for raw
    /// camera 0, which the model-ready hook applies ([`ModelState::seed_from_facts`]); `SetCamera`
    /// with no facts yet defers into it (`0x76cec0`'s two early legs). **While it is `Some`, the
    /// pane draws nothing at all** — the reference's draw gate is
    /// `76d5f0 cmp [this+0x320],-1 ; jne <skip everything>` (decision 2027).
    pub camera_pending: Option<i32>,
    /// The **installed** camera (`+0x31c`) as a RAW index into the file's camera table, or `None`
    /// for the NULL camera — which is what an index past the table's count installs (`76cf08`)
    /// and what a file with no cameras always gets. `None` is the **orthographic** render leg;
    /// `Some(n)` is the perspective one, framed by the file's own record `n`.
    ///
    /// Raw is the whole point: `0x76cec0` reads the count off `MD20+0x124` and the record at
    /// `[model+0x3c4] + idx·0x84 + 0x80`, and **never consults `cameraLookup`** — that array is
    /// the portrait bake's path (wow-re `modelframe-camera-law.md` §2.1).
    pub camera: Option<u32>,
    /// `SetPosition(x, y, z)` — the model's offset within the pane's scene.
    pub position: (f32, f32, f32),
    /// The pane's embedded `CGLight` (`CSimpleModel+0x324`) — see [`ModelLight`]. Typed since
    /// decision 2027: the render law (§5.1–§5.3) carves every field, its consumer and both of
    /// `SetLight`'s traps, so the tuple no longer has to be stored opaquely.
    pub light: ModelLight,
    /// `SetFogColor(r, g, b, a)` as the reference stores it: **one packed `0xAARRGGBB` dword**,
    /// which is why its getter is four values wide and why a Set→Get round trip is **lossy** —
    /// eight bits per channel (decision 1845).
    ///
    /// `0xffff_ffff` is the ctor's own terminal write, so the never-set answer is `1, 1, 1, 1` and
    /// not four zeros. It used to be `Option<(f32, f32, f32)>` here: three components, no alpha,
    /// and `None` for unset — every one of those three wrong.
    ///
    /// All seven fog verbs are built as of decision 2027 — this pair plus
    /// `SetFogNear`/`GetFogNear`/`SetFogFar`/`GetFogFar`/`ClearFog` — because the render law
    /// (§5.4) carves every one of their bodies.
    pub fog_color: u32,
    /// `+0x3a4` **bit 0** — fog armed. Set by `SetFogColor` (`76f059 or [edi+0x3a4],1`) and by the
    /// XML `<FogColor>` child; cleared by `ClearFog` (`76f5c5 and [edi+0x3a4],-2` — **bit 0 only**,
    /// so the colour, near and far all survive a clear and come back on the next `SetFogColor`).
    /// The ctor leaves it off.
    pub fog: bool,
    /// `+0x3ac` — fog near, raw and unclamped from `SetFogNear` (`76f282 fstp`), clamped at `≥ 0`
    /// only on the XML `fogNear` attribute path (`76cbbb`). Ctor `0.0`.
    pub fog_near: f32,
    /// `+0x3b0` — fog far, same shape as [`Self::fog_near`] (`76f432`, XML `fogFar` at `76cbf3`).
    /// **Ctor `1.0`**, not `0.0`: the fill callback stages `1/(far − near)` and the batch fogs
    /// only when that is `> 0`, so the ctor's pair is already a valid (if tiny) ramp.
    pub fog_far: f32,
}

/// A model pane's **armed** fog — [`ModelState::armed_fog`]'s answer, and the whole of what the
/// renderer needs: the disarmed values are engine state that nothing draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelFog {
    /// The packed `0xAARRGGBB` colour (`+0x3a8`). The alpha byte exists and is **never read** by
    /// the fill callback (`0x76d680` takes bytes 2,1,0 only).
    pub color: u32,
    /// `+0x3ac` / `+0x3b0`, raw. The fill stages `1/(far − near)` and a batch fogs only when that
    /// is `> 0`, so a `far <= near` pair arms the flag and still draws unfogged.
    pub near: f32,
    pub far: f32,
}

impl ModelFog {
    /// The colour as linear `[r, g, b]` in `0..=1` — the fill callback's own unpack
    /// (`0x7bbf20` -> bytes 2,1,0 × 1/255).
    pub fn rgb(&self) -> [f32; 3] {
        [16, 8, 0].map(|shift| ((self.color >> shift) & 0xff) as f32 / 255.0)
    }
}

/// A model pane's embedded **`CGLight`** — the 0x6c-byte object at `CSimpleModel+0x324` that the
/// per-paint fill callback `0x76d680` adds to the model's light collector, and the only light a
/// `<Model>` widget has (wow-re `modelframe-render-law.md` §5.1/§5.2).
///
/// **A plain `<Model>`'s is DISABLED and stays that way unless Lua enables it.** The ctor
/// `0x76c8e0` leaves `+0x60 = 0`, so `0x71bf90` returns before adding anything and the collector
/// finalizes with zero ambient and zero diffuse — under which a LIT batch draws **black**. That
/// is harmless by asset design (every in-game UI M2 is UNLIT on every material, §5.7) and it is
/// the faithful answer for an addon's lit one. `<PlayerModel>`'s ctor `0x505680` enables its own
/// instead, which is why the paper doll is lit; those panes are the portrait booth's, not this
/// widget's.
///
/// Each colour is stored **already multiplied by its intensity** — `SetLight` folds
/// `rgb/255 × intensity` before the copy — which is why there is no separate intensity field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelLight {
    /// `CGLight+0x60` — enabled. `SetLight`'s first argument writes it, and **only when nonzero**:
    /// `SetLight(0, …)` returns at `76e2cb` without touching the widget at all, so it is not a
    /// way to turn a light off (§5.3 trap 1).
    pub enabled: bool,
    /// `+0x08` — type. `true` = point/omni (`1`, what both ctors write), `false` = directional.
    /// Chooses which of the two vector setters `SetLight`'s `(x, y, z)` reaches.
    pub omni: bool,
    /// `+0x0c` position (when [`Self::omni`]) or `+0x24` direction (when not) — the direction is
    /// **normalised on write** by `0x71b6a0`, and is a *from-light* vector.
    pub vector: [f32; 3],
    /// `+0x30` ambient, intensity already folded in.
    pub ambient: [f32; 3],
    /// `+0x3c` diffuse, intensity already folded in.
    pub diffuse: [f32; 3],
}

/// The `<Model>` ctor's light (`0x76c8e0`): `0x71b4a0` (type 1, everything else zero), then
/// enabled `0` at `76c99c`, type `1` at `76c9a5`, ambient `(1,1,1)` at `76c9b4`–`76c9d3` and
/// diffuse `(1,1,1)` at `76c9d6`–`76c9ff`. White, and switched off.
impl Default for ModelLight {
    fn default() -> Self {
        Self {
            enabled: false,
            omni: true,
            vector: [0.0; 3],
            ambient: [1.0; 3],
            diffuse: [1.0; 3],
        }
    }
}

/// A fresh `Model` pane: no content, sequence 0, unrotated, unit scale, camera 0, at the origin.
///
/// `scale` is the one field whose zero value would be wrong — a model at scale 0 is invisible, and
/// the widget's documented default is 1 — so [`ModelState`] cannot use a derived `Default` for it
/// and this impl exists to say why.
impl Default for ModelState {
    fn default() -> Self {
        Self {
            path: None,
            unit: None,
            sequence: 0,
            clock_ms: 0,
            armed: None,
            pending_seed: false,
            icon: None,
            implicit_size: false,
            facing: 0.0,
            scale: 1.0,
            camera_pending: Some(0),
            camera: None,
            position: (0.0, 0.0, 0.0),
            light: ModelLight::default(),
            fog_color: 0xffff_ffff,
            fog: false,
            fog_near: 0.0,
            fog_far: 1.0,
        }
    }
}

/// What a pane's bone slot 0 is playing — the reference's `0x7121a0` arm (`SetSequence`
/// `0x76dec0` → `0x76cf50`, `SetSequenceTime` `0x76dfc0` → `0x76cf80`, and the loader's own
/// seed `0x70ebd0`): the id, and the **anchor** the cursor is read against. The reference
/// bakes `cursor_lo = sceneClock − trunc(ms)` once and its sampler re-reads that anchor every
/// frame; there is no counter that advances on its own, which is what lets the cooldown scrub
/// the pane every paint without the clock fighting it. Decision 2007.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArmedSequence {
    /// The `AnimationData.dbc` id — `SetSequence`'s argument, or the loader's Stand seed.
    pub anim_id: i32,
    /// The scene-clock value the cursor counts from: `cursor = clock_ms − anchor_ms`, so a
    /// `SetSequenceTime(id, ms)` at clock `c` stores `c − ms`.
    pub anchor_ms: i64,
    /// The scene clock when the arm was made — so a queued arm (made before the file's facts
    /// were known) can replay at residency with its original offset: `armed_at − anchor` is the
    /// `ms` the call asked for.
    pub armed_at_ms: u64,
    /// The completion callback has fired for this arm — the sequence ran its length (a clamp's
    /// end, or a loop's first pass) and the widget's `OnAnimFinished` ran. Once per arm: a
    /// re-arm starts a fresh one.
    pub finished: bool,
}

/// The playing sequence's cursor, as the renderer samples it — [`ModelState::play_head`]'s
/// answer under the file's facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelPlayHead {
    /// The `AnimationData.dbc` id of the armed sequence (the file owns it — see
    /// [`ModelFileFacts::owns`]).
    pub anim_id: u16,
    /// Milliseconds into the sequence: wrapped for a looping one, held at the last frame for a
    /// clamped one that has completed.
    pub cursor_ms: u32,
}

/// One sequence of a model file, as the clock needs it — see [`ModelFileFacts`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceFacts {
    /// The `AnimationData.dbc` id (`M2Sequence+0x00`).
    pub anim_id: u16,
    /// The sequence's length, `end − start` on the file's timeline (`+0x08 − +0x04`).
    pub duration_ms: u32,
    /// The sequence loops (the flag the formats crate decodes as `looping`); a clamped one
    /// holds its last frame and fires the completion callback once.
    pub looping: bool,
}

/// What the engine needs to know about a model **file** to run a pane's clock — the reference
/// reads these off the resident `MD20` (`animationLookup` at `md20+0x24`, the sequence table,
/// the header bounds); here the host's M2 loader has them and hands them over through
/// `UiScript::set_model_facts` once the asset lands. Keyed by the `SetModel` path
/// ([`model_key`]), shared by every pane holding that file. Decision 2007.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelFileFacts {
    /// The sequences the file owns, **in file order** — the first is `animations[0]`, the
    /// loader's fallback seed when the file does not own id 0.
    pub sequences: Vec<SequenceFacts>,
    /// The header bounding box, raw WoW model space (`min`, `max`) — the implicit rect of a
    /// size-less `<Model>` (render law §3) and the arrow's re-centring.
    pub bbox: ([f32; 3], [f32; 3]),
    /// How many records the file's **camera table** holds (`MD20+0x124`) — the bound
    /// `Model:SetCamera(n)` is checked against (`0x76cec0`: `76ceeb if idx >= count -> install
    /// NULL`). `0` for the overwhelming majority of models, which is why a plain `<Model>` almost
    /// always ends on the orthographic leg. Decision 2027.
    pub cameras: u32,
}

impl ModelFileFacts {
    /// Does the file own `anim_id` — the reference's `0x711960` ("does `animationLookup` map
    /// it"): a model owns an id iff some sequence carries it.
    pub fn owns(&self, anim_id: u16) -> bool {
        self.sequences.iter().any(|s| s.anim_id == anim_id)
    }

    /// The sequence `SetSequence(anim_id)` plays: the id's **first** file slot — variation 0,
    /// which is what both the loader's seed and the widget's arm pass (`0x7121a0`'s third
    /// argument is `0`, never `-1`, on this path).
    pub fn sequence(&self, anim_id: u16) -> Option<&SequenceFacts> {
        self.sequences.iter().find(|s| s.anim_id == anim_id)
    }

    /// The header bounding box's `(x, y)` extent in model units — the implicit rect of a
    /// size-less pane (render law §3) and the arrow's re-centring (`0x4a7b20`: `½·GetWidth`,
    /// `½·GetHeight`, both the geometry override's bbox extent).
    pub fn extent(&self) -> (f32, f32) {
        let (min, max) = self.bbox;
        ((max[0] - min[0]).max(0.0), (max[1] - min[1]).max(0.0))
    }

    /// Which camera raw index `idx` installs: `Some(idx)` when the table has it, `None` — the
    /// **NULL** camera, i.e. the orthographic leg — when it does not. Negative indices land in the
    /// same place the reference's unsigned `jae` puts them.
    pub fn camera_at(&self, idx: i32) -> Option<u32> {
        u32::try_from(idx).ok().filter(|&i| i < self.cameras)
    }

    /// The loader's idle seed (`0x70ebd0`'s tail, `0x710153`–`0x71019b`): **id 0 (`Stand`) if
    /// the file owns it, else `animations[0]`'s own id**; `None` only for a file with no
    /// sequences at all.
    pub fn stand_id(&self) -> Option<u16> {
        if self.owns(0) {
            Some(0)
        } else {
            self.sequences.first().map(|s| s.anim_id)
        }
    }
}

/// The key a model path is filed under: case-folded, forward slashes, no extension — so
/// `Interface\Cooldown\UI-Cooldown-Indicator.mdx` and its shipped `.m2` twin are one file, as
/// they are for the loader.
pub fn model_key(path: &str) -> String {
    let p = path.to_ascii_lowercase().replace('\\', "/");
    let stem = p
        .strip_suffix(".mdx")
        .or_else(|| p.strip_suffix(".mdl"))
        .or_else(|| p.strip_suffix(".m2"))
        .unwrap_or(&p);
    stem.to_string()
}

impl ModelState {
    /// `SetModel(path)` — `vt+0x94` (`0x76cce0`): a **fresh instance** of the file (`CreateModel`
    /// with flags 5), which displaces a unit, drops the previous instance's icon override and
    /// its arm, and runs the loader's completion (`0x70ebd0`) synchronously when the file is
    /// resident — [`Self::seed_from_facts`] — else waits for it ([`Self::pending_seed`]). The
    /// scene clock is the WIDGET's, not the instance's, and keeps running.
    pub fn set_file(&mut self, path: String, facts: Option<&ModelFileFacts>) {
        self.path = Some(path);
        self.unit = None;
        self.icon = None;
        self.armed = None;
        self.pending_seed = true;
        if let Some(facts) = facts {
            self.seed_from_facts(facts);
        }
    }

    /// `ClearModel` — releases the instance: no file, no unit, no override, nothing armed.
    pub fn clear_file(&mut self) {
        self.path = None;
        self.unit = None;
        self.icon = None;
        self.armed = None;
        self.pending_seed = false;
    }

    /// `SetSequence` / `SetSequenceTime` — the `0x7121a0` arm. It **interrupts** whatever plays
    /// first (the completion callback fires with a non-zero mode there, which the widget's
    /// `OnAnimFinished` gate ignores — §4.5: natural completion only), then arms `id` at `ms`
    /// into it — or arms nothing when the file does not own the id (the bounds check at
    /// `71247c` returns having armed nothing). With the facts not known yet the arm is kept and
    /// re-checked when they land ([`Self::seed_from_facts`]) — the reference's queued replay for
    /// a file still streaming.
    pub fn arm(&mut self, id: i32, ms: i64, facts: Option<&ModelFileFacts>) {
        self.sequence = id;
        let owned = u16::try_from(id)
            .ok()
            .is_some_and(|id| facts.is_none_or(|f| f.owns(id)));
        self.armed = owned.then(|| ArmedSequence {
            anim_id: id,
            anchor_ms: self.clock_ms as i64 - ms,
            armed_at_ms: self.clock_ms,
            finished: false,
        });
    }

    /// The loader's completion for a file that just became resident (`0x70ebd0`), then the
    /// replay of what was queued behind the load: the seed arms Stand (variation 0); an explicit
    /// `SetSequence`/`SetSequenceTime` made while the file streamed replays AFTER it — at its
    /// original offset — and decides the final state, which for an id the file does not own is
    /// **nothing armed** (the interrupt ran, the bounds check armed nothing). Idempotent once
    /// the seed has run: a second hand-over of the same facts only re-checks ownership.
    pub fn seed_from_facts(&mut self, facts: &ModelFileFacts) {
        let queued = self.armed.filter(|_| self.pending_seed);
        if self.pending_seed {
            self.pending_seed = false;
            self.armed = facts.stand_id().map(|id| ArmedSequence {
                anim_id: i32::from(id),
                anchor_ms: self.clock_ms as i64,
                armed_at_ms: self.clock_ms,
                finished: false,
            });
        }
        if let Some(q) = queued {
            let offset = q.armed_at_ms as i64 - q.anchor_ms;
            self.arm(q.anim_id, offset, Some(facts));
        } else if let Some(armed) = self.armed {
            if !u16::try_from(armed.anim_id).is_ok_and(|id| facts.owns(id)) {
                self.armed = None;
            }
        }
        // The model-ready hook's other half (`0x76ce00` `76ce3e`): apply the pending camera index
        // if the question is still open. The ctor's standing `Some(0)` is what gives a plain
        // `<Model>` its default camera 0, and it is applied here, once, exactly as the reference
        // applies it on the asset-ready edge.
        if let Some(idx) = self.camera_pending {
            self.install_camera(idx, Some(facts));
        }
    }

    /// The pane's fog, **only when it is armed** (`+0x3a4` bit 0) — the fill callback's own gate
    /// (`76d68a test byte [esi+0x3a4],1 ; je`). A disarmed pane stages nothing, and its collector
    /// keeps the zeroed `1/(far − near)` that the per-batch fog test refuses.
    pub fn armed_fog(&self) -> Option<ModelFog> {
        self.fog.then_some(ModelFog {
            color: self.fog_color,
            near: self.fog_near,
            far: self.fog_far,
        })
    }

    /// `0x76cec0` — select a camera by RAW table index. With no facts yet (the reference's "no
    /// model" / "not ready" legs at `76cece`/`76cedb`) the index is **deferred** into
    /// [`Self::camera_pending`] and nothing draws until it resolves; with facts, the index is
    /// bounds-checked against the table's count and either installed or answered with the NULL
    /// camera, and **either way the pending index is cleared** (`0x76ce80`'s
    /// `76cead mov [esi+0x320],-1` — installing *any* camera, NULL included, settles the
    /// question).
    pub fn install_camera(&mut self, idx: i32, facts: Option<&ModelFileFacts>) {
        match facts {
            Some(f) => {
                self.camera = f.camera_at(idx);
                self.camera_pending = None;
            }
            None => self.camera_pending = Some(idx),
        }
    }

    /// Where the armed sequence stands on the scene clock, under `facts`: wrapped for a looping
    /// sequence, held at the end for a clamped one. `None` when nothing is armed or the arm names
    /// a sequence the facts do not carry.
    pub fn play_head(&self, facts: &ModelFileFacts) -> Option<ModelPlayHead> {
        let armed = self.armed?;
        let anim_id = u16::try_from(armed.anim_id).ok()?;
        let seq = facts.sequence(anim_id)?;
        let raw = (self.clock_ms as i64 - armed.anchor_ms).max(0) as u64;
        let dur = u64::from(seq.duration_ms);
        let cursor = if dur == 0 {
            0
        } else if seq.looping {
            raw % dur
        } else {
            raw.min(dur)
        };
        Some(ModelPlayHead {
            anim_id,
            cursor_ms: cursor as u32,
        })
    }

    /// Has the armed sequence run its length without its completion having fired — the edge the
    /// tick turns into `OnAnimFinished` (the completion callback `0x76cdc0`, `mode == 0`). Once
    /// per arm, and **for a looping sequence too**: `0x719370` enqueues the completion when the
    /// widget clock reaches `lo + duration · replays` and tests the loop flag only after it, so a
    /// loop's first pass completes exactly like a clamp's end (wow-re
    /// `modelframe-texanim-and-sequence-law.md`, Q4; corrects 2007's "clamped only").
    pub fn completion_due(&self, facts: &ModelFileFacts) -> bool {
        let Some(armed) = self.armed else {
            return false;
        };
        if armed.finished {
            return false;
        }
        let Some(seq) = u16::try_from(armed.anim_id)
            .ok()
            .and_then(|id| facts.sequence(id))
        else {
            return false;
        };
        (self.clock_ms as i64 - armed.anchor_ms) >= i64::from(seq.duration_ms)
    }
}

/// The Minimap widget's modeled state — the client keeps **two independent zoom indices**, chosen by
/// whether the player is inside a WMO (wow-re minimap node, `wmo-interior-minimap.md` finding 2 Q7
/// CORRECTION): the inside flag `0xceaa60` routes the outdoor index `0x86f698` (CVar `minimapZoom`,
/// chunk table `0x8116d0`) or the indoor index `0x86f69c` (CVar `minimapInsideZoom`, radius table
/// `0x8116e8` = `{150,120,90,60,40,25}` yd). `GetZoom`/`SetZoom` read/write whichever is active, and
/// each persists across transitions — so zooming in indoors does not disturb the outdoor zoom, and
/// vice versa. (An earlier reading held the second index to be a rotate-minimap CVar and the interior
/// scale to be a constant; both were wrong — superseded in wow-re. Stronger since the ping RE
/// (`minimap-ping-law.md`): there is **no rotate-minimap CVar in 5875 at all** — a 214-site
/// `CVar::Register` census plus a whole-image string scan found no such knob, so nothing on this
/// path ever needs a rotation term.) `set_zoom` clamps to 5.
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
    /// The mask art the disc is cut to — `Minimap:SetMaskTexture(path)`, a real 1.12 method (the
    /// name is in the 5875 image; there is no `GetMaskTexture` beside it, so this is write-only
    /// from Lua). `None` = the engine default, [`MINIMAP_DEFAULT_MASK`].
    ///
    /// State-only here, pixels app-side, like the zoom index (0203): the app already masks the
    /// disc through its own `UiQuadMask`, and this only decides which texture it loads. It is what
    /// makes a **square minimap** possible, which is the single most recognisable thing pfUI does
    /// to the default UI (`modules/minimap.lua:27`).
    pub mask_texture: Option<String>,
    /// The **player-arrow** `Model` — the client's `[Minimap+0x338]`, ninth and last of the nine
    /// engine children the ctor builds ([`MINIMAP_ENGINE_CHILDREN`]). An explicit slot because that
    /// is the shape the client keeps: `CMinimap::SetPlayerFacing 0x4eb8e0` reaches the arrow through
    /// this field, never by counting children; "it is also `GetChildren()[9]`" is a *consequence* of
    /// the ctor's order, not the definition.
    pub player_arrow: Option<FrameHandle>,
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
            mask_texture: None,
            // Filled by the arena the moment the widget is inserted — the nine are ctor-time, so a
            // Minimap is never observably without them (`WidgetArena::create`).
            player_arrow: None,
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

/// The engine's own minimap mask — the circle `Minimap:SetMaskTexture` replaces. benilla's
/// renderer has loaded this path since the minimap existed; naming it here is what lets the Lua
/// setter override it.
pub const MINIMAP_DEFAULT_MASK: &str = "Textures\\MinimapMask";

/// **Nine `Model` children, built by the `CMinimap` ctor before anything else sees the widget** —
/// wow-re `ui/scratch/widget-list-bindings.md` §5 / `ui/ui.md`, VERIFIED. The ctor `0x4edbc0`
/// (TU `Ui\MinimapFrame.cpp`) allocates `CSimpleModel` (`0x76c8e0`) nine times with the Minimap as
/// parent, in three source-ordered groups whose `__LINE__` arguments give the order directly:
///
/// | # | stored at | `__LINE__` | what it is |
/// |---|---|---|---|
/// | 1–5 | `[Minimap+0x320 + 4k]` | 1424 | the rim/party arrows (`minimapArrowModel`) |
/// | 6–8 | `[Minimap+0x314 + 4k]` | 1438 | the three POI direction arrows (`minimapArrowModel`) |
/// | 9 | `[Minimap+0x338]` | 1450 | **the player arrow** (`minimapPlayerModel`) |
///
/// **Why the engine core carries them at all**, when the app draws every one of these arrows itself
/// from its own art (`benilla-app`'s `minimap::blips`): because they are *structure the Lua API can
/// see*, and two of the most-installed 1.12 addons read it. `Questie`'s `QuestieArrow.lua`
/// `GetPlayerFacing()` is `({Minimap:GetChildren()})[9]:GetFacing()`, and pfQuest's
/// `compat/client.lua` does the same — the ctor runs before the XML `<Frames>` descent
/// (`0x6ee408` before `0x6ee4ea`) and both linkers append at the tail, so on a stock client the
/// tuple is these nine then `Minimap.xml`'s six, and index 9 is the arrow. Without them that read
/// is `nil` and the addon dies calling a method on it.
///
/// The count is fixed and unconditional: an unloadable model file only skips the *file* assignment
/// (`0x4ee286` returns early), never the child.
pub const MINIMAP_ENGINE_CHILDREN: usize = 9;

/// The engine default for `<Minimap minimapArrowModel=…>` — the string at `0x84c768`, applied by
/// `0x4ee170` to engine children 1–8. (`Minimap.xml:97` sets the attribute explicitly, in the `.mdl`
/// spelling; the loader rewrites the extension either way.)
pub const MINIMAP_DEFAULT_ARROW_MODEL: &str = "Interface\\Minimap\\Rotating-MinimapArrow.mdx";

/// The engine default for `<Minimap minimapPlayerModel=…>` — the string at `0x8453c0`, applied by
/// `0x4ee260` to engine child 9 **alone**.
pub const MINIMAP_DEFAULT_PLAYER_MODEL: &str = "Interface\\Minimap\\MinimapArrow.mdx";

/// A `CSimpleScrollFrame`'s runtime state: the frame whose anchors are overridden to track the
/// scroll offset ([`crate::script::UiScript::resolve`]'s scroll-child override), and the current
/// vertical scroll position. `SetVerticalScroll` stores the offset VERBATIM — the reference's
/// `0x786db0` never reads the range (decision 2017) — and the range is always computed live from
/// the resolved rects (never cached here), so this struct carries only the two members the
/// client's `SetScrollChild`/`SetVerticalScroll` actually set (`[+0x318]`, `[+0x328]`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScrollFrameState {
    /// The scroll child (`SetScrollChild`) — the one frame whose content pans within this frame's
    /// rect. `None` = no child (nothing to clip or offset).
    pub child: Option<FrameHandle>,
    /// The vertical scroll offset in px (`SetVerticalScroll`), unclamped — the reference's
    /// `[+0x328]`. XML y-positive-up: a positive offset lifts the child
    /// (`child.top = scrollframe.top + vertical`), bringing content below the fold into view. The
    /// scroll bar's `[min, max]` is what keeps it inside the range — in FrameXML, never here.
    pub vertical: f32,
}

/// The face/size/flags one `Button:SetFont(file, height [, flags])` call writes.
///
/// **One record, not three, and that is the faithful shape here rather than a shortcut.** The
/// client's Button embeds three `CSimpleFont` sub-objects — normal `+0x33c`, disabled `+0x434`,
/// highlight `+0x3b8` — and `SetFont` (`0x780880`) retunes **all three with the same values**
/// (`GetFont` reads only the normal one). Nothing in the 1.12 Lua surface can set them apart: a
/// Button has `SetFont` and the `*FontObject` triple, and the latter changes which object each
/// state *inherits*, never its local face. So three identical copies could only ever diverge by
/// our own bug (wow-re `system/ui/scratch/widget-api-batch-benilla.md` Q8).
#[derive(Clone, Debug, PartialEq)]
pub struct ButtonFont {
    /// The TTF path (`"Fonts\\FRIZQT__.TTF"`).
    pub path: String,
    /// The font height in logical px.
    pub height: f32,
    /// The **normalized** OUTLINETYPE token — `""`, `"OUTLINE"` or `"THICKOUTLINE"` — so
    /// `GetFont`'s third return reads like the FontString/Font-object one rather than echoing
    /// whatever the addon spelled. Kept as a string, not `script::Outline`, because this module is
    /// the arena's vocabulary and deliberately names no script type.
    ///
    /// **An omitted `flags` argument clears the outline** (this is `""`), which is the reading the
    /// batch does not pin: it records arg4 as parsed against `{OUTLINE, THICKOUTLINE, MONOCHROME}`
    /// and says nothing about its absence. A `lua_tostring` on a missing argument yields no flags,
    /// so "absent means none" is what the shared impl most plausibly does, and it is the reading
    /// an addon setting a plain face expects.
    pub flags: String,
}

/// The client's button STATE INDEX — `[CSimpleButton+0x328]`, the one variable `SetState
/// 0x779790` writes and `IsEnabled 0x7800b0` / `GetButtonState 0x780180` read back.
///
/// The client numbers them 0 DISABLED / 1 NORMAL / 2 PUSHED, which is also the index into its
/// state-texture array (`[this + state*4 + 0x4b8]`); we name the variants instead, because
/// nothing here indexes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonVisualState {
    /// `Disable()` — the button fires no clicks and draws from the Disabled slot.
    Disabled,
    /// The resting state a button is born in.
    #[default]
    Normal,
    /// A mouse press captured over the button, or `SetButtonState("PUSHED")`.
    Pushed,
}

/// A Button's state model: which of the state textures draws is a **latched** consequence of the
/// interaction state, not a pure function of it — the client's texture array `+0x4b8` plus its
/// currently-shown pointer `+0x4c4`, which moves only on a transition into a state that *has* a
/// texture ([`Self::settle`]). The regions all exist in the arena; [`Self::region_visible`] reads
/// the pointer at extract time.
#[derive(Clone, Debug, PartialEq)]
pub struct ButtonState {
    /// `Enable`/`Disable`. A disabled button shows its DisabledTexture and fires no clicks.
    pub enabled: bool,
    /// The scripted PUSHED state — `SetButtonState("PUSHED"/"NORMAL") 0x780270` /
    /// `GetButtonState 0x780180`, the keybind visual's engine half (ref `ActionButtonDown/Up`,
    /// `ActionButton.lua:15-28`). ORs with the mouse-derived held+hovered press in
    /// [`Self::input_state`]; the mouse press itself stays outside the widget (the app's capture),
    /// which is why the state machine takes it as an argument.
    ///
    /// **The reference has one variable where we have two**, and the difference is stated rather
    /// than implied: `SetButtonState(state, locked)` writes `[+0x328]` *and* the lock at `+0x32c`,
    /// and an unlocked scripted push is therefore cleared by the next mouse press/release
    /// (`0x7793c2`'s `SetState(NORMAL)`, gated on `locked == 0`). Ours keeps the flag until Lua
    /// clears it. No 1.12 caller pushes without meaning it to stick: `ActionButtonDown` pairs
    /// every push with its own `ActionButtonUp`, and the micro buttons pass `locked = 1`.
    pub pushed_state: bool,
    /// [`FrameKind::LootButton`]'s own field — `CLootButton +0x4dc`, the **0-based** loot slot
    /// this row takes when clicked. `None` until `SetSlot` writes it (the ctor's zero is a slot
    /// id in the reference; ours is `None`, so a row that was never given one takes nothing
    /// rather than silently taking slot 0). Unused on every other button kind, the same way
    /// [`Self::checked`] below is unused on a plain Button — the client's subclasses add their one
    /// field the same way, and our flat state mirrors that rather than growing a variant.
    pub loot_slot: Option<u32>,
    /// CheckButton's checked flag (`+0x4dc`; XML `checked`). Unused on a plain Button.
    pub checked: bool,
    /// `<NormalTexture>`/`SetNormalTexture` (`+0x4bc`).
    pub normal: Option<RegionHandle>,
    /// `<PushedTexture>` (`+0x4c0`) — taken on the press transition. A button with none keeps
    /// whatever it was showing ([`Self::set_state`]), which is not a fallback but the absence of
    /// one.
    pub pushed: Option<RegionHandle>,
    /// `<DisabledTexture>` (`+0x4b8`).
    pub disabled: Option<RegionHandle>,
    /// `<HighlightTexture>` (`+0x4c8`) — additive over the current state texture while hovered
    /// (it lives in the HIGHLIGHT draw layer, above the others, not instead of them).
    pub highlight: Option<RegionHandle>,
    /// CheckButton `<CheckedTexture>` (`+0x4e0`) — additive while checked. A SEPARATE array from
    /// the state textures, with its own rule (`0x7854c0`): hide both, then show
    /// [`Self::disabled_checked`] if checked ∧ it exists ∧ the state is DISABLED, else this one if
    /// it exists, else nothing. **The fallback is one-way** — a disabled checked button with no
    /// DisabledChecked art falls back to this; a checked one never falls the other way.
    pub checked_tex: Option<RegionHandle>,
    /// CheckButton `<DisabledCheckedTexture>` (`+0x4e4`) — replaces CheckedTexture when disabled.
    /// The greyed tick a peace-forced faction's At War box wears (B369).
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
    /// See [`ButtonState::normal_font`] — the highlighted state (hovered **or**
    /// [`locked_highlight`](ButtonState::locked_highlight)). `None` means the button has no
    /// highlight instance at all and the label stays on the normal one, colour included.
    pub highlight_font: Option<String>,
    /// See [`ButtonState::normal_font`] — the disabled state.
    pub disabled_font: Option<String>,
    /// The NORMAL embedded font's own justify — `<NormalFont justifyH=>` (or the `<NormalText>`
    /// alias), a **local** write on the instance at `+0x33c` (`CSimpleButton::LoadXML 0x7788c0`
    /// → the `<Font>` loader `0x783c30`), severed from whatever object the instance inherits and
    /// surviving a later `SetTextFontObject`. `None` = the instance shows its object's justify.
    ///
    /// Two readers, and the first is the one that made this a field of the *button*: the label
    /// adopter `CSimpleButton::SetFontString 0x778d20` — the tail `SetText`'s lazy creation and
    /// the Lua adopter share — anchors an unanchored label to the button by exactly this word
    /// (`[button+0x390]`: LEFT→LEFT, RIGHT→RIGHT, else CENTER), which is how a row of the
    /// reference's `UIMenuButtonTemplate` (no `<ButtonText>`, `SetText` from Lua) hugs its left
    /// edge. The second is the label's paint and query surface, reached through the live link
    /// (`script::button::apply_normal_font`, `script::extract`). Decision 1996.
    pub normal_justify_h: Option<crate::script::JustifyH>,
    /// See [`ButtonState::normal_justify_h`] — `<HighlightFont justifyH=>`, the highlight instance
    /// (`+0x3b8`). Paint only: the adopter reads the normal instance alone.
    pub highlight_justify_h: Option<crate::script::JustifyH>,
    /// See [`ButtonState::normal_justify_h`] — `<DisabledFont justifyH=>`, the disabled instance
    /// (`+0x434`). Paint only.
    pub disabled_justify_h: Option<crate::script::JustifyH>,
    /// `Button:SetFont(file, height [, flags])` — the button's own face/size/flags, set on the
    /// embedded font objects themselves rather than on any font object they inherit. See
    /// [`ButtonFont`] for why one record covers the client's three.
    ///
    /// It lives here, not on the ButtonText's [`crate::script::RegionData`], because the reference
    /// **never dereferences the label pointer** (`+0x338`) in `SetFont`/`GetFont`: styling a
    /// `CreateFrame("Button")` with no `<ButtonText>` is a silent no-op there, and writing to a
    /// label would have meant lazily creating one — observable through `GetFontString()`, which
    /// must stay nil. `extract` applies it to the label whenever there is one, which also makes a
    /// later `SetText` (which *does* create the FontString) pick the style up for free.
    pub font: Option<ButtonFont>,
    /// Per-state label COLOR overrides (`Button:SetTextColor` and the Highlight/Disabled pair):
    /// when the matching state is current, extract repaints the ButtonText with this color over
    /// the state font object's own paint — the dropdown kit's rows lean on all three
    /// (`info.textR/G/B`, isTitle's NORMAL-yellow and notClickable's HIGHLIGHT-white recolors of
    /// a disabled row). `None` = the state font's paint.
    pub normal_color: Option<[f32; 4]>,
    /// See [`ButtonState::normal_color`] — the highlighted state. It does **not** fall back to
    /// `normal_color`: each state is its own font instance, so a `SetTextColor` cannot reach the
    /// highlighted label (which is why `UIDropDownMenu.lua` always pairs the two setters).
    pub highlight_color: Option<[f32; 4]>,
    /// See [`ButtonState::normal_color`] — the disabled state.
    pub disabled_color: Option<[f32; 4]>,
    /// `LockHighlight()` — the button reads as highlighted regardless of hover until
    /// `UnlockHighlight()` (ref `CButton::LockHighlight`; the dropdown kit keeps a checked row's
    /// highlight lit). That covers BOTH halves of the highlighted look: the HighlightTexture
    /// ([`Self::region_visible`]) and the label's font instance (`script::extract`). The list
    /// windows lean on the second alone — a tradeskill/craft/trainer recipe row blanks its
    /// highlight texture to `""` and locks the selected row anyway, purely for the white label.
    pub locked_highlight: bool,
    /// The client's **currently-shown state texture** — the pointer at `+0x4c4`. Private, and the
    /// only thing [`Self::region_visible`] consults for the three state textures: it is written by
    /// [`Self::set_state`] and [`Self::set_state_slot`] alone, which is what makes the transition
    /// rule un-bypassable.
    shown: Option<RegionHandle>,
    /// The state [`Self::shown`] was last resolved for — the client's `[+0x328]`. Latched from the
    /// three inputs ([`Self::enabled`], [`Self::pushed_state`] and the mouse's held+hovered) by
    /// [`Self::settle`], so a *transition* can be detected at all; the client keeps the same one
    /// variable for the same reason.
    state: ButtonVisualState,
}

impl Default for ButtonState {
    fn default() -> Self {
        ButtonState {
            loot_slot: None,
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
            normal_justify_h: None,
            highlight_justify_h: None,
            disabled_justify_h: None,
            font: None,
            normal_color: None,
            highlight_color: None,
            disabled_color: None,
            locked_highlight: false,
            shown: None,
            state: ButtonVisualState::Normal,
        }
    }
}

impl ButtonState {
    /// The slot a state draws from — the client's `[this + state*4 + 0x4b8]`.
    fn state_slot(&self, state: ButtonVisualState) -> Option<RegionHandle> {
        match state {
            ButtonVisualState::Disabled => self.disabled,
            ButtonVisualState::Normal => self.normal,
            ButtonVisualState::Pushed => self.pushed,
        }
    }

    /// The state the three inputs put the button in — disabled wins, then the press (the mouse's
    /// held+hovered, or the scripted [`Self::pushed_state`]), else resting.
    fn input_state(&self, hovered: bool, held: bool) -> ButtonVisualState {
        if !self.enabled {
            ButtonVisualState::Disabled
        } else if (held && hovered) || self.pushed_state {
            ButtonVisualState::Pushed
        } else {
            ButtonVisualState::Normal
        }
    }

    /// **`CSimpleButton::SetState 0x779790`** — the transition, and the whole of why the shown
    /// texture is state rather than a lookup.
    ///
    /// Both halves of the swap test the SAME dword — the new state's own slot: `0x7797b5` loads
    /// `[esi + 4*edi + 0x4b8]` and `0x7797be` skips the hide when it is null; `0x7797d9`/`0x7797e2`
    /// skips the show on the same value. The state itself is written regardless (`0x779801`). So a
    /// transition into a state with no texture of its own **changes nothing** — the
    /// previously-shown texture stays up, and there is no fallback path to Normal anywhere in the
    /// function. The equality early-out sits at `0x7797a3`/`0x7797a9`, *after* the unconditional
    /// `[+0x32c] = locked` store and *before* every texture step. wow-re
    /// `system/ui/scratch/button-disabled-state-texture-law.md`, VERIFIED.
    ///
    /// **A button is NORMAL before its art is loaded, which is what makes the rule bite.** The
    /// `CSimpleButton` ctor `0x7786a0` writes `[+0x328] = 0` and `[+0x4c4] = 0`, then ends
    /// `push 1; call 0x779160` → `[vtbl+0x9c]` = this function with NORMAL. Only then does
    /// `LoadXML` install the art, each child through the `0x778fd0` setter family
    /// ([`Self::set_state_slot`]) — `<NormalTexture>` at `0x77890e` with idx 1, which matches, so
    /// it is shown on the spot. Every button in the family therefore reaches its first `Disable()`
    /// already wearing its normal art.
    ///
    /// Three things the reference draws out of that one rule, which a pure `state → slot`
    /// resolution cannot:
    ///
    /// - **A press with no PushedTexture keeps its normal art.** (Our old resolution special-cased
    ///   this as `pushed.or(normal)` — the fallback was never a rule, it was this mechanism seen
    ///   from one side.)
    /// - **`Disable()` on a button with no DisabledTexture keeps its normal art.** This is B369:
    ///   `ReputationDetailAtWarCheckBox` has a `<NormalTexture>` and no `<DisabledTexture>`, and
    ///   `ReputationFrame_Update` `Disable()`s it for a faction whose war flag cannot be toggled —
    ///   in the reference the box stays on screen (greyed label, still a box); resolving the shown
    ///   texture as a pure function of the state made it vanish, leaving a bare label.
    ///   `Disable 0x77ffd0` reaches here through `0x78009a call [vtbl+0x90](0)` → `0x779160`.
    /// - **So does an empty spellbook slot's `UI-Quickslot2` ring**, which is the same shape and
    ///   was the case decision 0227 got backwards — see 2011.
    fn set_state(&mut self, new: ButtonVisualState) {
        if new == self.state {
            return;
        }
        self.state = new;
        if let Some(slot) = self.state_slot(new) {
            self.shown = Some(slot);
        }
    }

    /// Run the state machine over the current inputs — the caller's job at every point one of them
    /// can have moved (`script::button::settle`, which reads the mouse's two off the model).
    ///
    /// The client has no such call because it has no derived inputs: its mouse handlers call
    /// `SetState` directly (`0x7791ed` enter, `0x7793f0` leave, `0x7792ad` down, `0x7793c2` up,
    /// each gated on `locked == 0`) and so do `Enable`/`Disable`. Ours keeps the inputs as fields
    /// and latches [`Self::state`] from them here; the transitions that result are the same ones,
    /// in the same order.
    pub fn settle(&mut self, hovered: bool, held: bool) {
        self.set_state(self.input_state(hovered, held));
    }

    /// **`SetNormalTexture`/`SetPushedTexture`/`SetDisabledTexture 0x778fd0`** — the slot store,
    /// plus the conditional push to the shown pointer: `[this+idx*4+0x4b8] = tex`, and `+0x4c4`
    /// takes it **only when `idx == [this+0x328]`** (the gate at `0x779027`). Setting a state's
    /// texture while the button is in a *different* state does not display it: a fresh region is
    /// born hidden (`0x77f695` writes `+0xc4 = 0`), so a non-current slot is installed dark.
    ///
    /// This is also how a button's art first reaches the screen at all — see [`Self::set_state`]
    /// for why LoadXML always finds the button in NORMAL.
    ///
    /// **One stated divergence.** When the write DISPLACES an occupant, the reference *destroys*
    /// it (`0x77900a` calls the old object's `vtbl[0](1)`) and clears `+0x4c4` if that occupant
    /// was the shown one. Ours never gets there: `script::button::ensure_slot` creates a slot's
    /// region once and later `SetNormalTexture` calls repaint that same region, so the handle is
    /// stable and an addon holding `GetNormalTexture()` keeps a live object where the reference
    /// would have handed it a dead one. Nothing in the corpus reads a state texture across a
    /// replacement; the object-identity half is not modeled.
    pub fn set_state_slot(&mut self, state: ButtonVisualState, rh: Option<RegionHandle>) {
        match state {
            ButtonVisualState::Disabled => self.disabled = rh,
            ButtonVisualState::Normal => self.normal = rh,
            ButtonVisualState::Pushed => self.pushed = rh,
        }
        if state == self.state {
            self.shown = rh;
        }
    }

    /// Whether a region of this button draws. The three state textures answer from the shown
    /// pointer alone ([`Self::set_state`]) — which is why the press is no longer an argument here:
    /// it is an *input to the transition*, consumed at [`Self::settle`], not something a paint
    /// re-derives. `hovered` stays because the Highlight is not a state texture and really does
    /// track the cursor with no latch of its own. Checked draws additively per its own condition,
    /// and any region that is not one of these (ButtonText, user regions) always draws.
    pub fn region_visible(&self, rh: RegionHandle, hovered: bool) -> bool {
        let some = Some(rh);
        if some == self.normal || some == self.pushed || some == self.disabled {
            return some == self.shown;
        }
        if some == self.highlight {
            // No `enabled` term: a disabled button loses its highlight because `Disable()` turns
            // the whole HIGHLIGHT draw layer off (`script::button::set_enabled`), which extract
            // applies before it ever reaches here.
            return hovered || self.locked_highlight;
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

/// **The slider drag law — the one owner, for every lane.**
///
/// Two pure functions, stated on a single axis in *distance from the track's leading edge* (the
/// end the thumb sits at when the value is `min`). That framing is what makes them lane-neutral:
/// the Lua widget arena measures y **up** and the Bevy-UI glue screens measure y **down**, and
/// both reduce to "how far along the track is this", so neither has to restate the arithmetic.
///
/// Restating it is exactly what went wrong. The AddOns glue scrollbar shipped with a *decorative*
/// knob and no drag at all (1297 named the gap; B273's reporter hit it), the char-create glue
/// scrollbar grew its own accumulated-delta drag that drifts off the cursor, and the engine slider
/// held the real formula — three surfaces, one widget, no shared line of code. These two functions
/// are that shared line.
///
/// **The law is benilla's, and it diverges from 1.12 deliberately in one place.** wow-re's
/// `system/ui/scratch/slider-mouse-law.md` (a §5 1v1, VERIFIED off the bytes of `0x789ba0` /
/// `0x789ca0`) settled `CSimpleSlider`: there is **no thumb hit-test in the class at all** — every
/// press, track or thumb, warps the value to seat the thumb's CENTER under the cursor and begins
/// one continuous drag capture, button-agnostic, clamped by SetValue. We take all of that except
/// the thumb press: ours grabs **offset-preserving**, so the point you grabbed stays under the
/// finger instead of jumping to the thumb's middle (decision 0992 §6 — kept as the less surprising
/// feel, and invisible on a reference-sized thumb either way). If that ever flips to byte-faithful,
/// it flips here, once, for every surface at the same time.
///
/// [`slider_grab`] runs on the press, [`slider_fraction`] on the press and on every move after it.
///
/// Where a press grabs the thumb: the offset from the thumb's leading edge that stays under the
/// cursor for the rest of the drag.
///
/// `cursor` and `thumb_lead` are distances from the track's leading edge; `thumb_extent` is the
/// thumb's length along the axis. A press **on** the thumb keeps its grabbed point (0992 §6); a
/// press **off** it — anywhere on the track — grabs the thumb by its center, which is what makes
/// the value warp under the cursor and the drag continue as one gesture (1.12's whole law, and
/// 0989's directed requirement, which converged with it).
pub fn slider_grab(cursor: f32, thumb_lead: f32, thumb_extent: f32) -> f32 {
    if cursor >= thumb_lead && cursor <= thumb_lead + thumb_extent {
        cursor - thumb_lead
    } else {
        thumb_extent * 0.5
    }
}

/// Cursor → fraction of travel, absolute and drift-free: the thumb's leading edge goes to
/// `cursor − grab`, and that lands at `fraction × (track_extent − thumb_extent)`.
///
/// `None` when the travel is zero or negative — a thumb as long as its track has nowhere to go, so
/// there is no value to compute and the caller must leave the slider alone rather than divide by
/// zero. Otherwise clamped to `[0, 1]`: dragging past either end pins there, exactly as the
/// client's own out-of-span presses pin through SetValue's clamp.
pub fn slider_fraction(
    cursor: f32,
    grab: f32,
    track_extent: f32,
    thumb_extent: f32,
) -> Option<f32> {
    let travel = track_extent - thumb_extent;
    (travel > 0.0).then(|| ((cursor - grab) / travel).clamp(0.0, 1.0))
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

/// A `ColorSelect`'s colour (`CSimpleColorSelect`, ctor `0x78b220`) — **three HSV `f32`s**, hue in
/// **degrees**, and `-1` for the hue of anything grey.
///
/// **This is the corrected model, and the correction is the point.** The obvious store is three RGB
/// bytes, and it is wrong: wow-re's §5 trio (`system/ui/scratch/colorselect-color-law.md`,
/// 2026-08-11, dispatched from this repo for exactly this question) reads the members off the ctor —
/// `+0x328` hue-degrees, `+0x32c` saturation, `+0x330` value, all `f32`, initialised `(0, 0, 1)` =
/// white — with the packed dword at `+0x334` a *derived cache* for the value-strip gradient, not the
/// state. Two consequences a byte store cannot express: hue survives a drag that takes saturation to
/// zero, and `GetColorHSV` really does answer `-1` for a grey (`0x7bbc80` writes the sentinel at
/// `0x7bbccd`).
///
/// **The round trip is not the identity, and that is the client's arithmetic, not ours.** There are
/// two quantizers and they disagree:
///
/// * **A, inbound, round-half-up** — `SetColorRGB` only: `trunc(v·255 + 0.5)` via the CRT `__ftol`
///   (`0x78ec7d`/`0x78ec91`/`0x78eca9`).
/// * **B, outbound, floor** — every read path: `(bits_f32(v·255 + 512.0) >> 14) & 0xff`
///   (`0x7bbec0`), a one-sided `2^-15` window.
///
/// Composed over the lossy `f32`-degrees hue trip, wow-re measured **exhaustively over all 256³
/// reachable triples: 1,636,226 (9.7527 %) come back with exactly one channel exactly `-1`** — never
/// `+1`, never `±2`, never the minimum channel, greys never. And because `quantize_a(b/255) == b` for
/// every byte, it **ratchets**: the FrameXML idiom `r,g,b = f:GetColorRGB() … f:SetColorRGB(r,g,b)`
/// re-applies the same lossy map instead of settling — `(0, 8, 132)` walks to `(0, 0, 132)` in eight
/// cycles. Every Ace2/Dewdrop colour option is that idiom, once per open-and-accept.
///
/// It is transcribed rather than smoothed **because it is what the client computes** and the addons
/// were written against it (wow-re's own §7: *"not a fidelity defect to correct"*). If it ever has to
/// go, the surgical change is one line — using quantizer A on the read-back too drives the mismatch
/// count to exactly 0, measured — and it would be a deliberate, recorded deviation, not a bug fix.
///
/// No alpha: `SetColorRGB` reads an optional 5th argument, quantizes it, and *discards* it
/// (`0x7bbf20` reads three bytes; `0x7bbec0` hard-writes `0xff`). The reference's opacity is a
/// separate `Slider`. `SetColorHSV`/`GetColorHSV` (`0x78e920`/`0x78ea00`) have zero callers across
/// the 218-addon corpus, so they wait for a customer (decision 1195) — but the state they would read
/// and write is now the right shape for them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorSelectState {
    /// `[hue-degrees, saturation, value]` — the widget's `+0x328`/`+0x32c`/`+0x330`. Hue is `-1.0`
    /// whenever saturation is 0 (the grey sentinel).
    pub hsv: [f32; 3],
    /// `<ColorWheelTexture>` / `SetColorWheelTexture` (`0x78de90`), the widget's `+0x318` — the hue
    /// disc. The **rect of this region is the wheel's hit box**: the press handler tests the cursor
    /// against `[+0x318]+0x24`, not against the frame. Created on first set, like a Slider's thumb.
    pub wheel: Option<RegionHandle>,
    /// `<ColorWheelThumbTexture>` / `SetColorWheelThumbTexture` (`0x78e160`) — the little marker
    /// that rides the disc. Its rect is *derived* from `hsv` at extract, and an anchor authored on
    /// it in XML is **discarded**: `0x78b850` calls `ClearAllPoints 0x767ed0` and re-`SetPoint`s
    /// from C++ on every colour change. The two thumbs are the only elements here with a `file=`
    /// and the only ones the reference gives no `<Anchors>` — those two facts are the same fact.
    pub wheel_thumb: Option<RegionHandle>,
    /// `<ColorValueTexture>` / `SetColorValueTexture` (`0x78e450`), the widget's `+0x320` — the
    /// brightness strip, and the second hit box (`[+0x320]+0x24`).
    pub value_strip: Option<RegionHandle>,
    /// `<ColorValueThumbTexture>` / `SetColorValueThumbTexture` (`0x78e720`) — the strip's marker,
    /// rect derived from `hsv[2]`.
    pub value_thumb: Option<RegionHandle>,
}

impl Default for ColorSelectState {
    /// The ctor's own initial state: H=0, S=0, V=1 — white (`0x78b27e`/`0x78b298`/`0x78b28e`).
    fn default() -> ColorSelectState {
        ColorSelectState {
            hsv: [0.0, 0.0, 1.0],
            wheel: None,
            wheel_thumb: None,
            value_strip: None,
            value_thumb: None,
        }
    }
}

impl ColorSelectState {
    /// **Quantizer A** — `SetColorRGB`'s inbound leg only (`0x78ec7d`/`0x78ec91`/`0x78eca9`): clamp
    /// to `[0, 1]`, `·255.0 + 0.5`, then the CRT `__ftol` (`0x40a2b0`) which forces round-to-chop.
    /// Round-**half-up**. The `255.0`/`0.5` operands are `f32` in `.rdata` and widen exactly, so the
    /// chain runs in `f64` as the x87 PC_53 registers do.
    pub fn quantize_a(v: f64) -> u8 {
        let clamped = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        // In-range the product is `[0.5, 255.5]`, so the truncation is always a valid byte.
        (clamped * 255.0 + 0.5) as u8
    }

    /// **Quantizer B** — `0x7bbec0`'s magic-512 pack, on *every* outbound path (the `OnColorSelect`
    /// payload `0x78bb42`, `GetColorRGB` `0x78ede6`, the strip tint `0x78bbf6`). `v·255 + 512.0`
    /// lands in the `[512, 1024)` binade where the `f32` ulp is exactly `2^-14`, so the mantissa
    /// holds `RN(v·255·2^14)` and `>>14` reads its **floor**. No clamp — out-of-range wraps mod 256,
    /// which is reachable through `SetColorHSV` (unclamped) but not through `SetColorRGB`.
    pub fn quantize_b(v: f32) -> u8 {
        let c = (f64::from(v) * 255.0 + 512.0) as f32;
        ((c.to_bits() >> 14) & 0xff) as u8
    }

    /// `0x7bbf20`'s unpack — a colour byte back to `f32`, scaling by the **`f32`** `0x3b808081`
    /// (≈1/255, rounded *up*: this is why the only round-trip failure mode is a shortfall that
    /// floors down). Distinct from the `f64` `1/255` the Lua push uses; both appear in one call.
    fn unpack_byte(b: u8) -> f32 {
        let k = f32::from_bits(0x3b80_8081);
        (f64::from(i32::from(b)) * f64::from(k)) as f32
    }

    /// The Lua-facing normalize (`0x78bb47..0x78bb85` and `0x78edfa`/`0x78ee17`/`0x78ee34`): the
    /// channel byte times the **`f64`** `1/255` at `0x804578` — a multiply by the reciprocal, never a
    /// divide (they are not the same double).
    pub fn normalize(byte: u8) -> f64 {
        f64::from(byte) * (1.0_f64 / 255.0)
    }

    /// `0x7bf680` — the index of the largest `|component|`. Compares are strict `>` (`fcom`; equal
    /// takes the not-greater branch), so a tie resolves to the **later** index.
    fn dominant_axis(v: &[f32; 3]) -> usize {
        let (a0, a1, a2) = (v[0].abs(), v[1].abs(), v[2].abs());
        if a0 > a1 {
            if a0 > a2 {
                0
            } else {
                2
            }
        } else if a1 > a2 {
            1
        } else {
            2
        }
    }

    /// `0x7bf700` — the index of the smallest `|component|`, traced from the same flag pattern.
    fn minor_axis(v: &[f32; 3]) -> usize {
        let (a0, a1, a2) = (v[0].abs(), v[1].abs(), v[2].abs());
        if a0 >= a1 {
            if a2 < a1 {
                2
            } else {
                1
            }
        } else if a0 >= a2 {
            2
        } else {
            0
        }
    }

    /// `0x7bbc80` — RGB→HSV. `value` is the dominant channel; `saturation = (value − minor)/value`
    /// (0 when `value == 0`); `hue` is the `-1` sentinel when saturation is 0, else the 60°-sector
    /// formula on the chroma, wrapped `+360` if negative. Each store rounds to `f32`; the chroma
    /// divide runs on the un-rounded `f64` register.
    fn rgb_to_hsv(rgb: &[f32; 3]) -> [f32; 3] {
        let f = f64::from;
        let dom = Self::dominant_axis(rgb);
        let minor = Self::minor_axis(rgb);
        let value = rgb[dom];
        let sat = if value == 0.0 {
            0.0
        } else {
            ((f(value) - f(rgb[minor])) / f(value)) as f32
        };
        let hue = if sat == 0.0 {
            -1.0
        } else {
            let chroma = f(value) - f(rgb[minor]);
            let sector = match dom {
                0 => ((f(rgb[1]) - f(rgb[2])) / chroma) as f32,
                1 => ((f(rgb[2]) - f(rgb[0])) / chroma + 2.0) as f32,
                _ => ((f(rgb[0]) - f(rgb[1])) / chroma + 4.0) as f32,
            };
            let hue_deg = (f(sector) * 60.0) as f32;
            if hue_deg < 0.0 {
                (f(hue_deg) + 360.0) as f32
            } else {
                hue_deg
            }
        };
        [hue, sat, value]
    }

    /// `0x7bbd60` — HSV→RGB. `s == 0` short-circuits to `(v, v, v)` *without reading hue*, which is
    /// what makes the `-1` sentinel inert on the way out. Otherwise the 6-sector decode, with the
    /// sector floored by the same magic-512 trick as quantizer B and clamped to `≤ 5`. The
    /// `f32(1/60) = 0x3c888889` on the way back is the lossy step the whole `-1` drift comes from.
    pub fn hsv_to_rgb(hsv: &[f32; 3]) -> [f32; 3] {
        let f = f64::from;
        let (h, s, v) = (hsv[0], hsv[1], hsv[2]);
        if s == 0.0 {
            return [v, v, v];
        }
        let hue = if h == 360.0 { 0.0 } else { h };
        let inv60 = f32::from_bits(0x3c88_8889);
        let sector_float = f(hue) * f(inv60); // an un-rounded f64 register
        let magic = (sector_float + 512.0) as f32;
        let raw = (magic.to_bits() >> 14) & 0xff;
        let sector = if raw <= 5 { raw } else { 5 };
        let frac = (sector_float - f64::from(sector as i32)) as f32;
        let p = ((1.0 - f(s)) * f(v)) as f32;
        let q = ((1.0 - f(frac) * f(s)) * f(v)) as f32;
        let t = ((1.0 - (1.0 - f(frac)) * f(s)) * f(v)) as f32;
        match sector {
            0 => [v, t, p],
            1 => [q, v, p],
            2 => [p, v, t],
            3 => [p, q, v],
            _ if sector == 4 => [t, p, v],
            _ => [v, p, q],
        }
    }

    /// Store HSV **raw** — no clamp, no quantize, no round trip through RGB. This is what the
    /// widget's own drag handler does (`0x78bd80`: `fstp [esi+0x328]` / `[+0x32c]` / `[+0x330]`
    /// straight off the geometry) and what `SetColorHSV 0x78e920` does (wow-re
    /// `colorselect-color-law.md` §4/§5). The reachable state set is therefore *strictly finer*
    /// than [`Self::set_rgb`]'s, which can only ever land on the HSV image of the 8-bit lattice —
    /// the picker's wheel resolves colours the Lua boundary cannot name. What Lua *reads back* is
    /// quantized identically either way, because the quantize lives on the outbound leg.
    pub fn set_hsv(&mut self, h: f32, s: f32, v: f32) {
        self.hsv = [h, s, v];
    }

    /// `SetColorRGB`'s whole store, in the binary's order (`0x78eb7e`…`0x78ecfa`): clamp each
    /// argument to `[0, 1]` as `f32` → quantizer A → back to `f32` through `0x7bbf20`'s `1/255` →
    /// `0x7bbc80` RGB→HSV → store. Always stores — there is deliberately **no change-gate**, unlike
    /// [`SliderState::store_value`]: the only conditional between `SetColorRGB`'s entry and its
    /// handler invoke is `0x78bafd`, which tests whether a handler is *bound*, and none of the three
    /// writers reads the old HSV before overwriting it. Two sibling widgets in the same band,
    /// opposite gating.
    pub fn set_rgb(&mut self, r: f64, g: f64, b: f64) {
        let bytes = [
            Self::quantize_a(r),
            Self::quantize_a(g),
            Self::quantize_a(b),
        ];
        let rgb = [
            Self::unpack_byte(bytes[0]),
            Self::unpack_byte(bytes[1]),
            Self::unpack_byte(bytes[2]),
        ];
        self.hsv = Self::rgb_to_hsv(&rgb);
    }

    /// The read-back both `GetColorRGB` and the `OnColorSelect` payload run — literally the same two
    /// calls in the same order (`0x78edda`/`0x78ede6` vs `0x78bb36`/`0x78bb42`), which is *why* a
    /// handler's `arg1..arg3` are bit-identical to a `GetColorRGB()` on the next line: HSV→RGB, then
    /// quantizer B, then the `f64` `1/255`.
    pub fn rgb_f64(&self) -> (f64, f64, f64) {
        let rgb = Self::hsv_to_rgb(&self.hsv);
        (
            Self::normalize(Self::quantize_b(rgb[0])),
            Self::normalize(Self::quantize_b(rgb[1])),
            Self::normalize(Self::quantize_b(rgb[2])),
        )
    }
}
