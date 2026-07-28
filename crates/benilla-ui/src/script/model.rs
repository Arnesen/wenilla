use std::collections::{HashMap, HashSet};

use crate::layout::{LayoutInput, LayoutSolver, Rect};
use crate::widget::{FrameHandle, RegionHandle, WidgetArena};

use super::{
    backdrop, bank, char_stats, container, craft, cursor, death, duel, gossip, inspect, item_text,
    loot, loot_roll, mail, merchant, party, quest, quest_log, session, skills, slider, social,
    spellbook, taxi, trade, tradeskill, trainer, ActionSlot, AuraState, FontObject,
    ItemTemplateView, PlayerReqState, RegionData, ScriptValue, SoundRequest, UnitState,
};

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The Rust-side model (plain data; lives in lua.app_data)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The Rust-side model behind the Lua VM — the arena, the layout inputs + resolved rects, the
/// id↔handle bijection, region visuals, and the event/script registrations. Held in `lua.app_data`
/// (interior-mutable) so callbacks reach it; contains **no** mlua handles (the MAXCSTACK discipline).
pub(crate) struct Model {
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
    /// paying a `u64` compare instead of fingerprinting ~2k inputs. An idempotent-write burst (the
    /// bag-hover re-enter loop) bumps this every frame and falls through to the fingerprint, which
    /// absorbs it exactly as before — the tiers are complementary, not redundant.
    pub(crate) layout_epoch: u64,
    /// The [`Self::layout_epoch`] value the last SETTLED resolve closed on — one whose
    /// fingerprint matched, proving the input set (seeds included) has stopped moving. `None`
    /// forces the next resolve through tier 1: the initial state, a cycle-bailed pass, and every
    /// real solve (whose stored fingerprint answers for now-outgrown seeds — layout.rs, "Tier 1
    /// closes ONLY on a settled frame").
    pub(crate) layout_epoch_resolved: Option<u64>,
    /// How many times the change gate has LET A RESOLVE THROUGH. `UiScript::resolve` is called
    /// unconditionally every frame, so the gap between call count and this is the gate's whole
    /// value; it is also what lets a test assert that a no-op resolve really was one, rather than
    /// merely producing the same rects. Counts the gate's decision, so it reads the same with the
    /// `WOW_LAYOUT_VERIFY` self-check on (which re-runs skipped resolves) as with it off.
    pub(crate) layout_solves: u64,
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
    /// The named virtual **Font object** registry (`<Font name=…>` → resolved paint), keyed by name.
    /// Populated by the loader as it walks top-level `<Font>` nodes; read by `SetFontObject` and by
    /// FontString `inherits=` resolution. Data only — no Lua handles (MAXCSTACK discipline).
    pub(crate) font_objects: HashMap<String, FontObject>,
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
    /// Per-button, the frame a mouse-down last captured (`button name → frame`), for the `OnClick`
    /// same-frame press+release test in [`UiScript::mouse_button`]. Keyed by button so a `LeftButton`
    /// press is not cleared by a `RightButton` release.
    pub(crate) mouse_down_on: HashMap<String, FrameHandle>,

    /// Script errors collected from `pcall`'d handlers (never panics, never prints — decision 0068).
    pub(crate) errors: Vec<String>,
    /// Non-fatal warnings surfaced to the host (e.g. `CreateFrame`'s ignored `inherits=` template).
    pub(crate) warnings: Vec<String>,
    /// The screen-root rect (`[bottom, left, top, right]`), the anchor base for top-level frames.
    pub(crate) screen: Rect,

    /// The per-unit-token game-state snapshot the app pushes in each frame (`"player"`, `"target"`,
    /// …), read by the `Unit*` Lua bindings ([`unit`]). Plain data — the engine never touches the
    /// ECS/net; the app's feed writes here via [`UiScript::set_unit`] (decision 0068 §3). A token
    /// absent from the map is a non-existent unit (`UnitExists` false, numbers `0`/nil).
    pub(crate) units: HashMap<String, UnitState>,
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
    /// Set when `ClearTarget()` fired with a live target — the ESC chain's LAST leg
    /// (`ToggleGameMenu`'s order: the clear runs only when nothing earlier ate the press).
    /// Drained by [`super::UiScript::take_target_clear`]; the app commits the deselect ([`unit`]).
    pub(crate) target_clear: bool,

    /// The party/raid roster snapshot the app pushes (`GroupState`'s merged view, decision 0434
    /// §2) — `GetNumPartyMembers`/`GetPartyLeaderIndex`/`GetLootMethod`/… read it ([`party`]).
    /// `PartyState::default()` = not in a group (every getter reports the solo-player shape).
    /// Per-member game state rides the `units` map above, under `"party1"`..`"party4"` tokens.
    pub(crate) party: party::PartyState,
    /// Party/loot intents (`AcceptGroup`/`InviteToParty`/`SetLootMethod`/…) queued since the
    /// app's last [`super::UiScript::take_party_requests`] drain — the outbound seam ([`party`]).
    pub(crate) party_requests: Vec<party::PartyRequest>,
    /// The social snapshot the app pushes (friends, ignores, the last `/who` — decision 0668):
    /// `GetNumFriends`/`GetFriendInfo`/`GetWhoInfo`/… read it ([`social`]). Already
    /// display-resolved (names, class/zone names) because the reference resolves them
    /// engine-side too.
    pub(crate) social: social::SocialState,
    /// Social intents (`AddFriend`/`RemoveFriend`/`SendWho`/…) queued since the app's last
    /// [`super::UiScript::take_social_requests`] drain — the outbound seam ([`social`]).
    pub(crate) social_requests: Vec<social::SocialRequest>,
    /// Whisper targets `ChatFrame_SendTell` queued since the app's last
    /// [`super::UiScript::take_tell_requests`] drain — the app opens its chat edit box prefilled
    /// `/w <name> ` (the unit popup's WHISPER action; [`party`] registers the global).
    pub(crate) tell_requests: Vec<String>,
    /// Duel intents (`AcceptDuel`/`CancelDuel`/`StartDuel*`) queued since the app's last
    /// [`super::UiScript::take_duel_requests`] drain — the outbound seam ([`duel`]). There is no
    /// duel *snapshot* beside it: everything the UI reads arrives as event arguments.
    pub(crate) duel_requests: Vec<duel::DuelRequest>,
    /// Session-exit intents (`Logout`/`Quit`/`CancelLogout`/`ForceQuit`) queued since the app's
    /// last [`super::UiScript::take_session_requests`] drain — the outbound seam ([`session`]).
    /// Snapshot-less like the duel queue above: what the UI reads back (the camp/quit countdown)
    /// arrives as the `PLAYER_CAMPING`/`PLAYER_QUITING`/`LOGOUT_CANCEL` events, not as state.
    pub(crate) session_requests: Vec<session::SessionRequest>,
    /// PvP-flag toggles (`TogglePVP`) queued since the app's last
    /// [`super::UiScript::take_pvp_toggles`] drain — the outbound seam ([`pvp`]). A count, not a
    /// payload: `CMSG_TOGGLE_PVP` carries no body.
    pub(crate) pvp_toggles: u32,

    /// Sounds queued by the Lua `PlaySound`/`PlaySoundFile` bindings since the app's last
    /// [`UiScript::take_sounds`] drain — the outbound Lua→app intent seam ([`sound`]).
    pub(crate) sound_queue: Vec<SoundRequest>,

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
    /// Spell ids `CastSpell` queued since the app's last [`super::UiScript::take_spell_casts`]
    /// drain.
    pub(crate) spell_casts: Vec<u32>,
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

    /// The player's talent pages (decision 0304) — tabs + per-tab talents the
    /// `GetNumTalentTabs`/`GetTalentInfo`/… bindings read ([`super::talent`]). Durable player
    /// state like `spellbook`; "no talents yet" is simply empty.
    pub(crate) talents: super::talent::TalentUiState,
    /// `LearnTalent(tab, index)` clicks queued since the app's last
    /// [`super::UiScript::take_talent_learns`] drain.
    pub(crate) talent_learns: Vec<(u32, u32)>,

    /// The stance/shapeshift bar's form list (bar order) the app pushes — the
    /// `GetNumShapeshiftForms`/`GetShapeshiftFormInfo` family reads it ([`super::shapeshift`]).
    /// Durable player state like `spellbook`; "no forms" (a mage) is simply empty.
    pub(crate) shapeshift_forms: Vec<super::shapeshift::StoredShapeshiftForm>,
    /// Form spell ids `CastShapeshiftForm` queued since the app's last
    /// [`super::UiScript::take_shapeshift_casts`] drain.
    pub(crate) shapeshift_casts: Vec<u32>,

    /// The per-bag container snapshot (keyed by live-API bag id, 0 = backpack) the app pushes,
    /// and the `UseContainerItem` intents it drains — the container seam ([`container`]).
    pub(crate) containers: HashMap<i64, container::ContainerState>,
    pub(crate) container_uses: Vec<(i64, u32)>,
    /// Per-(bag, slot) use-cooldowns in `GetTime` seconds `(start, duration, enabled)` — stamped
    /// at `set_container` push time (the action-state pattern), read by
    /// `GetContainerItemCooldown`.
    pub(crate) container_cooldowns: HashMap<(i64, u32), (f64, f64, bool)>,
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
    pub(crate) cursor_grid_shown: bool,
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
    /// The armed item-targeted cast (the CraftFrame enchant pick, 0437 phase 3): while set, a
    /// bag click queues into [`Self::item_picks`] instead of the cursor gesture.
    pub(crate) item_pick_armed: bool,
    /// `(bag, slot)` clicks consumed by the armed item pick, drained by the app.
    pub(crate) item_picks: Vec<(i64, u32)>,
    /// `(bag, slot, count)` triples queued by `DeleteCursorItem` (`count == 0` = the whole
    /// stack) — the popup-confirmed destroy (decision 0216 §3), drained by the app into
    /// `CMSG_DESTROYITEM`.
    pub(crate) container_destroys: Vec<(i64, u32, u32)>,
    /// The Lua-set **displayed-cursor override** (`ShowContainerSellCursor`/`ShowMerchantSellCursor`/
    /// `ShowBuybackSellCursor`/`ShowInspectCursor` set it, `ResetCursor` clears it; [`container`] +
    /// [`merchant`]) — the single "displayed mode" the real client keeps at `0xbe2c2c`, restored to
    /// the base (world-classifier) mode by `ResetCursor`. `None` = no override (show the base mode).
    pub(crate) ui_cursor: Option<container::UiCursorMode>,
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
    /// The in-flight Slider thumb drag: set when a LeftButton press lands on a Slider's thumb, held
    /// until release / pointer-leave (decision 0250 §5, the engine's C++-equivalent thumb drag —
    /// like a scrollbar dragging in the real client, no Lua involved). `None` between drags
    /// ([`slider::SliderDrag`]).
    pub(crate) slider_drag: Option<slider::SliderDrag>,

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

    /// The open loot's row snapshot the app pushes (`None` = no loot open), the `LootSlot` row-pick
    /// intents it drains, and whether `CloseLoot` was called — the loot seam ([`loot`]).
    pub(crate) loot: Option<loot::LootState>,
    pub(crate) loot_picks: Vec<u32>,
    pub(crate) loot_close: bool,

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

    /// The paper doll's combat-stats snapshot (`None` until the app's feed lands), the
    /// equipment/ammo slot views, and the model pane's persistent bake yaw — the character-window
    /// seam ([`char_stats`], decision 0208 §3). All player-only data: the backing descriptor
    /// fields are PRIVATE/OWNER_ONLY.
    pub(crate) player_combat_stats: Option<char_stats::PlayerCombatStats>,
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
    /// Unit token → squared distance from the player, for every popup token the app resolved to a
    /// live inspectable player this frame — the input to both verified range predicates
    /// (`CanInspect`, `CheckInteractDistance`). Absent token = in range (see
    /// [`UiScript::set_inspect_reach`]).
    pub(crate) inspect_reach: HashMap<String, f64>,

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

    /// Chat lines the input EditBox submitted (its `OnEnterPressed` → the `SubmitChatInput` Lua
    /// binding) since the app's last [`UiScript::take_chat_input`] drain — the outbound Lua→app seam
    /// for the chat input (the twin of `loot_picks`). The app routes each through its slash-command
    /// parser into a `CMSG_MESSAGECHAT`/`CMSG_TEXT_EMOTE`.
    pub(crate) chat_input: Vec<String>,

    /// The world-map seam ([`worldmap`](super::worldmap)): the pushed catalog/feed + the
    /// engine-owned selection.
    pub(crate) worldmap: super::worldmap::WorldMapState,
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
    /// The modifier-key mirror `(shift, ctrl, alt)` behind `IsShiftKeyDown`/`IsControlKeyDown`/
    /// `IsAltKeyDown` — pushed by the app's input pass ([`UiScript::set_modifiers`]) BEFORE the
    /// frame's mouse events, so a click handler's fork reads the state at click time.
    pub(crate) modifiers: (bool, bool, bool),
}

impl Model {
    pub(crate) fn new() -> Model {
        Model {
            arena: WidgetArena::new(),
            layout_inputs: HashMap::new(),
            solver: LayoutSolver::new(),
            layout_fingerprint: None,
            layout_epoch: 0,
            layout_epoch_resolved: None,
            layout_solves: 0,
            resolved: HashMap::new(),
            link_spans: HashMap::new(),
            chat_tab: false,
            region_data: HashMap::new(),
            backdrops: HashMap::new(),
            font_objects: HashMap::new(),
            region_resolved: HashMap::new(),
            next_id: 1,
            id_to_frame: HashMap::new(),
            frame_to_id: HashMap::new(),
            id_to_region: HashMap::new(),
            region_to_id: HashMap::new(),
            region_names: HashMap::new(),
            scripts: HashMap::new(),
            event_to_frames: HashMap::new(),
            frame_events: HashMap::new(),
            focused_editbox: None,
            mouseover: None,
            mouse_down_on: HashMap::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            // Classic Era's UIParent virtual space is 1024×768-ish; a sensible default the host can
            // override with `set_screen_size`. y-up: [bottom, left, top, right].
            screen: Rect::new(0.0, 0.0, 768.0, 1024.0),
            units: HashMap::new(),
            auras: HashMap::new(),
            cancel_aura_requests: Vec::new(),
            tracking: None,
            target_requests: Vec::new(),
            target_clear: false,
            party: party::PartyState::default(),
            party_requests: Vec::new(),
            social: social::SocialState::default(),
            social_requests: Vec::new(),
            tell_requests: Vec::new(),
            duel_requests: Vec::new(),
            session_requests: Vec::new(),
            pvp_toggles: 0,
            sound_queue: Vec::new(),
            actions: HashMap::new(),
            action_states: HashMap::new(),
            bonus_bar_offset: 0,
            action_uses: Vec::new(),
            action_sets: Vec::new(),
            ui_errors: Vec::new(),
            spellbook: spellbook::SpellBookState::default(),
            spell_casts: Vec::new(),
            casting: false,
            spell_stop: false,
            talents: super::talent::TalentUiState::default(),
            talent_learns: Vec::new(),
            shapeshift_forms: Vec::new(),
            shapeshift_casts: Vec::new(),
            containers: HashMap::new(),
            container_uses: Vec::new(),
            container_cooldowns: HashMap::new(),
            cursor: None,
            cursor_grid_shown: false,
            world_pick: cursor::WorldPick::default(),
            container_moves: Vec::new(),
            container_repairs: Vec::new(),
            item_pick_armed: false,
            item_picks: Vec::new(),
            container_destroys: Vec::new(),
            ui_cursor: None,
            container_autoequips: Vec::new(),
            drag_registered: HashMap::new(),
            drag: None,
            slider_drag: None,
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
            loot_close: false,
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
            item_templates: HashMap::new(),
            item_sets: HashMap::new(),
            item_set_asks: HashSet::new(),
            item_stat_asks: HashSet::new(),
            player_req: PlayerReqState::default(),
            spell_tooltips: HashMap::new(),
            spell_tooltip_asks: HashSet::new(),
            quest_log_collapses: Vec::new(),
            quest_log_watched: Vec::new(),
            worldmap: super::worldmap::WorldMapState::default(),
            pending_events: Vec::new(),
            cursor_pos: (0.0, 0.0),
            modifiers: (false, false, false),
            money: 0,
            net_latency_ms: 0,
            player_xp: 0,
            player_next_level_xp: 0,
            player_combat_stats: None,
            inventory_slots: Default::default(),
            inventory_alerts: [0; 12],
            paperdoll_yaw: 0.0,
            inventory_uses: Vec::new(),
            inspect: None,
            inspect_notifies: Vec::new(),
            inspect_clear: false,
            inspect_yaw: 0.0,
            inspect_reach: HashMap::new(),
            chat_input: Vec::new(),
            skills: skills::SkillsState::default(),
            skills_groups: Vec::new(),
            skills_collapsed: HashSet::new(),
            skills_selected: None,
            skill_abandons: Vec::new(),
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
        self.layout_epoch = self.layout_epoch.wrapping_add(1);
    }
}
