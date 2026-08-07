//! The context-sensitive **world cursor** — the classifier half (wow-re cursor RE, note
//! `ui/scratch/cursor-system.md`, §3 per-bit service table + §5 gates, all byte-verified).
//!
//! Each frame, the hovered unit resolves to a [`CursorKind`] the way the real client's
//! `CGWorldFrame` classifier does (`0x4828d0` → unit branch `0x482200`):
//! - an **interactable NPC** (service flags, not hostile) → the `UNIT_NPC_FLAGS` service ladder
//!   (`0x482336..0x4824e3`), lowest bit wins — the full statically-unrolled map is
//!   [`service_cursor`]. Notably vendor → **Pickup** (the pouch), innkeeper → **Interact**,
//!   banker/auctioneer → **Buy**; REPAIR (0x4000) is *never consulted* — a repair-only unit falls
//!   through to the attack/clear leg (real repairers all carry VENDOR too).
//! - otherwise **loot / skin / attack** keyed on state: dead + `UNIT_DYNFLAG_LOOTABLE` →
//!   **Pickup or LootAll** — the loot leg's mode is `8 + (keyDown(0) ? 8 : 0)` (`0x48252c`):
//!   the triple-pouch LootAll(16) while the auto-loot modifier is held, generalized since 0961's
//!   auto-loot to the EFFECTIVE state (`autoLootDefault` XOR shift — [`loot_cursor`]); dead +
//!   `UNIT_FLAG_SKINNABLE` → Skin; alive and attackable → Attack.
//! - **`Unable*` (grayed) by a different gate per mode** (byte-verified): NPC services gray beyond
//!   **5.5556 yd** (`0x482320`); attack beyond a fixed **10.45 yd** (`0x4826a7`); skin outside the
//!   melee interact reach `max(reachA + reachB + 1.333, 5.0)` — the 5.0 is a **floor**, not a cap
//!   (`0x6e3480` for skin; the same formula inline in `CanLootNow 0x5ec110` @ `0x5ec142..0x5ec1c8`
//!   for loot, center-to-center, boundary-inclusive — director-measured ~5 yd, byte-confirmed).
//!   Loot *rights* never gray — they gate whether the loot cursor shows at all; the mid-loot state
//!   block and the open-loot-window able-override are not modeled.
//!
//! *Interim*: the reference's interactability predicate (`CGUnit::CanInteract 0x606880`) isn't
//! fully derived; we approximate it as "has service flags and isn't attack-worthy (reaction ≥
//! neutral)". Attackability approximates the PvP/attack matrix as "reaction rank ≤ neutral" —
//! the same approximation the ring's player branch documents. (The questgiver bit's own
//! quest-status gate, `0x5df490`, and the auto-loot Pickup/LootAll split, `0x41f8f0`, both used
//! to be listed here as unmodelled; both are modelled now — [`questgiver_has_quest`],
//! [`loot_cursor`].)

use bevy::prelude::*;

use benilla_protocol::EntityKind;

use crate::net::{NetEntity, ObjectStore, Reputations, SelfPlayer};

use super::ring::{ring_reaction, Factions};
use super::{go_is_nearest, Hovered, HoveredObject};

/// The blp-name set of world cursor modes benilla can currently trigger (named off the client's own
/// mode table `0x853b8c` — the strings are the `Interface\Cursor\<Name>.blp` stems).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CursorKind {
    Point,
    Attack,
    Speak,
    /// Pickup(8) — the vendor's pouch AND the loot leg's base mode.
    Pickup,
    /// LootAll(16) — the loot leg's triple pouch while the EFFECTIVE auto-loot is on
    /// (`autoLootDefault` XOR the held shift, 0961's own rule — in 1.12 the held key alone was
    /// the whole mechanism, `8 + (keyDown(0) ? 8 : 0)` @ `0x48252c`). Loot only: the vendor
    /// pouch never triples.
    LootAll,
    /// Interact(5) — the generic gear. A GameObject base type's cursor when it carries no
    /// data-named cursor (a door, button, chest, keyed/keyless lock, fishing, …); also the
    /// innkeeper service.
    Interact,
    Buy,
    /// Inspect(7) — the magnifier. The UI's Ctrl-hover cursor (`ShowInspectCursor`, wow-re
    /// cursor-system.md §7, overlaid by [`crate::cursor`]) **and** the world cursor over a
    /// readable TEXT(9) GameObject plaque (§4).
    Inspect,
    Trainer,
    Taxi,
    Skin,
    /// Repair(17) — never set by the world classifier (the ladder skips the REPAIR bit); it is
    /// the UI's repair-mode base cursor (`ShowRepairCursor`'s locked mode, wow-re
    /// repair-machinery.md), overlaid by [`crate::cursor`].
    Repair,
    /// Mail(15) — a MAILBOX(19) / RITUAL(18) / type-28 GameObject (wow-re cursor-system.md §4:
    /// the shared `0x5f6840`/`0x5f6e30` behavior). The mailbox's own cursor, not the gear.
    Mail,
    /// Mine(11) — a GameObject whose lock's first `LockType` is Mining (3). A LockType.dbc
    /// data-named cursor (§4), resolved off the GO's lock, not a fixed type.
    Mine,
    /// GatherHerbs(13) — a GameObject whose lock's first `LockType` is Herbalism (2). Also a
    /// LockType.dbc data-named cursor.
    GatherHerbs,
    /// PickLock(14) — a GameObject whose lock's first `LockType` is Pick Lock (`LockType.Id == 1`).
    /// The one data-named GO cursor that is **never grayed** (§4: `LockType.Id == 1` skips the
    /// usable gate), since any rogue can attempt the lock.
    PickLock,
    /// Cast(2) — the spell-targeting cursor (wow-re cursor-system.md §5, VERIFIED law): while a
    /// spell awaits a target, dispatcher step 2 pre-empts the WHOLE object classifier with
    /// Cast/UnableCast. Never set by the classifier here — it is [`crate::cursor`]'s
    /// armed-enchant-pick overlay (the one spell-targeting state benilla ships).
    Cast,
}

impl CursorKind {
    /// The cursor's BLP stem in `Interface\Cursor\` (the client's mode-name table strings).
    fn name(self) -> &'static str {
        match self {
            CursorKind::Point => "Point",
            CursorKind::Attack => "Attack",
            CursorKind::Speak => "Speak",
            CursorKind::Pickup => "Pickup",
            CursorKind::LootAll => "LootAll",
            CursorKind::Interact => "Interact",
            CursorKind::Buy => "Buy",
            CursorKind::Inspect => "Inspect",
            CursorKind::Trainer => "Trainer",
            CursorKind::Taxi => "Taxi",
            CursorKind::Skin => "Skin",
            CursorKind::Repair => "Repair",
            CursorKind::Mail => "Mail",
            CursorKind::Mine => "Mine",
            CursorKind::GatherHerbs => "GatherHerbs",
            CursorKind::PickLock => "PickLock",
            CursorKind::Cast => "Cast",
        }
    }
}

/// The resolved world cursor for this frame — what the OS cursor should show. Written by
/// [`classify_cursor`] (after hover), read by [`crate::cursor`]'s platform drivers. `unable`
/// selects the grayed `Unable<Name>` twin (`mode + 20` in the client's enum — out of range).
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct WorldCursor {
    pub(crate) kind: CursorKind,
    pub(crate) unable: bool,
}

impl Default for WorldCursor {
    fn default() -> Self {
        Self {
            kind: CursorKind::Point,
            unable: false,
        }
    }
}

impl WorldCursor {
    /// The BLP file stem (`Attack` / `UnableAttack` / …) — the key the platform cursor caches use.
    pub(crate) fn stem(&self) -> String {
        if self.unable && self.kind != CursorKind::Point {
            format!("Unable{}", self.kind.name())
        } else {
            self.kind.name().to_string()
        }
    }
}

/// Vanilla `UNIT_NPC_FLAGS` bits (vmangos `UnitDefines.h`, 1.12 values — later expansions differ).
/// REPAIR (0x4000) exists but the classifier never consults it (falls `je 0x4826cb`).
/// `pub(super)` so the right-click dispatch ([`super::click`]) reuses BANKER to split the shared
/// Buy cursor kind (banker vs auctioneer) without a duplicate table (decision 0604).
pub(super) mod npc_flags {
    pub const GOSSIP: u32 = 0x1;
    pub const QUESTGIVER: u32 = 0x2;
    pub const VENDOR: u32 = 0x4;
    pub const FLIGHTMASTER: u32 = 0x8;
    pub const TRAINER: u32 = 0x10;
    pub const SPIRITHEALER: u32 = 0x20;
    pub const SPIRITGUIDE: u32 = 0x40;
    pub const INNKEEPER: u32 = 0x80;
    pub const BANKER: u32 = 0x100;
    pub const PETITIONER: u32 = 0x200;
    pub const TABARDDESIGNER: u32 = 0x400;
    pub const BATTLEMASTER: u32 = 0x800;
    pub const AUCTIONEER: u32 = 0x1000;
    pub const STABLEMASTER: u32 = 0x2000;
}

/// `UNIT_FLAG_SKINNABLE` in `UNIT_FIELD_FLAGS` (vanilla).
const UNIT_FLAG_SKINNABLE: u32 = 0x0400_0000;

/// NPC-service range gate: gray beyond 5.5556 yd (squared 30.864 — the client's `0xb4b32c` cell,
/// `[0x804328]²`; checked at `0x482320`, boundary-inclusive). Shared with the merchant window's
/// out-of-range auto-close ([`crate::ui_merchant`]) so the window closes exactly where the cursor
/// says the vendor is out of service.
pub(crate) const SERVICE_RANGE_SQ: f32 = 30.864;
/// Attack's fixed range gate: gray beyond 10.45 yd (squared 109.2025, const `0x80447c`, checked at
/// `0x4826a7` — *not* the melee reach; that gates skin/insignia only).
const ATTACK_RANGE_SQ: f32 = 109.2025;
/// The melee interact reach offset + floor (`0x80b058` / `0x80a1e8`): reach = **max**(rA + rB +
/// 1.333, 5.0) — the 5.0 is a floor (`fcomp`-then-keep-larger, `0x6e35bf` / `0x5ec1a4`), so small
/// pairs always get 5 yd and large creatures reach *farther*. Gates skin (`0x6e3480`) and loot
/// (inline in `CanLootNow 0x5ec110`); distance is center-to-center, boundary-inclusive.
const MELEE_OFFSET: f32 = 1.333_33;
const MELEE_FLOOR: f32 = 5.0;

/// `GAMEOBJECT_TYPE_GENERIC` (vmangos `SharedDefines.h`) — world decoration whose highlightable
/// predicate is constant-false, so it never shows an interact cursor (wow-re cursor-system §4a).
pub(crate) const GO_TYPE_GENERIC: i32 = 5;
/// The transport family — TRANSPORT(11), MAP_OBJECT(14), MO_TRANSPORT(15): their strategy vtables'
/// highlightable slot (+0x14) is constant-false too (`32 c0 c3` — vtable dump from the 5875 binary:
/// `0x80ba58+0x14`→`0x5f5c70`, `0x80b710`/`0x80b798+0x14`→`0x5f48b0`), so a boat / zeppelin /
/// elevator never shows a cursor, tooltip, or right-click USE.
const GO_TYPE_TRANSPORT: i32 = 11;
const GO_TYPE_MAP_OBJECT: i32 = 14;
const GO_TYPE_MO_TRANSPORT: i32 = 15;
/// The **marker set** — SPELL_FOCUS(8), DUEL_ARBITER(16), FISHINGHOLE(25), AURA_GENERATOR(30).
/// Their strategy vtables are the mirror image of the transports': `+0xc` (mouseover eligibility) is
/// a constant `b0 01 c3` = `mov al,1; ret` and `+0x14` (**highlightable**) a constant
/// `32 c0 c3` = `xor al,al; ret` (wow-re `object-layer/scratch/go-render-gate.md`, "the per-type
/// handler vtables": vtables `0x80b8a8`/`0x80bb68`/`0x80bbf8`/`0x80c2a0`, `+0xc` targets
/// `0x5f57e0`/`0x5f65f0`/`0x5f6660`/`0x5f6ea0`, `+0x14` targets
/// `0x5f57f0`/`0x5f6600`/`0x5f6670`/`0x5f6eb0` — const bytes read for all four).
///
/// So they hover but never *interact*: the anvil, the forge, the campfire, the duel flag, the fishing
/// school publish a mouseover (gold-name tooltip) and then show **cursor mode 0** — the plain
/// pointer, not the Interact gear — take no `+64` brighten, and swallow the right-click
/// (`OnUse 0x5f8660` gates on the same `+0x14`). Without this arm they fell through to
/// [`highlightable_flags`], whose flag/faction terms a SPELL_FOCUS passes trivially (`flags 0x0`),
/// and every anvil in the world wore the gear.
const GO_TYPE_SPELL_FOCUS: i32 = 8;
const GO_TYPE_DUEL_ARBITER: i32 = 16;
const GO_TYPE_FISHINGHOLE: i32 = 25;
const GO_TYPE_AURA_GENERATOR: i32 = 30;
/// `GAMEOBJECT_TYPE_TEXT` (9) — a readable plaque/sign; its per-type behavior shows the **Inspect**
/// magnifier (wow-re cursor-system §4, `0x5f5890`), not the gear.
const GO_TYPE_TEXT: i32 = 9;
/// The three GameObject types that show the **Mail** cursor (wow-re cursor-system §4): RITUAL(18)
/// and MAILBOX(19) share one behavior (`0x5f6840`), and type 28 (`0x5f6e30`) resolves to Mail too.
/// Type 28 has no live 1.12 data but is included for byte-fidelity with the factory switch.
const GO_TYPE_RITUAL: i32 = 18;
/// `pub(super)` so the right-click dispatch ([`super::act_on_right_click`]) reuses the one type
/// constant to route a mailbox to the client-side window open (decision 0544), not a duplicate.
pub(super) const GO_TYPE_MAILBOX: i32 = 19;
const GO_TYPE_28: i32 = 28;
/// Interim GameObject interact-range gray (decision 0236): reuse the ~5.56 yd service reach until the
/// size-dependent GO interact distance is byte-pinned. Squared, boundary-inclusive like the unit gates.
const GO_INTERACT_RANGE_SQ: f32 = SERVICE_RANGE_SQ;
/// `GAMEOBJECT_TYPE_FISHINGNODE` (17) — the fishing bobber. Its strategy (vtable `0x80bc80`, wow-re
/// `fishing-bobber-interaction.md`) overrides exactly one consumed slot: highlightable is the
/// channel-ownership gate ([`highlightable_flags`]'s type-17 arm); everything else is the shared base.
pub(crate) const GO_TYPE_FISHINGNODE: i32 = 17;
/// The per-type interact range the shared `usable 0x5f3130` compares — the strategy ctor's
/// `[strat+0xc]` constant, **squared at the compare**, boundary-inclusive (wow-re
/// `fishing-bobber-interaction.md` §3). FISHINGNODE's ctor sets **100.0 yd** (`0x5f66b0`,
/// `[0x80b0b0]`) — the bobber is effectively un-range-gated for any real cast, never the ~5.56 yd
/// reach. Other types stay on the 0236 interim (the base default is 5.0 and several per-type ctors
/// override it — 5.5556/10.0/…; their population is a later byte-pin).
fn go_interact_range_sq(type_id: i32) -> f32 {
    if type_id == GO_TYPE_FISHINGNODE {
        100.0 * 100.0
    } else {
        GO_INTERACT_RANGE_SQ
    }
}
/// `GameObjectFlags` bits consulted by the highlightable gate (decision 0243, wow-re cursor-system §4a):
/// `0x1` IN_USE (busy) and `0x10` NO_INTERACT both suppress interaction; their union is the fast reject.
const GO_FLAG_IN_USE_OR_NO_INTERACT: u32 = 0x11;
/// `GO_FLAG_INTERACT_COND` (`0x4`) — the object is usable **only** when its per-player activate dyn-flag
/// is set. This is the quest gate: a quest chest/goober carries it, an ordinary door does not.
const GO_FLAG_INTERACT_COND: u32 = 0x4;
/// `GO_DYNFLAG_LO_ACTIVATE` (`0x1` in `GAMEOBJECT_DYN_FLAGS`) — the per-player "usable for me now" bit the
/// server sets from `GameObject::ActivateToQuest` (sparkle). Consulted only under `INTERACT_COND`.
const GO_DYNFLAG_ACTIVATE: u32 = 0x1;

/// The GameObject types whose strategy vtable **overrides** the highlightable slot (`+0x14`) with a
/// constant `xor al,al` — so `0x5f2f80` is never reached for them and they are never highlightable,
/// whatever their flags, faction or activate bit say. Three families, all byte-read off the 5875
/// vtables: GENERIC(5) decoration (`0x5f47f0`), the transports (see [`GO_TYPE_TRANSPORT`]), and the
/// marker set (see [`GO_TYPE_SPELL_FOCUS`]).
///
/// This is the whole per-type half of the predicate, kept as one named list because the reference
/// keeps it as one vtable slot: adding a type here removes its cursor, its brighten, its right-click
/// USE and its pick priority together, exactly as the binary does.
fn strategy_never_highlightable(type_id: i32) -> bool {
    matches!(
        type_id,
        GO_TYPE_GENERIC
            | GO_TYPE_TRANSPORT
            | GO_TYPE_MAP_OBJECT
            | GO_TYPE_MO_TRANSPORT
            | GO_TYPE_SPELL_FOCUS
            | GO_TYPE_DUEL_ARBITER
            | GO_TYPE_FISHINGHOLE
            | GO_TYPE_AURA_GENERATOR
    )
}

/// The client's GameObject **highlightable** predicate (decision 0243, wow-re cursor-system §4a,
/// `0x5f2f80`) over its wire flags: whether the object shows an interact cursor / is clickable at all.
/// The types whose vtable constant-falses the slot never get here at all
/// ([`strategy_never_highlightable`]); of the rest, a busy (IN_USE) or NO_INTERACT object isn't
/// highlightable, and an **INTERACT_COND** object (the quest gate) is highlightable only when the
/// server has set its per-player **activate** dyn-flag — so a quest chest sparkles and opens only once
/// the quest is held, while an ordinary door (no INTERACT_COND) is always highlightable regardless of
/// its (zero) activate bit. `usable` (lock / range / player-state → the grayed twin) rides on top and
/// is a later refinement.
///
/// `channel_owned` is the FISHINGNODE override's one input (`0x5f6710`, wow-re
/// `fishing-bobber-interaction.md` §2): whether the active player's `UNIT_FIELD_CHANNEL_OBJECT`
/// equals **this** GO's guid. The bobber is highlightable only to the player currently channeling
/// at exactly it — NOT a `CREATED_BY` compare — then tail-jumps into this shared gate. Someone
/// else's bobber (or yours after the channel drops) produces nothing at all: no cursor, no
/// tooltip/brighten (the `+0xc` thunk), and a silent dead right-click. Ignored for every other type.
fn highlightable_flags(
    type_id: i32,
    flags: u32,
    dyn_flags: u32,
    reaction: Option<u8>,
    channel_owned: bool,
) -> bool {
    if strategy_never_highlightable(type_id) {
        return false;
    }
    if type_id == GO_TYPE_FISHINGNODE && !channel_owned {
        return false;
    }
    // The FACTION term (`0x5f2f80` @ `0x5f3026/29`, decision 0764). `reaction` is the GameObject's
    // reaction **toward us** ([`go_reaction`]); the ordinary test is `> 1`, i.e. anything but
    // hostile. `None` = unresolvable (no faction catalog / our store not streamed) and passes, so a
    // data gap never blanks the world.
    let ordinary_faction_ok = |r: Option<u8>| r.is_none_or(|r| r > 1);
    if type_id == GO_TYPE_TRAP {
        // TRAP alone INVERTS it (`0x5f2fc6/c9: cmp eax,1; jg` — reject when greater): a trap is
        // highlightable only to whoever it is hostile to. Everything below still applies.
        if !reaction.is_none_or(|r| r == 1) {
            return false;
        }
    } else if !ordinary_faction_ok(reaction) {
        return false;
    }
    if flags & GO_FLAG_IN_USE_OR_NO_INTERACT != 0 {
        return false;
    }
    if flags & GO_FLAG_INTERACT_COND != 0 && dyn_flags & GO_DYNFLAG_ACTIVATE == 0 {
        return false;
    }
    true
}

/// `GAMEOBJECT_TYPE_TRAP` (6) — the one type whose faction term is inverted.
const GO_TYPE_TRAP: i32 = 6;

/// A GameObject's reaction **toward us** — the reference's `0x5f7fd0` → `0x606530` → `0x606640`
/// chain, whose direction is GO→player (`this` = the GO's own `FactionTemplate` row, the argument =
/// the player; `0x606640` tests `self.enemyGroupMask & other.ourMask`). On the client's 1/3/4 scale.
///
/// `None` when it cannot be resolved (no catalog, our store not streamed, either template missing) —
/// callers treat that as "no opinion" and pass. A GameObject with **no** faction resolves NEUTRAL(3),
/// which is the binary's own default (`0x5f8025: mov eax,3`).
///
/// INTERIM, and narrower than the unit path on purpose: the reference's `0x606530` also has a
/// player-controlled/reputation arm, but every GameObject faction that ships (114 monster, 35
/// friendly-to-players, 14, 1375) resolves through the plain template comparator, so the
/// reputation branch is not modelled here. `GAMEOBJECT_CREATED_BY` (a player-summoned object taking
/// its creator's reaction) is likewise not modelled.
pub(crate) fn go_reaction(
    factions: Option<&Factions>,
    go_faction: u32,
    self_store: Option<&ObjectStore>,
) -> Option<u8> {
    if go_faction == 0 {
        return Some(benilla_formats::Reaction::Neutral as u8);
    }
    let catalog = factions?.catalog();
    let go_tpl = catalog.template(go_faction)?;
    let self_tpl = catalog.template(self_store?.0.unit_faction_template()?)?;
    Some(go_tpl.reaction_toward(self_tpl) as u8)
}

/// The **mouseover-eligibility** virtual `[obj->vtbl+0x54]` — the gate that decides whether a picked
/// object becomes the mouseover **at all** (wow-re `tooltip-content-law.md` §2-GAMEOBJECT, rewritten
/// 2026-07-29; decision 0762).
///
/// This sits one level above everything the tooltip law used to be reasoned from. The per-frame
/// classifier `CGWorldFrame::UpdateMouseoverCursor 0x4828d0` calls it on the picked object and, when
/// it answers false, publishes the **null** mouseover GUID (`0x482985 test eax,eax; je 0x4829ed` →
/// `0x482090(0,0)` + `ResetCursor 0x523d30`). The publisher `0x492890`, the tooltip builder
/// `0x52aa20` and the +64 brighten `0x4945e0` are then never reached: **no tooltip, no highlight, no
/// cursor — nothing.** That is the "some GameObjects show nothing at all on hover" report.
///
/// For a GameObject, slot `+0x54` is `0x5f8620`, which tail-calls the **per-GO-TYPE strategy slot
/// `+0xc`** — and that slot is *not* uniform:
///
/// | `[strat_vtbl+0xc]` | types |
/// |---|---|
/// | `0x5f9db0` = `jmp [+0x14]` = **[`highlightable_flags`] itself** | 0 DOOR · 1 BUTTON · 2 QUESTGIVER · 3 CHEST · 4 BINDER · 6 TRAP · 7 CHAIR · 9 TEXT · 10 GOOBER · 12 AREADAMAGE · 13 CAMERA · 17 FISHINGNODE · 18 RITUAL · 19 MAILBOX · 20 AUCTIONHOUSE · 22 SPELLCASTER · 23 MEETINGSTONE · 24 FLAGSTAND · 26 FLAGDROP · 27 MINI_GAME · 28 LOTTERY_KIOSK · + the default |
/// | `0x5f4830` = **`template.data[1] != 0`** (the vanilla "highlight" column) | 5 GENERIC |
/// | `mov al,1` — always | 8 SPELL_FOCUS · 16 DUEL_ARBITER · 25 FISHINGHOLE · 30 AURA_GENERATOR |
/// | `xor al,al` — never | 11 TRANSPORT · 14 MAP_OBJECT · 15 MO_TRANSPORT |
///
/// **This slot is not the cursor's.** For the marker set the two slots point opposite ways — `+0xc`
/// constant-TRUE, `+0x14` constant-FALSE ([`GO_TYPE_SPELL_FOCUS`]) — so an anvil is *fully* hovered
/// (mouseover published, gold-name tooltip) and *not at all* interactable (plain pointer, no
/// brighten, no USE). Eligibility is never a stand-in for [`highlightable_flags`], and neither is
/// its negation.
///
/// **This refutes the old "the tooltip is not gated by highlightable" reading** — that note verified
/// the publisher correctly and then generalised **GENERIC's** behaviour to all 31 types. The
/// signpost half survives (GENERIC really does answer from `data[1]`, and 1387 of 1870 shipped
/// type-5 templates carry `data1 = 1`); the chest / IN_USE / INTERACT_COND half is wrong, and this
/// is why a pre-quest Stone of Binding or a Stratholme portcullis shows nothing in the reference.
///
/// `generic_highlight` is GENERIC's `data[1]`; `None` = its ask-once template hasn't answered, which
/// reads as eligible so a signpost isn't blank for the first frames of its query.
pub(crate) fn mouseover_eligible(
    type_id: i32,
    flags: u32,
    dyn_flags: u32,
    generic_highlight: Option<bool>,
    reaction: Option<u8>,
    channel_owned: bool,
) -> bool {
    match type_id {
        GO_TYPE_TRANSPORT | GO_TYPE_MAP_OBJECT | GO_TYPE_MO_TRANSPORT => false,
        GO_TYPE_SPELL_FOCUS
        | GO_TYPE_DUEL_ARBITER
        | GO_TYPE_FISHINGHOLE
        | GO_TYPE_AURA_GENERATOR => true,
        GO_TYPE_GENERIC => generic_highlight.unwrap_or(true),
        _ => highlightable_flags(type_id, flags, dyn_flags, reaction, channel_owned),
    }
}

/// [`highlightable_flags`] read off a hovered GameObject's descriptor store. An absent
/// `GAMEOBJECT_TYPE_ID` is the wire default `0` = DOOR (vmangos omits the zero field), so a door
/// resolves to a highlightable type rather than being wrongly rejected as "unknown".
///
/// Gates the **cursor**, the `+64` **brighten**, the right-click **USE** (`OnUse 0x5f8660` calls this
/// same `+0x14` first) and the GO pick's **pass-2 priority** (decision 1071: classify `0x480c90`
/// ranks a highlightable GameObject `1` via `0x5f8800`, else `0`) — the four consumers of one vtable
/// slot. It is **not** the mouseover publish, which is the sibling slot `+0xc`
/// ([`mouseover_eligible`]): the two agree for most types and deliberately disagree for GENERIC(5)
/// (a signpost tooltips while showing no cursor — 0349's reference close-up) and for the marker set
/// (an anvil tooltips while showing no cursor either).
pub(crate) fn go_highlightable(
    store: &ObjectStore,
    reaction: Option<u8>,
    channel_owned: bool,
) -> bool {
    highlightable_flags(
        store.0.gameobject_type_id(),
        store.0.gameobject_flags(),
        store.0.gameobject_dynamic_flags(),
        reaction,
        channel_owned,
    )
}

/// The FISHINGNODE highlightable override's input (`0x5f6710`): is the local player currently
/// channeling at exactly this GameObject — `self.UNIT_FIELD_CHANNEL_OBJECT == go_guid`. `false`
/// when the self store hasn't streamed or the guid is unknown: an unverifiable bobber shows
/// nothing, matching the reference's no-active-player early-false.
pub(crate) fn fishing_channel_owned(
    self_store: Option<&ObjectStore>,
    go_guid: Option<u64>,
) -> bool {
    match (self_store, go_guid) {
        (Some(s), Some(g)) => s.0.unit_channel_object() == Some(g),
        _ => false,
    }
}

/// A `LockType.dbc` **CursorName** stem → the [`CursorKind`] it names — the client's
/// `CursorModeFromName` step (wow-re cursor-system.md §4, `0x523d40`) over the only three cursor-
/// bearing lock kinds in 5875. An unknown/empty name resolves to `None`, which the base GO path
/// reads as "the generic Interact gear."
fn cursor_kind_from_lock_name(name: &str) -> Option<CursorKind> {
    match name {
        "PickLock" => Some(CursorKind::PickLock),
        "GatherHerbs" => Some(CursorKind::GatherHerbs),
        "Mine" => Some(CursorKind::Mine),
        _ => None,
    }
}

/// The **data-named** cursor for a base-type GameObject's lock (wow-re cursor-system.md §4): the GO
/// template's `lockId` → the `Lock.dbc` row → its **first** requirement slot's index (the client
/// reads `[lockRow+0x24]` = `Index[0]` unconditionally, no scan) → the `LockType.dbc` **CursorName**.
/// `None` when there's no lock, no client data, or the LockType has no distinct cursor — the caller
/// falls back to the Interact gear. A [`CursorKind::PickLock`] result *is* the `LockType.Id == 1`
/// signal the classifier keys on to skip the grayed twin ("Pick Lock" is the only name that maps
/// there), so no separate flag is needed.
fn go_lock_cursor(
    lock_id: u32,
    locks: Option<&crate::go_templates::Locks>,
    lock_types: Option<&crate::go_templates::LockTypes>,
) -> Option<CursorKind> {
    if lock_id == 0 {
        return None;
    }
    let slots = locks?.0.slots(lock_id)?;
    let lock_type_id = slots[0].index; // Index[0] — the client's single, first-slot read.
    let name = lock_types?.0.cursor_name(lock_type_id)?;
    cursor_kind_from_lock_name(name)
}

/// The GameObject cursor kind (wow-re cursor-system.md §4), given a **highlightable** GO's type and
/// its resolved lock cursor. The per-type behaviors that override the base gear: TEXT(9) → the
/// Inspect magnifier; RITUAL(18)/MAILBOX(19)/type-28 → Mail. Every other type is the base behavior:
/// its data-named lock cursor (Mine/GatherHerbs/PickLock) if it has one, else the generic Interact.
fn go_cursor_kind(type_id: i32, lock_cursor: Option<CursorKind>) -> CursorKind {
    match type_id {
        GO_TYPE_TEXT => CursorKind::Inspect,
        GO_TYPE_RITUAL | GO_TYPE_MAILBOX | GO_TYPE_28 => CursorKind::Mail,
        _ => lock_cursor.unwrap_or(CursorKind::Interact),
    }
}

/// The QUESTGIVER leg's own gate — the bit alone is not enough. The ladder calls `0x5df490(unit)`,
/// which is `NPC_FLAGS bit 1` **AND** the cached quest status `[unit+0xcb8] ∉ {0, 1}` (wow-re
/// `ui/scratch/cursor-system.md` §3, byte-verified row; `object-layer/scratch/questgiver-marker.md`
/// independently pins `+0xcb8` as the `SMSG_QUESTGIVER_STATUS` cache, written by `0x607440` and with
/// only three writers repo-wide). So NONE(0) and UNAVAILABLE(1) do **not** make a unit talkable;
/// every other status does.
///
/// This is what keeps a questgiver-flagged NPC with nothing to offer from being clickable at all —
/// and it is load-bearing far beyond the cursor. Melika Isenstrider (vmangos entry 6778) is flagged
/// QUESTGIVER, carries no other service bit, and has zero rows in `creature_questrelation`: without
/// this gate she classifies Speak, we send `CMSG_GOSSIP_HELLO`, and vmangos answers the resulting
/// `DEFAULT_GOSSIP_MESSAGE` text query with eight literal `"Greetings $N"` blocks
/// (`QueryHandler.cpp:210-217`) — an empty gossip frame carrying a placeholder greeting, on an NPC
/// the reference never opens anything for. That was the whole of the reported "the client invents
/// 'Greetings NAME'" bug: the text was genuine, the *asking* was ours.
///
/// `None` (no status ever sent) reads as no quest: the server sends the status unprompted for every
/// questgiver in range, so its absence means the unit isn't offering us one.
fn questgiver_has_quest(quest_status: Option<u32>) -> bool {
    use benilla_protocol::messages::dialog_status::{NONE, UNAVAILABLE};
    !matches!(quest_status, None | Some(NONE) | Some(UNAVAILABLE))
}

/// The per-bit service ladder (`0x482336..0x4824e3`, statically unrolled — every row byte-verified
/// in the RE note's §3 table): lowest set bit wins. `None` = no *consulted* bit set — the unit
/// falls through to the attack/clear leg (this is where repair-only units land: bit 14 is never
/// tested in the binary).
///
/// `quest_status` is the unit's last `SMSG_QUESTGIVER_STATUS` (`None` = never sent), which gates the
/// QUESTGIVER leg — see [`questgiver_has_quest`].
fn service_cursor(service: u32, quest_status: Option<u32>) -> Option<CursorKind> {
    use npc_flags::*;
    // Bits 0 and 1 both land on Speak, and bit 0 is tested first, so the two rows fold into one
    // condition without changing a single outcome — the same folding the SPIRITHEALER/SPIRITGUIDE
    // and PETITIONER/TABARDDESIGNER/BATTLEMASTER rows already use. Only bit 1 carries a gate.
    if service & GOSSIP != 0 || (service & QUESTGIVER != 0 && questgiver_has_quest(quest_status)) {
        Some(CursorKind::Speak)
    } else if service & VENDOR != 0 {
        Some(CursorKind::Pickup)
    } else if service & FLIGHTMASTER != 0 {
        Some(CursorKind::Taxi)
    } else if service & TRAINER != 0 {
        Some(CursorKind::Trainer)
    } else if service & (SPIRITHEALER | SPIRITGUIDE) != 0 {
        Some(CursorKind::Speak)
    } else if service & INNKEEPER != 0 {
        Some(CursorKind::Interact)
    } else if service & BANKER != 0 {
        Some(CursorKind::Buy)
    } else if service & (PETITIONER | TABARDDESIGNER | BATTLEMASTER) != 0 {
        Some(CursorKind::Speak)
    } else if service & AUCTIONEER != 0 {
        Some(CursorKind::Buy)
    } else if service & STABLEMASTER != 0 {
        Some(CursorKind::Speak)
    } else {
        None
    }
}

/// The loot leg's mode split (`8 + (keyDown(0) ? 8 : 0)` @ `0x48252c`): the single pouch, or
/// the triple LootAll while the EFFECTIVE auto-loot is on — the 0961 rule (`autoLootDefault`
/// XOR the held modifier) so the cursor always announces what the click will actually do. In
/// 1.12 the held key alone *was* the effective state (no CVar existed).
fn loot_cursor(auto_loot: bool, shift_held: bool) -> CursorKind {
    if auto_loot != shift_held {
        CursorKind::LootAll
    } else {
        CursorKind::Pickup
    }
}

/// Resolve this frame's [`WorldCursor`] from the hovered unit — the reference's classifier order:
/// interactable-NPC service ladder, else loot/skin/attack by state, each grayed by its own range
/// gate. No hover (or anything unresolvable) → Point.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(super) fn classify_cursor(
    hovered: Res<Hovered>,
    hovered_object: Res<HoveredObject>,
    factions: Option<Res<Factions>>,
    reputations: Res<Reputations>,
    mut cursor: ResMut<WorldCursor>,
    units: Query<(
        &Transform,
        Option<&ObjectStore>,
        Option<&NetEntity>,
        Option<&crate::go_anim::GoAnim>,
    )>,
    self_q: Query<(&Transform, &ObjectStore), With<SelfPlayer>>,
    // The GameObject cursor is data-driven (decision 0236, wow-re cursor-system §4): the ask-once
    // template (its `lockId`) + Lock.dbc + LockType.dbc name the cursor. All absent without client
    // data or before a hovered GO's template answers — a lock-bearing GO then reads as the gear.
    // The same param carries the lock chain `usable` consults (decision 0752) — one
    // [`super::lock::GoLockInputs`], so the cursor and the click ask literally the same question.
    go_inputs: super::lock::GoLockInputs,
    player_actions: Res<crate::ui_action::PlayerActions>,
    // `[0xb700e4]`/`[0xb700e8]` — the skin leg's learned-ability precondition (decision 0752).
    learned: Res<crate::ui_action::LearnedAbilities>,
    // The QUESTGIVER leg's gate reads the per-guid `SMSG_QUESTGIVER_STATUS` store — see
    // [`questgiver_has_quest`].
    quest: Res<crate::ui_quest::QuestGiver>,
    // The loot leg's Pickup/LootAll split (0965): the auto-loot knob (0961) + the live shift.
    loot_cfg: Res<crate::ui_loot::LootConfig>,
    keys: Res<ButtonInput<KeyCode>>,
) {
    // A **highlightable** GameObject shows its **data-driven** cursor (wow-re cursor-system §4): a
    // mailbox's Mail, a plaque's Inspect, a vein's Mine / herb's GatherHerbs / picked lock's PickLock
    // (off its LockType), else the generic Interact gear — each grayed out of interact range (except
    // PickLock, never grayed). A non-highlightable GO yields no cursor, like the reference handler's
    // clear (decision 0243): a GENERIC decoration, a busy/NO_INTERACT object, or a quest object whose
    // per-player activate bit the server hasn't set (INTERACT_COND without the quest). The usable-
    // grayed twin is still the interim distance gate (decision 0243); the client's fuller `usable`
    // (lock satisfaction, player-state) is later.
    let resolve_go = || {
        let (go_tf, store, _, anim) = units.get(hovered_object.target?).ok()?;
        let store = store?;
        let (self_tf, self_store) = self_q.single().ok()?;
        let reaction = go_reaction(
            factions.as_deref(),
            store.0.gameobject_faction(),
            Some(self_store),
        );
        let channel_owned = fishing_channel_owned(Some(self_store), hovered_object.guid);
        if !go_highlightable(store, reaction, channel_owned) {
            return None;
        }
        // The type's own cursor wins; a base type reads its lock's data-named cursor (needs the
        // ask-once template — a not-yet-answered GO falls back to the gear until it arrives).
        let tmpl = hovered_object.guid.and_then(|g| go_inputs.templates.get(g));
        let lock_id = tmpl.map_or(0, |t| t.lock_id);
        let lock_cursor = go_lock_cursor(
            lock_id,
            go_inputs.locks.as_deref(),
            go_inputs.lock_types.as_deref(),
        );
        let kind = go_cursor_kind(store.0.gameobject_type_id(), lock_cursor);
        let dist_sq = go_tf.translation.distance_squared(self_tf.translation);
        // `usable 0x5f3130`, the two arms we model — the LOCK arm first, as the binary orders
        // them (`0x5f32a6` before the range check at `0x5f330c`), then the interim range gray.
        // The lock arm runs **only when `GO_FLAG_LOCKED` is set** (§8.8): that is why an
        // ungatherable herb node keeps its lit GatherHerbs cursor and only refuses on the click,
        // while a padlocked door grays. `PickLock` never grays (§4: `LockType.Id == 1` skips the
        // usable gate), so the rogue's affordance still reads as available.
        let facts = super::lock::go_facts(Some((store, crate::go_anim::go_state(anim, store))));
        let lock_unmet = go_inputs
            .locks
            .as_deref()
            .and_then(|l| l.0.slots(lock_id).filter(|_| lock_id != 0))
            .is_some_and(|slots| {
                super::lock::resolve_lock(
                    slots,
                    &player_actions.spells,
                    go_inputs.spells.as_deref(),
                    Some(self_store),
                    &go_inputs.items,
                    facts,
                    &mut None,
                )
                .blocks_usable(facts.flag_locked)
            });
        let unable = kind != CursorKind::PickLock
            && (lock_unmet || dist_sq > go_interact_range_sq(store.0.gameobject_type_id()));
        Some((kind, unable))
    };
    // The reference makes one pick over all CGObjects and switches on type; benilla picks unit and
    // GameObject separately, then classifies whichever is nearer under the cursor.
    let resolve_unit = || {
        let (unit_tf, store, net, _) = units.get(hovered.target?).ok()?;
        let store = store?;
        let (self_tf, self_store) = self_q.single().ok()?;
        let dist_sq = unit_tf.translation.distance_squared(self_tf.translation);
        // Melee interact reach (loot + skin's gate): both units' combat reach + the offset,
        // floored at 5 yd. Boundary-inclusive, center-to-center.
        let reach = (store.0.unit_combat_reach() + self_store.0.unit_combat_reach() + MELEE_OFFSET)
            .max(MELEE_FLOOR);
        let in_melee = dist_sq <= reach * reach;

        let dead = store.0.unit_is_dead();
        if dead {
            if store.0.unit_lootable() {
                // The loot mode split `8 + (keyDown(0) ? 8 : 0)` @ `0x48252c`, over 0961's
                // effective auto-loot ([`loot_cursor`]). Gray by the byte-verified gate
                // (inline in `CanLootNow 0x5ec110`): the same melee interact reach as skin —
                // ~5 yd vs a normal mob (director-measured, confirmed).
                let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
                return Some((loot_cursor(loot_cfg.auto_loot, shift), !in_melee));
            }
            // The skin leg's SECOND precondition, byte-verified: the flag alone is not enough —
            // the reference also requires the learn-time latch `[0xb700e4 + 4×isPlayerTarget]` to
            // be non-null (wow-re cursor-system.md §3, `0x482589`), i.e. **the player must have
            // learned a Skinning spell**. A non-skinner gets no knife on a skinnable corpse; the
            // corpse falls through to Point like any other unlootable one (decision 0752).
            if store.0.unit_flags() & UNIT_FLAG_SKINNABLE != 0 && learned.skinning.is_some() {
                return Some((CursorKind::Skin, !in_melee));
            }
            return None; // a plain corpse: Point
        }

        // Reaction rank (0..=7) toward us — the ring's own resolver (reputation first, then the
        // faction-template comparator).
        let rank = ring_reaction(
            factions.as_deref(),
            &reputations,
            Some(store),
            Some(self_store),
        );
        let is_player = net.is_some_and(|n| n.kind == EntityKind::Player);
        // Interactable NPC (interim CanInteract): a consulted service bit + not attack-worthy.
        // A repair-only unit yields None here and falls through, exactly like the binary.
        if !is_player && rank >= 3 {
            let status = hovered.guid.and_then(|g| quest.status(g));
            if let Some(kind) = service_cursor(store.0.unit_npc_flags(), status) {
                return Some((kind, dist_sq > SERVICE_RANGE_SQ));
            }
        }
        // Attackable (interim matrix): hostile/unfriendly/neutral NPC, or a hostile player.
        if (!is_player && rank <= 3) || (is_player && rank <= 1) {
            return Some((CursorKind::Attack, dist_sq > ATTACK_RANGE_SQ));
        }
        None // friendly non-service unit / friendly player: Point
    };
    let resolved = if go_is_nearest(&hovered, &hovered_object) {
        resolve_go()
    } else {
        resolve_unit()
    };
    let (kind, unable) = resolved.unwrap_or((CursorKind::Point, false));
    let want = WorldCursor { kind, unable };
    if *cursor != want {
        *cursor = want;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_protocol::messages::dialog_status;

    /// The loot pouch follows the EFFECTIVE auto-loot (0961: setting XOR shift) — the cursor
    /// and the click can never disagree about what a pick will do.
    #[test]
    fn the_loot_pouch_triples_with_the_effective_auto_loot() {
        assert_eq!(loot_cursor(false, false), CursorKind::Pickup);
        assert_eq!(loot_cursor(true, false), CursorKind::LootAll);
        // Shift inverts BOTH ways (era's AUTOLOOTTOGGLE; vanilla's whole mechanism).
        assert_eq!(loot_cursor(false, true), CursorKind::LootAll);
        assert_eq!(loot_cursor(true, true), CursorKind::Pickup);
        // The grayed twin rides the same stem.
        let far = WorldCursor {
            kind: CursorKind::LootAll,
            unable: true,
        };
        assert_eq!(far.stem(), "UnableLootAll");
    }

    #[test]
    fn stems_name_the_shipped_blps() {
        let attack = WorldCursor {
            kind: CursorKind::Attack,
            unable: false,
        };
        assert_eq!(attack.stem(), "Attack");
        let far = WorldCursor {
            kind: CursorKind::Attack,
            unable: true,
        };
        assert_eq!(far.stem(), "UnableAttack");
        // Point has no grayed twin — unable never redirects it.
        let point = WorldCursor {
            kind: CursorKind::Point,
            unable: true,
        };
        assert_eq!(point.stem(), "Point");
    }

    /// The mouseover-eligibility table (decision 0762) — the gate that decides whether an object
    /// becomes the mouseover at all, so a false here is "no tooltip, no brighten, no cursor".
    #[test]
    fn mouseover_eligibility_matches_the_per_type_slot_table() {
        // The constant-TRUE slots (`mov al,1`) ignore their flags entirely.
        for t in [
            GO_TYPE_SPELL_FOCUS,
            GO_TYPE_DUEL_ARBITER,
            GO_TYPE_FISHINGHOLE,
            GO_TYPE_AURA_GENERATOR,
        ] {
            assert!(mouseover_eligible(
                t,
                GO_FLAG_INTERACT_COND,
                0,
                None,
                None,
                false
            ));
            assert!(mouseover_eligible(t, 0x10, 0, None, None, false));
        }
        // The three constant-FALSE slots (`xor al,al`) — the transport family is never a mouseover.
        for t in [GO_TYPE_TRANSPORT, GO_TYPE_MAP_OBJECT, GO_TYPE_MO_TRANSPORT] {
            assert!(!mouseover_eligible(
                t,
                0,
                GO_DYNFLAG_ACTIVATE,
                None,
                None,
                false
            ));
        }
        // GENERIC answers from its template's `data[1]` alone — the signpost that hovers vs the
        // scenery beside it that does not. Not from `highlightable_flags`, which rejects GENERIC.
        assert!(mouseover_eligible(
            GO_TYPE_GENERIC,
            0,
            0,
            Some(true),
            None,
            false
        ));
        assert!(!mouseover_eligible(
            GO_TYPE_GENERIC,
            0,
            0,
            Some(false),
            None,
            false
        ));
        assert!(
            mouseover_eligible(GO_TYPE_GENERIC, 0, 0, None, None, false),
            "template not answered yet reads eligible, so a signpost isn't blank while it queries"
        );
        // Everything else IS highlightable — which is the whole correction. A pre-quest
        // INTERACT_COND object (the Stone of Binding, a Stratholme portcullis at flags 0x24) and an
        // IN_USE / NO_INTERACT object show NOTHING, where we used to tooltip them.
        assert!(!mouseover_eligible(
            0,
            GO_FLAG_INTERACT_COND,
            0,
            None,
            None,
            false
        ));
        assert!(mouseover_eligible(
            0,
            GO_FLAG_INTERACT_COND,
            GO_DYNFLAG_ACTIVATE,
            None,
            None,
            false
        ));
        assert!(!mouseover_eligible(3, 0x10, 0, None, None, false)); // NO_INTERACT chest
        assert!(!mouseover_eligible(3, 0x1, 0, None, None, false)); // IN_USE chest
                                                                    // The FACTION term (decision 0764): a door whose template is hostile to us is not
                                                                    // eligible — no cursor, no tooltip, no brighten. This is Deadmines' Factory Door
                                                                    // (faction 114, flags 0x20), which the director could still hover and open.
        assert!(!mouseover_eligible(0, 0x20, 0, None, Some(1), false));
        assert!(mouseover_eligible(0, 0x20, 0, None, Some(3), false));
        assert!(mouseover_eligible(0, 0x20, 0, None, Some(4), false));
        // TRAP inverts it: hostile-to-us is exactly the case a trap DOES highlight for.
        assert!(mouseover_eligible(GO_TYPE_TRAP, 0, 0, None, Some(1), false));
        assert!(!mouseover_eligible(
            GO_TYPE_TRAP,
            0,
            0,
            None,
            Some(3),
            false
        ));
        // An unresolvable reaction never blanks the world.
        assert!(mouseover_eligible(0, 0x20, 0, None, None, false));
        // A plain door is still eligible — the everyday case must not regress.
        assert!(mouseover_eligible(0, 0, 0, None, None, false));
        assert!(mouseover_eligible(19, 0, 0, None, None, false)); // mailbox
    }

    #[test]
    fn highlightable_gates_the_quest_object_but_not_the_plain_door() {
        // An ordinary unlocked door (no INTERACT_COND) is highlightable regardless of its zero activate
        // bit — plain doors must never gray out.
        assert!(highlightable_flags(0, 0, 0, None, false)); // DOOR, no flags
                                                            // A GENERIC decoration is never highlightable.
        assert!(!highlightable_flags(GO_TYPE_GENERIC, 0, 0, None, false));
        // Neither is the transport family (the byte-dumped constant-false +0x14 slots): no gear,
        // no tooltip, no USE on a boat / elevator / map object — flags can't make them so.
        for t in [GO_TYPE_TRANSPORT, GO_TYPE_MAP_OBJECT, GO_TYPE_MO_TRANSPORT] {
            assert!(!highlightable_flags(t, 0, GO_DYNFLAG_ACTIVATE, None, false));
        }
        // Nor the marker set, whose `+0x14` is the same byte-dumped `xor al,al` — an anvil / forge /
        // brazier (SPELL_FOCUS, flags 0x0, faction 0 ⇒ NEUTRAL, every flag term passing) is exactly
        // the case that used to reach the predicate and come back true, wearing the Interact gear.
        for t in [
            GO_TYPE_SPELL_FOCUS,
            GO_TYPE_DUEL_ARBITER,
            GO_TYPE_FISHINGHOLE,
            GO_TYPE_AURA_GENERATOR,
        ] {
            assert!(!highlightable_flags(t, 0, 0, None, false));
            assert!(!highlightable_flags(
                t,
                0,
                GO_DYNFLAG_ACTIVATE,
                Some(3),
                false
            ));
        }
        // And the two slots point OPPOSITE ways for the marker set: hovered (tooltip) but never
        // interactable (no cursor, no brighten, no USE). The reported anvil, both halves at once.
        assert!(mouseover_eligible(
            GO_TYPE_SPELL_FOCUS,
            0,
            0,
            None,
            Some(3),
            false
        ));
        assert!(!highlightable_flags(
            GO_TYPE_SPELL_FOCUS,
            0,
            0,
            Some(3),
            false
        ));
        // A busy (IN_USE) or NO_INTERACT object is not highlightable.
        assert!(!highlightable_flags(3, 0x1, 0, None, false)); // CHEST, IN_USE
        assert!(!highlightable_flags(3, 0x10, 0, None, false)); // CHEST, NO_INTERACT
                                                                // The quest gate: an INTERACT_COND object is highlightable only with the activate bit set — the
                                                                // exact bug, a quest chest without the quest.
        assert!(!highlightable_flags(
            3,
            GO_FLAG_INTERACT_COND,
            0,
            None,
            false
        )); // quest chest, no quest → clear
        assert!(highlightable_flags(
            3,
            GO_FLAG_INTERACT_COND,
            GO_DYNFLAG_ACTIVATE,
            None,
            false
        )); // quest chest, quest held → usable
    }

    #[test]
    fn the_bobber_is_channel_gated_and_reaches_a_hundred_yards() {
        // The FISHINGNODE highlightable override (`0x5f6710`, wow-re fishing-bobber-interaction.md):
        // only the player whose UNIT_FIELD_CHANNEL_OBJECT names exactly this GO passes — someone
        // else's bobber (or yours after the channel drops) shows nothing at all.
        assert!(highlightable_flags(GO_TYPE_FISHINGNODE, 0, 0, None, true));
        assert!(!highlightable_flags(GO_TYPE_FISHINGNODE, 0, 0, None, false));
        // The pass tail-jumps the SHARED gate, so the flags still apply on top.
        assert!(!highlightable_flags(
            GO_TYPE_FISHINGNODE,
            0x10,
            0,
            None,
            true
        ));
        // The mouseover thunk (`+0xc` → `+0x14`) rides the same predicate: no tooltip either.
        assert!(!mouseover_eligible(
            GO_TYPE_FISHINGNODE,
            0,
            0,
            None,
            None,
            false
        ));
        assert!(mouseover_eligible(
            GO_TYPE_FISHINGNODE,
            0,
            0,
            None,
            None,
            true
        ));
        // Every other type ignores the channel input entirely.
        assert!(highlightable_flags(0, 0, 0, None, false));
        // The per-type range: the bobber's ctor constant is 100.0 yd (squared at the compare) —
        // effectively un-range-gated — while everything else stays on the interim reach.
        assert_eq!(go_interact_range_sq(GO_TYPE_FISHINGNODE), 100.0 * 100.0);
        assert_eq!(go_interact_range_sq(0), GO_INTERACT_RANGE_SQ);
        // The channel compare itself: owned iff self's channel object is exactly this guid.
        assert!(!fishing_channel_owned(None, Some(7)));
        assert!(!fishing_channel_owned(None, None));
    }

    #[test]
    fn melee_reach_floors_at_five() {
        // The byte-verified floor (`0x80a1e8` is a max, not a min): a typical player-vs-mob pair
        // (1.5 + 1.5 + 1.333 = 4.333) is lifted to 5 yd — the threshold the director measured.
        assert_eq!((1.5_f32 + 1.5 + MELEE_OFFSET).max(MELEE_FLOOR), 5.0);
        // Large creatures reach *farther* than 5 — the floor never cuts a big sum down.
        assert!(((3.0_f32 + 3.0 + MELEE_OFFSET).max(MELEE_FLOOR) - 7.333_33).abs() < 1e-4);
    }

    #[test]
    fn service_ladder_matches_the_unrolled_binary() {
        use npc_flags::*;
        // A quest to offer, so the QUESTGIVER row behaves like the rest of the ladder here; the
        // gate itself is `questgiver_flag_alone_is_not_talkable`.
        let has = Some(dialog_status::AVAILABLE);
        // The rows the RE table pins per byte address.
        assert_eq!(service_cursor(GOSSIP, None), Some(CursorKind::Speak));
        assert_eq!(service_cursor(QUESTGIVER, has), Some(CursorKind::Speak));
        assert_eq!(service_cursor(VENDOR, None), Some(CursorKind::Pickup));
        assert_eq!(service_cursor(FLIGHTMASTER, None), Some(CursorKind::Taxi));
        assert_eq!(service_cursor(TRAINER, None), Some(CursorKind::Trainer));
        assert_eq!(service_cursor(SPIRITHEALER, None), Some(CursorKind::Speak));
        assert_eq!(service_cursor(INNKEEPER, None), Some(CursorKind::Interact));
        assert_eq!(service_cursor(BANKER, None), Some(CursorKind::Buy));
        assert_eq!(service_cursor(BATTLEMASTER, None), Some(CursorKind::Speak));
        assert_eq!(service_cursor(AUCTIONEER, None), Some(CursorKind::Buy));
        assert_eq!(service_cursor(STABLEMASTER, None), Some(CursorKind::Speak));
        // Lowest bit wins: a gossiping vendor speaks; an innkeeper-banker interacts.
        assert_eq!(
            service_cursor(GOSSIP | VENDOR, None),
            Some(CursorKind::Speak)
        );
        assert_eq!(
            service_cursor(INNKEEPER | BANKER, None),
            Some(CursorKind::Interact)
        );
        // REPAIR (0x4000) is never consulted — repair-only falls to the attack/clear leg.
        assert_eq!(service_cursor(0x4000, None), None);
        assert_eq!(service_cursor(0, None), None);
    }

    /// The QUESTGIVER leg's `0x5df490` gate: the bit alone never makes a unit talkable. This is the
    /// "client invents 'Greetings NAME'" bug at its root — a questgiver-flagged NPC with nothing to
    /// offer must fall out of the ladder entirely, so we never send `CMSG_GOSSIP_HELLO` and never
    /// open the empty gossip frame the server would answer with a placeholder greeting.
    #[test]
    fn questgiver_flag_alone_is_not_talkable() {
        use npc_flags::*;
        // Melika Isenstrider's exact shape: QUESTGIVER, no other service bit, nothing on offer.
        for status in [
            None,
            Some(dialog_status::NONE),
            Some(dialog_status::UNAVAILABLE),
        ] {
            assert_eq!(
                service_cursor(QUESTGIVER, status),
                None,
                "status {status:?} must not classify Speak"
            );
        }
        // Every other status is a quest worth talking about — `[unit+0xcb8] ∉ {0, 1}`.
        for status in [
            dialog_status::CHAT,
            dialog_status::INCOMPLETE,
            dialog_status::REWARD_REP,
            dialog_status::AVAILABLE,
            dialog_status::REWARD_OLD,
            dialog_status::REWARD2,
        ] {
            assert_eq!(
                service_cursor(QUESTGIVER, Some(status)),
                Some(CursorKind::Speak),
                "status {status} must classify Speak"
            );
        }
        // The gate is the QUESTGIVER leg's alone: a quest-less unit that also gossips still speaks
        // (bit 0 is tested first and carries no gate), and a quest-less vendor still shows Pickup
        // rather than falling out of the ladder.
        assert_eq!(
            service_cursor(GOSSIP | QUESTGIVER, None),
            Some(CursorKind::Speak)
        );
        assert_eq!(
            service_cursor(QUESTGIVER | VENDOR, None),
            Some(CursorKind::Pickup)
        );
    }

    #[test]
    fn lock_names_resolve_to_the_three_data_cursors() {
        // The only cursor-bearing LockType CursorNames in 5875 (byte-confirmed in the LockType
        // catalog test) — the client's `CursorModeFromName` over them.
        assert_eq!(
            cursor_kind_from_lock_name("PickLock"),
            Some(CursorKind::PickLock)
        );
        assert_eq!(
            cursor_kind_from_lock_name("GatherHerbs"),
            Some(CursorKind::GatherHerbs)
        );
        assert_eq!(cursor_kind_from_lock_name("Mine"), Some(CursorKind::Mine));
        // Anything else (an empty CursorName, or an unmodeled name) → no data cursor → Interact.
        assert_eq!(cursor_kind_from_lock_name(""), None);
        assert_eq!(cursor_kind_from_lock_name("Fishing"), None);
    }

    #[test]
    fn go_cursor_kind_maps_type_then_lock() {
        // MAILBOX(19), RITUAL(18) and type 28 all show Mail — regardless of any (irrelevant) lock.
        assert_eq!(go_cursor_kind(19, None), CursorKind::Mail);
        assert_eq!(go_cursor_kind(18, None), CursorKind::Mail);
        assert_eq!(go_cursor_kind(28, Some(CursorKind::Mine)), CursorKind::Mail);
        // TEXT(9) plaque → the Inspect magnifier.
        assert_eq!(go_cursor_kind(9, None), CursorKind::Inspect);
        // Base types: a door(0)/button(1)/chest(3)/goober(10) with no data cursor → the Interact gear.
        for t in [0, 1, 3, 10, 6, 24] {
            assert_eq!(go_cursor_kind(t, None), CursorKind::Interact);
        }
        // A base type carrying a data-named lock cursor shows it (a chest over a vein/herb/picked lock).
        assert_eq!(go_cursor_kind(3, Some(CursorKind::Mine)), CursorKind::Mine);
        assert_eq!(
            go_cursor_kind(3, Some(CursorKind::GatherHerbs)),
            CursorKind::GatherHerbs
        );
        assert_eq!(
            go_cursor_kind(3, Some(CursorKind::PickLock)),
            CursorKind::PickLock
        );
    }

    #[test]
    fn go_cursor_stems_name_the_shipped_blps() {
        // The new GO cursor kinds resolve to real `Interface\Cursor\<stem>.blp` stems (all present
        // in 5875, confirmed by extraction); their grayed twins prepend Unable, except Point.
        for (kind, stem) in [
            (CursorKind::Mail, "Mail"),
            (CursorKind::Mine, "Mine"),
            (CursorKind::GatherHerbs, "GatherHerbs"),
            (CursorKind::PickLock, "PickLock"),
        ] {
            assert_eq!(
                WorldCursor {
                    kind,
                    unable: false
                }
                .stem(),
                stem
            );
            assert_eq!(
                WorldCursor { kind, unable: true }.stem(),
                format!("Unable{stem}")
            );
        }
    }
}
