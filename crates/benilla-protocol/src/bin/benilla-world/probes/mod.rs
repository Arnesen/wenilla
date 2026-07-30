//! The probe registry: one file per `--flag` scenario, each a self-contained [`Probe`] with a
//! uniform lifecycle. Registration order (in `main`) = today's stream-loop/verify block order, so
//! output and execution order are preserved.

use anyhow::Result;
use benilla_protocol::{SessionEvent, WorldSession};

use crate::world::World;

/// What a [`Probe`]'s lifecycle methods borrow: the live session (to send) + the shared [`World`].
pub(crate) struct Ctx<'a> {
    pub session: &'a mut WorldSession,
    pub world: &'a mut World,
}

/// One scripted wire-verification scenario. Registered from a `--flag`; the pump drives every
/// registered probe through this lifecycle in registration order.
pub(crate) trait Probe {
    /// One-time pre-stream staging (GM teleports, cleanup commands).
    fn stage(&mut self, cx: &mut Ctx) -> Result<()> {
        let _ = cx;
        Ok(())
    }
    /// Every pump iteration, before recv.
    fn poll(&mut self, cx: &mut Ctx) -> Result<()> {
        let _ = cx;
        Ok(())
    }
    /// Every decoded event, AFTER [`World::on_event`] has processed it.
    fn on_event(&mut self, ev: &SessionEvent, cx: &mut Ctx) -> Result<()> {
        let _ = (ev, cx);
        Ok(())
    }
    /// Post-stream: assertions + follow-up round trips (each block's bespoke drains move verbatim).
    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        let _ = cx;
        Ok(())
    }
}

// Quest consts shared by more than one probe (put here rather than `pub(super)` in quest.rs
// because giverstatus/questlog re-use them and neither is quest.rs's child).

/// Marshal McBride's teleport spot — the `--quest` turn-in NPC, and the `--questlog`/`--giverstatus`
/// stage target (McBride is quest 7's giver *and* ender).
pub(crate) const QUEST_TURNIN_TP: &str = ".go xyz -8902.59 -162.606 82.0223"; // onto Marshal McBride
pub(crate) const QUEST_TURNIN_ENTRY: u32 = 197; // Marshal McBride — takes 783
/// `PLAYER_QUEST_LOG_1_1` UpdateField index for 1.12.1 (UNIT_END 188 + 0xA), 3 fields per slot ×
/// 20 slots (vmangos `UpdateFields_1_12_1.h:128`).
pub(crate) const FIELD_PLAYER_QUEST_LOG_1_1: u16 = 198;

/// The `--questlog` probe target: quest 7 "Kobold Camp Cleanup" — Marshal McBride (entry 197, the
/// same NPC [`QUEST_TURNIN_TP`]/[`QUEST_TURNIN_ENTRY`] already land on) is *both* giver and ender
/// (VERIFIED live against the running vmangos: `mangos.creature_questrelation` and
/// `creature_involvedrelation` each carry exactly one row for quest 7, both `197`). Its one
/// objective — kill 10 of creature entry 6 (Kobold) — is a real `required_count > 0` slot, unlike
/// 783's report-only "A Threat Within" the `--quest` probe turns in; a GM `.quest complete`
/// substitutes for the grind, same as `--quest`. Shared with `--giverstatus`, which `.quest remove`s
/// it in staging so McBride reads AVAILABLE for a fresh log.
pub(crate) const QUESTLOG_ID: u32 = 7;

mod attack;
mod aura;
mod charge;
mod death;
mod equip_pack_slot;
mod giverstatus;
mod groundfx;
mod loot;
mod query_names;
mod quest;
mod quest_item;
mod questlog;
mod speed;
mod spells;
mod spirit;
mod swap_pack_slots;
mod use_pack_slot;
mod vendor;
mod worldstate;

pub(crate) use attack::Attack;
pub(crate) use aura::Aura;
pub(crate) use charge::Charge;
pub(crate) use death::Death;
pub(crate) use equip_pack_slot::EquipPackSlot;
pub(crate) use giverstatus::GiverStatus;
pub(crate) use groundfx::GroundFx;
pub(crate) use loot::Loot;
pub(crate) use query_names::QueryNames;
pub(crate) use quest::Quest;
pub(crate) use quest_item::QuestItem;
pub(crate) use questlog::QuestLog;
pub(crate) use speed::Speed;
pub(crate) use spells::Spells;
pub(crate) use spirit::Spirit;
pub(crate) use swap_pack_slots::SwapPackSlots;
pub(crate) use use_pack_slot::UsePackSlot;
pub(crate) use vendor::Vendor;
pub(crate) use worldstate::WorldState;
