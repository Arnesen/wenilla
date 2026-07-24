//! The partner live probe (`WOW_PROBE=partner`) — the party arc's instrument (decision 0434):
//! a second client that says YES. Once in-world it auto-accepts every group invite, so the
//! director can exercise the whole party surface — invite, roster lines, member frames,
//! leader/loot management, pings — solo, with the probe account as the other member. Non-combat
//! (the unattended-combat ban, `method.md` "The local vmangos server", stands untouched: this
//! probe never swings). Pair with the slot-keyed probe identity (`WOW_USER=probeN …`, method.md).

use bevy::prelude::*;

use crate::net::SelfPlayer;
use crate::ui_party::GroupState;

pub(crate) struct ProbePartnerPlugin;

impl Plugin for ProbePartnerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, partner_probe);
    }
}

/// A pending invite is answered with `CMSG_GROUP_ACCEPT` the frame it lands. Clearing
/// `pending_invite` here keeps the probe's own PARTY_INVITE popup from ever arming (and if the
/// popup won a same-frame race, its hide-path `DeclineGroup` is a server no-op once we're
/// grouped — vmangos ignores a decline with no pending invite).
fn partner_probe(
    self_player: Query<(), With<SelfPlayer>>,
    mut group: ResMut<GroupState>,
    net: Res<crate::net::NetCommands>,
) {
    if self_player.is_empty() {
        return;
    }
    if let Some(inviter) = group.pending_invite.take() {
        info!("partner probe: accepting {inviter}'s group invite");
        let _ = net.0.send(crate::net::ClientCommand::GroupAccept);
    }
}
