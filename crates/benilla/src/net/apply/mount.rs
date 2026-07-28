//! The mount arc's arm bodies for [`super::apply_net_updates`]'s dispatch match (decision 0441) —
//! the (dis)mount attempt's result code (P1) and a nearby rider's flourish (P2). Each `pub(super)`
//! fn here is exactly one arm's body; the match at the call site stays the dispatcher, one call per
//! arm.

use bevy::prelude::*;

use crate::creature_anim::MountFlourish;
use crate::ui_action::MountErrors;

use super::super::{GuidIndex, SelfGuid};

/// `SMSG_MOUNTRESULT`/`SMSG_DISMOUNTRESULT` — OK is silent in the reference (10 mounting,
/// 3 dismounting); a failure queues the red error line (`ui_action::mount_result_key` — resolved
/// against the VM's GlobalStrings at drain).
pub(super) fn mount_result(mount: bool, code: u32, errors: &mut MountErrors) {
    let ok = if mount { code == 10 } else { code == 3 };
    if !ok {
        info!(
            "net: {}mount refused (code {code})",
            if mount { "" } else { "dis" }
        );
        errors.0.push((mount, code));
    }
}

/// `SMSG_MOUNTSPECIAL_ANIM` — a nearby rider's flourish: rear their mount (MountSpecial 94 on the
/// mount child; the hop happens in `creature_anim::flourish_to_anim`).
///
/// Our OWN guid is dropped: we played it locally at send time, and whether the sender gets the
/// SMSG echoed back is a server-config detail (LIVE-VERIFIED 2026-07-17, double-flourish probe:
/// vmangos's `SendMovementMessageToSet(.., false)` only cheat-logs on the flag — the
/// non-broadcaster delivery hardcodes self=true, so our deployment echoes; the optional per-player
/// broadcaster honors it and would not). Self-suppression on receive is correct under both configs.
pub(super) fn mount_special(
    guid: u64,
    self_guid: &SelfGuid,
    index: &GuidIndex,
    out: &mut MessageWriter<MountFlourish>,
) {
    if self_guid.0 != Some(guid) {
        if let Some(&e) = index.0.get(&guid) {
            out.write(MountFlourish { unit: e });
        }
    }
}
