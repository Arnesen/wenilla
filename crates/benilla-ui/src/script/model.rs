use std::collections::{HashMap, HashSet};

use crate::layout::{LayoutInput, LayoutSolver, Rect};
use crate::widget::{FrameHandle, RegionHandle, WidgetArena};

use super::{
    auction, backdrop, bank, char_stats, chat_window, colorselect, container, craft, cursor, death,
    duel, follow, gossip, guild, inspect, item_text, loot, loot_roll, macros, mail, merchant,
    party, petition, pvp, quest, quest_log, reputation, session, simplehtml, skills, slider,
    social, spellbook, stable, taxi, trade, tradeskill, trainer, weapon_enchant, ActionSlot,
    AuraState, FontObject, ItemTemplateView, PlayerReqState, RegionData, ScriptValue, SoundRequest,
    UnitState,
};

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The Rust-side model (plain data; lives in lua.app_data)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The host's texture-path resolvability oracle — see [`Model::texture_probe`].
pub type TextureProbe = Box<dyn Fn(&str) -> bool>;

/// The host's texture **texel-size** oracle — see [`Model::texture_size_probe`].
pub type TextureSizeProbe = Box<dyn Fn(&str) -> Option<(u32, u32)>>;

/// The Rust-side model behind the Lua VM — the arena, the layout inputs + resolved rects, the
/// id↔handle bijection, region visuals, and the event/script registrations. Held in `lua.app_data`
/// (interior-mutable) so callbacks reach it; contains **no** mlua handles (the MAXCSTACK discipline).
pub(crate) struct Model {
    /// Every discovered addon, in load order — the AddOn API's registry, filled by the host at
    /// world entry ([`super::UiScript::register_addons`]). See [`super::addon`].
    pub(crate) addons: Vec<super::addon::AddOnInfo>,
    /// The AddOns folder, so `LoadAddOn` can read an addon's files from inside a Lua binding.
    pub(crate) addons_root: Option<std::path::PathBuf>,
    /// The host's font engine ([`super::UiScript::set_text_measurer`]) — what lets a metric read
    /// answer inside the Lua call that asked, instead of a frame later. `None` in every VM without
    /// a font atlas, which is the measure round-trip's own world and behaves exactly as it did.
    pub(crate) measurer: Option<Box<dyn super::TextMeasure>>,
    /// The host's texture-path oracle ([`super::UiScript::set_texture_probe`]): does this sprite
    /// reference resolve to a file — patch chain or loose addon folder? What lets the path form of
    /// `SetTexture` return the reference's **1 | nil** load verdict inline (wow-re
    /// `widget-api-batch-benilla.md` Q1 — Atlas branches on it to pick its map art). `None` in an
    /// engine-less VM (tests, the addon harness), where the path form keeps answering nil: no
    /// backend, nothing loads — which is also exactly what those tests always saw.
    pub(crate) texture_probe: Option<TextureProbe>,
    /// The host's texture **texel-size** oracle ([`super::UiScript::set_texture_size_probe`]): how
    /// many texels wide and tall is the art at this path?
    ///
    /// What lets a region whose authored size on an axis is exactly `0` take its span from its
    /// CONTENT, the way the real client's size getters do — `CSimpleTexture::GetWidth 0x770720` /
    /// `GetHeight 0x770790` return the authored value only when it is not `0.0`, and otherwise read
    /// the loaded texture's `[tex+0x144]`/`[tex+0x148]` through the same converter `<AbsDimension>`
    /// uses, so **one texel is one FrameXML unit** (wow-re `region-size-fallback.md` §2, decision
    /// 1349). The host answers off the same candidate walk the renderer decodes with and memoises,
    /// so the size layout resolves with is the size the screen shows.
    ///
    /// `None` in an engine-less VM (tests, the addon harness), where a zero-size texture keeps the
    /// rect it always had — there is no art to measure without a backend.
    pub(crate) texture_size_probe: Option<TextureSizeProbe>,
    /// Where per-addon saved variables live: the account-scoped folder, then this character's.
    /// Both are directories holding one `<Addon>.lua` per declaring addon.
    pub(crate) addons_saved_account: Option<std::path::PathBuf>,
    pub(crate) addons_saved_character: Option<std::path::PathBuf>,
    /// The FrameXML **template registry**, persisted across [`crate::loader::load`] calls — the
    /// client's template table is global (`0x6ee500`, rf24), so a file may `inherits=` a template
    /// an *earlier file* registered (the real MerchantFrame.xml inherits
    /// CharacterFrameTemplates.xml's tab template). Register-before-use in load order, exactly
    /// the client's rule; a per-document registry silently dropped every cross-file inherit.
    ///
    /// **Here rather than on `UiScript`** so the loader can run from a bare `&Lua`: that is what
    /// lets `LoadAddOn` load an addon from inside a Lua binding, synchronously, the way the
    /// reference does (1188 phase 2). `font_objects` below was already here, and these two are
    /// the same kind of thing — VM-global registries the loader fills.
    pub(crate) framexml_templates:
        std::cell::RefCell<std::collections::HashMap<String, crate::framexml::Element>>,
    /// The FrameXML **font-element registry** (a separate namespace — a font inherits a font,
    /// never a frame template), persisted for the same cross-file reason.
    pub(crate) framexml_fonts:
        std::cell::RefCell<std::collections::HashMap<String, crate::framexml::Element>>,
    /// The frame arena (create/destroy + show/hide/strata/level/scale/alpha propagation).
    pub(crate) arena: WidgetArena,
    /// Per-frame layout input (anchors/size/scale). Every live frame has one (created at
    /// `CreateFrame`); `SetPoint`/`SetSize`/… mutate it; `resolve` runs the graph over them.
    pub(crate) layout_inputs: HashMap<FrameHandle, LayoutInput>,
    /// The last [`UiScript::resolve`] result: each resolvable frame's rect. Empty until `resolve`.
    pub(crate) resolved: HashMap<FrameHandle, Rect>,
    /// The anchor-graph solver, kept alive across resolves so its per-handle arrays and the
    /// per-frame anchor `Vec`s are reused — a steady round allocates nothing (see
    /// [`LayoutSolver`]). Scratch only: it holds no state between rounds that `begin` doesn't
    /// clear, and `resolved`/`region_resolved` remain the model's own answer.
    pub(crate) solver: LayoutSolver,
    /// The fingerprint of the input set the last CONVERGED resolve ran against — the change gate
    /// (see `script::layout::InputFingerprint`). `None` forces the next resolve to run: the
    /// initial state, and what a non-converging (cyclic) pass leaves behind so its progress can
    /// carry into the next frame.
    pub(crate) layout_fingerprint: Option<super::layout::InputFingerprint>,
    /// The layout mutation epoch — tier 1 of the resolve change gate (the fingerprint is tier 2).
    /// Bumped by [`Model::touch_layout`] at every write path that can move the anchor solve's
    /// read set; `resolve` returns immediately while it still equals [`Self::layout_epoch_resolved`],
    /// paying a `u64` compare instead of fingerprinting ~2k inputs. Tier 2 remains the correctness
    /// backstop for a write that bumps this without moving anything; it is not a licence to bump
    /// per frame, because *reaching* it costs the whole-UI hash. The bag-hover re-enter loop used
    /// to do exactly that (~0.65 ms/frame at solves=0) until its wrap-pin round trip was made
    /// change-gated — `tooltip::append_line`, and the gate
    /// `layout_gate::the_hover_re_enter_loop_neither_re_measures_nor_re_solves`.
    pub(crate) layout_epoch: u64,
    /// [`Self::layout_epoch`]'s **precise** form: the nodes a write actually NAMED since the last
    /// converged resolve — tier 1 promoted from "something moved" to "*these* moved" (decision
    /// 1388).
    ///
    /// `Some(nodes)` carries a second, stronger claim than its contents: **every** write since the
    /// cached graph was built named its node *and* left the graph's SHAPE alone, so
    /// [`super::layout::LayoutScope`]'s roster, edges and per-node hashes still describe the live
    /// model and a resolve may seed the dirty closure straight from this list. `None` is the
    /// conservative state — a write that could not name its node, or one that moved the roster or
    /// an anchor's TARGET — and forces the next resolve to derive the graph from scratch, which is
    /// exactly what every resolve did before 1388.
    ///
    /// That asymmetry is the safety argument. A site that keeps calling plain
    /// [`Model::touch_layout`] is *correct by construction* (it lands in `None`, i.e. today's
    /// behaviour); only a site that opts into [`Model::touch_layout_region`] /
    /// [`Model::touch_layout_frame`] can be wrong, and `WOW_LAYOUT_VERIFY` re-resolves every
    /// incremental pass in full scope and compares rects, so being wrong fails a test rather than
    /// shipping a stale one.
    /// The list holds **layout ids**, not handles: the touch sites below already have to prove the
    /// node is in the cached roster before they may name it, and that proof is a `node_of[id]`
    /// probe — which hands back the roster row, and with it the handle, for free at resolve time.
    pub(crate) layout_touched: Option<Vec<u32>>,
    /// The MEASURE lane's ledger — [`Self::layout_touched`]'s twin for the text-measure sweep
    /// (`UiScript::fontstrings_needing_measure`), which used to walk every `region_data` row
    /// twice a frame to *discover* silent writes (`SetText` touches no layout until the extent
    /// moves — that silence is the lane's defining property, decision 1370's ~300 µs standing
    /// `resolve` lap at a city pin). `Some(handles)` = every write since the last sweep that
    /// could move a region's measure key ([`crate::script::types::RegionData::measure_key`]:
    /// text, font path/height, text height, outline, wrap width, owner scale) named its region
    /// here, so the sweep asks exactly these. `None` = some write could not name one (a
    /// font-object fan-out, a scale propagation, `invalidate_text_measures`) and the next sweep
    /// walks the whole roster exactly as it always did — which is also the initial state.
    ///
    /// UNLIKE the layout ledger, `None` is not re-entered by unmigrated sites automatically:
    /// nothing at a `region_data` field write *had* to announce itself before this existed, so
    /// completeness of the enrolled sites is the correctness claim — and it is machine-checked,
    /// not argued: with `WOW_LAYOUT_VERIFY` on (forced for every benilla-ui test) the sweep
    /// re-runs the whole-roster walk beside the drained ledger and panics on any row the ledger
    /// missed. A candidate the ledger names that the roster walk doesn't is fine (a same-value
    /// write — the staleness check absorbs it); the reverse is a stale label waiting to ship.
    pub(crate) measure_dirty: Option<Vec<crate::widget::RegionHandle>>,
    /// The message-line sweep's clean tokens: frame → (lines_gen, env hash) at its last
    /// ZERO-REQUEST sweep. A frame matching both is skipped whole (`message_lines_needing_measure`
    /// hashes none of its lines); a sweep that produced requests stores nothing, so an unanswered
    /// request re-requests forever exactly like the region lane's ledger (1410's rule). Stale
    /// entries for destroyed frames are inert (generational handles never re-match) and the map
    /// holds a handful of message frames, so no destroy hook.
    pub(crate) msg_swept: std::collections::HashMap<crate::widget::FrameHandle, (u64, u64)>,
    /// Lines actually hashed by the sweep — the skip's honest meter (tests assert 0 on a settled
    /// second sweep; 0735's counts-never-milliseconds rule).
    pub(crate) msg_lines_hashed: u64,
    /// `WOW_LAYOUT_VERIFY`'s flag that the resolve just taken was the incremental one and owes a
    /// full-derive re-run to prove it (see `layout::UiScript::resolve_layout`). Always `false` in
    /// production — nothing reads it unless the verify build set it.
    pub(crate) layout_verify_recheck: bool,
    /// Per-node input hashes + the dirty-closure scratch — what makes a resolve cost the nodes
    /// that MOVED rather than the whole graph (decision 1350; see `script::layout::LayoutScope`).
    /// Tiers 1 and 2 above decide *whether* to solve; this decides *what*.
    pub(crate) layout_scope: super::layout::LayoutScope,
    /// The [`Self::layout_epoch`] value the last CONVERGED resolve closed on. `None` forces the
    /// next resolve through tier 1: the initial state, a cycle-bailed pass, and
    /// [`super::UiScript::force_full_layout_resolve`].
    ///
    /// **Every converged resolve closes tier 1** (decision 1385) — a real solve as much as a
    /// skipping one. It could not, while the fingerprint was hashed over the 0294 seeds as well
    /// as the inputs: a solve outgrew the value it had just stored, so a mutated frame paid three
    /// whole-roster walks (solve, settle, skip) instead of one. Hashing inputs alone makes the
    /// stored fingerprint exactly the one the next mutation-free resolve recomputes, so a
    /// per-frame layout write — the castbar's spark, any addon that animates a region — costs one.
    pub(crate) layout_epoch_resolved: Option<u64>,
    /// How many times the change gate has LET A RESOLVE THROUGH. `UiScript::resolve` is called
    /// unconditionally every frame, so the gap between call count and this is the gate's whole
    /// value; it is also what lets a test assert that a no-op resolve really was one, rather than
    /// merely producing the same rects. Counts the gate's decision, so it reads the same with the
    /// `WOW_LAYOUT_VERIFY` self-check on (which re-runs skipped resolves) as with it off.
    pub(crate) layout_solves: u64,
    /// How many times a resolve got **past tier 1** — the count of whole-roster preamble WALKS
    /// (decision 1385). This, not [`Self::layout_solves`], is the honest cost counter for the
    /// gate: a walk that ends in `gate_skips` still rebuilt the ids/plan, re-synced every frame's
    /// scale and re-hashed all ~10k anchored regions to conclude "nothing moved" — ~1.0 ms at the
    /// Stormwind pin — and `layout_solves` cannot see it, because it counts only the walks that
    /// went on to solve.
    ///
    /// The castbar pin is why it exists: one moving spark cost **three** walks per frame and only
    /// two of them were solves, so the counter the tests asserted on under-reported the bug by a
    /// third. `resolve_bench::a_region_moving_every_frame_costs_one_gate_walk_on_the_shipped_ui`
    /// is the guard that reads it.
    ///
    /// **Unlike [`Self::layout_solves`], this is NOT verify-independent.** Under
    /// `layout_verify_enabled` a tier-1-clean resolve falls through and walks anyway, so a verify
    /// build counts walks production never pays — which is the honest reading (the walk happened)
    /// but makes the number useless as an assertion there. Assert on it only from a crate that
    /// consumes `benilla-ui` as a dependency, where `cfg!(test)` is false and the count is
    /// production's; inside this crate's own tests, assert on `layout_solves`.
    pub(crate) layout_gate_walks: u64,
    /// How many times a resolve **derived the layout graph** — the whole-roster walk that rebuilds
    /// the frame/region roster, the reverse edges and every per-node hash (decision 1388).
    ///
    /// This is the cost counter [`Self::layout_gate_walks`] used to be. 1385 got a moving castbar
    /// spark down from three walks per frame to one; 1388 made that one walk *cheap* by seeding the
    /// dirty closure from a ledger of named nodes instead of rediscovering it, so "walks" no longer
    /// separates the 1.48 ms case from the 20 µs one and this does. A UI that animates anything
    /// should hold this at **zero** frame after frame — a non-zero steady-state reading means some
    /// write site is falling back to the conservative `touch_layout`, and that is the regression.
    ///
    /// Verify-independent in the direction that matters: the `WOW_LAYOUT_VERIFY` re-run's
    /// derivation is counted, but `resolve_layout` restores the meter afterwards, so a test reads
    /// the same number in both modes.
    pub(crate) layout_derives: u64,
    /// The SCOPE of the last solve that ran: `(frames solved, regions swept)` — decision 1350's
    /// own meter, and the number its gate asserts on. `layout_solves` says how often the gate let
    /// a solve through and `layout_rounds` how deep each went; this says how WIDE, which is the
    /// axis that used to be "the whole UI" and is the one a growing UI silently inflates.
    pub(crate) layout_last_scope: (usize, usize),
    /// How many fixpoint ROUNDS those solves ran in total. A solve's cost is rounds × the whole
    /// graph, so this is what separates "the gate let too much through" from "each pass is doing
    /// too much" — the two have different fixes.
    pub(crate) layout_rounds: u64,
    /// Hyperlink spans per frame — `(rect, link payload, full |H…|h markup)`, rects in the
    /// engine's y-up screen space. Fed by the app each frame after it rasterizes message lines
    /// ([`super::UiScript::set_link_spans`]); consumed by the pointer's release dispatch
    /// (`OnHyperlinkClick` — decision 0288 P2).
    pub(crate) link_spans: HashMap<FrameHandle, Vec<(Rect, String, String)>>,
    /// Tab was pressed in the chat edit box since the last drain (`BenillaChatTabPressed` →
    /// [`super::UiScript::take_chat_tab`]) — the app's whisper-target cycle reads it.
    pub(crate) chat_tab: bool,
    /// Region visuals (texture path/color/text) + region layout (anchors/size/justify).
    pub(crate) region_data: HashMap<RegionHandle, RegionData>,
    /// Per-frame installed [`Backdrop`] (the tooltip/dialog/panel plate — `<Backdrop>` or
    /// `SetBackdrop`). Absent ⇒ the frame draws no plate. Stored beside the arena like `region_data`
    /// (the arena models structure, not paint). The client's `frame+0x1ac` pointer.
    pub(crate) backdrops: HashMap<FrameHandle, backdrop::Backdrop>,
    /// Per-`SimpleHTML` widget state (decision-free transcription of `CSimpleHTML`'s own members):
    /// the four element fonts `+0x350`, the hyperlink format `+0x360`, and the CONTENTNODE list
    /// `+0x340` of blocks the last `SetText` built. Stored beside the arena, like `backdrops`,
    /// because its contents are script-layer types and `widget::kinds` is the layer below.
    pub(crate) simple_html: simplehtml::SimpleHtmlStates,
    /// The named virtual **Font object** registry (`<Font name=…>` → resolved paint), keyed by name.
    /// Populated by the loader as it walks top-level `<Font>` nodes; read by `SetFontObject` and by
    /// FontString `inherits=` resolution. Data only — no Lua handles (MAXCSTACK discipline).
    /// The named `<Font>` objects, **keyed by the ASCII-LOWERCASED name**.
    ///
    /// 1.12's font registry hashes the name and compares keys with `SStrCmpI` — case-INSENSITIVE
    /// (`0x783870`/`0x7838c7`, wow-re `system/ui/scratch/font-object-lua-surface.md`: *"Font names
    /// are matched case-insensitively"*), and a name string handed to `SetFontObject` folds the
    /// same way. `Recap/RecapOptions.xml:32` inherits `GameFontHighLightSmall` — the shipped font
    /// is `GameFontHighlightSmall`, one letter's case apart — and on the real client that resolves.
    ///
    /// Read it through [`Model::font_object`], never directly: that is where the fold lives, and
    /// the name says so because nothing else guards the invariant (1247's shape).
    pub(crate) font_objects_by_lower: HashMap<String, FontObject>,
    /// The last [`UiScript::resolve`] result for **anchored** regions (those with a non-empty
    /// [`RegionData::anchors`]): the region's own resolved rect, owner-relative (see [`extract`]).
    /// Regions with no anchors are absent here — they fall to the size-centered / fill-owner path.
    pub(crate) region_resolved: HashMap<RegionHandle, Rect>,

    /// Monotonic id source (starts at 1; `0` is [`SCREEN`]).
    pub(crate) next_id: u32,
    pub(crate) id_to_frame: HashMap<u32, FrameHandle>,
    pub(crate) frame_to_id: HashMap<FrameHandle, u32>,
    pub(crate) id_to_region: HashMap<u32, RegionHandle>,
    pub(crate) region_to_id: HashMap<RegionHandle, u32>,
    /// Region name → layout id — the region twin of the arena's frame-name publish (same
    /// non-overwriting first-wins rule). `SetPoint`'s string `relativeTo` resolves through frames
    /// first, then here — the real client anchors regions to *sibling regions* by name everywhere
    /// (e.g. the merchant label plate to its `$parentSlot` texture), which owner-fallback silently
    /// mis-anchored before (the jutting-plates bug the director's A/B caught).
    pub(crate) region_names: HashMap<String, u32>,

    /// Which handler kinds each frame has a script for (presence mirror; the closures live Lua-side
    /// in the `REG_SCRIPTS` table). Lets `tick` find OnUpdate frames without scanning Lua.
    pub(crate) scripts: HashMap<FrameHandle, HashSet<&'static str>>,
    /// The frames carrying an `OnUpdate` script, maintained by `SetScript` — `scripts`' one
    /// writer — so the tick iterates exactly the OnUpdate population instead of re-filtering
    /// (and re-allocating from) the whole scripts map every frame (decision 1446). Order is
    /// irrelevant here: the tick sorts its visible subset by frame id every pass (the
    /// deterministic-dispatch law stays where it was).
    pub(crate) on_update_frames: Vec<FrameHandle>,
    /// The frames carrying an `OnSizeChanged` script — 1446's list, for the layout resolve's
    /// "before" snapshot instead of the tick. Same one writer, same reason: that snapshot's own
    /// comment says it covers "a handful, usually zero", and it found them by filtering the WHOLE
    /// `scripts` map with a string compare per entry, on every let-through resolve — 36–72 µs a
    /// resolve at a 3,988-frame roster, which made it the biggest phase left in the preamble
    /// (decision 1634's `[layout-pre] watched=`).
    pub(crate) on_size_changed_frames: Vec<FrameHandle>,
    /// `event name → frames registered for it` (RegisterEvent), in **registration order** — an
    /// ordered Vec, never a set: the client's `SignalEvent 0x703e50` walks a per-event listener
    /// LIST, so cross-frame dispatch order is a law, not an accident (the abbey territory-line
    /// bug: two ZoneText frames write the same FontString on one event — last writer decides).
    /// Re-registering is a no-op (position kept); Unregister removes. Data only — no Lua handles.
    pub(crate) event_to_frames: HashMap<String, Vec<FrameHandle>>,
    /// `frame → its registered events` (for UnregisterEvent / cleanup).
    pub(crate) frame_events: HashMap<FrameHandle, HashSet<String>>,

    /// The EditBox that currently owns keyboard focus — the engine's twin of the client's
    /// class-owned focus global `DAT_00cf4dc8` (`CSimpleEditBox* E`, 0 = none; RF-0082 §1). A focused
    /// box consumes every key/char; `None` lets an `autoFocus` box self-acquire on the first event.
    /// Gated on effective-visibility at read time (a box hidden while focused stops taking input).
    pub(crate) focused_editbox: Option<FrameHandle>,

    /// The frame currently under the cursor (the last [`UiScript::mouse_move`] capture), so the next
    /// move knows whether to fire `OnLeave`/`OnEnter`. `None` = cursor over no mouse-enabled frame.
    pub(crate) mouseover: Option<FrameHandle>,
    /// The hover **re-pick** is due: the world under a stationary cursor changed — the hovered
    /// frame hid (its `OnLeave` already fired at hide time), or a frame was shown that may now
    /// be the topmost under the cursor. The reference keeps exactly this flag on the frame
    /// manager (`[root+0x1100]`) and its per-tick pump re-runs the hover walk at the **saved**
    /// cursor position, explicitly bypassing the didn't-move coalesce — so the newly exposed
    /// frame gets `OnEnter` with no physical mouse move (wow-re
    /// `ui/scratch/hover-hide-and-tooltip-owner-law.md`: writers `0x764cbb`/`0x764b8d`, pump
    /// tail `0x7657a1` → `0x7660d0` self-alias). [`UiScript::tick`] drains it the same way.
    pub(crate) hover_repick: bool,
    /// Per-button, the frame a mouse-down last captured (`button name → frame`), for the `OnClick`
    /// same-frame press+release test in [`UiScript::mouse_button`]. Keyed by button so a `LeftButton`
    /// press is not cleared by a `RightButton` release.
    pub(crate) mouse_down_on: HashMap<String, FrameHandle>,
    /// The client's single mouse-capture slot **`root+0x80`** — the frame a press captured, held
    /// until every button is up. The same fact as [`Model::mouse_down_on`] in the one-slot form,
    /// and the two differ only while more than one button is held; this is the form the
    /// mouse-down **raise** reads, which is what needs the precedence to be unambiguous.
    ///
    /// Written at `0x7663e6` with the resolved target — capture-**else**-hover, so an existing
    /// capture survives a chorded press over a different frame — and cleared at `0x7664bb` only
    /// when the post-event button mask is empty, so a chorded release keeps it. A title-region
    /// press never reaches `0x7663e6` and so captures nothing.
    pub(crate) mouse_capture: Option<FrameHandle>,
    /// Per **frame**, the [`UiScript::now`] second at which its last single `OnClick` fired — the
    /// double-click detector's entire state, and the faithful stand-in for the client's per-widget
    /// timestamp `[CButton+0x334]` (wow-re `ui/scratch/button-doubleclick-law.md`).
    ///
    /// Three properties of that field are load-bearing, and are why this is keyed the way it is.
    /// It lives on the **widget**, so two frames can never pair with each other. It carries **no
    /// button identity** — the detector is button-agnostic, so a left release followed by a right
    /// one on a button registered for both completes a double click (with `arg1` = the *second*
    /// button); what normally confines double-clicks to the left button is `RegisterForClicks`'
    /// `{"LeftButtonUp"}` default, not the detector. And it is written only when a single click
    /// actually fires, then **zeroed** when a double completes — which is what makes four rapid
    /// clicks read `Click · DoubleClick · Click · DoubleClick` rather than one click and three
    /// doubles. Nothing else clears it: not hide, not disable, not the cursor leaving the frame
    /// (`[+0x334]` has exactly three writers binary-wide — ctor, the fired-double zero, and the
    /// fired-single stamp).
    pub(crate) last_click: HashMap<FrameHandle, f64>,

    /// Frames whose resolved SIZE moved in the last [`UiScript::resolve_layout`], as
    /// `(frame id, width, height)` — queued there (it holds only a `&mut Model`, so it cannot call
    /// Lua) and drained by [`super::event::fire_size_changes`] at the next `&Lua` seam, which turns
    /// each into `OnSizeChanged(self, width, height)`. Same compute-under-borrow / fire-outside
    /// shape as the pointer path's `OnValueChanged`/`OnDragStart` hand-offs.
    pub(crate) pending_size_changed: Vec<(u32, f32, f32)>,

    /// Script errors collected from `pcall`'d handlers (never panics, never prints — decision 0068).
    pub(crate) errors: Vec<String>,
    /// Script errors awaiting dispatch to the **Lua-side** error handler (decision 1305) — the
    /// reference calls `geterrorhandler()`'s function on every caught script error, which is how
    /// FrameXML's `_ERRORMESSAGE` puts the red ScriptErrors dialog on the player's screen. Every
    /// message recorded through [`Model::record_script_error`] lands here as well as in `errors`;
    /// [`super::UiScript::dispatch_script_errors_to_handler`] drains it at a safe seam (never from
    /// inside the failed call). A message produced *by* the handler itself goes to `errors` only —
    /// that asymmetry is the recursion guard.
    pub(crate) pending_error_dispatch: Vec<String>,
    /// The **retained** script-error log — what [`errors`](Self::errors) and the load walk both
    /// throw away once logged (decision 1495). `errors` is a per-frame drain for the host's
    /// terminal; this is the session's memory, deduplicated and bounded, and the only thing a
    /// player can actually read. See [`super::diagnostics`] for why the reference's own dialog is
    /// not enough on its own.
    pub(crate) diagnostics: super::diagnostics::DiagnosticLog,
    /// Non-fatal warnings surfaced to the host (e.g. `CreateFrame`'s ignored `inherits=` template).
    pub(crate) warnings: Vec<String>,
    /// The screen-root rect (`[bottom, left, top, right]`), the anchor base for top-level frames.
    pub(crate) screen: Rect,

    /// The per-unit-token game-state snapshot the app pushes in each frame (`"player"`, `"target"`,
    /// …), read by the `Unit*` Lua bindings ([`unit`]). Plain data — the engine never touches the
    /// ECS/net; the app's feed writes here via [`UiScript::set_unit`] (decision 0068 §3). A token
    /// absent from the map is a non-existent unit (`UnitExists` false, numbers `0`/nil).
    ///
    /// **Keyed by the ASCII-LOWERCASED token, and the name says so because the invariant has no
    /// other guard.** 1.12's shared token resolver `0x515970` compares every literal with
    /// `SStrCmpI` → `_strnicmp`, which folds `'A'..'Z'` by `+0x20`; not one of its ten compares
    /// reaches the case-sensitive sibling. So `"Player"`, `"PLAYER"` and `"player"` are the same
    /// unit on the real client, and ~10 corpus addons rely on it — `Accountant.lua:107`'s
    /// `UnitFactionGroup("Player")` is the one that cost an addon its session.
    ///
    /// Read it through [`Model::unit`], never directly: that is where the fold lives. The field was
    /// called `units` until the fold landed, and it was renamed precisely so that every existing
    /// reader had to come through here rather than be trusted to remember (wow-re
    /// `system/ui/scratch/unit-token-grammar.md`).
    pub(crate) units_by_lower: HashMap<String, UnitState>,
    /// Per-unit-token aura list, **in display order**, pushed by the app's aura feed each frame and
    /// read by the `UnitAura` family ([`super::aura`]). The order is the app's decision, not the
    /// engine's: the local player's is a maintained insertion-order cache, every other unit's is
    /// ascending aura slot (decision 0257). A token absent here has no auras.
    pub(crate) auras: HashMap<String, Vec<AuraState>>,
    /// Spell ids `CancelUnitBuff` queued since the app's last drain — one `CMSG_CANCEL_AURA` each.
    pub(crate) cancel_aura_requests: Vec<u32>,
    /// The player's active tracking aura (the reference's `GetTrackingTexture` global — see
    /// [`super::aura::TrackingState`]), pushed by the app's aura feed beside `auras`. `None` =
    /// no tracking active.
    pub(crate) tracking: Option<super::aura::TrackingState>,
    /// Unit tokens `TargetUnit` queued since the app's last [`UiScript::take_target_requests`] drain
    /// — the outbound twin of `units` above (the reference's `TargetUnit` Lua shim → SetSelection).
    /// The app resolves each token to a streamed entity and commits the selection; a token it can't
    /// resolve is a no-op, as the real client no-ops `TargetUnit` on a nonexistent unit ([`unit`]).
    pub(crate) target_requests: Vec<String>,
    /// `(name, exactMatch)` pairs `TargetByName` queued since the app's last
    /// [`UiScript::take_target_by_name_requests`] drain — the by-NAME twin of
    /// [`Self::target_requests`], which takes unit tokens. The app runs the shared by-name
    /// resolver (`crate::target::by_name`, decision 0886) and commits the selection ([`unit`]).
    pub(crate) target_by_name_requests: Vec<(String, bool)>,
    /// Set when `ClearTarget()` fired with a live target — the ESC chain's LAST leg
    /// (`ToggleGameMenu`'s order: the clear runs only when nothing earlier ate the press).
    /// Drained by [`super::UiScript::take_target_clear`]; the app commits the deselect ([`unit`]).
    pub(crate) target_clear: bool,
    /// Unit tokens `DropItemOnUnit` queued since the app's last
    /// [`UiScript::take_drop_item_on_unit`] drain — the cursor's held item being dropped onto a
    /// unit (`0x48d960`). The binding queues the token only; **every gate is the app's**, because
    /// all of them read state the VM does not hold (the pet's owner fields, the learned Feed-Pet
    /// spell, the held item's guid). A token the app refuses is silent and keeps the payload,
    /// which is the reference's own behaviour ([`super::cursor`]).
    pub(crate) drop_item_on_unit: Vec<String>,

    /// The party/raid roster snapshot the app pushes (`GroupState`'s merged view, decision 0434
    /// §2) — `GetNumPartyMembers`/`GetPartyLeaderIndex`/`GetLootMethod`/… read it ([`party`]).
    /// `PartyState::default()` = not in a group (every getter reports the solo-player shape).
    /// Per-member game state rides the `units` map above, under `"party1"`..`"party4"` tokens.
    /// The channels this session has CONFIRMED joining, in join order — the client-side number
    /// law `GetChannelName` answers with ([`super::channel`]). Mirrored from `ui_chat`'s
    /// `ChannelState` by `set_joined_channels`; empty until the server's first YOU_JOINED.
    pub(crate) joined_channels: Vec<Option<String>>,
    pub(crate) party: party::PartyState,
    /// Party/loot intents (`AcceptGroup`/`InviteToParty`/`SetLootMethod`/…) queued since the
    /// app's last [`super::UiScript::take_party_requests`] drain — the outbound seam ([`party`]).
    pub(crate) party_requests: Vec<party::PartyRequest>,
    /// The saved raid lockouts the Raid tab's info panel reads (decision 1549), pushed by the app
    /// from `SMSG_RAID_INSTANCE_INFO`. Its own field rather than a [`party::PartyState`] member
    /// because it arrives on its own packet and survives every roster change.
    pub(crate) saved_instances: Vec<party::SavedInstanceInfo>,
    /// `SetRaidRosterSelection`'s cursor — a client-side raid row index, never sent anywhere.
    pub(crate) raid_selection: i64,
    /// The social snapshot the app pushes (friends, ignores, the last `/who` — decision 0668):
    /// `GetNumFriends`/`GetFriendInfo`/`GetWhoInfo`/… read it ([`social`]). Already
    /// display-resolved (names, class/zone names) because the reference resolves them
    /// engine-side too.
    pub(crate) social: social::SocialState,
    /// Social intents (`AddFriend`/`RemoveFriend`/`SendWho`/…) queued since the app's last
    /// [`super::UiScript::take_social_requests`] drain — the outbound seam ([`social`]).
    pub(crate) social_requests: Vec<social::SocialRequest>,
    /// The guild snapshot the app pushes (roster, ranks, MOTD, info text — decision 1257):
    /// `GetNumGuildMembers`/`GetGuildRosterInfo`/`GuildControlGetRankFlags`/… read it
    /// ([`guild`]). Already display-resolved and already sorted + filtered, because the sort
    /// field and the show-offline toggle live app-side where the roster does.
    pub(crate) guild: guild::GuildState,
    /// The rank-control popup's staging buffer ([`guild::GuildRankEdit`]) — deliberately NOT part
    /// of [`Self::guild`], because a snapshot push mid-edit would discard the user's unsaved
    /// checkbox clicks. Flushed by `GuildControlSaveRank`.
    pub(crate) guild_control: guild::GuildRankEdit,
    /// Guild intents (`GuildInviteByName`/`GuildSetMOTD`/`GuildControlSaveRank`/…) queued since
    /// the app's last [`super::UiScript::take_guild_requests`] drain — the outbound seam
    /// ([`guild`]).
    pub(crate) guild_requests: Vec<guild::GuildRequest>,
    /// The guild-charter snapshot the app pushes (the registrar's price and the open petition —
    /// decision 1672): `GetPetitionInfo`/`GetPetitionNameInfo`/`GetGuildCharterCost`/… read it
    /// ([`petition`]). Names and the proposed guild's title are already resolved, because in the
    /// real client too they come from caches the engine owns and not from the packet that opens
    /// the window.
    pub(crate) petition: petition::PetitionState,
    /// Charter intents (`BuyGuildCharter`/`SignPetition`/`TurnInGuildCharter`/…) queued since the
    /// app's last [`super::UiScript::take_petition_requests`] drain — the outbound seam
    /// ([`petition`]).
    pub(crate) petition_requests: Vec<petition::PetitionRequest>,
    /// Whisper targets `ChatFrame_SendTell` queued since the app's last
    /// [`super::UiScript::take_tell_requests`] drain — the app opens its chat edit box prefilled
    /// `/w <name> ` (the unit popup's WHISPER action; [`party`] registers the global).
    pub(crate) tell_requests: Vec<String>,
    /// Prefill texts `ChatFrame_OpenChat` queued since the app's last
    /// [`super::UiScript::take_open_chat_requests`] drain — `tell_requests`' sibling, one step
    /// less resolved: a name there, a whole draft line here ([`chat_window`] registers the global).
    pub(crate) open_chat_requests: Vec<String>,
    /// Per-chat-window **look** — background tint, background alpha, font size — index 0 =
    /// `ChatFrame1` (decision 1589, [`chat_window`]). The engine's own per-window record narrowed
    /// to the three fields the tab menu's Display block can move; the host seeds it from the
    /// player's saved file and persists what Lua writes back.
    pub(crate) chat_window_looks: [chat_window::ChatWindowLook; chat_window::NUM_CHAT_WINDOWS],
    /// The 0-based window indices whose look Lua moved since the app's last
    /// [`super::UiScript::take_chat_window_changes`] drain — the persist cue. A set, so a slider
    /// drag costs one entry however many steps it took.
    pub(crate) chat_window_changes: HashSet<usize>,
    /// Whether a **user-placed** frame's geometry moved since the app's last
    /// [`super::UiScript::take_user_placed_change`] drain — the layout cache's persist cue
    /// ([`super::layout_cache`]). One bit rather than a set of handles, because the file is
    /// rewritten whole either way; set by the move/resize pumps and by `SetUserPlaced` itself.
    pub(crate) user_placed_changed: bool,
    /// The player's default chat language name, app-resolved from `ChrRaces.BaseLanguage` ×
    /// `Languages.dbc` ([`super::UiScript::set_default_language`]). `None` = the reference's
    /// no-player-object state, where `GetDefaultLanguage()` returns **zero Lua values**
    /// ([`chat_send`]).
    pub(crate) default_language: Option<String>,
    /// Duel intents (`AcceptDuel`/`CancelDuel`/`StartDuel*`) queued since the app's last
    /// [`super::UiScript::take_duel_requests`] drain — the outbound seam ([`duel`]). There is no
    /// duel *snapshot* beside it: everything the UI reads arrives as event arguments.
    pub(crate) duel_requests: Vec<duel::DuelRequest>,
    /// Follow intents (`FollowUnit`/`FollowByName`) queued since the app's last
    /// [`super::UiScript::take_follow_requests`] drain — the outbound seam ([`follow`]). The duel
    /// queue's twin, snapshot-less for the same reason: the only thing the UI reads back is the
    /// `AUTOFOLLOW_BEGIN`/`AUTOFOLLOW_END` pair the app fires, which carries its own argument.
    pub(crate) follow_requests: Vec<follow::FollowRequest>,
    /// Session-exit intents (`Logout`/`Quit`/`CancelLogout`/`ForceQuit`) queued since the app's
    /// last [`super::UiScript::take_session_requests`] drain — the outbound seam ([`session`]).
    /// Snapshot-less like the duel queue above: what the UI reads back (the camp/quit countdown)
    /// arrives as the `PLAYER_CAMPING`/`PLAYER_QUITING`/`LOGOUT_CANCEL` events, not as state.
    pub(crate) session_requests: Vec<session::SessionRequest>,
    /// PvP-flag toggles (`TogglePVP`) queued since the app's last
    /// [`super::UiScript::take_pvp_toggles`] drain — the outbound seam ([`pvp`]). A count, not a
    /// payload: `CMSG_TOGGLE_PVP` carries no body.
    pub(crate) pvp_toggles: u32,
    /// The local player's honor snapshot ([`pvp::HonorState`], decision 1512) — the PRIVATE honor
    /// descriptor fields the app decodes and pushes ([`super::UiScript::set_honor`]). `None`
    /// before the first push, which the six self getters read as a zeroed snapshot (see
    /// [`pvp`]'s module doc for why they never return short).
    pub(crate) honor: Option<pvp::HonorState>,
    /// The last `MSG_INSPECT_HONOR_STATS` reply the app pushed ([`pvp::InspectHonorData`]). Its
    /// PRESENCE is `HasInspectHonorData`'s answer — the latch the reference's inspect pane gates
    /// its request on — so `None` is a state, not a gap.
    pub(crate) inspect_honor: Option<pvp::InspectHonorData>,
    /// `RequestInspectHonorData` calls since the app's last
    /// [`super::UiScript::take_inspect_honor_requests`] drain. A count for [`Self::pvp_toggles`]'s
    /// reason: the binding takes no argument, and the guid the send needs is the app's own inspect
    /// target. With [`Self::inspect_honor_pending`] gating the binding it can only ever drain `0`
    /// or `1` — the engine has at most one query outstanding.
    pub(crate) inspect_honor_requests: u32,
    /// The engine's `pending` flag (`0xb71fcc`): a `MSG_INSPECT_HONOR_STATS` query is queued and
    /// no reply has landed. `RequestInspectHonorData` (`0x4c80a0`) refuses while it is set, which
    /// is what stops a pane shown/hidden/shown in quick succession sending duplicate queries.
    /// Cleared by **either** arm of [`super::UiScript::set_inspect_honor`] — a reply landed
    /// (`0x4c6f4c`), or the app invalidated the slot (`0x4c6f9d`).
    pub(crate) inspect_honor_pending: bool,
    /// The two **equipment-display** preferences as the VM currently believes them —
    /// `ShowingHelm()` / `ShowingCloak()` ([`worn_display`], decision 1472). Not a setting: the
    /// truth is `PLAYER_FLAGS`' `HIDE_HELM`/`HIDE_CLOAK` bits, pushed here on the descriptor edge
    /// by [`super::UiScript::set_worn_display`]. Both start **shown**, which is both the reference's
    /// panel default and the zero-flags state a fresh character logs in with.
    pub(crate) helm_shown: bool,
    /// The cloak half of [`Self::helm_shown`].
    pub(crate) cloak_shown: bool,
    /// Worn-display flips (`ShowHelm`/`ShowCloak` asked for a state we were not in) queued since
    /// the app's last [`super::UiScript::take_worn_display_toggles`] drain — one
    /// `CMSG_TOGGLE_HELM`/`CMSG_TOGGLE_CLOAK` each. A list, not a count like [`Self::pvp_toggles`]:
    /// the two slots are different packets.
    pub(crate) worn_display_toggles: Vec<super::worn_display::WornDisplay>,

    /// The server's `PLAYER_FIELD_BYTES` **byte 2** — which of the four extra action bars are on
    /// ([`action_bar_toggles`], wow-re `system/ui/scratch/action-bar-toggles.md`). `None` until the
    /// app pushes it ([`super::UiScript::set_action_bar_toggles`]), which is the ONLY thing that
    /// moves it: the real client never writes this cell, so `SetActionBarToggles` deliberately
    /// leaves it alone and the value lags the setter by a round trip. That is the opposite
    /// arrangement to [`Self::helm_shown`] next door, and the difference is the wire verb's — a
    /// blind flip has to be predicted, an absolute post does not.
    pub(crate) action_bar_toggles: Option<u8>,
    /// `CMSG_SET_ACTIONBAR_TOGGLES` payloads queued since the app's last
    /// [`super::UiScript::take_action_bar_toggle_sends`] drain — one per `SetActionBarToggles`
    /// call. A list, because the binding gates nothing: two calls in a frame are two packets.
    pub(crate) action_bar_toggle_sends: Vec<u8>,

    /// Sounds queued by the Lua `PlaySound`/`PlaySoundFile` bindings since the app's last
    /// [`UiScript::take_sounds`] drain — the outbound Lua→app intent seam ([`sound`]).
    pub(crate) sound_queue: Vec<SoundRequest>,

    /// The CVar table (decision 0954, [`super::cvars`]): lowercase name → slot. Host-registered
    /// only; Lua reads/writes through `GetCVar`/`SetCVar`/`GetCVarDefault`.
    pub(crate) cvars: HashMap<String, super::cvars::CvarSlot>,
    /// The persisted values registration honors (decision 1291): lowercase name → the config
    /// file's value, set by the host **before** any registration. A CVar registered while its
    /// name is in here — host-registered or an addon's `RegisterCVar` — starts at the saved
    /// value, not the default. This is what makes a knobless CVar (`statusBarText`) and an
    /// addon-declared one survive the VM being replaced: in the reference the table is engine
    /// memory and outlives every `ReloadUI`; ours is per-VM, so the file value is the bridge.
    pub(crate) cvars_saved_base: HashMap<String, String>,
    /// `(registered name, new value)` per Lua `SetCVar` since the app's last
    /// [`super::UiScript::take_cvar_changes`] drain — the knob-sync + config-dirty cue.
    pub(crate) cvar_changes: Vec<(String, String)>,
    /// Unknown CVar names already warned about (warn-once, the era-atlas-miss posture).
    pub(crate) cvars_warned: HashSet<String>,

    /// The device-enumerated multisample formats the Video options dropdown offers, in the order
    /// it offers them — pushed by the host ([`super::UiScript::set_multisample_formats`]) from what
    /// the render adapter actually accepts, and read by `GetMultisampleFormats` /
    /// `GetCurrentMultisampleFormat` / `SetMultisampleFormat`.
    ///
    /// In the reference this is the distilled `{colorBits, depthBits, multisample}` triple list at
    /// `[0xb4b444]` (count `[0xb4b440]`), built by `0x48c3e0` from the D3D
    /// `CheckDeviceMultiSampleType` sweep or the GL `wglGetPixelFormatAttribivARB` sweep. Empty
    /// until the host pushes it — a VM with no device behind it (every extract test) offers no
    /// formats rather than inventing some.
    pub(crate) multisample_formats: Vec<super::cvars::MultisampleFormat>,

    /// The globals `RegisterForSave` declared, in registration order — the saved-variables set the
    /// host writes out at logout/exit and re-executes at load (decision 1128, [`super::saved`]).
    pub(crate) saved_names: Vec<String>,

    /// The key-binding table (decision 0997, [`super::keybind`]) — the chord→command store the
    /// Key Bindings window edits, plus its stored account/character sets. The CVar table's twin:
    /// host-registered commands, Lua reads/writes synchronously, the app re-derives dispatch when
    /// [`super::UiScript::keybinds_generation`] moves and persists on the queued save requests.
    pub(crate) keybinds: super::keybind::KeybindState,

    /// The action-slot snapshot (keyed by Lua action id 1..120) + the stance page offset the app
    /// pushes, and the `UseAction` intents it drains — the action seam ([`action`]).
    pub(crate) actions: HashMap<u32, ActionSlot>,
    /// The per-action **dynamic** state (usable/range/current/cooldown — decision 0137 phase 4),
    /// pushed beside `actions` by the app's feed; the `IsUsableAction`/`GetActionCooldown` family
    /// reads it ([`action`]). Keyed like `actions`; an absent action reads cold/nil.
    pub(crate) action_states: HashMap<u32, super::action::StoredActionState>,
    pub(crate) bonus_bar_offset: u8,
    pub(crate) action_uses: Vec<u32>,
    /// `(lua action id, packed)` pairs queued by `PickupAction`/`PlaceAction` (decision 0216 §7,
    /// slice 4) — one entry per local slot mutation, `packed == 0` clearing the slot. Drained by
    /// the app into `CMSG_SET_ACTION_BUTTON`, one send per entry (client-authoritative, per-change
    /// — 0218 §4: a drag-swap is two sends, never atomic).
    pub(crate) action_sets: Vec<(u32, u32)>,
    /// GlobalStrings keys for **client-local refusals the ENGINE raises** — the engine-free half
    /// of the app's `ui_action::UiErrorKeys`, which cannot reach in here. The reference does this
    /// inline (`push <errorId>; call CGGameUI::DisplayError 0x496720`) because its engine owns
    /// both the refusal and the message; ours splits at the crate boundary, so the refusal queues
    /// its key and the app's action feed resolves it against the VM's own GlobalStrings and fires
    /// `UI_ERROR_MESSAGE`. Always `&'static str`: every tenant is a literal read out of the ref's
    /// errorId table (`0xb4b498`, stride `0x14`), never runtime-built.
    pub(crate) ui_errors: Vec<&'static str>,

    /// The player's known-spell book (decision 0216 §8, slice 5) — tabs + the flat slot list the
    /// `GetSpellTabInfo`/`GetSpellName`/… bindings read ([`spellbook`]). Durable player state like
    /// `actions` above, never `Option` — "no known spells yet" is simply empty vectors.
    pub(crate) spellbook: spellbook::SpellBookState,
    /// The **pet's** book (decision 1032) — a second flat slot list, no tabs, with its own
    /// add-gate and its own class token ([`spellbook::PetBookState`]). Held apart from
    /// [`Self::spellbook`] because the reference holds two arrays too (`0xb700f0` / `0xb6f098`)
    /// and every `bookType`-taking binding is a fork between them, never a filter over one.
    pub(crate) pet_book: spellbook::PetBookState,
    /// The player's **macros** (decision 0983) — the one game-state table this crate owns
    /// outright, because 1.12 macros have no server side at all ([`macros`]'s module docs). The
    /// app seeds it from `benilla-config/macros/…` and reads it back to persist.
    pub(crate) macros: macros::MacroState,
    /// A script mutated [`Self::macros`] since the app's last
    /// [`super::UiScript::take_macros_dirty`] drain — the save + `UPDATE_MACROS` trigger.
    pub(crate) macros_dirty: bool,
    /// Bumped by every seed and every mutation — the *undrained* change signal per-frame
    /// consumers gate on ([`super::UiScript::macros_generation`]), so the drained
    /// [`Self::macros_dirty`] edge keeps its single owner.
    pub(crate) macros_generation: u64,
    /// The macro-icon chooser list (full texture paths, app-built off `SpellIcon.dbc`) —
    /// `GetNumMacroIcons`/`GetMacroIconInfo`.
    pub(crate) macro_icons: Vec<String>,
    /// Spell ids `CastSpell` queued since the app's last [`super::UiScript::take_spell_casts`]
    /// drain.
    pub(crate) spell_casts: Vec<u32>,
    /// Pet spell ids `CastSpell(id, "pet")` queued — a separate list because the wire verb is a
    /// separate opcode (`CMSG_PET_ACTION` with a synthesized type-1 word, `0x4b34ce`).
    pub(crate) pet_spell_casts: Vec<u32>,
    /// Pet spell ids `ToggleSpellAutocast` queued — `CMSG_PET_SPELL_AUTOCAST 0x2F3`, which names a
    /// spell rather than the pet bar's slot ([`Self::pet_autocast_toggles`] is the bar's).
    pub(crate) pet_spell_autocasts: Vec<u32>,
    /// Whether the app's own cast lifecycle holds something `SpellStopCasting()` can stop — a
    /// running auto-repeat or an in-flight cast; a channel is NOT stoppable there (wow-re
    /// `esc-stopcasting.md`; pushed each frame by the app's cast feed,
    /// [`super::UiScript::set_casting`]). The 1/nil return is load-bearing:
    /// `ToggleGameMenu`'s ESC chain (`UIParent.lua:1489`) only falls through to
    /// `CloseAllWindows()` on nil.
    pub(crate) casting: bool,
    /// Set when `SpellStopCasting()` fired while [`Self::casting`] — the ESC local-cancel
    /// trigger, drained by [`super::UiScript::take_spell_stop`] ([`spellbook`]).
    pub(crate) spell_stop: bool,
    /// Whether the app's spell-targeting cursor mode is active — the `flag_word != 0` mirror
    /// (`SpellIsTargeting 0x6e6cd0`, decision 0792). Pushed each frame by the app's targeting
    /// feed ([`super::UiScript::set_spell_targeting`]); read by `SpellIsTargeting()` and gating
    /// `SpellStopTargeting()`, whose 1/nil return the ESC chain's rung (`UIParent.lua:1490`)
    /// falls through on, exactly like [`Self::casting`]'s.
    pub(crate) spell_targeting: bool,
    /// Whether the standing targeting word could bind a UNIT — what `SpellCanTargetUnit` answers
    /// (`0x6e6d00` → `0x6e6460`'s unit leg). Pushed by the app beside `spell_targeting`.
    ///
    /// Always `false` today, and *derived* rather than hardcoded so it stops being false the moment
    /// that stops being true: benilla's targeting cursor models the location / item / gameobject
    /// words (0792/0923/0939), and no unit satisfies any of them — a unit-target spell never enters
    /// targeting mode at all, it resolves to `CastWireTarget::Unit` or refuses.
    pub(crate) spell_can_target_unit: bool,
    /// Set when `SpellStopTargeting()` fired while [`Self::spell_targeting`] — the ESC-chain
    /// targeting cancel, drained by [`super::UiScript::take_stop_targeting`] ([`spellbook`]).
    pub(crate) spell_stop_targeting: bool,

    /// The player's talent pages (decision 0304) — tabs + per-tab talents the
    /// `GetNumTalentTabs`/`GetTalentInfo`/… bindings read ([`super::talent`]). Durable player
    /// state like `spellbook`; "no talents yet" is simply empty.
    pub(crate) talents: super::talent::TalentUiState,
    /// `LearnTalent(tab, index)` clicks queued since the app's last
    /// [`super::UiScript::take_talent_learns`] drain.
    pub(crate) talent_learns: Vec<(u32, u32)>,
    /// `ConfirmTalentWipe()` calls queued since the app last drained them — each is one outbound
    /// `MSG_TALENT_WIPE_CONFIRM`. A COUNT for [`Self::binder_confirms`]'s reason: the app holds
    /// the trainer's guid, so the intent carries no payload of its own (decision 1580).
    pub(crate) talent_wipe_confirms: u32,
    /// Is a trainer's respec question still live and in range — the answer
    /// `CheckTalentMasterDist()` gives, pushed by the app each frame
    /// ([`super::UiScript::set_talent_master_pending`]). The CONFIRM_TALENT_WIPE dialog polls it
    /// from OnUpdate and hides itself when it goes false; the binder question's twin, and the same
    /// range gate stands behind both (decision 1580).
    pub(crate) talent_master_pending: bool,

    /// The stance/shapeshift bar's form list (bar order) the app pushes — the
    /// `GetNumShapeshiftForms`/`GetShapeshiftFormInfo` family reads it ([`super::shapeshift`]).
    /// Durable player state like `spellbook`; "no forms" (a mage) is simply empty.
    pub(crate) shapeshift_forms: Vec<super::shapeshift::StoredShapeshiftForm>,
    /// Form spell ids `CastShapeshiftForm` queued since the app's last
    /// [`super::UiScript::take_shapeshift_casts`] drain.
    pub(crate) shapeshift_casts: Vec<u32>,

    /// The pet action bar the app pushes — the ten slots plus the two bar-wide bits the
    /// `PetHasActionBar`/`GetPetActionsUsable`/`GetPetActionInfo` family reads ([`super::pet`],
    /// decision 0982). Unlike the stance bar's, this state is **server-authoritative**: it is
    /// replaced wholesale on every `SMSG_PET_SPELLS`, and is empty whenever there is no pet.
    pub(crate) pet_bar: super::pet::PetBarState,
    /// 1-based slot indices `CastPetAction` queued since the app's last
    /// [`super::UiScript::take_pet_actions`] drain.
    pub(crate) pet_actions_pressed: Vec<u32>,
    /// 1-based slot indices `TogglePetAutocast` queued.
    pub(crate) pet_autocast_toggles: Vec<u32>,
    /// `PetStopAttack()` calls queued — a count, since the verb takes no argument.
    pub(crate) pet_stop_attacks: u32,
    /// Pet bar writes queued by the drag ([`cursor::pet`], decision 1010) — **one entry per
    /// `CMSG_PET_SET_ACTION`**, each holding the one or two `(0-based position, packed word)` pairs
    /// that send names. The nesting is the point: the server tells the one-pair form from the
    /// two-pair form **by body size**, so a relocation and its write must travel together and must
    /// not be flattened into a stream of singles.
    pub(crate) pet_set_actions: Vec<Vec<(u32, u32)>>,
    /// `PetAbandon()` and `PetDismiss()` calls queued — two counts, not one, even though both menu
    /// rows end at the same opcode (decision 1066). They are two *bindings*, with two Lua names and
    /// two menu rows the reference shows to different classes, so the seam keeps them apart and
    /// lets the app decide each one's wire; folding them together here would bake a wire fact into
    /// an engine that is supposed to hold none.
    pub(crate) pet_abandons: u32,
    pub(crate) pet_dismisses: u32,
    /// Names `PetRename(name)` queued, in order — the `PETRENAMECONFIRM` popup's payload.
    pub(crate) pet_renames: Vec<String>,

    /// The per-bag container snapshot (keyed by live-API bag id, 0 = backpack) the app pushes,
    /// and the `UseContainerItem` intents it drains — the container seam ([`container`]).
    pub(crate) containers: HashMap<i64, container::ContainerState>,
    pub(crate) container_uses: Vec<(i64, u32)>,
    /// Per-(bag, slot) use-cooldowns in `GetTime` seconds `(start, duration, enabled)` — stamped
    /// at `set_container` push time (the action-state pattern), read by
    /// `GetContainerItemCooldown`.
    pub(crate) container_cooldowns: HashMap<(i64, u32), (f64, f64, bool)>,
    /// `HasKey()` — whether the player owns any item of `BagFamily` KEYS, anywhere the reference's
    /// own search reaches (equipment, bags, backpack, **bank**, keyring). App-resolved like every
    /// other item fact; the engine holds no item knowledge of its own. Gates the whole keyring UI
    /// (decision 0765).
    pub(crate) has_key: bool,
    /// What the cursor carries (`PickupContainerItem`/`SplitContainerItem`/… set it; `None` =
    /// empty cursor) — the real client's transient drag state, typed by payload arm
    /// ([`cursor::CursorPayload`], decision 0216). Purely visual + intent-routing: no item moves
    /// locally, the server's field updates settle the bag ([`container`]). The app draws the
    /// held icon at the mouse and reads `None`/`Some` to show/hide it.
    pub(crate) cursor: Option<cursor::CursorPayload>,
    /// Mirrors "is `cursor` currently `Some`" across transitions — [`cursor::queue_cursor_update`]
    /// compares it against the live state on every call to derive `ACTIONBAR_SHOWGRID`/
    /// `ACTIONBAR_HIDEGRID` (decision 0216 §7): fires exactly on a None↔Some edge, any payload
    /// arm, any surface (bags/doll/actions alike — the reference's own "any placeable payload
    /// shows the bar's drop grid"). A Some→Some transition (the action hop) updates nothing here,
    /// so no spurious HIDE+SHOW churns out of one gesture.
    ///
    /// The **pet** payload arm is excluded (decision 1010): `PlaceAction` refuses it, so lighting
    /// the action bar's empty slots for a payload that cannot land there would be an invitation to
    /// a no-op. It drives [`Self::pet_grid_shown`] instead.
    pub(crate) cursor_grid_shown: bool,
    /// The same mirror for the PET bar's grid — `PET_BAR_SHOWGRID`/`PET_BAR_HIDEGRID`, which the
    /// reference fires from inside the pet-action pickup builder itself (`0x494f28`) rather than
    /// from a shared cursor transition. So the two grids are disjoint: a spell lights the action
    /// bar's, a pet action lights the pet bar's, and neither lights both.
    pub(crate) pet_grid_shown: bool,
    /// The app's world pick under the cursor — the reference's click-time pick state
    /// (`[this+0x350]`: nothing / terrain / object), fed once per frame
    /// ([`super::UiScript::set_world_pick`]; stays `Nothing` in tests/captures). Routes
    /// [`cursor::world_drop_click`]'s legs (decisions 0571 + 0574): an `Object` pick drops no
    /// payload at all, `Terrain` drops items only (a spell/action survives the ground click),
    /// `Nothing` drops any arm.
    pub(crate) world_pick: cursor::WorldPick,
    /// Backpack pick/place/swap intents queued by `PickupContainerItem`/`SplitContainerItem` when
    /// the cursor was holding (src bag/slot → dst bag/slot, live-API space, optional split count),
    /// drained by the app into `CMSG_SWAP_INV_ITEM`/`CMSG_SPLIT_ITEM`.
    pub(crate) container_moves: Vec<container::ContainerMove>,
    /// Repair-mode clicks the pickup intercept queued (`(bag, slot)`; [`container`]).
    pub(crate) container_repairs: Vec<(i64, u32)>,
    /// The targeting cursor's **item** half is up (`TargetingWantsItem 0x6e6330`, decision 0923;
    /// first built for the CraftFrame enchant pick, 0437 phase 3): while set, a bag OR paper-doll
    /// click queues into [`Self::item_picks`] instead of running the cursor gesture.
    pub(crate) item_pick_armed: bool,
    /// `(bag, slot)` clicks consumed by the armed item pick, drained by the app. A doll click
    /// reports as [`super::EQUIPMENT_BAG`] + its 1-based inventory slot, so both seams speak the
    /// one bag space the drain already resolves.
    pub(crate) item_picks: Vec<(i64, u32)>,
    /// `BindEnchant()`/`ReplaceEnchant()` — the two enchant-confirm popups' answers to a pick the
    /// app already parked, drained by it (decision 0928; [`cursor::EnchantConfirm`]).
    pub(crate) enchant_confirms: Vec<cursor::EnchantConfirm>,
    /// `(bag, slot, count)` triples queued by `DeleteCursorItem` (`count == 0` = the whole
    /// stack) — the popup-confirmed destroy (decision 0216 §3), drained by the app into
    /// `CMSG_DESTROYITEM`.
    pub(crate) container_destroys: Vec<(i64, u32, u32)>,
    /// The Lua-set **displayed-cursor override** (`ShowContainerSellCursor`/`ShowMerchantSellCursor`/
    /// `ShowBuybackSellCursor`/`ShowInspectCursor` set it, `ResetCursor` clears it; [`container`] +
    /// [`merchant`]) — the single "displayed mode" the real client keeps at `0xbe2c2c`, restored to
    /// the base (world-classifier) mode by `ResetCursor`. `None` = no override (show the base mode).
    pub(crate) ui_cursor: Option<container::UiCursorMode>,
    /// Set by every FrameXML cursor call — `Show*SellCursor` / `ShowInspectCursor` / `SetCursor` /
    /// `ResetCursor` — and drained by the app each frame ([`UiScript::take_cursor_write`]).
    ///
    /// **The cursor mode is a WRITE, not a level** (decision 1061), and that distinction is the
    /// whole of B208's regression. The reference keeps one sticky global (`0xbe2c2c`): the world
    /// classifier writes it while the pointer is over the world, FrameXML writes it from a hover
    /// handler, and in between **nothing** writes it — so the last value simply stands. Reading
    /// `ui_cursor` as a level made "no override" mean "show the base", which turned every UI
    /// element with no cursor handler at all (a spellbook button, a panel) into a forced Point and
    /// killed the armed cast cursor the moment the mouse left the world.
    pub(crate) ui_cursor_dirty: bool,
    /// `(bag, slot)` sources queued by `AutoEquipCursorItem` (decision 0208 phase 1b, `cursor`'s
    /// `doll` submodule) — drained by the app into `CMSG_AUTOEQUIP_ITEM`.
    pub(crate) container_autoequips: Vec<(i64, u32)>,

    /// Per-frame drag-button registrations (`RegisterForDrag`) — kind-independent (any Frame, not
    /// just a Button), so it lives beside [`Model::mouse_down_on`] rather than inside a per-kind
    /// state. Verbatim button-name sets (`"LeftButton"`, …); [`UiScript::mouse_button`]/
    /// [`UiScript::mouse_move`] compare case-insensitively (RF's `RegisterForClicks` precedent).
    /// Same destroy-cleanup status as [`Model::scripts`]/[`Model::frame_events`]: nothing in this
    /// engine destroys a frame yet (no Lua `Destroy`, no call to `WidgetArena::destroy` outside
    /// its own tests), so none of the three is pruned today — a future destroy path must prune
    /// all three together.
    pub(crate) drag_registered: HashMap<FrameHandle, HashSet<String>>,
    /// The in-flight drag gesture: armed at mouse-down on a [`Model::drag_registered`] frame,
    /// `started` once the cursor has moved past the drag-start threshold — `None` between
    /// gestures ([`cursor::DragGesture`], decision 0216 §3).
    pub(crate) drag: Option<cursor::DragGesture>,
    /// The one in-flight `StartMoving()` — the client's single root-side drag slot (`root+0xcfc`
    /// and the cursor sample beside it), `None` between moves ([`super::object::FrameMove`]). Held
    /// beside [`Model::drag`] because that gesture is what normally opens and closes it: the
    /// canonical addon idiom is `OnDragStart → StartMoving`, `OnDragStop → StopMovingOrSizing`.
    /// The two remain separate systems, exactly as in the reference — a drag can carry a payload
    /// with nothing moving, and a move outlives the mouse button (the reference's mouse-up
    /// auto-stop skips the Lua drag type).
    pub(crate) moving: Option<super::object::FrameMove>,
    /// The in-flight `StartSizing` drag — the resize twin of [`Self::moving`], cleared by the same
    /// `StopMovingOrSizing` (`0x776990`).
    pub(crate) sizing: Option<super::object::FrameSizing>,
    /// The in-flight Slider thumb drag: set when a LeftButton press lands on a Slider's thumb, held
    /// until release / pointer-leave (decision 0250 §5, the engine's C++-equivalent thumb drag —
    /// like a scrollbar dragging in the real client, no Lua involved). `None` between drags
    /// ([`slider::SliderDrag`]).
    pub(crate) slider_drag: Option<slider::SliderDrag>,
    /// The in-flight ColorSelect drag — the colour picker's wheel/value-strip twin of
    /// [`Self::slider_drag`], held from press to release. The widget keeps two independent capture
    /// flags rather than one target ([`colorselect::ColorDrag`]).
    pub(crate) color_drag: Option<colorselect::ColorDrag>,

    /// The open gossip menu the app pushes (`None` = no menu), the `SelectGossipOption` intents it
    /// drains, and whether `CloseGossip` was called — the gossip seam ([`gossip`]).
    pub(crate) gossip: Option<gossip::GossipMenu>,
    pub(crate) gossip_selects: Vec<u32>,
    pub(crate) gossip_close: bool,
    /// 1-based quest-row selects queued by `SelectGossipQuest` (decision 0088) — the app maps each
    /// to the row's quest id and sends `CMSG_QUESTGIVER_QUERY_QUEST`.
    pub(crate) gossip_quest_selects: Vec<u32>,

    /// The open merchant's stock snapshot the app pushes (`None` = no vendor open), the
    /// `BuyMerchantItem` intents it drains, and whether `CloseMerchant` was called — the merchant
    /// seam ([`merchant`]).
    pub(crate) merchant: Option<merchant::MerchantState>,
    pub(crate) merchant_buys: Vec<(u32, u32)>,
    pub(crate) merchant_close: bool,
    /// `BuybackItem` intents (1-based buyback slots), the `RepairAllItems` flag, and the
    /// client-side repair-mode latch (`ShowRepairCursor`/`InRepairMode`) — the rest of the
    /// merchant seam.
    pub(crate) merchant_buybacks: Vec<u32>,
    pub(crate) repair_all: bool,
    pub(crate) repair_mode: bool,

    /// The stable window's whole state — the pushed snapshot, the selection, the frame-local
    /// drag, and the queued verbs (the stable seam, [`stable`], decision 1676). One sub-struct
    /// rather than five loose fields because the close path resets several of them together.
    pub(crate) stable: stable::StableModel,

    /// The open bank's purchase-row snapshot the app pushes (`None` = no bank open), the
    /// `PurchaseSlot` intent, and whether `CloseBankFrame` was called — the bank seam ([`bank`],
    /// decision 0604; the bank's *contents* ride the container seam as bags −1/5..=10).
    pub(crate) bank: Option<bank::BankState>,
    pub(crate) bank_purchase: bool,
    pub(crate) bank_close: bool,

    /// The open trainer's service snapshot the app pushes (`None` = no trainer open), the
    /// `BuyTrainerService` intents it drains, the engine-held 1-based selection (0 = none), and
    /// whether `CloseTrainer` was called — the trainer seam ([`trainer`], decision 0237).
    pub(crate) trainer: Option<trainer::TrainerState>,
    pub(crate) trainer_buys: Vec<u32>,
    pub(crate) trainer_selection: u32,
    pub(crate) trainer_close: bool,
    /// The three state filters (available / unavailable / used) — the real client hides filtered
    /// service rows itself ([`trainer`]); all shown by default. A state filter hides *services*, never
    /// headers (decision 0247).
    pub(crate) trainer_filter: [bool; 3],
    /// The skill lines the player has collapsed in the tree — their services hide, the header stays
    /// (decision 0247). Keyed by skill-line id so it survives a content update (`set_trainer` keeps
    /// only still-present lines); cleared when the trainer closes.
    pub(crate) trainer_collapsed: HashSet<u32>,

    /// The open taxi map's snapshot the app pushes (`None` = closed), the `TakeTaxiNode` intents
    /// it drains, whether `CloseTaxiMap` was called, and whether our own player is riding a
    /// taxi — the taxi seam ([`taxi`], decision 0484).
    pub(crate) taxi: Option<taxi::TaxiUiState>,
    pub(crate) taxi_takes: Vec<usize>,
    pub(crate) taxi_close: bool,
    pub(crate) taxi_riding: bool,

    /// The open tradeskill window's recipe snapshot the app pushes (`None` = no window open) — the
    /// tradeskill seam ([`tradeskill`], decision 0437 phase 2).
    pub(crate) trade_skill: Option<tradeskill::TradeSkillState>,
    /// `(spell id, count)` intents `DoTradeSkill` queued since the app's last
    /// [`super::UiScript::take_trade_skill_dos`] drain.
    pub(crate) trade_skill_dos: Vec<(u32, u32)>,
    /// The engine-held 1-based selection (0 = none), preserved across a `set_trade_skill` re-push by
    /// the recipe's spell id (see [`super::UiScript::set_trade_skill`]).
    pub(crate) trade_skill_selection: u32,
    /// Whether `CloseTradeSkill` was called since the app's last
    /// [`super::UiScript::take_trade_skill_close`] drain.
    pub(crate) trade_skill_close: bool,
    /// The recipe groups the player has collapsed, by group key `(ItemClass, ItemSubClass)`
    /// (wow-re `tradeskill` TU-B) — a group's recipes hide, its header stays (the
    /// trainer/skills-pane precedent, [`Model::trainer_collapsed`]/[`Model::skills_collapsed`]).
    /// Survives a `set_trade_skill` content re-push (pruned to the groups the fresh recipes still
    /// produce) AND a same-profession close→reopen (wow-re `tradeskill` TU-G §6 — the collapse
    /// mask `0x84dd68` round-trips the rebuild by header key); reset on a profession switch
    /// ([`Model::trade_skill_last_line`]).
    pub(crate) trade_skill_collapsed: HashSet<(u32, u32)>,
    /// Group keys the SubClass filter dropdown has hidden (empty = "All Subclasses"). Keyed like
    /// [`Model::trade_skill_collapsed`] — the real client's per-header shown flag (`header+0xc`)
    /// survives a rebuild by exactly this `(ItemClass, ItemSubClass)` key match (`0x4fca20`'s
    /// save→restore loop; the position mask `0x84dd60` is the derived form — wow-re `tradeskill`
    /// TU-G). Persists across a same-profession close/reopen; reset on a profession switch
    /// ([`Model::trade_skill_last_line`]).
    pub(crate) trade_skill_subclass_hidden: HashSet<(u32, u32)>,
    /// The InvSlot filter mask (`0x84dd64`, wow-re `tradeskill` TU-G): **bit set = slot shown**,
    /// all-ones = "All Slots". The real client's build never touches this static — it is NOT
    /// pruned on a re-push, and an exclusive pick therefore keeps every OTHER bit clear even for
    /// slots that appear later. Reset (all-ones) only on a profession switch.
    pub(crate) trade_skill_invslot_mask: u32,
    /// The skill line the tradeskill state was last built for (`0xbde064`, wow-re TU-G's cache
    /// key): a `set_trade_skill` push for a DIFFERENT line resets the two filters, the collapse
    /// set, and the selection; the same line (including a close→reopen round trip) keeps them.
    pub(crate) trade_skill_last_line: u32,
    /// The selected recipe's SPELL ID (`0xbde044` — the real client stores the selection by spell
    /// id, not index), shadowing [`Model::trade_skill_selection`]'s flat position so the selection
    /// survives a same-profession close→reopen (TU-G: untouched on that path) and remaps across a
    /// re-push.
    pub(crate) trade_skill_selected_spell: u32,
    /// Set by the engine-side mutators the real client answers with a `TRADE_SKILL_UPDATE` event
    /// from inside the C call (`Set*Filter`/`Expand/CollapseTradeSkillSubClass` → the
    /// `0x4fd710`/`0x4fd730`/`0x4fd750` writer trio → recompute+resort `0x4fd180` + event
    /// **0x13a**); the app drains it ([`super::UiScript::take_trade_skill_touched`]) and fires
    /// the event the same frame.
    pub(crate) trade_skill_touched: bool,

    /// The open craft window's recipe snapshot the app pushes (`None` = no window open) — the craft
    /// seam ([`craft`], decision 0437 phase 3), TradeSkill's exact twin.
    pub(crate) craft: Option<craft::CraftState>,
    /// Spell id intents `DoCraft` queued since the app's last [`super::UiScript::take_craft_dos`]
    /// drain — no count (the 1.12 CraftFrame has no Create All, unlike `trade_skill_dos`).
    pub(crate) craft_dos: Vec<u32>,
    /// The engine-held 1-based selection (0 = none), preserved across a `set_craft` re-push by the
    /// recipe's spell id (see [`super::UiScript::set_craft`]).
    pub(crate) craft_selection: u32,
    /// Whether `CloseCraft` was called since the app's last [`super::UiScript::take_craft_close`]
    /// drain.
    pub(crate) craft_close: bool,

    /// The open loot's row snapshot the app pushes (`None` = no loot open), the row-pick intents it
    /// drains, and whether `CloseLoot` was called — the loot seam ([`loot`]).
    ///
    /// **The picks come in two flavours, and the flavour is load-bearing** (decision 1744): the
    /// reference's take dispatcher `0x4c2790(slot, flag)` is one function with two entries, and the
    /// flag decides whether a bind-on-pickup row raises the LOOT_BIND confirm or is taken outright.
    /// `flag == 0` is the row click, which in 1.12 is the C `CLootButton`'s own behaviour and here
    /// is `BenillaTakeLootSlot`; `flag != 0` is the confirm continuation, which is the ONLY thing
    /// the 1.12 Lua binding `LootSlot` does (`0x4c2e70` passes `edx = 1`, and its sole FrameXML
    /// caller is `StaticPopupDialogs["LOOT_BIND"].OnAccept`).
    pub(crate) loot: Option<loot::LootState>,
    pub(crate) loot_picks: Vec<u32>,
    /// The `LootSlot(slot)` confirm continuations — 1-based display rows, drained separately from
    /// [`Self::loot_picks`] so the app can honour the reference's pending-slot gate.
    pub(crate) loot_confirms: Vec<u32>,
    pub(crate) loot_close: bool,
    /// The `GiveMasterLoot(slot, candidateIndex)` assignments the app drains — both 1-based and
    /// both display-side: the row as the window numbers it, and the candidate's position in
    /// [`loot::LootState::master_candidates`]. The app owns the translation to the wire slot and
    /// the recipient's guid (decision 1675).
    pub(crate) loot_master_gives: Vec<(u32, u32)>,

    /// The open group-loot rolls the app pushes (empty = none open) and the `RollOnLoot`
    /// `(roll_id, roll_type)` votes it drains — the roll seam ([`loot_roll`], decision 0591).
    pub(crate) loot_rolls: loot_roll::LootRollsState,
    pub(crate) loot_roll_votes: Vec<(u32, u8)>,
    /// Need/Greed on a **bind-on-pickup** roll: not a vote but a request for the
    /// `CONFIRM_LOOT_ROLL` popup, which `ConfirmLootRoll` then re-enters past the gate
    /// (decision 0594's binary correction).
    pub(crate) loot_roll_confirms: Vec<(u32, u8)>,

    /// The open mailbox's inbox snapshot the app pushes (`None` = no mailbox open) and the intents
    /// the app drains — the mail seam ([`mail`], decision 0544). `mail_opens` are the 1-based rows
    /// `GetInboxText` touched (the app marks each read + ask-once fetches its body); the take/delete/
    /// return vecs are 1-based row picks; `mail_send` is the pending `SendMail(target,subject,body)`
    /// the app folds `mail_send_money`/`mail_send_cod`/`mail_send_item` into at drain.
    /// The item-text reader session ([`item_text`]): the pushed snapshot + the close/page-turn
    /// intents the app drains.
    pub(crate) item_text: Option<item_text::ItemTextState>,
    pub(crate) item_text_close: bool,
    pub(crate) item_text_page_turns: Vec<i32>,
    pub(crate) mail: Option<mail::MailState>,
    pub(crate) mail_check_inbox: bool,
    pub(crate) mail_opens: Vec<u32>,
    pub(crate) mail_take_items: Vec<u32>,
    pub(crate) mail_take_money: Vec<u32>,
    pub(crate) mail_deletes: Vec<u32>,
    pub(crate) mail_returns: Vec<u32>,
    pub(crate) mail_take_texts: Vec<u32>,
    pub(crate) mail_close: bool,
    pub(crate) mail_send: Option<(String, String, String)>,
    pub(crate) mail_send_money: u32,
    pub(crate) mail_send_cod: u32,
    /// The Send tab's attached bag item (a cursor drop, decision 0216) — carried until the send
    /// fires; the app resolves its `(bag, slot)` to the wire item guid then.
    pub(crate) mail_send_item: Option<cursor::CursorItem>,
    /// `HasNewMail()` — login-scoped (survives the mailbox window closing, unlike [`Self::mail`]
    /// above): the app's `MSG_QUERY_NEXT_MAIL_TIME`/`SMSG_RECEIVED_MAIL`-fed countdown reduced to
    /// one flag (decision 0544 P3, wow-re §5 `mail-interaction.md`). The reference minimap icon
    /// (`MiniMapMailFrame`) reads this on `UPDATE_PENDING_MAIL`.
    pub(crate) has_new_mail: bool,

    /// The open auction house's snapshot the app pushes (`None` = no auctioneer session) and the
    /// intents the app drains — the auction seam ([`auction`], decision 1511). `auction_selected`
    /// is engine-local per list ([`auction::LIST`]/`BIDDER`/`OWNER`) and holds the selected
    /// **auction id**, not a row position (wow-re §5 TU-5): an id follows its row through a
    /// re-sort, where an index would quietly come to mean the auction that took its place. `0` =
    /// nothing selected. It is engine-local because the reference reads the selection back
    /// synchronously inside the same handler that sets it, so it cannot be a round trip. `auction_can_query` is the app's 5 s browse
    /// throttle, pushed separately from the snapshot because the Search button polls it every
    /// frame and it would otherwise churn the snapshot's diff.
    pub(crate) auction: Option<auction::AuctionState>,
    pub(crate) auction_selected: [u32; 3],
    pub(crate) auction_can_query: bool,
    pub(crate) auction_query: Option<auction::AuctionQuery>,
    pub(crate) auction_owner_query: Option<u32>,
    pub(crate) auction_bidder_query: Option<u32>,
    pub(crate) auction_bids: Vec<auction::AuctionBid>,
    pub(crate) auction_cancels: Vec<u32>,
    pub(crate) auction_start: Option<auction::AuctionStartRequest>,
    pub(crate) auction_sorts: Vec<(usize, String)>,
    pub(crate) auction_close: bool,
    /// The sell slot's staged item (a cursor drop, decision 0216) — carried until `StartAuction`
    /// fires, when the app resolves its `(bag, slot)` to the wire item guid.
    pub(crate) auction_sell_item: Option<cursor::CursorItem>,

    /// The open trade window's snapshot the app pushes (`None` = no trade open) and the intents the
    /// app drains — the trade seam ([`trade`], decision 0592 P1). `trade_initiates` are the unit
    /// tokens `InitiateTrade` queued (the app resolves each → guid → `CMSG_INITIATE_TRADE`); the
    /// three flags are the accept / un-accept / cancel verbs (`CMSG_ACCEPT_TRADE` /
    /// `CMSG_UNACCEPT_TRADE` / `CMSG_CANCEL_TRADE`). `trade_set_money` is the copper `SetTradeMoney`
    /// offered (→ `CMSG_SET_TRADE_GOLD`, decision 0592 P2).
    pub(crate) trade: Option<trade::TradeState>,
    pub(crate) trade_initiates: Vec<String>,
    pub(crate) trade_accept: bool,
    pub(crate) trade_unaccept: bool,
    pub(crate) trade_close: bool,
    pub(crate) trade_set_money: Option<u32>,
    /// Cursor-drop placements onto our trade slots: `(trade_id 1-based, bag, slot)` → the app resolves
    /// `(bag, slot)` → wire position → `CMSG_SET_TRADE_ITEM`. `trade_clear_items` are the 1-based ids
    /// an empty-cursor click cleared (→ `CMSG_CLEAR_TRADE_ITEM`). Decision 0592 P2.
    pub(crate) trade_set_items: Vec<(u32, i64, u32)>,
    pub(crate) trade_clear_items: Vec<u32>,

    /// The open questgiver panel the app pushes (`None` = no window), the greeting-row selects and
    /// the button intents it drains — the questgiver seam ([`quest`]).
    pub(crate) quest: Option<quest::QuestState>,
    pub(crate) quest_selects: Vec<quest::QuestSelect>,
    pub(crate) quest_actions: Vec<quest::QuestAction>,

    /// The death-arc seam ([`death`], decision 0308): the snapshot the app pushes (countdowns +
    /// offer bits) and the drained release/reclaim/resurrect intents.
    pub(crate) death: death::DeathUiState,
    pub(crate) death_actions: Vec<death::DeathAction>,

    /// The quest-log seam ([`quest_log`]): the snapshot the app pushes, the engine-owned
    /// synchronous selection + click-time abandon mark (1-based entry indices, `0` = none), and
    /// the drained abandon intents.
    pub(crate) quest_log: quest_log::QuestLogState,
    pub(crate) quest_log_selection: u32,
    pub(crate) quest_log_abandon_mark: u32,
    pub(crate) quest_log_abandons: Vec<u32>,
    /// Quest ids `QuestLogPushQuest()` queued for the app to push to the party (decision 1733).
    /// **Ids, not entry indices** — unlike the abandon mark above, whose two-step confirm is the
    /// whole reason it pins an index: a push is one click, so the id is resolved right then and a
    /// log shuffle between click and drain cannot retarget it.
    pub(crate) quest_log_pushes: Vec<u32>,
    /// How many times `ConfirmAcceptQuest()` was called — the escort confirm's Yes. A counter, not
    /// a quest id: the reference's verb takes no argument and answers whatever confirm the client
    /// is holding, so the id lives app-side with the pending confirm (decision 1733).
    pub(crate) quest_confirms: u32,
    /// The shared item-template store: `item id → full tooltip view` ([`item_stats`] module doc,
    /// decision 0274 P1).
    pub(crate) item_templates: HashMap<u32, ItemTemplateView>,
    /// Item ids the renderer asked for that the store lacks — drained by the app
    /// ([`UiScript::take_item_stat_asks`]), answered via [`UiScript::set_item_template`].
    pub(crate) item_stat_asks: HashSet<u32>,
    /// The item-set store (`set id → §22 SET-block view`) + its ask-once misses — the same
    /// push/ask flow as the templates ([`item_stats`] module doc).
    pub(crate) item_sets: HashMap<u32, super::ItemSetView>,
    pub(crate) item_set_asks: HashSet<u32>,
    /// The **random-suffix roll** store (`ItemRandomProperties` id → its resolved view) — pushed
    /// WHOLE at startup, not asked for (decision 1547). It is a static DBC the app has in memory
    /// from load, and its consumers are click-driven (a chat-link tooltip has no hover re-enter
    /// loop to repaint on a late answer), so the ask-once shape the template store uses would
    /// leave a first click showing an item with no lines. This mirrors the reference exactly: the
    /// builder resolves its `+0x424` against the loaded DBC store `0xc0dbd4` at draw time, and
    /// every source supplies only the id (wow-re `tooltip-content-law.md` §1-ENCHANT §E5).
    pub(crate) random_properties: HashMap<u32, super::RandomPropertyView>,
    /// The red-line law's player state (level/class/race/skills — [`item_stats`] module doc).
    pub(crate) player_req: PlayerReqState,
    /// The spell-tooltip store: `spell id → resolved view` ([`tooltip_spell`] module doc,
    /// decision 0274 P2) + the ask-once misses the app drains.
    pub(crate) spell_tooltips: HashMap<u32, super::SpellTooltipView>,
    pub(crate) spell_tooltip_asks: HashSet<u32>,
    /// Header collapse/expand intents `(1-based entry index, collapse)` — `CollapseQuestHeader`/
    /// `ExpandQuestHeader` push, the app drains ([`UiScript::take_quest_log_collapses`]); index 0
    /// = all headers.
    pub(crate) quest_log_collapses: Vec<(u32, bool)>,
    /// The watched (tracked) quests, by stable quest id in watch order — the tracker HUD's set
    /// (engine-owned like the selection; pruned by `set_quest_log` when a quest leaves the log).
    pub(crate) quest_log_watched: Vec<u32>,
    /// The server's wall clock in unix-epoch seconds, pushed by the app each frame it has one
    /// ([`UiScript::set_server_unix_time`]); `None` before the first `SMSG_QUERY_TIME_RESPONSE`.
    ///
    /// Held here rather than folded into the quest-log snapshot because the *engine* owns every
    /// countdown the Lua reads: a timed quest's deadline is an absolute stamp in this epoch, and
    /// `GetQuestTimers` subtracts against it **per call**, exactly as the reference's C binding
    /// does. That is what lets the reference `QuestTimerFrame` tick smoothly from its OnUpdate
    /// while the log snapshot itself only changes when the log does (decision 1150).
    pub(crate) server_unix_time: Option<f64>,
    /// The player's purse in copper (`GetMoney`), pushed each frame it changes by the app's
    /// `PLAYER_FIELD_COINAGE` feed ([`UiScript::set_money`]). Plain data — the money display + the
    /// merchant window's coin line read it.
    pub(crate) money: u64,

    /// The reported connection round trip in ms — `GetNetStats`'s third return, pushed by the app's
    /// net feed ([`UiScript::set_latency_ms`]). The AVERAGE over the app's RTT ring, not the last
    /// sample; `0` = nothing measured yet. See [`super::net_stats`].
    pub(crate) net_latency_ms: u32,

    /// The player's experience within the current level (`UnitXP("player")`) and the amount required
    /// to level (`UnitXPMax("player")`), pushed each frame they change by the app's `PLAYER_XP` /
    /// `PLAYER_NEXT_LEVEL_XP` feed ([`UiScript::set_player_xp`]). A player-level pair (like `money`),
    /// not a per-unit-token field: PLAYER_XP is a PRIVATE descriptor field, only ever our own avatar's.
    pub(crate) player_xp: u32,
    pub(crate) player_next_level_xp: u32,

    /// The player's banked combo points and the unit they sit on — the raw `PLAYER_FIELD_BYTES`
    /// byte 1 and `PLAYER_FIELD_COMBO_TARGET` GUID, pushed together each frame either moves
    /// ([`UiScript::set_combo_points`]). Player-global PRIVATE fields, like `money`.
    ///
    /// **Raw wire values, deliberately ungated.** The server banks a point here for a *warrior*
    /// too (the Overpower window), and the usable walk's leg 5 consumes exactly that. The two
    /// gates the real `GetComboPoints 0x51a190` applies — rogue-or-druid only, and the banked
    /// target must be the CURRENT target — live in the binding, where the binary puts them
    /// (decision 0875). Read [`Self::combo_points`] directly and you are reading the wire, not
    /// what the reference UI can see.
    pub(crate) combo_points: u8,
    pub(crate) combo_target: u64,

    /// The player's rest snapshot, pushed together each frame any part moves
    /// ([`UiScript::set_rest_state`]) — player-globals like `money`. `rest_state` is the raw
    /// `PLAYER_BYTES_2` byte 3 (1 = rested, 2 = normal — the server writes it with hysteresis
    /// off the pool). **Defaults to 2, not 0**: every live 1.12 descriptor carries 2 from
    /// character creation on, and the tick's faithful FrameXML compares `GetRestState() >= 3`
    /// unguarded — a pre-feed 0 would render the binary's nil-triple fail path into an event
    /// handler the real client can only ever run with the byte present. Byte 0 stays reachable
    /// by explicit push, and the fail path stays faithful there. `rest_pool` is the raw
    /// `PLAYER_REST_STATE_EXPERIENCE` value in **base kill-XP units**, `resting` the
    /// `PLAYER_FLAGS_RESTING (0x20)` bit (inside an inn/city). `GetRestState`/`GetXPExhaustion`/
    /// `IsResting` read these; the pool's display scaling is `exhaustion` row 1's factor,
    /// applied in the `GetXPExhaustion` binding, where the real client applies it (decisions
    /// 1082/1087).
    pub(crate) rest_state: u8,
    pub(crate) rest_pool: u32,
    pub(crate) resting: bool,
    /// Is a cinematic playing? What `InCinematic()` reports, pushed by the cinematic plugin.
    ///
    /// Read by more than the letterbox: `StaticPopup_Show` refuses any dialog whose entry lacks
    /// `interruptCinematic` while this is set, exactly as the reference's `StaticPopup.lua:1454`
    /// does — which is what stops a "your character will be logged out" box from landing on top
    /// of a first login's race intro.
    pub(crate) in_cinematic: bool,
    /// Exhaustion.dbc as the rest bindings consume it — rest-state byte → (localized name,
    /// factor), the table `GetRestState` indexes directly and whose row 1 scales
    /// `GetXPExhaustion` (wow-re rested-xp-bindings.md; decision 1087). Seeded with the shipped
    /// 5875 enUS rows so the engine tests and a failed DBC read behave like the shipped client
    /// (the GlobalStrings-fallback posture); the app overwrites it with the install's real —
    /// localized — rows at startup ([`UiScript::set_exhaustion_rows`]).
    pub(crate) exhaustion: HashMap<u8, (String, f64)>,

    /// The paper doll's combat-stats snapshot (`None` until the app's feed lands), the
    /// equipment/ammo slot views, and the model pane's persistent bake yaw — the character-window
    /// seam ([`char_stats`], decision 0208 §3).
    pub(crate) player_combat_stats: Option<char_stats::UnitCombatStats>,
    /// The **pet's** combat-stats snapshot (decision 1057) — the same shape under the `"pet"`
    /// token, because the reference's own pet sheet calls the very same `UnitStat`/`UnitResistance`/
    /// `PaperDollFrame_Set*(unit, prefix)` family with `unit = "pet"` (ref
    /// `PetPaperDollFrame.lua:73-81`). A second slot rather than a token map: exactly two units
    /// ever have one, they are fed by two different systems on two different clocks, and every
    /// binding's route is a fork between them (the [`Self::pet_book`] precedent).
    pub(crate) pet_combat_stats: Option<char_stats::UnitCombatStats>,
    pub(crate) inventory_slots: char_stats::InventorySlots,
    /// The 12 alert-region statuses (`GetInventoryAlertStatus`, DurabilityFrame's armor-guy
    /// feed) in the client's own `0x806eb8` table order (11 equipment regions + the low-ammo
    /// 12th) — recomputed on every inventory push, which fires `UPDATE_INVENTORY_ALERTS`
    /// unconditionally, the client's own shape (see [`char_stats`]).
    pub(crate) inventory_alerts: [u8; 12],
    pub(crate) paperdoll_yaw: f32,
    /// Inventory-slot ids queued by `UseInventoryItem` (decision 0208 phase 1b, `cursor`'s `doll`
    /// submodule) — drained by the app into `CMSG_USE_ITEM` against the equipped position.
    pub(crate) inventory_uses: Vec<u32>,
    /// The two weapons' **temporary** enchantments — `[0]` main hand, `[1]` off hand, in the order
    /// `GetWeaponEnchantInfo` pushes them ([`weapon_enchant`]). A separate slot from
    /// [`Self::inventory_slots`] on purpose: this one carries a live countdown and is pushed every
    /// frame, where the slot snapshot is change-gated and fires an event.
    pub(crate) weapon_enchants: [Option<weapon_enchant::WeaponEnchant>; 2],

    /// The inspected unit's equipment view, keyed by unit token ([`inspect`], decision 0631) —
    /// the *second* source behind the unit-keyed `GetInventoryItem*` family. Unlike
    /// [`Self::inventory_slots`] this is PUBLIC descriptor data (`PLAYER_VISIBLE_ITEM_*`), which
    /// is the whole reason a foreign player's gear can be read at all. `None` = nothing inspected.
    pub(crate) inspect: Option<inspect::InspectView>,
    /// Unit tokens queued by `NotifyInspect` — drained by the app into `CMSG_INSPECT`.
    pub(crate) inspect_notifies: Vec<String>,
    /// `ClearInspectPlayer` was called — drained by the app, which drops its inspect target.
    pub(crate) inspect_clear: bool,
    /// The inspect model pane's bake yaw, the twin of [`Self::paperdoll_yaw`].
    pub(crate) inspect_yaw: f32,
    /// The **pet** paper doll's model-pane bake yaw (decision 1057) — a third scalar for the same
    /// reason the inspect pane got a second: character tab 1 and tab 2 are two panes that can sit
    /// at two different facings, and the ref carries a `rotation` per `<PlayerModel>`.
    pub(crate) pet_paperdoll_yaw: f32,
    /// The dressing room's queued intents (decision 1060) — `BenillaDressUpModel_Dress/TryOn/Close`,
    /// drained by the app in order (see [`super::dressup`] on why order matters).
    pub(crate) dressup_intents: Vec<super::dressup::DressUpIntent>,
    /// The dressing-room pane's bake yaw, the fourth of those scalars.
    pub(crate) dressup_yaw: f32,
    /// Unit token (lowercase) → what the app resolved about it this frame, for **every** token
    /// that named a **live unit object**, creature as readily as player — the input to both
    /// verified range predicates (`CanInspect`, `CheckInteractDistance`). An absent token is one
    /// the object manager holds nothing for, and both answer `nil` there (see
    /// [`UiScript::set_unit_reach`] and [`super::UnitReach`]).
    pub(crate) unit_reach: HashMap<String, super::UnitReach>,

    /// The skills-pane snapshot the app pushes ([`skills::SkillsState`], decision 0437 phase 4) and
    /// the synthesized display tree built from it ([`skills::UiScript::set_skills`]) — the skills
    /// seam ([`skills`]).
    pub(crate) skills: skills::SkillsState,
    pub(crate) skills_groups: Vec<skills::SkillGroup>,
    /// The categories the player has collapsed (by `category_id`) — survives a content update the
    /// same way [`Model::trainer_collapsed`] does.
    pub(crate) skills_collapsed: HashSet<u32>,
    /// The engine-held selection, by SKILL ID (not visible index — see [`skills`]'s module doc).
    pub(crate) skills_selected: Option<u32>,
    /// Skill line ids the `AbandonSkill` Lua binding queued since the app's last
    /// [`UiScript::take_skill_abandons`] drain — the outbound unlearn seam (the app sends each as
    /// a `CMSG_UNLEARN_SKILL`; the engine mutates NOTHING locally — the real client waits for the
    /// server's skill-field update, vmangos `SkillHandler.cpp`'s `SetSkill(id, 0, 0)` round trip).
    pub(crate) skill_abandons: Vec<u32>,

    /// The reputation-pane snapshot the app pushes ([`reputation::ReputationState`]) and the
    /// synthesized display tree built from it ([`reputation::UiScript::set_reputation`]) — the
    /// reputation seam ([`reputation`]).
    pub(crate) reputation: reputation::ReputationState,
    pub(crate) reputation_groups: Vec<reputation::FactionGroup>,
    /// The header groups the player has folded, by HEADER KEY (a `Faction.dbc` id, or the
    /// synthetic `0` "Other" / `-1` "Inactive") — an identity a re-push cannot move, unlike a row
    /// index. Reset to all-expanded-but-Inactive on every push, exactly as
    /// [`Model::skills_collapsed`] is: the client's own rebuild does the same.
    pub(crate) reputation_collapsed: HashSet<i64>,
    /// The engine-held selection, by REPUTATION-LIST SLOT (not visible index — see
    /// [`reputation`]'s module doc).
    pub(crate) reputation_selected: Option<u32>,
    /// Reputation verbs the pane's bindings queued since the app's last
    /// [`UiScript::take_reputation_sends`] drain — the outbound seam. Unlike the skills abandon
    /// above, the engine DOES mutate locally first: none of the three sends is acked.
    pub(crate) reputation_sends: Vec<reputation::ReputationSend>,

    /// Chat lines the input EditBox submitted (its `OnEnterPressed` → the `SubmitChatInput` Lua
    /// binding) since the app's last [`UiScript::take_chat_input`] drain — the outbound Lua→app seam
    /// for the chat input (the twin of `loot_picks`). The app routes each through its slash-command
    /// parser into a `CMSG_MESSAGECHAT`/`CMSG_TEXT_EMOTE`.
    pub(crate) chat_input: Vec<String>,

    /// The world-map seam ([`worldmap`](super::worldmap)): the pushed catalog/feed + the
    /// engine-owned selection.
    pub(crate) worldmap: super::worldmap::WorldMapState,
    /// The always-up world-state readout's rows ([`worldstate`](super::worldstate)), already
    /// gated and resolved app-side.
    pub(crate) worldstate: super::worldstate::WorldStateUiState,
    /// Events (name + args) queued by Lua bindings (`SetMapZoom` → `WORLD_MAP_UPDATE`;
    /// `PickupContainerItem`/`ClearCursor`/… → `CURSOR_UPDATE`/`ITEM_LOCK_CHANGED`/
    /// `DELETE_ITEM_CONFIRM`, decision 0216 §4/§5) to fire at the next
    /// [`UiScript::tick`](super::UiScript::tick) — a binding executes *inside* Lua, so it can't
    /// re-enter the handler dispatch synchronously; one tick of deferral is invisible at frame
    /// rate (the reference fires these synchronously as pure repaint triggers).
    pub(crate) pending_events: Vec<(String, Vec<ScriptValue>)>,
    /// The last cursor position [`UiScript::mouse_move`](super::UiScript::mouse_move)/`mouse_button`
    /// saw (UI space: logical px, y-up — the same frame `resolve` rects live in). Behind Lua's
    /// `GetCursorPosition()`; the reference world map polls it every OnUpdate for hover/click math.
    pub(crate) cursor_pos: (f32, f32),

    /// A `Minimap:PingLocation(x, y)` call, parked for the app to drain **in the same frame**
    /// ([`UiScript::take_minimap_ping_request`]): centre-relative offsets in **UI units**
    /// (x right, y up — `GetCursorPosition`'s own space, which is NOT the window-pixel space
    /// the app's minimap geometry lives in; the app applies the 0582 seam scale on the way in,
    /// and getting that wrong is decision 1596's first root cause).
    pub(crate) minimap_ping_request: Option<(f32, f32)>,
    /// The live minimap ping's **normalized** offsets from the widget centre (fractions of the
    /// widget side, x right / y up — the `MINIMAP_PING` event's arg2/arg3 space), republished
    /// by the app every frame a ping is live and cleared when the ping ends. Behind
    /// `Minimap:GetPingPosition()`. There is exactly one ping, so it lives here rather than on
    /// each Minimap widget's [`KindState`](crate::widget::KindState) — one write, one read,
    /// no arena walk (decision 1596).
    pub(crate) minimap_ping: Option<(f32, f32)>,
    /// The realm this session is on, behind `GetRealmName()` (decision 1195). `""` until the app
    /// pushes one — the glue screen's own answer, and never `nil`, because the corpus idiom is
    /// `db[GetRealmName()] = …` at file scope and a nil index errors one call deeper.
    /// Lines an addon queued with `SendChatMessage`, drained by the app into the wire
    /// (decision 1199). Deliberately a different queue from the chat box's input: the box's drain
    /// runs the slash grammar and this one must not.
    pub(crate) chat_sends: Vec<super::chat_send::ChatSend>,
    /// Broadcasts an addon queued with `SendAddonMessage`, drained by the app into the wire
    /// (decision 1235). Its own queue rather than [`Self::chat_sends`] because it is a different
    /// wire: `LANG_ADDON` in the language field, a four-value distribution set, and a payload the
    /// binding already composed as `prefix` TAB `message`.
    pub(crate) addon_sends: Vec<super::addon_message::AddonSend>,
    /// `RequestTimePlayed()` asks queued since the app last drained them — each is one
    /// `CMSG_PLAYED_TIME`. A COUNT, not a payload, for [`super::pvp`]'s reason: the packet is
    /// empty, so two asks in a frame are two sends rather than one collapsed intent.
    pub(crate) played_time_asks: u32,
    /// `Screenshot()` calls queued since the app last drained them — each is one capture
    /// (decision 1487). A COUNT for [`Self::played_time_asks`]'s reason: the request carries no
    /// payload, so two calls in a frame are two captures.
    pub(crate) screenshot_asks: u32,
    pub(crate) realm_name: String,
    /// The hearthstone bind location's NAME, behind `GetBindLocation()` — the app resolves the
    /// `SMSG_BINDPOINTUPDATE` area id through the same AreaTable catalog the hearthstone's `$z`
    /// token already uses, and pushes the resolved string here.
    ///
    /// Empty only before that packet has landed — `""`, never nil, matching [`Self::realm_name`]
    /// beside it: a consumer concatenates this (`Necrosis.lua:1089`), so nil would be a raise.
    pub(crate) bind_location: String,
    /// `ConfirmBinder()` calls queued since the app last drained them — each is one
    /// `CMSG_BINDER_ACTIVATE`. A COUNT for [`Self::played_time_asks`]'s reason: the intent carries
    /// no payload (the app holds the innkeeper's guid), so two calls are two sends.
    /// The `GMTicketCategory.dbc` rows behind `GetGMTicketCategories()` — `(id, name)` in file
    /// order, pushed once by the app ([`super::UiScript::set_gm_ticket_categories`]). The ids are
    /// the wire values, so this is an ordered pair list rather than an indexable table
    /// (decision 1673).
    pub(crate) gm_ticket_categories: Vec<(u32, String)>,
    /// Every ticket verb the window called since the app's last drain, **in call order** — ask,
    /// status-ask, delete, create, edit, one packet each. ONE queue rather than a counter per verb
    /// so that `DeleteGMTicket(); GetGMTicket()` in a single chunk reaches the wire in that order;
    /// per-verb drains cannot express it, and the get would answer with the pre-delete state
    /// (decision 1673).
    pub(crate) gm_ticket_intents: Vec<super::gm_ticket::GmTicketIntent>,
    /// `Stuck()` calls queued — each is one cast of spell 7355, the Help window's Auto-Unstuck.
    pub(crate) stuck_casts: u32,
    pub(crate) binder_confirms: u32,
    /// Is an innkeeper's bind question still live and in range — the answer `CheckBinderDist()`
    /// gives, pushed by the app each frame ([`super::UiScript::set_binder_pending`]). The
    /// CONFIRM_BINDER dialog polls it from OnUpdate and hides itself when it goes false, which is
    /// how walking away from the innkeeper takes the question off screen (decision 1331).
    pub(crate) binder_pending: bool,
    /// The summon question's three resolved reads, pushed by the app each frame
    /// ([`super::UiScript::set_summon_confirm`]) — what `GetSummonConfirmSummoner`,
    /// `GetSummonConfirmAreaName` and `GetSummonConfirmTimeLeft` answer ([`super::summon`]).
    /// Default (two empty strings and a zero) is exactly what the reference's getters answer with
    /// nothing pending, so the pre-push window behaves like the post-expiry one (decision 1747).
    pub(crate) summon_confirm: super::summon::SummonConfirmUiState,
    /// `ConfirmSummon()` calls queued since the app's last drain — each is one outbound
    /// `CMSG_SUMMON_RESPONSE`. A COUNT for [`Self::binder_confirms`]'s reason: the app holds the
    /// summoner's guid, so the intent carries no payload of its own.
    pub(crate) summon_confirms: u32,
    /// Frames per second, behind `GetFramerate()`. Pushed per tick by the app, which owns the
    /// clock this crate does not have.
    pub(crate) framerate: f64,
    /// The modifier-key mirror `(shift, ctrl, alt)` behind `IsShiftKeyDown`/`IsControlKeyDown`/
    /// `IsAltKeyDown` — pushed by the app's input pass ([`UiScript::set_modifiers`]) BEFORE the
    /// frame's mouse events, so a click handler's fork reads the state at click time.
    pub(crate) modifiers: (bool, bool, bool),
}

impl Model {
    /// A unit token's snapshot, **case-folded the way the client folds it**.
    ///
    /// 1.12's resolver `0x515970` compares each of its literals with `SStrCmpI` → `_strnicmp`,
    /// whose fold is `'A'..'Z' += 0x20` and nothing else: the `jb`/`ja` bounds are unsigned, so a
    /// byte ≥ 0x80 is never folded. `to_ascii_lowercase` is exactly that rule, and the reason this
    /// does not use `to_lowercase()` — a locale-aware fold would map bytes the client leaves alone.
    ///
    /// The uppercase scan is a fast path, not an optimisation for its own sake: every internal
    /// caller passes a lowercase literal (`"player"`, `"target"`), so the common case allocates
    /// nothing and only an addon's `"Player"` pays for a `String`.
    /// A named font object, **case-folded the way the client folds it** — `SStrCmpI` over the font
    /// registry's keys. ASCII only, and the uppercase scan is a fast path: every internal caller
    /// passes an exact shipped name, so only an addon's odd spelling pays for a `String`.
    pub(crate) fn font_object(&self, name: &str) -> Option<&FontObject> {
        if name.bytes().any(|b| b.is_ascii_uppercase()) {
            self.font_objects_by_lower.get(&name.to_ascii_lowercase())
        } else {
            self.font_objects_by_lower.get(name)
        }
    }

    /// Record one script error on **both** channels: the host's `errors` vec (the instruments'
    /// channel — the harness blocker tables, the app's log drain) and the handler-dispatch queue
    /// (the player's channel — decision 1305). Every engine-caught script error goes through here;
    /// a failure raised by the error handler *itself* is pushed to `errors` directly instead,
    /// which is what keeps the dispatch from recursing.
    pub(crate) fn record_script_error(&mut self, msg: String) {
        // Three channels now, and the third is the one with a memory (decision 1495): the dialog
        // shows a burst's first message and the host drain empties every frame, so without this
        // the 1,112 raises after the first exist nowhere a player can reach.
        self.diagnostics
            .record(super::diagnostics::DiagnosticKind::Error, &msg);
        self.pending_error_dispatch.push(msg.clone());
        self.errors.push(msg);
    }

    pub(crate) fn unit(&self, token: &str) -> Option<&UnitState> {
        if token.bytes().any(|b| b.is_ascii_uppercase()) {
            self.units_by_lower.get(&token.to_ascii_lowercase())
        } else {
            self.units_by_lower.get(token)
        }
    }

    pub(crate) fn new() -> Model {
        Model {
            addons: Vec::new(),
            addons_root: None,
            measurer: None,
            texture_probe: None,
            texture_size_probe: None,
            addons_saved_account: None,
            addons_saved_character: None,
            framexml_templates: Default::default(),
            framexml_fonts: Default::default(),
            arena: WidgetArena::new(),
            layout_inputs: HashMap::new(),
            solver: LayoutSolver::new(),
            layout_fingerprint: None,
            layout_epoch: 0,
            layout_touched: None,
            measure_dirty: None,
            msg_swept: std::collections::HashMap::new(),
            msg_lines_hashed: 0,
            layout_verify_recheck: false,
            layout_derives: 0,
            layout_scope: super::layout::LayoutScope::default(),
            layout_last_scope: (0, 0),
            layout_epoch_resolved: None,
            layout_solves: 0,
            layout_gate_walks: 0,
            layout_rounds: 0,
            resolved: HashMap::new(),
            link_spans: HashMap::new(),
            chat_tab: false,
            region_data: HashMap::new(),
            backdrops: HashMap::new(),
            simple_html: simplehtml::SimpleHtmlStates::new(),
            font_objects_by_lower: HashMap::new(),
            region_resolved: HashMap::new(),
            next_id: 1,
            id_to_frame: HashMap::new(),
            frame_to_id: HashMap::new(),
            id_to_region: HashMap::new(),
            region_to_id: HashMap::new(),
            region_names: HashMap::new(),
            scripts: HashMap::new(),
            on_update_frames: Vec::new(),
            on_size_changed_frames: Vec::new(),
            event_to_frames: HashMap::new(),
            frame_events: HashMap::new(),
            focused_editbox: None,
            mouseover: None,
            hover_repick: false,
            mouse_down_on: HashMap::new(),
            mouse_capture: None,
            last_click: HashMap::new(),
            pending_size_changed: Vec::new(),
            errors: Vec::new(),
            pending_error_dispatch: Vec::new(),
            diagnostics: Default::default(),
            warnings: Vec::new(),
            // Classic Era's UIParent virtual space is 1024×768-ish; a sensible default the host can
            // override with `set_screen_size`. y-up: [bottom, left, top, right].
            screen: Rect::new(0.0, 0.0, 768.0, 1024.0),
            units_by_lower: HashMap::new(),
            auras: HashMap::new(),
            cancel_aura_requests: Vec::new(),
            tracking: None,
            target_requests: Vec::new(),
            target_by_name_requests: Vec::new(),
            drop_item_on_unit: Vec::new(),
            target_clear: false,
            joined_channels: Vec::new(),
            party: party::PartyState::default(),
            party_requests: Vec::new(),
            saved_instances: Vec::new(),
            raid_selection: 0,
            social: social::SocialState::default(),
            social_requests: Vec::new(),
            guild: guild::GuildState::default(),
            guild_control: guild::GuildRankEdit::default(),
            guild_requests: Vec::new(),
            petition: petition::PetitionState::default(),
            petition_requests: Vec::new(),
            tell_requests: Vec::new(),
            open_chat_requests: Vec::new(),
            // Seeded per window, because the stock `DOCKED` position is not the same for all
            // seven (`ChatWindowLook::stock`): 1 and 2 are the dock, 3..7 are undocked.
            chat_window_looks: std::array::from_fn(chat_window::ChatWindowLook::stock),
            chat_window_changes: HashSet::new(),
            user_placed_changed: false,
            default_language: None,
            duel_requests: Vec::new(),
            follow_requests: Vec::new(),
            session_requests: Vec::new(),
            pvp_toggles: 0,
            honor: None,
            inspect_honor: None,
            inspect_honor_requests: 0,
            inspect_honor_pending: false,
            helm_shown: true,
            cloak_shown: true,
            worn_display_toggles: Vec::new(),
            action_bar_toggles: None,
            action_bar_toggle_sends: Vec::new(),
            sound_queue: Vec::new(),
            cvars: HashMap::new(),
            cvars_saved_base: HashMap::new(),
            cvar_changes: Vec::new(),
            cvars_warned: HashSet::new(),
            multisample_formats: Vec::new(),
            saved_names: Vec::new(),
            keybinds: super::keybind::KeybindState::default(),
            actions: HashMap::new(),
            action_states: HashMap::new(),
            bonus_bar_offset: 0,
            action_uses: Vec::new(),
            action_sets: Vec::new(),
            ui_errors: Vec::new(),
            spellbook: spellbook::SpellBookState::default(),
            pet_book: spellbook::PetBookState::default(),
            macros: macros::MacroState::default(),
            macros_dirty: false,
            macros_generation: 0,
            macro_icons: Vec::new(),
            spell_casts: Vec::new(),
            pet_spell_casts: Vec::new(),
            pet_spell_autocasts: Vec::new(),
            casting: false,
            spell_stop: false,
            spell_targeting: false,
            spell_can_target_unit: false,
            spell_stop_targeting: false,
            talents: super::talent::TalentUiState::default(),
            talent_learns: Vec::new(),
            talent_wipe_confirms: 0,
            talent_master_pending: false,
            shapeshift_forms: Vec::new(),
            shapeshift_casts: Vec::new(),
            pet_bar: super::pet::PetBarState::default(),
            pet_actions_pressed: Vec::new(),
            pet_autocast_toggles: Vec::new(),
            pet_stop_attacks: 0,
            pet_set_actions: Vec::new(),
            pet_abandons: 0,
            pet_dismisses: 0,
            pet_renames: Vec::new(),
            containers: HashMap::new(),
            container_uses: Vec::new(),
            container_cooldowns: HashMap::new(),
            has_key: false,
            cursor: None,
            cursor_grid_shown: false,
            pet_grid_shown: false,
            world_pick: cursor::WorldPick::default(),
            container_moves: Vec::new(),
            container_repairs: Vec::new(),
            item_pick_armed: false,
            item_picks: Vec::new(),
            enchant_confirms: Vec::new(),
            container_destroys: Vec::new(),
            ui_cursor: None,
            ui_cursor_dirty: false,
            container_autoequips: Vec::new(),
            drag_registered: HashMap::new(),
            drag: None,
            moving: None,
            sizing: None,
            slider_drag: None,
            color_drag: None,
            gossip: None,
            gossip_selects: Vec::new(),
            gossip_close: false,
            gossip_quest_selects: Vec::new(),
            merchant: None,
            merchant_buys: Vec::new(),
            merchant_close: false,
            merchant_buybacks: Vec::new(),
            repair_all: false,
            repair_mode: false,
            stable: Default::default(),
            bank: None,
            bank_purchase: false,
            bank_close: false,
            trainer: None,
            trainer_buys: Vec::new(),
            trainer_selection: 0,
            trainer_close: false,
            trainer_filter: [true; 3],
            trainer_collapsed: HashSet::new(),
            taxi: None,
            taxi_takes: Vec::new(),
            taxi_close: false,
            taxi_riding: false,
            trade_skill: None,
            trade_skill_dos: Vec::new(),
            trade_skill_selection: 0,
            trade_skill_close: false,
            trade_skill_collapsed: HashSet::new(),
            trade_skill_subclass_hidden: HashSet::new(),
            trade_skill_invslot_mask: u32::MAX,
            trade_skill_last_line: 0,
            trade_skill_selected_spell: 0,
            trade_skill_touched: false,
            craft: None,
            craft_dos: Vec::new(),
            craft_selection: 0,
            craft_close: false,
            loot: None,
            loot_picks: Vec::new(),
            loot_confirms: Vec::new(),
            loot_close: false,
            loot_master_gives: Vec::new(),
            loot_rolls: loot_roll::LootRollsState::default(),
            loot_roll_votes: Vec::new(),
            loot_roll_confirms: Vec::new(),
            item_text: None,
            item_text_close: false,
            item_text_page_turns: Vec::new(),
            mail: None,
            mail_check_inbox: false,
            mail_opens: Vec::new(),
            mail_take_items: Vec::new(),
            mail_take_money: Vec::new(),
            mail_deletes: Vec::new(),
            mail_returns: Vec::new(),
            mail_take_texts: Vec::new(),
            mail_close: false,
            mail_send: None,
            mail_send_money: 0,
            mail_send_cod: 0,
            mail_send_item: None,
            has_new_mail: false,
            auction: None,
            auction_selected: [0; 3],
            auction_can_query: true,
            auction_query: None,
            auction_owner_query: None,
            auction_bidder_query: None,
            auction_bids: Vec::new(),
            auction_cancels: Vec::new(),
            auction_start: None,
            auction_sorts: Vec::new(),
            auction_close: false,
            auction_sell_item: None,
            trade: None,
            trade_initiates: Vec::new(),
            trade_accept: false,
            trade_unaccept: false,
            trade_close: false,
            trade_set_money: None,
            trade_set_items: Vec::new(),
            trade_clear_items: Vec::new(),
            quest: None,
            quest_selects: Vec::new(),
            quest_actions: Vec::new(),
            death: death::DeathUiState::default(),
            death_actions: Vec::new(),
            quest_log: quest_log::QuestLogState::default(),
            quest_log_selection: 0,
            quest_log_abandon_mark: 0,
            quest_log_abandons: Vec::new(),
            quest_log_pushes: Vec::new(),
            quest_confirms: 0,
            item_templates: HashMap::new(),
            item_sets: HashMap::new(),
            item_set_asks: HashSet::new(),
            random_properties: HashMap::new(),
            item_stat_asks: HashSet::new(),
            player_req: PlayerReqState::default(),
            spell_tooltips: HashMap::new(),
            spell_tooltip_asks: HashSet::new(),
            quest_log_collapses: Vec::new(),
            quest_log_watched: Vec::new(),
            server_unix_time: None,
            worldmap: super::worldmap::WorldMapState::default(),
            worldstate: super::worldstate::WorldStateUiState::default(),
            pending_events: Vec::new(),
            cursor_pos: (0.0, 0.0),
            minimap_ping_request: None,
            minimap_ping: None,
            chat_sends: Vec::new(),
            addon_sends: Vec::new(),
            played_time_asks: 0,
            screenshot_asks: 0,
            realm_name: String::new(),
            bind_location: String::new(),
            gm_ticket_categories: Vec::new(),
            gm_ticket_intents: Vec::new(),
            stuck_casts: 0,
            binder_confirms: 0,
            binder_pending: false,
            summon_confirm: super::summon::SummonConfirmUiState::default(),
            summon_confirms: 0,
            framerate: 0.0,
            modifiers: (false, false, false),
            money: 0,
            net_latency_ms: 0,
            player_xp: 0,
            player_next_level_xp: 0,
            combo_points: 0,
            combo_target: 0,
            rest_state: 2,
            rest_pool: 0,
            resting: false,
            in_cinematic: false,
            exhaustion: [
                (1, ("Rested".to_string(), 2.0)),
                (2, ("Normal".to_string(), 1.0)),
                (3, ("XXXTired".to_string(), 1.0)),
                (4, ("XXXTired".to_string(), 0.5)),
                (5, ("XXXExhausted".to_string(), 0.25)),
            ]
            .into_iter()
            .collect(),
            player_combat_stats: None,
            pet_combat_stats: None,
            inventory_slots: Default::default(),
            inventory_alerts: [0; 12],
            paperdoll_yaw: 0.0,
            inventory_uses: Vec::new(),
            weapon_enchants: [None; 2],
            inspect: None,
            inspect_notifies: Vec::new(),
            inspect_clear: false,
            inspect_yaw: 0.0,
            pet_paperdoll_yaw: 0.0,
            dressup_intents: Vec::new(),
            dressup_yaw: 0.0,
            unit_reach: HashMap::new(),
            chat_input: Vec::new(),
            skills: skills::SkillsState::default(),
            skills_groups: Vec::new(),
            skills_collapsed: HashSet::new(),
            skills_selected: None,
            skill_abandons: Vec::new(),
            reputation: reputation::ReputationState::default(),
            reputation_groups: Vec::new(),
            reputation_collapsed: HashSet::new(),
            reputation_selected: None,
            reputation_sends: Vec::new(),
        }
    }

    /// Mint (or fetch) the stable id of a frame handle.
    pub(crate) fn frame_id(&mut self, h: FrameHandle) -> u32 {
        if let Some(&id) = self.frame_to_id.get(&h) {
            return id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.frame_to_id.insert(h, id);
        self.id_to_frame.insert(id, h);
        // A fresh id means a frame just entered the layout graph (`frame_to_id` is the resolve's
        // roster) — the mint is the one chokepoint every creation path funnels through (bindings,
        // the XML loader, anchor-target resolution), so the tier-1 gate can't miss a birth.
        self.touch_layout();
        id
    }

    /// Mint (or fetch) the stable id of a region handle.
    pub(crate) fn region_id(&mut self, h: RegionHandle) -> u32 {
        if let Some(&id) = self.region_to_id.get(&h) {
            return id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.region_to_id.insert(h, id);
        self.id_to_region.insert(id, h);
        // A fresh region id can seat the region in the resolve's external set — same birth
        // chokepoint as `frame_id`'s.
        self.touch_layout();
        id
    }

    /// A write that can move [`UiScript::resolve_layout`]'s read set happened: anchors, sizes,
    /// measured text, scale, clamp, scroll state, frame/region births, or the screen rect. Every
    /// mutating BINDING (and app-facing setter) on that set calls this — tier 1 of the resolve
    /// change gate. The resolve's own pre-pass (`tooltip::layout_tooltips`) deliberately does NOT:
    /// its writes are derived from state that already arrived through touched paths, and a
    /// self-touch would pin the gate open forever. A missed site is a silently stale layout —
    /// exactly the failure `WOW_LAYOUT_VERIFY` (on for every benilla-ui test) exists to catch
    /// loudly, by proving the fingerprint still matches whenever tier 1 claims quiet.
    pub(crate) fn touch_layout(&mut self) {
        // The conservative half of the tier-1 ledger (decision 1388): this write did not name a
        // node, so the cached graph can no longer be trusted to describe the live model and the
        // next resolve must derive it in full. Every site that has NOT been migrated to a precise
        // touch lands here, which is why migration can be incremental and a missed one is slow
        // rather than wrong.
        self.give_up_naming();
        self.bump_layout_epoch();
    }

    /// A write that moved **one region's** layout inputs and changed nothing else about the graph:
    /// its anchor OFFSETS, its explicit size, its measured extent. Not its anchor targets, not its
    /// membership, not its liveness — those move edges or the roster, and belong on
    /// [`Self::touch_layout`].
    ///
    /// Falls back to the conservative touch whenever it cannot prove the region is a node of the
    /// **cached** graph: no minted id, an id past the roster's arrays, or an id the last derive
    /// left unmapped (an anchor-less region, a region born since). That fallback is what makes the
    /// call safe to reach for — the worst a mistaken one can do is derive the graph again.
    pub(crate) fn touch_layout_region(&mut self, rh: RegionHandle) {
        match self.region_to_id.get(&rh) {
            Some(&id) => self.touch_layout_node(id),
            None => self.touch_layout(),
        }
    }

    /// [`Self::touch_layout_region`]'s frame twin — an anchor offset, a width/height, and nothing
    /// that moves an edge or the roster.
    pub(crate) fn touch_layout_frame(&mut self, h: FrameHandle) {
        match self.frame_to_id.get(&h) {
            Some(&id) => self.touch_layout_node(id),
            None => self.touch_layout(),
        }
    }

    /// An anchor RETARGET that names its node: the write moved node `h`'s (or `rh`'s) set of
    /// anchor TARGETS, and hands over both target lists so the cached graph's edges can be
    /// re-pointed instead of thrown away (decision 1625, extending 1388).
    ///
    /// `old`/`new` are the node's FULL anchor-target lists either side of the write. Falls back to
    /// the conservative touch whenever the scope refuses the patch — the same "worst case is a
    /// derivation" safety the other precise touches have.
    pub(crate) fn touch_layout_retarget_frame(&mut self, h: FrameHandle, old: &[u32], new: &[u32]) {
        match self.frame_to_id.get(&h).copied() {
            // Already conservative ⇒ the next resolve rebuilds every edge from scratch anyway, so
            // patching them would be work with no reader.
            Some(id)
                if self.layout_touched.is_some() && self.layout_scope.retarget(id, old, new) =>
            {
                self.touch_layout_node(id);
            }
            _ => self.touch_layout(),
        }
    }

    /// [`Self::touch_layout_retarget_frame`]'s region twin.
    pub(crate) fn touch_layout_retarget_region(
        &mut self,
        rh: RegionHandle,
        old: &[u32],
        new: &[u32],
    ) {
        match self.region_to_id.get(&rh).copied() {
            Some(id)
                if self.layout_touched.is_some() && self.layout_scope.retarget(id, old, new) =>
            {
                self.touch_layout_node(id);
            }
            _ => self.touch_layout(),
        }
    }

    /// A write that could move region `rh`'s measure key — its text, its font face/height/
    /// outline, its text height, its explicit size (the wrap width) — names the region on the
    /// measure ledger ([`Self::measure_dirty`]). Push-only and unfiltered: non-FontStrings and
    /// same-value writes cost one `Vec` push here and one staleness probe at the next sweep,
    /// which is the price of keeping every enrolled site a one-liner.
    pub(crate) fn touch_measure(&mut self, rh: RegionHandle) {
        if let Some(list) = &mut self.measure_dirty {
            list.push(rh);
        }
    }

    /// A write that moves measure keys WITHOUT being able to name the regions — a font-object
    /// edit fanning out to every inheriting region, a scale change propagating down a subtree,
    /// [`super::UiScript::invalidate_text_measures`]. The next sweep walks the whole roster,
    /// exactly as every sweep did before the ledger existed. All human-rate paths.
    pub(crate) fn touch_measure_all(&mut self) {
        self.measure_dirty = None;
    }

    /// Name a node **without opening tier 1** — for the resolve's own pre-pass
    /// (`tooltip::layout_tooltips`), which writes layout inputs derived from state that already
    /// arrived through touched paths.
    ///
    /// It deliberately does not call [`Self::touch_layout`]: bumping the epoch from inside a
    /// resolve would pin the gate open forever (the pre-pass runs on every let-through resolve, so
    /// it would re-dirty the very resolve it is running in). But the ledger still has to hear about
    /// it, or an incremental pass would inherit a node whose hash the pre-pass moved and no write
    /// site named — the one shape that could ship a stale rect. Naming it is free: the hash it
    /// recomputes is unchanged on the frames the pre-pass writes idempotently, which is nearly all
    /// of them, so no dirty seed comes of it.
    pub(crate) fn note_layout_frame_write(&mut self, h: FrameHandle) {
        if self.layout_touched.is_some() {
            match self.frame_to_id.get(&h) {
                Some(&id) if self.layout_scope.has_node(id) => {
                    self.layout_touched
                        .as_mut()
                        .expect("checked above")
                        .push(id);
                }
                _ => self.give_up_naming(),
            }
        }
    }

    /// [`Self::note_layout_frame_write`]'s region twin.
    pub(crate) fn note_layout_region_write(&mut self, rh: RegionHandle) {
        if self.layout_touched.is_some() {
            match self.region_to_id.get(&rh) {
                Some(&id) if self.layout_scope.has_node(id) => {
                    self.layout_touched
                        .as_mut()
                        .expect("checked above")
                        .push(id);
                }
                _ => self.give_up_naming(),
            }
        }
    }

    /// Name `id` as the only thing this write moved — if the cached graph has a node for it and no
    /// earlier write in this frame already gave up on naming.
    fn touch_layout_node(&mut self, id: u32) {
        let in_graph = self.layout_scope.has_node(id);
        match &mut self.layout_touched {
            // Already conservative: a precise touch cannot un-say an imprecise one.
            None => {}
            Some(_) if !in_graph => self.give_up_naming(),
            Some(list) => list.push(id),
        }
        self.bump_layout_epoch();
    }

    /// Give up naming what this write moved: the cached graph can no longer be trusted, so the
    /// next resolve derives it in full. **Every** site that sets [`Self::layout_touched`] to
    /// `None` goes through here, because that transition is the only event worth a name.
    ///
    /// `WOW_LAYOUT_DERIVE_TRACE=<secs>:<n>` — backtrace the first `n` of them after `secs`. 1388
    /// left `layout_derivations` as "the counter to watch", and a non-zero reading as the proof
    /// that "a write site somewhere fell back to the conservative touch" — but gave nobody a way
    /// to find out WHICH, and `WOW_LAYOUT_TOUCH_TRACE` cannot answer it: it fires on every touch,
    /// and the precise ones outnumber the poisoning one by hundreds to one. Only the Some → None
    /// EDGE is printed: once a frame has given up, the fallbacks behind it are consequences, not
    /// causes, and printing them buries the one line that matters.
    fn give_up_naming(&mut self) {
        if self.layout_touched.is_some() && derive_trace_armed() {
            eprintln!(
                "[layout-derive] the ledger gave up naming, at:\n{}",
                std::backtrace::Backtrace::force_capture()
            );
        }
        self.layout_touched = None;
    }

    /// Tier 1's counter, shared by every touch above. Bumping it is what re-opens the gate; which
    /// of [`Self::layout_touched`]'s two states the caller leaves behind is what decides how much
    /// the re-opened resolve has to derive.
    fn bump_layout_epoch(&mut self) {
        self.layout_epoch = self.layout_epoch.wrapping_add(1);
        // `WOW_LAYOUT_TOUCH_TRACE=<n>` — name the epoch's per-frame toucher: print a backtrace
        // for the first n touches (a mechanism probe, meaningful in a debuginfo build). Built
        // because the SW meters read `resolve≈240 µs, solves=0` on every steady frame: the
        // tier-1 gate is being defeated by a toucher whose writes never move a fingerprint,
        // and 48 call sites is too many to tag by hand.
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::OnceLock;
        // Spec `<secs>:<n>`: arm after `secs` (past UI load + settle, so the trace names the
        // STEADY toucher, not the thousands of legitimate load-time touches), then print `n`.
        static SPEC: OnceLock<Option<(std::time::Instant, f64, AtomicU32)>> = OnceLock::new();
        let spec = SPEC.get_or_init(|| {
            let v = std::env::var("WOW_LAYOUT_TOUCH_TRACE").ok()?;
            let (secs, n) = v.split_once(':')?;
            Some((
                std::time::Instant::now(),
                secs.trim().parse().ok()?,
                AtomicU32::new(n.trim().parse().ok()?),
            ))
        });
        if let Some((t0, delay, left)) = spec {
            if t0.elapsed().as_secs_f64() >= *delay
                && left
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_sub(1))
                    .is_ok()
            {
                eprintln!(
                    "[layout-touch] epoch={} at:\n{}",
                    self.layout_epoch,
                    std::backtrace::Backtrace::force_capture()
                );
            }
        }
    }
}

/// `WOW_LAYOUT_DERIVE_TRACE=<secs>:<n>`'s latch — arm after `secs` (past UI load, whose thousands
/// of legitimate births are all conservative by construction and would eat the budget), then allow
/// `n` prints. Same spec shape as `WOW_LAYOUT_TOUCH_TRACE`, deliberately: one thing to remember.
fn derive_trace_armed() -> bool {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::OnceLock;
    static SPEC: OnceLock<Option<(std::time::Instant, f64, AtomicU32)>> = OnceLock::new();
    let Some((t0, delay, left)) = SPEC.get_or_init(|| {
        let v = std::env::var("WOW_LAYOUT_DERIVE_TRACE").ok()?;
        let (secs, n) = v.split_once(':')?;
        Some((
            std::time::Instant::now(),
            secs.trim().parse().ok()?,
            AtomicU32::new(n.trim().parse().ok()?),
        ))
    }) else {
        return false;
    };
    t0.elapsed().as_secs_f64() >= *delay
        && left
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_sub(1))
            .is_ok()
}
