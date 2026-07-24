//! The party VM feed/drain (decision 0434 phase 2) — the systems half of [`super`]: push the
//! roster + `party1..party4` snapshots into the VM, fire the party events on their edges, and
//! drain the Lua-side [`PartyRequest`] intents into their `CMSG_*` sends. The state + the line
//! composer live in the parent module; this file owns only the per-frame bridge.

use benilla_protocol::messages::{
    member_status, GroupLootInfo, GroupMemberEntry, PartyMemberStatsInfo,
};
use benilla_ui::script::{
    PartyMemberInfo, PartyRequest, PartyState, ScriptValue, UiScript, UnitState,
};
use bevy::prelude::*;

use crate::names::NameCache;
use crate::net::{ClientCommand, Guid, GuidIndex, NetCommands, ObjectStore, SelfPlayer};
use crate::target::Selection;

use super::GroupState;

// ─── The VM feed/drain (decision 0434 phase 2) ───────────────────────────────────────────────────

/// The last-pushed fingerprints the feed fires Era event edges on.
#[derive(Default)]
pub(super) struct FedParty {
    roster: Vec<u64>,
    leader: u64,
    loot: Option<GroupLootInfo>,
    invite: Option<String>,
    units: [Option<UnitState>; 4],
}

const PARTY_TOKENS: [&str; 4] = ["party1", "party2", "party3", "party4"];

/// Push the roster snapshot + the `party1..party4` unit snapshots into the VM and fire the party
/// events on their edges. The per-member unit state is the 0434 §2 **merged view**: a streamed
/// member's live descriptor wins; the `PARTY_MEMBER_STATS` snapshot covers the rest — and the
/// roster status byte overlays both (the descriptor never carries connected/AFK/DND).
pub(super) fn feed_party(
    script: Option<NonSendMut<UiScript>>,
    group: Res<GroupState>,
    index: Res<GuidIndex>,
    stores: Query<&ObjectStore>,
    self_q: Query<&Guid, With<SelfPlayer>>,
    mut fed: Local<FedParty>,
) {
    let Some(mut script) = script else {
        return;
    };
    let self_guid = self_q.iter().next().map(|g| g.0);

    // The party1..4 slot view: own-subgroup members, packet order (`GroupState::party_slots`,
    // the 0440 byte law) — in a plain party this is simply the roster.
    let slots: Vec<&GroupMemberEntry> = group.party_slots().collect();

    // The roster-level snapshot (leader/master indices on the Lua scale: 0 = the player). A
    // raid's other subgroups feed only the count (the engine contract's v1 gap; a leader in
    // another subgroup reads as index 0 until the raid UI slice).
    let members: Vec<PartyMemberInfo> = slots
        .iter()
        .map(|m| PartyMemberInfo {
            name: m.name.clone(),
            guid: m.guid,
        })
        .collect();
    let leader_index = if Some(group.leader) == self_guid {
        0
    } else {
        slots
            .iter()
            .position(|m| m.guid == group.leader)
            .map_or(0, |i| i as u32 + 1)
    };
    let (loot_method, master_looter, loot_threshold) = match &group.loot {
        Some(loot) => {
            let method = match loot.method {
                0 => "freeforall",
                1 => "roundrobin",
                2 => "master",
                4 => "needbeforegreed",
                _ => "group",
            };
            let master = (loot.method == 2 && loot.master != 0).then(|| {
                if Some(loot.master) == self_guid {
                    0
                } else {
                    slots
                        .iter()
                        .position(|m| m.guid == loot.master)
                        .map_or(0, |i| i as u32 + 1)
                }
            });
            (method.to_string(), master, u32::from(loot.threshold))
        }
        None => ("group".to_string(), None, 0),
    };
    script.set_party(PartyState {
        members,
        leader_index,
        raid_members: if group.group_type == 1 {
            group.members.len() as u32 + 1
        } else {
            0
        },
        loot_method,
        master_looter,
        loot_threshold,
    });

    // party1..party4 unit snapshots + their per-field UNIT_* transitions.
    for (i, token) in PARTY_TOKENS.iter().enumerate() {
        let snap = slots
            .get(i)
            .map(|m| member_unit_state(m, group.stats.get(&m.guid), &group, &index, &stores));
        script.set_unit(token, snap.clone());
        if let Some(cur) = &snap {
            crate::ui_unit::fire_transitions(&mut script, token, fed.units[i].as_ref(), cur);
        }
        fed.units[i] = snap;
    }

    // The party events, on edges.
    let roster: Vec<u64> = group.members.iter().map(|m| m.guid).collect();
    if roster != fed.roster {
        script.fire_event("PARTY_MEMBERS_CHANGED", vec![]);
        fed.roster = roster;
    }
    if group.leader != fed.leader {
        script.fire_event("PARTY_LEADER_CHANGED", vec![]);
        fed.leader = group.leader;
    }
    if group.loot != fed.loot {
        script.fire_event("PARTY_LOOT_METHOD_CHANGED", vec![]);
        fed.loot = group.loot;
    }
    if group.pending_invite != fed.invite {
        match &group.pending_invite {
            Some(inviter) => script.fire_event(
                "PARTY_INVITE_REQUEST",
                vec![ScriptValue::Str(inviter.clone())],
            ),
            // Fires on Accept/Decline edges too — hiding an already-hidden popup is a no-op,
            // and the accepted-guard keeps the hide path from double-declining.
            None => script.fire_event("PARTY_INVITE_CANCEL", vec![]),
        }
        fed.invite = group.pending_invite.clone();
    }
}

/// One member's merged-view unit snapshot (see [`feed_party`]).
fn member_unit_state(
    m: &GroupMemberEntry,
    stats: Option<&PartyMemberStatsInfo>,
    group: &GroupState,
    index: &GuidIndex,
    stores: &Query<&ObjectStore>,
) -> UnitState {
    let mut s = match index.0.get(&m.guid).and_then(|e| stores.get(*e).ok()) {
        // In visibility range: the live descriptor is the truth (the server keeps it current).
        Some(store) => crate::ui_unit::snapshot(store, Some(m.name.clone()), 0),
        // Out of range: the PARTY_MEMBER_STATS snapshot (vmangos only sends these to members
        // who can't see the subject — the two sources are complementary by construction).
        None => UnitState {
            exists: true,
            name: Some(m.name.clone()),
            health: stats.and_then(|s| s.cur_hp).map_or(0, u32::from),
            max_health: stats.and_then(|s| s.max_hp).map_or(0, u32::from),
            level: stats.and_then(|s| s.level).map_or(0, u32::from),
            power_type: stats.and_then(|s| s.power_type).unwrap_or(0),
            power: stats.and_then(|s| s.cur_power).map_or(0, u32::from),
            max_power: stats.and_then(|s| s.max_power).map_or(0, u32::from),
            ..Default::default()
        },
    };
    s.is_player = true;
    // Identity + the raid-target mark (decision 0434 §5/§6), and a party member is always a
    // same-faction FRIENDLY player in 1.12 — the popup's UnitCanCooperate gate (whisper/trade
    // rows) reads the reaction, which neither merged-view leg resolves for party tokens.
    s.guid = m.guid;
    s.raid_target = group.raid_target_index(m.guid);
    s.reaction = 5;
    // The roster status byte overlays BOTH paths (GetGroupMemberStatus's Lua-predicate bits).
    s.is_connected = m.status & member_status::ONLINE != 0;
    s.is_afk = m.status & member_status::AFK != 0;
    s.is_dnd = m.status & member_status::DND != 0;
    s.is_pvp_ffa = m.status & member_status::PVP_FFA != 0;
    s.pvp = s.pvp || m.status & member_status::PVP != 0;
    s.ghost = s.ghost || m.status & member_status::GHOST != 0;
    s.dead = s.dead || m.status & member_status::DEAD != 0;
    s
}

/// `"party1".."party4"` → the roster entry it names.
fn member_for_token<'a>(group: &'a GroupState, token: &str) -> Option<&'a GroupMemberEntry> {
    let n: usize = token.strip_prefix("party")?.parse().ok()?;
    // The same slot view the feed publishes the tokens from — the mapping must agree.
    (1..=4)
        .contains(&n)
        .then(|| group.party_slots().nth(n - 1))?
}

/// Drain the Lua-side party intents into their `CMSG_*` sends (the popup's Accept/Decline, the
/// future UnitPopup's invite/uninvite/promote/loot calls).
pub(super) fn drain_party(
    script: Option<NonSendMut<UiScript>>,
    mut group: ResMut<GroupState>,
    selection: Res<Selection>,
    mut names: ResMut<NameCache>,
    self_q: Query<&Guid, With<SelfPlayer>>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    let self_guid = self_q.iter().next().map(|g| g.0);
    for req in script.take_party_requests() {
        // Sandbox mode: the roster is synthetic and the server holds no group, so the CMSG
        // sends below would vanish into a void — the group-mutating intents apply to the local
        // mirror instead (what the echo would have done). Invites and the popup's
        // accept/decline stay real: a genuine group forming switches the sandbox off.
        if group.test && test_apply_local(&mut group, &req, self_guid, selection.guid) {
            continue;
        }
        match req {
            PartyRequest::Accept => {
                let _ = commands.0.send(ClientCommand::GroupAccept);
                group.pending_invite = None;
            }
            PartyRequest::Decline => {
                let _ = commands.0.send(ClientCommand::GroupDecline);
                group.pending_invite = None;
            }
            PartyRequest::Leave => {
                let _ = commands.0.send(ClientCommand::GroupLeave);
            }
            PartyRequest::InviteName(name) => {
                let _ = commands.0.send(ClientCommand::GroupInvite { name });
            }
            PartyRequest::InviteUnit(token) => {
                // "target" resolves through the selection iff it's a player (the ref's
                // InviteToParty(unit) path); a roster token is already named.
                let name = if token == "target" {
                    selection
                        .guid
                        .filter(|g| benilla_protocol::guid::is_player(*g))
                        .and_then(|g| names.resolve(g, &commands).map(str::to_string))
                } else {
                    member_for_token(&group, &token).map(|m| m.name.clone())
                };
                if let Some(name) = name {
                    let _ = commands.0.send(ClientCommand::GroupInvite { name });
                }
            }
            PartyRequest::UninviteUnit(token) => {
                if let Some(m) = member_for_token(&group, &token) {
                    let _ = commands.0.send(ClientCommand::GroupUninvite {
                        name: m.name.clone(),
                    });
                }
            }
            PartyRequest::PromoteUnit(token) => {
                if let Some(m) = member_for_token(&group, &token) {
                    let _ = commands
                        .0
                        .send(ClientCommand::GroupSetLeader { guid: m.guid });
                }
            }
            PartyRequest::LootMethod {
                method,
                master_name,
            } => {
                let Some(method_id) = loot_method_id(&method) else {
                    continue;
                };
                let master = if method_id == 2 {
                    master_name
                        .as_deref()
                        .and_then(|n| {
                            group
                                .members
                                .iter()
                                .find(|m| m.name.eq_ignore_ascii_case(n))
                                .map(|m| m.guid)
                        })
                        .or(self_guid)
                        .unwrap_or(0)
                } else {
                    0
                };
                let threshold = group
                    .loot
                    .map(|l| u32::from(l.threshold))
                    .filter(|t| *t >= 2)
                    .unwrap_or(2);
                let _ = commands.0.send(ClientCommand::LootMethod {
                    method: method_id,
                    master,
                    threshold,
                });
            }
            PartyRequest::SetRaidTarget { unit, index } => {
                // Token → guid: self, the selection, or a roster slot (the popup passes the
                // token it was opened for). An unresolvable token is a no-op, like TargetUnit.
                let guid = match unit.as_str() {
                    "player" => self_q.iter().next().map(|g| g.0),
                    "target" => selection.guid,
                    t => member_for_token(&group, t).map(|m| m.guid),
                };
                let Some(guid) = guid else { continue };
                // Lua marks are 1..8 over wire icons 0..7. Setting sends (mark-1, guid) — the
                // server clears the unit's old icon itself; clearing (Lua 0) re-sends the
                // unit's CURRENT icon with guid 0 (vmangos Group::SetTargetIcon's shape — there
                // is no "clear by unit" on the wire).
                if index >= 1 {
                    let _ = commands.0.send(ClientCommand::SetRaidTarget {
                        icon: index - 1,
                        guid,
                    });
                } else {
                    let current = group.raid_target_index(guid);
                    if current >= 1 {
                        let _ = commands.0.send(ClientCommand::SetRaidTarget {
                            icon: current - 1,
                            guid: 0,
                        });
                    }
                }
            }
            PartyRequest::LootThreshold(threshold) => {
                let (method, master) = group
                    .loot
                    .map_or((3, 0), |l| (u32::from(l.method), l.master));
                let _ = commands.0.send(ClientCommand::LootMethod {
                    method,
                    master,
                    threshold,
                });
            }
        }
    }
}

/// The sandbox half of the drain ([`drain_party`]'s `group.test` branch): apply one
/// group-mutating intent to the local mirror, mimicking the server echo a real group would have
/// sent — the same board/roster/loot writes the apply arms make from the wire. Returns `false`
/// for the intents that stay real even while testing (invites, accept/decline).
fn test_apply_local(
    group: &mut GroupState,
    req: &PartyRequest,
    self_guid: Option<u64>,
    target_guid: Option<u64>,
) -> bool {
    match req {
        PartyRequest::Leave => {
            // A local disband: the real path's all-zero list resets everything, sandbox
            // flag included — `/partytest` starts a fresh one.
            *group = GroupState::default();
            true
        }
        PartyRequest::UninviteUnit(token) => {
            if let Some(guid) = member_for_token(group, token).map(|m| m.guid) {
                group.members.retain(|m| m.guid != guid);
                group.stats.remove(&guid);
            }
            true
        }
        PartyRequest::PromoteUnit(token) => {
            if let Some(guid) = member_for_token(group, token).map(|m| m.guid) {
                group.leader = guid;
            }
            true
        }
        PartyRequest::LootMethod {
            method,
            master_name,
        } => {
            if let Some(method_id) = loot_method_id(method) {
                let master = if method_id == 2 {
                    master_name
                        .as_deref()
                        .and_then(|n| {
                            group
                                .members
                                .iter()
                                .find(|m| m.name.eq_ignore_ascii_case(n))
                                .map(|m| m.guid)
                        })
                        .or(self_guid)
                        .unwrap_or(0)
                } else {
                    0
                };
                let threshold = group.loot.map_or(2, |l| l.threshold.max(2));
                group.loot = Some(GroupLootInfo {
                    method: method_id as u8,
                    master,
                    threshold,
                });
            }
            true
        }
        PartyRequest::LootThreshold(threshold) => {
            let (method, master) = group.loot.map_or((3, 0), |l| (l.method, l.master));
            group.loot = Some(GroupLootInfo {
                method,
                master,
                threshold: *threshold as u8,
            });
            true
        }
        PartyRequest::SetRaidTarget { unit, index } => {
            let guid = match unit.as_str() {
                "player" => self_guid,
                "target" => target_guid,
                t => member_for_token(group, t).map(|m| m.guid),
            };
            let Some(guid) = guid else { return true };
            if *index >= 1 {
                // The server's SetTargetIcon clears the unit's old icon before setting the
                // new one — one mark per unit.
                for slot in group.raid_targets.iter_mut() {
                    if *slot == guid {
                        *slot = 0;
                    }
                }
                group.apply_raid_target(index - 1, guid);
            } else {
                let current = group.raid_target_index(guid);
                if current >= 1 {
                    group.apply_raid_target(current - 1, 0);
                }
            }
            true
        }
        _ => false,
    }
}

/// `SetLootMethod`'s string → the wire's `LootMethod` id.
fn loot_method_id(method: &str) -> Option<u32> {
    Some(match method {
        "freeforall" => 0,
        "roundrobin" => 1,
        "master" => 2,
        "group" => 3,
        "needbeforegreed" => 4,
        _ => return None,
    })
}

/// The `/partytest` instrument (decision 0434, the 0288 `/chattest` pattern): a synthetic
/// 4-member roster with mixed statuses + out-of-range stats, pumped through the REAL apply path
/// (so the composer's lines print too) — the whole frame surface eyeballable with no server.
/// The guids are unstreamed player-range fakes, so every member exercises the stats-snapshot leg
/// of the merged view. `player_xy` (the caller's live WoW position) plants stats positions
/// around the player so the minimap blips show too: Alice 30 yd out (dot), Bob 80 yd (dot that
/// becomes a rim arrow one zoom in), Carol 300 yd (rim arrow), Dave offline (no blip).
pub(crate) fn synthetic_roster(
    group: &mut GroupState,
    player_xy: Option<(f32, f32)>,
) -> Vec<String> {
    let members = vec![
        GroupMemberEntry {
            name: "Alice".into(),
            guid: 0xF001,
            status: member_status::ONLINE,
            flags: 0,
        },
        GroupMemberEntry {
            name: "Bob".into(),
            guid: 0xF002,
            status: member_status::ONLINE | member_status::AFK,
            flags: 0,
        },
        GroupMemberEntry {
            name: "Carol".into(),
            guid: 0xF003,
            status: member_status::ONLINE | member_status::DEAD,
            flags: 0,
        },
        GroupMemberEntry {
            name: "Dave".into(),
            guid: 0xF004,
            status: member_status::OFFLINE,
            flags: 0,
        },
    ];
    let lines = group.apply_list(
        0,
        0,
        members,
        0xF001,
        Some(GroupLootInfo {
            method: 2,
            master: 0xF003,
            threshold: 3,
        }),
    );
    // The blip seats: WoW-axis offsets from the player (+x north, +y west), i16-truncated
    // exactly like the wire. Dave (offline) gets none.
    let seat = |dx: f32, dy: f32| player_xy.map(|(px, py)| ((px + dx) as i16, (py + dy) as i16));
    for (guid, hp, max, level, power_type, pos) in [
        (0xF001u64, 820u16, 1240u16, 32u16, 0u8, seat(30.0, 0.0)),
        (0xF002, 455, 980, 30, 3, seat(0.0, 80.0)),
        (0xF003, 0, 1105, 31, 0, seat(-300.0, 0.0)),
        (0xF004, 0, 0, 0, 0, None),
    ] {
        group.apply_stats(
            guid,
            true,
            PartyMemberStatsInfo {
                status: None,
                cur_hp: Some(hp),
                max_hp: Some(max),
                level: Some(level),
                power_type: Some(power_type),
                cur_power: Some(300),
                max_power: Some(410),
                position: pos,
                ..Default::default()
            },
        );
    }
    // Sandbox on (apply_list just cleared it — the wire-wins default): the drain now applies
    // group-mutating menu intents locally, so the popup is exercisable serverless too.
    group.test = true;
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sandbox drain half: each group-mutating intent lands on the local mirror exactly as
    /// the server echo would have — and a real wire list always switches the sandbox back off.
    #[test]
    fn partytest_sandbox_applies_menu_intents_locally() {
        let mut group = GroupState::default();
        synthetic_roster(&mut group, None);
        assert!(group.test, "the synthetic roster arms the sandbox");
        let (me, mob) = (Some(0x5E1Fu64), Some(0xB0B0u64));

        // Mark the target with Skull, then move it to Cross — one mark per unit.
        let mark = |i| PartyRequest::SetRaidTarget {
            unit: "target".into(),
            index: i,
        };
        assert!(test_apply_local(&mut group, &mark(8), me, mob));
        assert_eq!(group.raid_target_index(0xB0B0), 8);
        test_apply_local(&mut group, &mark(7), me, mob);
        assert_eq!(group.raid_target_index(0xB0B0), 7, "the old icon clears");
        assert_eq!(group.raid_targets[7], 0);
        test_apply_local(&mut group, &mark(0), me, mob);
        assert_eq!(group.raid_target_index(0xB0B0), 0, "NONE clears");

        // Promote party1 (Alice), kick party2 (Bob), retune the loot.
        test_apply_local(
            &mut group,
            &PartyRequest::PromoteUnit("party1".into()),
            me,
            None,
        );
        assert_eq!(group.leader, 0xF001);
        test_apply_local(
            &mut group,
            &PartyRequest::UninviteUnit("party2".into()),
            me,
            None,
        );
        assert!(!group.members.iter().any(|m| m.guid == 0xF002));
        assert!(!group.stats.contains_key(&0xF002));
        test_apply_local(
            &mut group,
            &PartyRequest::LootMethod {
                method: "needbeforegreed".into(),
                master_name: None,
            },
            me,
            None,
        );
        test_apply_local(&mut group, &PartyRequest::LootThreshold(4), me, None);
        assert_eq!(
            group.loot,
            Some(GroupLootInfo {
                method: 4,
                master: 0,
                threshold: 4
            })
        );

        // Invites stay real; Leave disbands the sandbox; a real list turns the flag off.
        assert!(!test_apply_local(
            &mut group,
            &PartyRequest::InviteName("Zed".into()),
            me,
            None
        ));
        test_apply_local(&mut group, &PartyRequest::Leave, me, None);
        assert!(!group.in_group && !group.test);
        synthetic_roster(&mut group, None);
        group.apply_list(0, 0, vec![], 0x123, None);
        assert!(!group.test, "the real wire always wins");
    }
}
