//! Death-arc arm bodies for [`super::apply_net_updates`]'s dispatch match (decision 0308) — the
//! wire-fed [`DeathNet`] stores (the corpse marker and its guid latch, the reclaim clock, the
//! resurrect offer, the spirit-healer confirm, the death durability notice) plus the granted
//! movement-mode forward the server addresses to our own mover. Each `pub(super)` fn here is
//! exactly one arm's body — except the two corpse-guid latches, which ride the object-lifecycle
//! arms; the match at the call site stays the dispatcher, one call per arm.

use benilla_protocol::{EntityKind, MoveMode, ObjectFields};
use bevy::prelude::*;

use crate::death::{CorpsePoint, DeathNet, ResurrectOffer};
use crate::net::MoveModeMessage;
use crate::ui_items::UiErrorLines;

use super::super::SelfGuid;

/// OUR corpse streaming into range (a `TYPEID_CORPSE` create whose owner is us): remember its guid
/// for the reclaim send (decision 0308 §5). Corpses classify as [`EntityKind::Other`], and the
/// owner field is corpse-only, so the filter is exact. Rides the `ObjectCreate` arm.
pub(super) fn note_corpse(
    guid: u64,
    kind: EntityKind,
    fields: &ObjectFields,
    self_guid: &SelfGuid,
    death_net: &mut DeathNet,
) {
    if kind == EntityKind::Other && fields.corpse_owner() == self_guid.0 {
        death_net.corpse_guid = Some(guid);
    }
}

/// The corpse-to-bones swap destroys the corpse object under its guid (0308 §1); a stale guid must
/// not ride a later reclaim. Rides the `ObjectDestroyed` arm.
pub(super) fn forget_corpse(guid: u64, death_net: &mut DeathNet) {
    if death_net.corpse_guid == Some(guid) {
        death_net.corpse_guid = None;
    }
}

/// `MSG_CORPSE_QUERY`'s answer — where the corpse marker goes. A not-found (reactive, or the
/// server's unprompted bones-conversion push) drops the marker.
pub(super) fn corpse_query(
    found: bool,
    display_map: i32,
    position: [f32; 3],
    corpse_map: u32,
    death_net: &mut DeathNet,
) {
    death_net.corpse = found.then_some(CorpsePoint {
        display_map,
        position,
        corpse_map,
    });
}

/// `SMSG_CORPSE_RECLAIM_DELAY` — the delay, anchored to arrival time (`now_secs` is the same
/// `Time::elapsed_secs_f64` the feed reads back). The client's `0x269` handler re-fires the
/// corpse-range events through its latch (wow-re death-ui.md §4), so the feed re-announces on the
/// generation bump.
pub(super) fn corpse_reclaim_delay(delay_ms: u32, now_secs: f64, death_net: &mut DeathNet) {
    death_net.reclaim_at = Some(now_secs + f64::from(delay_ms) / 1000.0);
    death_net.reclaim_generation = death_net.reclaim_generation.wrapping_add(1);
}

/// `SMSG_RESURRECT_REQUEST` — the RESURRECT popup's data.
pub(super) fn resurrect_request(
    caster: u64,
    name: String,
    sickness: bool,
    has_timer: bool,
    death_net: &mut DeathNet,
) {
    death_net.resurrect = Some(ResurrectOffer {
        caster,
        name,
        sickness,
        has_timer,
    });
}

/// `SMSG_SPIRIT_HEALER_CONFIRM` — the healer awaiting the XP-loss two-step's Accept. The message
/// IS the announce (decision 1068): the healer's gossip re-sends it on every ask, and the
/// reference fires `CONFIRM_XP_LOSS` per arrival — so the generation bump is what re-shows a
/// cancelled confirm, exactly the `SMSG_CORPSE_RECLAIM_DELAY` re-fire pattern above.
pub(super) fn spirit_healer_confirm(npc: u64, death_net: &mut DeathNet) {
    death_net.spirit_healer = Some(npc);
    death_net.confirm_generation = death_net.confirm_generation.wrapping_add(1);
}

/// `SMSG_DURABILITY_DAMAGE_DEATH` — the red line, verbatim GlobalStrings `DURABILITYDAMAGE_DEATH`
/// (the `%%` unescaped).
pub(super) fn durability_damage_death(lines: &mut UiErrorLines) {
    lines
        .0
        .push("Your equipped items suffer a 10% durability loss.".to_string());
}

/// **The ack'd movement-mode family** (decision 0866) — root, water-walk, feather-fall, hover. The
/// server only ever addresses the controlling client's own mover; the guard keeps a stray relay
/// harmless. Water-walk is additionally mirrored into [`DeathNet`], which reads it as a ghost-form
/// cue — but the *mover* effect of every mode, this one included, is the controller's
/// ([`crate::player::wire_in`]).
pub(super) fn move_mode(
    guid: u64,
    counter: u32,
    mode: MoveMode,
    apply: bool,
    self_guid: &SelfGuid,
    death_net: &mut DeathNet,
    out: &mut MessageWriter<MoveModeMessage>,
) {
    if self_guid.0 != Some(guid) {
        return;
    }
    if mode == MoveMode::WaterWalk {
        death_net.water_walk = apply;
    }
    out.write(MoveModeMessage {
        guid,
        counter,
        mode,
        apply,
    });
}
