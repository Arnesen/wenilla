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
//!   **Pickup** — the loot leg's base mode is `mode = 8 + (keyDown(0) ? 8 : 0)` (`0x48252c`), i.e.
//!   Pickup(8), becoming LootAll(16) only while the auto-loot modifier key is *held*; dead +
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
//! the same approximation the ring's player branch documents. The auto-loot modifier-key split
//! (Pickup vs LootAll, `0x41f8f0`) waits on auto-loot existing at all — until then the base Pickup
//! is the only mode the loot leg can honestly show. (The questgiver bit's own quest-status gate,
//! `0x5df490`, used to be listed here as unmodelled; it is modelled now — [`questgiver_has_quest`].)

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
    /// Pickup(8) — the vendor's pouch AND the loot leg's base mode (LootAll(16) is this same leg
    /// with the auto-loot modifier held — deferred until auto-loot exists).
    Pickup,
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
const GO_TYPE_GENERIC: i32 = 5;
/// The transport family — TRANSPORT(11), MAP_OBJECT(14), MO_TRANSPORT(15): their strategy vtables'
/// highlightable slot (+0x14) is constant-false too (`32 c0 c3` — vtable dump from the 5875 binary:
/// `0x80ba58+0x14`→`0x5f5c70`, `0x80b710`/`0x80b798+0x14`→`0x5f48b0`), so a boat / zeppelin /
/// elevator never shows a cursor, tooltip, or right-click USE.
const GO_TYPE_TRANSPORT: i32 = 11;
const GO_TYPE_MAP_OBJECT: i32 = 14;
const GO_TYPE_MO_TRANSPORT: i32 = 15;
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
/// `GameObjectFlags` bits consulted by the highlightable gate (decision 0243, wow-re cursor-system §4a):
/// `0x1` IN_USE (busy) and `0x10` NO_INTERACT both suppress interaction; their union is the fast reject.
const GO_FLAG_IN_USE_OR_NO_INTERACT: u32 = 0x11;
/// `GO_FLAG_INTERACT_COND` (`0x4`) — the object is usable **only** when its per-player activate dyn-flag
/// is set. This is the quest gate: a quest chest/goober carries it, an ordinary door does not.
const GO_FLAG_INTERACT_COND: u32 = 0x4;
/// `GO_DYNFLAG_LO_ACTIVATE` (`0x1` in `GAMEOBJECT_DYN_FLAGS`) — the per-player "usable for me now" bit the
/// server sets from `GameObject::ActivateToQuest` (sparkle). Consulted only under `INTERACT_COND`.
const GO_DYNFLAG_ACTIVATE: u32 = 0x1;

/// The client's GameObject **highlightable** predicate (decision 0243, wow-re cursor-system §4a,
/// `0x5f2f80`) over its wire flags: whether the object shows an interact cursor / is clickable at all.
/// GENERIC decoration and the transport family are never highlightable (their strategy vtables
/// override the slot with constant-false); a busy (IN_USE) or NO_INTERACT object isn't; and an
/// **INTERACT_COND** object (the quest gate) is highlightable only when the server has set its per-player
/// **activate** dyn-flag — so a quest chest sparkles and opens only once the quest is held, while an
/// ordinary door (no INTERACT_COND) is always highlightable regardless of its (zero) activate bit. The
/// client's faction-reaction>1 term is INFERRED and not the quest gate, so it is not modeled here;
/// `usable` (lock / range / player-state → the grayed twin) rides on top and is a later refinement.
fn highlightable_flags(type_id: i32, flags: u32, dyn_flags: u32) -> bool {
    if matches!(
        type_id,
        GO_TYPE_GENERIC | GO_TYPE_TRANSPORT | GO_TYPE_MAP_OBJECT | GO_TYPE_MO_TRANSPORT
    ) {
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

/// [`highlightable_flags`] read off a hovered GameObject's descriptor store. An absent
/// `GAMEOBJECT_TYPE_ID` is the wire default `0` = DOOR (vmangos omits the zero field), so a door
/// resolves to a highlightable type rather than being wrongly rejected as "unknown". Gates the
/// CURSOR (and, via it, the click) only — the mouseover tooltip is *not* coupled to it
/// (§5-VERIFIED, wow-re 2026-07-20): the pick registers every GO regardless of highlightable and
/// the publisher dispatches the tooltip by kind, so a GENERIC(5) signpost tooltips (0349's
/// reference close-up) while showing no cursor.
fn go_highlightable(store: &ObjectStore) -> bool {
    highlightable_flags(
        store.0.gameobject_type_id(),
        store.0.gameobject_flags(),
        store.0.gameobject_dynamic_flags(),
    )
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
    units: Query<(&Transform, Option<&ObjectStore>, Option<&NetEntity>)>,
    self_q: Query<(&Transform, &ObjectStore), With<SelfPlayer>>,
    // The GameObject cursor is data-driven (decision 0236, wow-re cursor-system §4): the ask-once
    // template (its `lockId`) + Lock.dbc + LockType.dbc name the cursor. All absent without client
    // data or before a hovered GO's template answers — a lock-bearing GO then reads as the gear.
    go_templates: Res<crate::go_templates::GameObjectTemplates>,
    locks: Option<Res<crate::go_templates::Locks>>,
    lock_types: Option<Res<crate::go_templates::LockTypes>>,
    // The QUESTGIVER leg's gate reads the per-guid `SMSG_QUESTGIVER_STATUS` store — see
    // [`questgiver_has_quest`].
    quest: Res<crate::ui_quest::QuestGiver>,
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
        let (go_tf, store, _) = units.get(hovered_object.target?).ok()?;
        let store = store?;
        let (self_tf, _) = self_q.single().ok()?;
        if !go_highlightable(store) {
            return None;
        }
        // The type's own cursor wins; a base type reads its lock's data-named cursor (needs the
        // ask-once template — a not-yet-answered GO falls back to the gear until it arrives).
        let lock_cursor = hovered_object
            .guid
            .and_then(|g| go_templates.get(g))
            .and_then(|t| go_lock_cursor(t.lock_id, locks.as_deref(), lock_types.as_deref()));
        let kind = go_cursor_kind(store.0.gameobject_type_id(), lock_cursor);
        let dist_sq = go_tf.translation.distance_squared(self_tf.translation);
        let unable = kind != CursorKind::PickLock && dist_sq > GO_INTERACT_RANGE_SQ;
        Some((kind, unable))
    };
    // The reference makes one pick over all CGObjects and switches on type; benilla picks unit and
    // GameObject separately, then classifies whichever is nearer under the cursor.
    let resolve_unit = || {
        let (unit_tf, store, net) = units.get(hovered.target?).ok()?;
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
                // The loot base mode is Pickup(8) — `8 + (keyDown(0) ? 8 : 0)` @ `0x48252c`;
                // LootAll waits on auto-loot. Gray by the byte-verified gate (inline in
                // `CanLootNow 0x5ec110`): the same melee interact reach as skin — ~5 yd vs a
                // normal mob (director-measured, confirmed).
                return Some((CursorKind::Pickup, !in_melee));
            }
            if store.0.unit_flags() & UNIT_FLAG_SKINNABLE != 0 {
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

    #[test]
    fn highlightable_gates_the_quest_object_but_not_the_plain_door() {
        // An ordinary unlocked door (no INTERACT_COND) is highlightable regardless of its zero activate
        // bit — plain doors must never gray out.
        assert!(highlightable_flags(0, 0, 0)); // DOOR, no flags
                                               // A GENERIC decoration is never highlightable.
        assert!(!highlightable_flags(GO_TYPE_GENERIC, 0, 0));
        // Neither is the transport family (the byte-dumped constant-false +0x14 slots): no gear,
        // no tooltip, no USE on a boat / elevator / map object — flags can't make them so.
        for t in [GO_TYPE_TRANSPORT, GO_TYPE_MAP_OBJECT, GO_TYPE_MO_TRANSPORT] {
            assert!(!highlightable_flags(t, 0, GO_DYNFLAG_ACTIVATE));
        }
        // A busy (IN_USE) or NO_INTERACT object is not highlightable.
        assert!(!highlightable_flags(3, 0x1, 0)); // CHEST, IN_USE
        assert!(!highlightable_flags(3, 0x10, 0)); // CHEST, NO_INTERACT
                                                   // The quest gate: an INTERACT_COND object is highlightable only with the activate bit set — the
                                                   // exact bug, a quest chest without the quest.
        assert!(!highlightable_flags(3, GO_FLAG_INTERACT_COND, 0)); // quest chest, no quest → clear
        assert!(highlightable_flags(
            3,
            GO_FLAG_INTERACT_COND,
            GO_DYNFLAG_ACTIVATE
        )); // quest chest, quest held → usable
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
