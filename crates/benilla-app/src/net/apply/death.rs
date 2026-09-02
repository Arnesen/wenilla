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

use super::super::SelfGuid;

/// OUR corpse streaming into range (a `TYPEID_CORPSE` create whose owner is us): remember its guid
/// for the reclaim send (decision 0308 §5). The kind is exact ([`EntityKind::Corpse`] since 1706 —
/// it was `Other` while nothing rendered a corpse), and the owner field is corpse-only, so nothing
/// else can match. Rides the `ObjectCreate` arm.
///
/// **Two gates, not one** (wow-re `corpse-click-and-reclaim.md`, §5 cross-checked): the reference's
/// reclaim guid `[0xb4e328/32c]` has exactly one writer, `0x4920d0`, called from three sites in the
/// CGCorpse translation unit — and every one of them is gated on `CORPSE_FIELD_OWNER == me`
/// **and `CORPSE_FIELD_FLAGS` bit 0 clear**, i.e. a real body and not a bone pile. So the latch is
/// a property of the streamed corpse OBJECT, not of the `MSG_CORPSE_QUERY` answer, and a pile of
/// bones never arms it. That matters because `RetrieveCorpse` itself carries **no guard of any
/// kind** — whatever is in the latch is what goes out on `CMSG_RECLAIM_CORPSE`. The bones gate is
/// the only thing standing between a converted corpse and a reclaim send naming it.
pub(super) fn note_corpse(
    guid: u64,
    kind: EntityKind,
    fields: &ObjectFields,
    self_guid: &SelfGuid,
    death_net: &mut DeathNet,
) {
    if kind == EntityKind::Corpse
        && fields.corpse_owner() == self_guid.0
        && !fields.corpse_is_bones()
    {
        death_net.corpse_guid = Some(guid);
    }
}

/// The same latch, re-evaluated when a corpse's **`CORPSE_FIELD_FLAGS` changes under a live guid**.
///
/// [`forget_corpse`] covers the conversion shape where the server destroys the corpse object and
/// creates the bone pile as a new one. It does not cover an in-place flip — the same guid gaining
/// `CORPSE_FLAG_BONES` through a values delta — and the reference is immune to that shape by
/// construction: its three latch writers include the `FLAGS` mirror handler `0x5d6d60`, so the
/// gate is re-asked on every change of the very field that carries the bones bit. Ours asks it
/// here, on the same edge. Rides the `ObjectValues` arm.
pub(super) fn recheck_corpse(
    guid: u64,
    fields: &ObjectFields,
    self_guid: &SelfGuid,
    death_net: &mut DeathNet,
) {
    // A values delta carries only what changed: an untouched FLAGS field reads absent, and absent
    // must not be read as "bit clear" (that is the `ObjectFields` create-vs-delta seam). So act
    // only when the field is actually present in this delta.
    let Some(flags) = fields.corpse_flags_present() else {
        return;
    };
    let mine = fields.corpse_owner() == self_guid.0 || death_net.corpse_guid == Some(guid);
    if !mine {
        return;
    }
    if flags & 0x01 != 0 {
        // Converted to bones under our own guid — drop it before a reclaim can name it.
        if death_net.corpse_guid == Some(guid) {
            death_net.corpse_guid = None;
        }
    } else if fields.corpse_owner() == self_guid.0 {
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
    // Through [`DeathNet::ask_spirit_healer`], which the right-click's own bit-5 arm also calls —
    // the reference raises this dialog client-side and vmangos also pushes it, and one entry
    // point is what keeps the two roads saying the same thing (decision 1861).
    death_net.ask_spirit_healer(npc);
}

/// `SMSG_DURABILITY_DAMAGE_DEATH` — the 10% death durability loss.
///
/// **It is a combat-log line, not a red error** (1703, correcting the shape 0308 shipped):
/// `0x628e60` is itself the packet's handler and it emits at the literal chat type `0x19`
/// `COMBAT_MISC_INFO`, with **zero arguments** — the packet's body is empty and both of vmangos's
/// fields are ignored. Routing it to `UIErrorsFrame` put it in the wrong frame and, worse, meant
/// hard-coding Blizzard's English sentence in our source; as a combat-log family it resolves
/// `DURABILITYDAMAGE_DEATH` out of the player's own `GlobalStrings.lua` like every other line.
pub(super) fn durability_damage_death(log: &mut crate::ui_chat::ChatLog) {
    log.push_combat(crate::ui_chat::combat::PendingCombat {
        kind: crate::ui_chat::ChatEventKind::CombatMiscInfo,
        family: crate::ui_chat::combat::DURABILITYDAMAGE_DEATH,
        variant: crate::ui_chat::combat::Variant::OtherOther,
        subject: 0,
        object: 0,
        fills: crate::ui_chat::combat::Fills::default(),
        named: crate::ui_chat::combat::Named::Ready,
        tries: 0,
    });
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
