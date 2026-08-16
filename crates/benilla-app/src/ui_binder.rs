//! Setting your hearthstone — the innkeeper's bind question, its dialog, and its answer
//! (decision 1331).
//!
//! **The law this module exists for: selecting the innkeeper's gossip line binds nothing.** It
//! makes the server close the gossip menu and ask (`SMSG_BINDER_CONFIRM`, vmangos
//! `Player::SetBindPoint`); the bind happens only when the client answers `CMSG_BINDER_ACTIVATE`,
//! at which point the innkeeper casts spell 3286 "Bind" and `SPELL_EFFECT_BIND` sends
//! `SMSG_BINDPOINTUPDATE` + `SMSG_PLAYERBOUND`. Before this module the question was parsed as an
//! unknown opcode and dropped, so the click looked like it did nothing at all — half of B249.
//!
//! The Era surface it drives is the reference's own, and it is small (`StaticPopup.lua:1308-1322`,
//! `UIParent.lua:125`/`547-548`):
//!
//! - the `CONFIRM_BINDER` event, whose **one argument** is the name that fills
//!   `"Do you want to make %s your new home?"`;
//! - `ConfirmBinder()` — the dialog's Accept, the only call that sends the activate;
//! - `CheckBinderDist()` — polled from the dialog's `OnUpdate`, and the dialog hides itself the
//!   frame it goes false.
//!
//! Both halves are now **byte-pinned** against the reference's own handler `0x5dfdc0` and its
//! `SMSG_BINDER_CONFIRM` arm `0x5e4aa2` (wow-re
//! `system/ui/scratch/gossip-icon-and-binder-flow.md`; folded back by decision 1335, which
//! promotes what decision 1331 had to leave INFERRED):
//!
//! 1. **`arg1` is an AREA name, never the NPC's** — the handler never looks a name up. It resolves
//!    the player's own **sub-area** through `AreaTable.dbc`, falls back to the **parent zone** when
//!    that row is missing, and falls back again to the localized `HOME_INN` GlobalString. It always
//!    fires; there is no path on which the question is withheld for want of a name. [`area_name`]
//!    is that chain.
//! 2. **`CheckBinderDist()` is the same gate the rest of the NPC surface uses** — not a
//!    resemblance: the reference's range test is `d² <= [0xc4c28c]`, and `[0xc4c28c]` is
//!    `5.55555534362793²`, the identical constant behind [`crate::target::SERVICE_RANGE_SQ`]
//!    (boundary-inclusive at both sites). So modelling the question as an [`NpcSession`] and
//!    letting [`crate::ui_session::close_npc_session_out_of_range`] close it is faithful, not a
//!    convenience. It is also why nothing here needs a disconnect teardown: an unstreamed guid
//!    reads as gone and closes the question on the next frame.
//!
//! One deviation remains, and it is deliberate: **the reference never clears its latched guid**
//! (`0xc4d7c0`), so a second `ConfirmBinder()` while still in range re-sends the activate. We drop
//! the guid when `SMSG_PLAYERBOUND` says the bind took, because a question that has been answered
//! is not a question — the reference's own dialog is gone by then either way.

use benilla_ui::script::{ScriptValue, UiScript};
use bevy::prelude::*;

use crate::area::AreaTableRes;
use crate::net::{ClientCommand, NetCommands};
use crate::ui_script::UiInput;
use crate::ui_session::{close_npc_session_out_of_range, NpcSession};

/// `HOME_INN` (GlobalStrings.lua:2278, verbatim) — the tail of the reference's `arg1` chain, used
/// when neither the sub-area nor the zone resolves an `AreaTable` name. Its own `GetBindLocation`
/// uses the identical fallback, which is how wow-re cross-checked the order.
const HOME_INN: &str = "your inn";

/// The innkeeper's pending bind question. Written by the net drain's `BinderConfirm` arm, read by
/// [`feed_binder`] (which fires `CONFIRM_BINDER` and publishes `CheckBinderDist`'s answer) and by
/// [`drain_binder`] (which turns the dialog's Accept into the activate).
///
/// There is no snapshot beside it: everything the dialog reads arrives as the event's argument —
/// [`crate::ui_duel::DuelState`]'s shape, for the same reason.
#[derive(Resource, Default)]
pub(crate) struct BinderState {
    /// The innkeeper who asked; `None` = no question pending. This guid is what goes back on the
    /// wire, and vmangos resolves it to a live innkeeper in range — a stale one binds nothing.
    npc: Option<u64>,
    /// A question the feed still owes the UI. Set per *packet*, not per state edge: asking the
    /// same innkeeper twice in a row (decline, then click the line again) is two dialogs, and an
    /// edge-diff would swallow the second.
    ask: bool,
}

impl BinderState {
    /// `SMSG_BINDER_CONFIRM` — park the innkeeper's guid and owe the UI a dialog.
    pub(crate) fn ask(&mut self, npc: u64) {
        self.npc = Some(npc);
        self.ask = true;
    }

    /// The guid to answer with, if a question is live.
    fn pending(&self) -> Option<u64> {
        self.npc
    }

    /// Retract the question — the range guard's close, and `SMSG_PLAYERBOUND`'s (the bind landed,
    /// so there is nothing left to ask). Inherent so the net drain can call it without importing
    /// [`NpcSession`], exactly as [`crate::ui_gossip::GossipState::clear`] is.
    pub(crate) fn clear(&mut self) {
        self.npc = None;
        self.ask = false;
    }
}

/// The question is an NPC session: the standardized range guard closes it — the same
/// no-packet clear declining does — when the player walks out of the innkeeper's service range or
/// the innkeeper despawns. That close is what `CheckBinderDist()` reports (module doc, deviation 2).
impl NpcSession for BinderState {
    fn npc(&self) -> Option<u64> {
        self.npc
    }

    fn close(&mut self) {
        self.clear();
    }
}

/// Fire `CONFIRM_BINDER` for a question the UI is still owed, and publish `CheckBinderDist()`'s
/// answer every frame.
///
/// The fire is **unconditional** once a packet has asked — [`area_name`] always yields a string,
/// exactly as the reference's handler always reaches its `SignalEvent2`.
fn feed_binder(
    script: Option<NonSendMut<UiScript>>,
    mut binder: ResMut<BinderState>,
    world: benilla_world::world_point::WorldPoint,
    areas: Option<Res<AreaTableRes>>,
) {
    let Some(mut script) = script else {
        return;
    };
    script.set_binder_pending(binder.pending().is_some());

    if !binder.ask {
        return;
    }
    binder.ask = false;
    let name = area_name(&world, areas.as_deref());
    script.fire_event("CONFIRM_BINDER", vec![ScriptValue::Str(name)]);
}

/// The name that fills the dialog's `%s` — the reference's own three-step chain (`0x5dfe5e`):
/// **sub-area, else parent zone, else `HOME_INN`**.
///
/// Never `None`, because the reference never withholds the question: a missing catalog or an
/// unresolvable area still fires, carrying the GlobalString. That is the difference between "we
/// don't know where you are" and "there is nothing to ask" — only the second should be silent, and
/// only the packet decides it.
fn area_name(
    world: &benilla_world::world_point::WorldPoint,
    areas: Option<&AreaTableRes>,
) -> String {
    area_name_of(world.area(), areas)
}

/// [`area_name`]'s chain over a bare leaf id — split out so the three legs are testable against the
/// real `AreaTable` without standing up a world.
fn area_name_of(leaf: Option<u32>, areas: Option<&AreaTableRes>) -> String {
    let named = |id: u32| {
        areas
            .and_then(|a| a.0.get(id))
            .filter(|row| !row.name.is_empty())
            .map(|row| row.name.clone())
    };
    // The parent zone is the leaf's single-hop `zone_id` (itself when 0) — the same walk
    // `crate::area`'s zone-text resolve does.
    let zone = leaf
        .and_then(|id| areas.and_then(|a| a.0.get(id)))
        .map(|row| row.zone_id)
        .filter(|z| *z != 0);
    leaf.and_then(named)
        .or_else(|| zone.and_then(named))
        .unwrap_or_else(|| HOME_INN.to_string())
}

/// The `SMSG_PLAYERBOUND` line, composed — `ERR_DEATHBIND_SUCCESS_S` with the packet's area name.
fn bound_line(area_name: &str) -> String {
    ERR_DEATHBIND_SUCCESS_S.replace("%s", area_name)
}

/// Turn the dialog's Accept into `CMSG_BINDER_ACTIVATE`.
///
/// Gated on a question still being pending, exactly as the arbiter gate guards
/// [`crate::ui_duel`]'s accept: a `ConfirmBinder()` typed at the console with no innkeeper asking
/// would otherwise send a zero guid the server can only drop.
fn drain_binder(
    script: Option<NonSendMut<UiScript>>,
    binder: Res<BinderState>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    let confirms = script.take_binder_confirms();
    if confirms == 0 {
        return;
    }
    let Some(npc) = binder.pending() else {
        return;
    };
    for _ in 0..confirms {
        let _ = commands
            .0
            .send(ClientCommand::BinderActivate { binder: npc });
    }
}

/// `ERR_DEATHBIND_SUCCESS_S` (GlobalStrings.lua:1543, verbatim) — the line the reference prints
/// when the bind lands, substituted with the **packet's** area name.
const ERR_DEATHBIND_SUCCESS_S: &str = "%s is now your home.";

/// `SoundEntries.dbc` id 1141 — played on `SMSG_PLAYERBOUND` **unconditionally**, before the area
/// is resolved (`0x5e3d6e`, ahead of the bounds test). So an area id the catalog cannot name is
/// silent but still audible, which is the reference's own ordering rather than an accident of ours.
const SOUND_PLAYERBOUND: u32 = 1141;

/// The net drain's `SessionEvent::PlayerBound` arm, factored here so the wire law lives beside the
/// state it drives.
pub(crate) mod apply {
    use super::*;

    use bevy::ecs::message::MessageWriter;

    use crate::net::{ServerSoundKind, ServerSoundMessage};
    use crate::ui_chat::{ChatEvent, ChatEventKind, ChatLog};

    /// `SMSG_PLAYERBOUND` — the bind took. Retract the question, play the sound, and print
    /// "<area> is now your home." as CHAT_MSG_SYSTEM (`0x5e3d3f`: the sound first, then
    /// `DisplayError(0x138)` with the packet's own area id resolved through `AreaTable`).
    ///
    /// This is the **feedback half of B249** — "accepting appears to change nothing" was partly
    /// that nothing ever said it had. The hearthstone itself moves on `SMSG_BINDPOINTUPDATE`, the
    /// packet beside this one; this arm deliberately does not touch it, exactly as the reference's
    /// does not (its `GetBindLocation` reads a different global entirely).
    pub(crate) fn bound(
        area: u32,
        binder: &mut BinderState,
        chat_log: &mut ChatLog,
        areas: Option<&AreaTableRes>,
        sounds: &mut MessageWriter<ServerSoundMessage>,
    ) {
        binder.clear();
        sounds.write(ServerSoundMessage {
            kind: ServerSoundKind::Sound2d,
            sound_id: SOUND_PLAYERBOUND,
            source: None,
        });
        let Some(name) = areas.and_then(|a| a.0.name(area)).filter(|n| !n.is_empty()) else {
            debug!("ui_binder: bound to area {area}, which AreaTable does not name — no line");
            return;
        };
        chat_log.push_event(ChatEvent::text_only(
            ChatEventKind::System,
            super::bound_line(name),
        ));
    }
}

/// The innkeeper bind flow: the range guard, the dialog's feed, and its answer.
pub(crate) struct UiBinderPlugin;

impl Plugin for UiBinderPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BinderState>().add_systems(
            Update,
            (
                // Range-close before the feed so walking away takes the dialog down the same
                // frame (the gossip window's ordering, for the same reason).
                close_npc_session_out_of_range::<BinderState>.before(feed_binder),
                feed_binder.before(UiInput),
                drain_binder.after(UiInput),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second question from the same innkeeper is a second dialog. The `ask` flag is per packet
    /// precisely so a decline-then-click-again (which never passes through `None`) is not
    /// swallowed by a state diff.
    #[test]
    fn asking_twice_owes_two_dialogs() {
        let mut binder = BinderState::default();
        assert_eq!(binder.pending(), None);

        binder.ask(0x2a);
        assert!(binder.ask);
        binder.ask = false; // the feed fired the first dialog
        assert_eq!(binder.pending(), Some(0x2a), "the guid outlives the fire");

        binder.ask(0x2a);
        assert!(binder.ask, "the same innkeeper asking again owes a dialog");
    }

    /// The `arg1` chain, against the REAL AreaTable (`0x5dfe5e`): sub-area, else parent zone, else
    /// the `HOME_INN` GlobalString — and never nothing, because the reference never withholds the
    /// question for want of a name. 186 is Dolanaar, the leaf the bug's own innkeeper stands in.
    /// Skips without client data.
    #[test]
    fn the_dialogs_name_falls_back_sub_area_then_zone_then_home_inn() {
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let areas = AreaTableRes(
            benilla_formats::load_area_table_catalog(&mut chain).expect("AreaTable.dbc"),
        );

        assert_eq!(area_name_of(Some(186), Some(&areas)), "Dolanaar");
        // No row, no catalog, no id at all — all three reach the GlobalString rather than "".
        assert_eq!(area_name_of(Some(0xffff), Some(&areas)), HOME_INN);
        assert_eq!(area_name_of(Some(186), None), HOME_INN);
        assert_eq!(area_name_of(None, Some(&areas)), HOME_INN);
    }

    /// The line the bind lands with — the feedback half of B249, since "accepting appears to change
    /// nothing" was partly that nothing ever said it had.
    #[test]
    fn the_bind_prints_the_area_is_now_your_home() {
        assert_eq!(bound_line("Dolanaar"), "Dolanaar is now your home.");
    }

    /// Closing (the range guard, or the bind landing) retracts both the guid and any unfired
    /// question — so `CheckBinderDist()` goes false and a later `ConfirmBinder()` sends nothing.
    #[test]
    fn closing_retracts_the_guid_and_the_unfired_dialog() {
        let mut binder = BinderState::default();
        binder.ask(0x2a);
        binder.close();
        assert_eq!(binder.pending(), None);
        assert!(!binder.ask);
    }
}
