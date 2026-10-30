//! The bridge's world verbs: select a unit, interact with an object, walk to a point, stop.
//!
//! Each goes through the client's own path, so a page cannot do anything a player cannot:
//! - `target` is `SetSelection` ([`crate::target::SelectCommit::select_unit`], the path a loot
//!   window's select and `TargetUnit` share): a unit the object manager holds, by guid — which
//!   1.12's Lua cannot name, since its tokens stop at `target`/`mouseover`/party slots.
//! - `interact` is the right-click's object dispatch ([`crate::target::Interact`]): talk to an
//!   NPC (gossip, vendor, trainer, quest giver, flight master, innkeeper, banker), loot or skin a
//!   body, use a GameObject (a chest, a herb, a vein, a mailbox, a quest object), attack an enemy.
//!   Its reach is the client's own; out of reach it is the client's own refusal (or, with Click to
//!   Move's AutoInteract on, the client's own walk there).
//! - `moveto` is Click to Move's walk to a point on the ground ([`AutoMove::walk_to_ground`]): a
//!   straight line at run speed, the client's own steering and stuck stop, cancelled by any
//!   movement input, like a right-click on the ground. There is no pathfinding: a wall between
//!   here and there stops the walk, which a page sees as `self.autoMove` going false short of the
//!   point. It runs whatever the AutoInteract option says (the option gates the player's
//!   right-click, not this); the player's setting is left as it was.
//! - `stop` ends a walk in flight.
//! - `path` finds a walkable way from the player to a point through the loaded collision world
//!   ([`super::path`]) and answers its waypoints, which a page walks with `moveto`.
//!
//! Every verb answers with an `act` event — `{ id, op, ok, reason? }` — so a page can await it.

use bevy::prelude::*;

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_ui::script::plain::PlainValue;
use benilla_world::collision::WorldCollision;

use crate::net::GuidIndex;
use crate::player::{AutoMove, Player, StandStateRequest};
use crate::target::{Interact, SelectCommit};

use super::{BridgeInput, BridgeOutbox};

/// One queued world verb, with the page's id for its `act` answer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ActOp {
    Target(u64),
    /// `auto_loot`: open a body or a container with 1.12's auto-loot (the Shift-click), so the
    /// client takes everything — the only way a script loots, since a scripted `LootButton`
    /// click is refused as the reference refuses it.
    Interact {
        guid: u64,
        auto_loot: bool,
    },
    /// A WoW-coordinate point (+X north, +Y west, +Z up).
    MoveTo([f32; 3]),
    Stop,
    /// A path from the player to a WoW-coordinate point, searching at most `budget` cells.
    Path {
        to: [f32; 3],
        budget: usize,
    },
}

impl ActOp {
    fn name(&self) -> &'static str {
        match self {
            ActOp::Target(_) => "target",
            ActOp::Interact { .. } => "interact",
            ActOp::MoveTo(_) => "moveto",
            ActOp::Stop => "stop",
            ActOp::Path { .. } => "path",
        }
    }
}

/// The selection and interaction verbs. Two systems, not one: `SelectCommit` and `AutoMove`
/// borrow some of the same resources mutably, which Bevy refuses in a single system (B0002).
/// Each takes its own verbs off the queue, in the order the page queued them.
pub(super) fn run_object_ops(
    mut input: ResMut<BridgeInput>,
    mut out: ResMut<BridgeOutbox>,
    index: Res<GuidIndex>,
    mut select: SelectCommit,
    mut interact: MessageWriter<Interact>,
    mut loot_cfg: ResMut<crate::ui_loot::LootConfig>,
) {
    // An auto-loot interact's override ends once the click has been dispatched (it reads the
    // byte as it runs, a frame or two after the message), restoring the player's own setting.
    if let Some((was, frames)) = input.auto_loot_restore.as_mut() {
        if *frames == 0 {
            loot_cfg.auto_loot = *was;
            input.auto_loot_restore = None;
        } else {
            *frames -= 1;
        }
    }
    for (id, op) in take(&mut input.act, |op| {
        matches!(op, ActOp::Target(_) | ActOp::Interact { .. })
    }) {
        let result = match op {
            // `select_unit` commits only a unit the object manager holds; a held non-unit (a
            // chest, a corpse) is a silent no-op there, which the next frame's `target` shows.
            ActOp::Target(guid) => {
                if index.0.contains_key(&guid) {
                    select.select_unit(guid);
                    Ok(())
                } else {
                    Err("no such object")
                }
            }
            ActOp::Interact { guid, auto_loot } => match index.0.get(&guid) {
                Some(&entity) => {
                    if auto_loot {
                        let was = input
                            .auto_loot_restore
                            .map_or(loot_cfg.auto_loot, |(w, _)| w);
                        loot_cfg.auto_loot = true;
                        input.auto_loot_restore = Some((was, 3));
                    }
                    interact.write(Interact(entity));
                    Ok(())
                }
                None => Err("no such object"),
            },
            _ => unreachable!(),
        };
        out.push("act", answer(id, op.name(), result));
    }
}

/// The movement verbs: Click to Move's ground walk, and its stop.
pub(super) fn run_move_ops(
    mut input: ResMut<BridgeInput>,
    mut out: ResMut<BridgeOutbox>,
    mut walk: AutoMove,
    mut stand: MessageWriter<StandStateRequest>,
) {
    for (id, op) in take(&mut input.act, |op| {
        matches!(op, ActOp::MoveTo(_) | ActOp::Stop)
    }) {
        let result = match op {
            ActOp::MoveTo(pos) => {
                // The option gates the player's right-click; the page's walk runs regardless, and
                // the arm is all that reads it (the steer does not), so the setting is restored.
                let was = walk.approach.enabled;
                walk.approach.enabled = true;
                let armed = walk.walk_to_ground(wow_to_bevy(pos), &mut stand);
                walk.approach.enabled = was;
                if armed {
                    Ok(())
                } else {
                    Err("cannot move now (dead, stunned, on a taxi, or controlled)")
                }
            }
            ActOp::Stop => {
                walk.approach.stop();
                Ok(())
            }
            _ => unreachable!(),
        };
        out.push("act", answer(id, op.name(), result));
    }
}

/// The path verb: an A* through the loaded collision ([`super::path`]), answered with
/// `points` (WoW `[x, y, z]`, the player's position first), `partial` (the goal was not reached;
/// the points end at the nearest place found) and `expanded` (cells searched).
pub(super) fn run_path_ops(
    mut input: ResMut<BridgeInput>,
    mut out: ResMut<BridgeOutbox>,
    world: WorldCollision,
    player: Res<Player>,
) {
    for (id, op) in take(&mut input.act, |op| matches!(op, ActOp::Path { .. })) {
        let ActOp::Path { to, budget } = op else {
            unreachable!()
        };
        let probe = super::path::WorldProbe::new(&world);
        let found = super::path::find(&probe, player.pos, wow_to_bevy(to), budget);
        let payload = match found {
            Some(path) => {
                let mut m = answer_fields(id, "path", Ok(()));
                m.push((
                    "points".into(),
                    PlainValue::List(
                        path.points
                            .iter()
                            .map(|p| {
                                PlainValue::List(
                                    bevy_to_wow(*p)
                                        .iter()
                                        .map(|v| PlainValue::Num(f64::from(*v)))
                                        .collect(),
                                )
                            })
                            .collect(),
                    ),
                ));
                m.push(("partial".into(), PlainValue::Bool(path.partial)));
                m.push(("expanded".into(), PlainValue::Num(path.expanded as f64)));
                PlainValue::Map(m)
            }
            None => answer(id, "path", Err("no floor under the player")),
        };
        out.push("act", payload);
    }
}

/// The queued verbs `pick` selects, removed from the queue in order; the rest stay.
fn take(queue: &mut Vec<(u32, ActOp)>, pick: impl Fn(&ActOp) -> bool) -> Vec<(u32, ActOp)> {
    let (picked, rest) = std::mem::take(queue)
        .into_iter()
        .partition(|(_, op)| pick(op));
    *queue = rest;
    picked
}

/// Drop queued verbs when the world is not up to run them, with a refusal each.
pub(super) fn refuse_world_ops(mut input: ResMut<BridgeInput>, mut out: ResMut<BridgeOutbox>) {
    for (id, op) in std::mem::take(&mut input.act) {
        out.push("act", answer(id, op.name(), Err("not in the world")));
    }
}

fn answer(id: u32, op: &str, result: Result<(), &str>) -> PlainValue {
    PlainValue::Map(answer_fields(id, op, result))
}

fn answer_fields(id: u32, op: &str, result: Result<(), &str>) -> Vec<(String, PlainValue)> {
    let mut m = vec![
        ("id".into(), PlainValue::Num(f64::from(id))),
        ("op".into(), PlainValue::Str(op.into())),
        ("ok".into(), PlainValue::Bool(result.is_ok())),
    ];
    if let Err(reason) = result {
        m.push(("reason".into(), PlainValue::Str(reason.into())));
    }
    m
}

/// A guid as the page sends it: the snapshot's own hex string (`"0xf130…"`), or decimal.
pub(crate) fn parse_guid(s: &str) -> Option<u64> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
    .filter(|&g| g != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_parse_from_the_snapshots_hex_and_from_decimal() {
        assert_eq!(
            parse_guid("0xf130000000000001"),
            Some(0xf130_0000_0000_0001)
        );
        assert_eq!(parse_guid(" 0X1a "), Some(0x1a));
        assert_eq!(parse_guid("42"), Some(42));
        assert_eq!(parse_guid("0x0"), None);
        assert_eq!(parse_guid("bob"), None);
    }
}
