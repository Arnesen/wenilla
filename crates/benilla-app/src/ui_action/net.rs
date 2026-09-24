//! The action bar's packet handler (in the net handler table since 2322, moved out of the drain's
//! mount arm file) — the (dis)mount attempt's result code (decision 0441 P1) onto the mount error
//! line. The flourish half of that arc is [`crate::creature_anim::net`]'s.

use benilla_protocol::{SessionEvent, SessionEventKind};
use bevy::prelude::*;

use super::MountErrors;
use crate::net::NetHandlerApp;

/// Register the handler — called from [`super::UiActionPlugin`].
pub(super) fn register(app: &mut App) {
    app.net_handler(SessionEventKind::MountResult, on_mount_result);
}

fn on_mount_result(In(ev): In<SessionEvent>, mut errors: ResMut<MountErrors>) {
    if let SessionEvent::MountResult { mount, code } = ev {
        mount_result(mount, code, &mut errors);
    }
}

/// `SMSG_MOUNTRESULT`/`SMSG_DISMOUNTRESULT` — OK is silent in the reference (10 mounting,
/// 3 dismounting); a failure queues the red error line (`ui_action::mount_result_key` — resolved
/// against the VM's GlobalStrings at drain).
fn mount_result(mount: bool, code: u32, errors: &mut MountErrors) {
    let ok = if mount { code == 10 } else { code == 3 };
    if !ok {
        info!(
            "net: {}mount refused (code {code})",
            if mount { "" } else { "dis" }
        );
        errors.0.push((mount, code));
    }
}
