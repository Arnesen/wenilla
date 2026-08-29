//! The app-side **stable feed** (decision 1676) — `PetStableFrame`'s data, the inward half of the
//! stable seam around [`benilla_ui::script`]'s `stable` module, and the twin of
//! [`crate::ui_trainer`]'s trainer feed.
//!
//! The net bridge fills [`StableOpen`] from the wire (`MSG_LIST_STABLED_PETS`, which the server
//! sends unprompted when the gossip stable option is chosen — the trainer's and the vendor's
//! arrangement exactly). Each frame [`feed_stable`] resolves each wire [`StabledPet`] into a
//! Lua-facing [`StablePetSlot`], pushes the snapshot, and fires `PET_STABLE_SHOW` on open /
//! `PET_STABLE_UPDATE` on a content change / `PET_STABLE_CLOSED` on clear. [`drain_stable`] pulls
//! the intents back out into the four wire verbs.
//!
//! ## The resolve, and the one round trip it needs
//!
//! The wire names a `creature_template` **entry** and a loyalty **level**; the window wants an
//! icon, a family word, a loyalty name and a diet. Three of those come from tables already loaded
//! for the live pet ([`PetFamilyTables`], [`PetStatTables`] — decisions 1062/1005); the fourth,
//! the family *id*, is not on the wire at all and must come from the creature query
//! ([`NameCache::resolve_creature`], ask-once).
//!
//! **So a stabled pet's row fills in two steps**, and that is not a defect to paper over: the name
//! and level render immediately off the wire, and the icon/family/diet appear when the template
//! answer lands a frame or two later. The feed diffs, so the arrival re-pushes and fires
//! `PET_STABLE_UPDATE` on its own. The reference has the same two-step (its own template cache
//! misses the first time too); what it does not have is our *empty* first step, because
//! `creaturecache.wdb` persists across sessions and ours does not yet.
//!
//! ## Why the current pet's row is read from the wire, not from the live pet
//!
//! Slot 0 can be occupied while `UnitExists("pet")` is false — a dismissed pet, or one left too far
//! away to be summoned, still gets a row from the server's character-pet cache. The reference
//! prefers the live unit and falls back to `GetStablePetInfo(0)` (`PetStable.lua:120-146`), so both
//! sources must be right; this feed fills the wire half for every row uniformly and lets the Lua
//! choose, rather than special-casing slot 0 into a live-unit read that would be blank for exactly
//! the hunter who needs the window most.

use benilla_protocol::messages::StabledPet;
use bevy::prelude::*;

use benilla_ui::script::{StableIntent, StablePetSlot, StableState, UiScript, NUM_STABLE_SLOTS};

use crate::names::NameCache;
use crate::net::{ClientCommand, NetCommands};
use crate::ui_pet_stats::{PetFamilyTables, PetStatTables};
use crate::ui_script::UiInput;
use crate::ui_session::{close_npc_session_out_of_range, npc_switched, NpcSession};

pub(crate) struct UiStablePlugin;

impl Plugin for UiStablePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StableOpen>().add_systems(
            Update,
            (
                // Range-close before the feed so the clear becomes PET_STABLE_CLOSED the same
                // frame; push before the input pass so an open/close is on screen the same frame;
                // drain after it (the trainer/merchant/gossip ordering).
                close_npc_session_out_of_range::<StableOpen>.before(feed_stable),
                feed_stable.before(UiInput),
                drain_stable.after(UiInput),
            ),
        );
    }
}

/// The open stable, filled by the net bridge ([`crate::net`]) and read by [`feed_stable`]. Holds
/// the stable master's guid and the pet rows exactly as the wire delivered them. Cleared on a
/// client-side close and on disconnect.
#[derive(Resource, Default)]
pub(crate) struct StableOpen {
    /// The stable master whose window is open; `None` = no stable open.
    pub(crate) npc: Option<u64>,
    /// Stable slots **purchased** (0..=2) — the wire's `numStableSlots`, not a count of occupied
    /// ones.
    pub(crate) num_stable_slots: u8,
    /// The wire rows, each already carrying its rebased client slot index.
    pub(crate) pets: Vec<StabledPet>,
}

impl StableOpen {
    /// Open (or replace) the window with a stable master's freshly-listed pets.
    pub(crate) fn open(&mut self, npc: u64, num_stable_slots: u8, pets: Vec<StabledPet>) {
        self.npc = Some(npc);
        self.num_stable_slots = num_stable_slots;
        self.pets = pets;
    }

    pub(crate) fn clear(&mut self) {
        self.npc = None;
        self.num_stable_slots = 0;
        self.pets.clear();
    }
}

/// The stable window is an NPC session: the standardized range guard ([`crate::ui_session`])
/// client-side-closes it when the player walks out of the stable master's service range or the NPC
/// despawns. This matters more here than for most windows — the server re-checks the interaction on
/// **every** stable verb (`CheckStableMaster`), so a window left open at a distance would fail each
/// button with an indistinguishable generic error instead of simply not being there.
impl NpcSession for StableOpen {
    fn npc(&self) -> Option<u64> {
        self.npc
    }

    fn close(&mut self) {
        self.clear();
    }
}

/// Resolve one wire row into the Lua-facing slot. The template answer drives icon/family/diet and
/// arrives asynchronously (module doc), so each of those is independently `None`-able rather than
/// gating the whole row: a pet whose query is in flight still shows its name and level.
fn resolve_pet(
    wire: &StabledPet,
    names: &mut NameCache,
    families: Option<&PetFamilyTables>,
    stats: Option<&PetStatTables>,
    commands: &NetCommands,
) -> StablePetSlot {
    // Ask-once for the template; `guid: 0` is the template-only convention (this pet has no spawn
    // to name — it is asleep in a stable, which is the whole point).
    let _ = names.resolve_creature(wire.creature_entry, 0, commands);
    let family_id = names
        .creature_record(wire.creature_entry)
        .map(|r| r.pet_family)
        .unwrap_or(0);
    let family_row = families.and_then(|t| t.families.get(family_id));
    StablePetSlot {
        pet_number: wire.pet_number,
        // The family's own `CreatureFamily.dbc` icon column — a pet has no item behind it, so this
        // is the only icon there is. `None` until the query lands; the reference's
        // `SetItemButtonTexture` renders the empty-slot art for it, which is right (decision 1046:
        // a path resolving to nothing would draw WHITE).
        icon: families
            .and_then(|t| t.families.icon(family_id))
            .map(str::to_string),
        // The pet's given name, straight off the wire — never the template's.
        name: wire.name.clone(),
        level: wire.level,
        family: family_row.map(|f| f.name.clone()),
        // The wire carries a loyalty LEVEL; the window shows the `PetLoyalty.dbc` name for it, the
        // same table `GetPetLoyalty` reads for the live pet (decision 1005).
        loyalty: stats
            .and_then(|t| t.loyalty.name(wire.loyalty))
            .map(str::to_string),
        // The family row's food mask expanded to localized diet names — `GetPetFoodTypes`'s law
        // (decision 1062). Legitimately empty for a zero mask; the reference guards on that.
        diet: family_row
            .map(|f| {
                families
                    .map(|t| {
                        t.foods
                            .for_mask(f.pet_food_mask)
                            .into_iter()
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default(),
    }
}

/// Build the Lua-facing snapshot from [`StableOpen`] — `None` when no stable is open.
fn snapshot(
    open: &StableOpen,
    names: &mut NameCache,
    families: Option<&PetFamilyTables>,
    stats: Option<&PetStatTables>,
    next_slot_cost: u32,
    commands: &NetCommands,
) -> Option<StableState> {
    open.npc?;
    let mut slots: [Option<StablePetSlot>; NUM_STABLE_SLOTS] = Default::default();
    for wire in &open.pets {
        // Seat each row by its OWN slot, never by position: the current-pet row is absent for a
        // petless hunter, so `pets[0]` is not slot 0 (decision 1676's decode note). A slot byte
        // past the window's three is dropped rather than panicking — the server never sends one,
        // and a malformed list must not take the window down.
        match slots.get_mut(usize::from(wire.slot)) {
            Some(seat) => *seat = Some(resolve_pet(wire, names, families, stats, commands)),
            None => debug!(
                "ui_stable: pet {} arrived in slot {} — past the {NUM_STABLE_SLOTS} the window has, dropped",
                wire.pet_number, wire.slot
            ),
        }
    }
    Some(StableState {
        num_stable_slots: u32::from(open.num_stable_slots),
        next_slot_cost,
        slots,
    })
}

/// Push the current stable into the VM and fire the show/update/close events on a transition (or a
/// content change). Diffed against a `Local` memory, exactly like the trainer/merchant feeds.
#[allow(clippy::too_many_arguments)] // the resolver's full catalog set
fn feed_stable(
    script: Option<NonSendMut<UiScript>>,
    open: Res<StableOpen>,
    families: Option<Res<PetFamilyTables>>,
    stats: Option<Res<PetStatTables>>,
    prices: Option<Res<StableSlotPrices>>,
    commands: Res<NetCommands>,
    mut names: ResMut<NameCache>,
    mut last: Local<crate::ui_script::VmMemo<Option<StableState>>>,
    mut last_npc: Local<crate::ui_script::VmMemo<Option<u64>>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let last = last.get(&script);
    let last_npc = last_npc.get(&script);

    // The next slot's price is the client's own job (the wire never sends it) — `StableSlotPrices`
    // row `purchased + 1`, the bank's arrangement. `0` past the table, a state in which the
    // reference has already hidden the row that would show it.
    let next_slot_cost = prices
        .as_deref()
        .and_then(|p| p.0.next_slot_price(open.num_stable_slots))
        .unwrap_or(0);

    let fresh = snapshot(
        &open,
        &mut names,
        families.as_deref(),
        stats.as_deref(),
        next_slot_cost,
        &commands,
    );
    let switched = npc_switched(*last_npc, open.npc);
    if fresh == *last && !switched {
        return;
    }
    // PUSH before firing: `fire_event` dispatches the Lua handlers synchronously, so the snapshot
    // must already be in the VM when `PetStable_Update` reads it back (decision 1073's rule).
    script.set_stable(fresh.clone());
    if switched {
        // A different stable master while the window is open is a real close+open — the reference's
        // ShowUIPanel early-returns when visible, so the open sound only re-plays after a hide
        // (decision 0096). Consume the close intent the CLOSED→OnHide→ClosePetStables round queues,
        // so the drain does not clear the stable we just re-opened to.
        script.fire_event("PET_STABLE_CLOSED", vec![]);
        script.fire_event("PET_STABLE_SHOW", vec![]);
        let _ = script.take_stable_close();
    } else {
        match (&*last, &fresh) {
            (None, Some(_)) => script.fire_event("PET_STABLE_SHOW", vec![]),
            // The paperdoll event rides the content change: the reference repaints the model pane
            // from it, and a stabled pet's icon/family arriving late is exactly when it must.
            (Some(_), Some(_)) => {
                script.fire_event("PET_STABLE_UPDATE", vec![]);
                script.fire_event("PET_STABLE_UPDATE_PAPERDOLL", vec![]);
            }
            (Some(_), None) => script.fire_event("PET_STABLE_CLOSED", vec![]),
            (None, None) => {}
        }
    }
    *last = fresh;
    *last_npc = open.npc;
}

/// Drain the Lua intents into the four wire verbs, and the close into a local clear (no packet
/// exists — vmangos has no close opcode).
///
/// Every verb is addressed to the **open session's** NPC: the guid is the app's, never Lua's, so an
/// addon cannot aim a stable verb at an arbitrary unit.
fn drain_stable(
    script: Option<NonSendMut<UiScript>>,
    mut open: ResMut<StableOpen>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    for intent in script.take_stable_intents() {
        let Some(npc) = open.npc else {
            debug!("ui_stable: {intent:?} with no open stable — ignored");
            continue;
        };
        debug!("ui_stable: {intent:?} (stable master {npc:#x})");
        let command = match intent {
            StableIntent::Stable => ClientCommand::StablePet { npc },
            StableIntent::Unstable(pet_number) => ClientCommand::UnstablePet { npc, pet_number },
            StableIntent::Swap(pet_number) => ClientCommand::StableSwapPet { npc, pet_number },
            StableIntent::BuySlot => ClientCommand::BuyStableSlot { npc },
        };
        let _ = commands.0.send(command);
    }
    if script.take_stable_close() {
        debug!("ui_stable: client-side close (no packet)");
        open.clear();
    }
}

/// `StableSlotPrices.dbc`, loaded once at startup — the purchase row's price oracle. A missing
/// table quotes `0`, which is the same shape as "past the ladder": degraded, never wrong.
#[derive(Resource)]
pub(crate) struct StableSlotPrices(pub(crate) benilla_formats::StableSlotPrices);
