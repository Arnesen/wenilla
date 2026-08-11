//! The party VM feed/drain (decision 0434 phase 2) — the systems half of [`super`]: push the
//! roster + `party1..party4` snapshots into the VM, fire the party events on their edges, and
//! drain the Lua-side [`PartyRequest`] intents into their `CMSG_*` sends. The state + the line
//! composer live in the parent module; this file owns only the per-frame bridge.

use benilla_protocol::messages::{
    member_status, GroupLootInfo, GroupMemberEntry, PartyMemberStatsInfo, GROUP_MEMBER_ASSISTANT,
};
use benilla_ui::script::{
    PartyMemberInfo, PartyRequest, PartyState, RaidMemberInfo, ScriptValue, UiScript, UnitState,
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

/// `GROUPTYPE_RAID` — `SMSG_GROUP_LIST`'s first byte (`0` party, `1` raid; vmangos `Group.h:116`).
const GROUPTYPE_RAID: u8 = 1;

/// The subgroup index in a member's flags byte — bits 0-2 (`GroupMemberEntry::flags`'s own doc;
/// the assistant bit `0x80` is its neighbour, and the `0x7f` mask `party_slots` uses is a
/// different question, "same subgroup AND same assistant state").
const GROUP_MEMBER_SUBGROUP: u8 = 0x07;

/// Push the roster snapshot + the `party1..party4` unit snapshots into the VM and fire the party
/// events on their edges. The per-member unit state is the 0434 §2 **merged view**: a streamed
/// member's live descriptor wins; the `PARTY_MEMBER_STATS` snapshot covers the rest — and the
/// roster status byte overlays both (the descriptor never carries connected/AFK/DND).
#[allow(clippy::too_many_arguments)] // a Bevy system's param list IS its dependency set
pub(super) fn feed_party(
    script: Option<NonSendMut<UiScript>>,
    group: Res<GroupState>,
    index: Res<GuidIndex>,
    stores: Query<&ObjectStore>,
    self_q: Query<(&Guid, &ObjectStore), With<SelfPlayer>>,
    factions: Option<Res<crate::target::Factions>>,
    names: Res<NameCache>,
    areas: Option<Res<crate::area::AreaTableRes>>,
    // The leaf area under us, through the SAME accessor `crate::area`'s zone-text resolver uses.
    // Deliberately not `terrain_stream::CurrentArea` directly: that item is named today only by
    // the instruments, and naming it from a game module would push it across the world-API wall
    // (`tests/world_api_wall.rs`, decision 1164) for a value this already answers.
    here: benilla_world::world_point::WorldPoint,
    mut fed: Local<FedParty>,
) {
    let Some(mut script) = script else {
        return;
    };
    let self_pair = self_q.iter().next();
    let self_guid = self_pair.map(|(g, _)| g.0);
    // The party's PvP faction group (decision 0646 §1): our own. A 1.12 party is always one
    // faction, and a member out of streaming range has no descriptor to resolve one from — so
    // reading it off ourselves is exact for every member, present or not.
    let own_group =
        self_pair.and_then(|(_, store)| crate::ui_unit::faction_group(store, factions.as_deref()));

    // The party1..4 slot view: own-subgroup members, packet order (`GroupState::party_slots`,
    // the 0440 byte law) — in a plain party this is simply the roster.
    let slots: Vec<&GroupMemberEntry> = group.party_slots().collect();

    // The roster-level snapshot (leader/master indices on the Lua scale: 0 = the player). These
    // indices are the PARTY view — our own subgroup's four slots — so in a raid a leader in
    // another subgroup still reads as index 0 here; the whole-raid answer is `raid` below, which
    // `GetRaidRosterInfo` reads and which does carry every subgroup.
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
    // The raid roster — `GetNumRaidMembers`/`GetRaidRosterInfo`/`UnitInRaid` all read this one
    // list, so the count can never disagree with the array the way it would if we kept both.
    let me = self_pair.map(|(g, store)| RaidSelf {
        guid: g.0,
        flags: group.own_flags,
        level: store.0.unit_level().unwrap_or(0),
        area: here.area(),
        dead: store.0.unit_is_dead(),
    });
    let zone_name = |area: u32| {
        let areas = areas.as_ref()?;
        // The wire's per-member `zone` is already a top-level zone id; our own `CurrentArea` is
        // the finest MCNK leaf, so it needs the parent walk first. `top_zone` is idempotent on a
        // row that is already a zone, which is what lets one resolver serve both.
        areas
            .0
            .name(areas.0.top_zone(area).unwrap_or(area))
            .map(str::to_string)
    };
    let raid = raid_roster(&group, me.as_ref(), &names, &zone_name);

    script.set_party(PartyState {
        members,
        leader_index,
        raid,
        loot_method,
        master_looter,
        loot_threshold,
    });

    // party1..party4 unit snapshots + their per-field UNIT_* transitions.
    for (i, token) in PARTY_TOKENS.iter().enumerate() {
        let snap = slots.get(i).map(|m| {
            member_unit_state(
                m,
                group.stats.get(&m.guid),
                &group,
                &index,
                &stores,
                own_group.clone(),
            )
        });
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

/// The player's own raid row — the one entry `GroupState` cannot supply, because
/// `SMSG_GROUP_LIST` never lists the recipient.
pub(super) struct RaidSelf {
    pub(super) guid: u64,
    /// Our own subgroup bits + the assistant flag, `SMSG_GROUP_LIST`'s second byte.
    pub(super) flags: u8,
    pub(super) level: u32,
    /// The finest area under us (`WorldPoint::area()`), walked to a zone by the caller's resolver
    /// — the reference reads a zone-id global here (`0xb4e314`) on the live-object arm.
    pub(super) area: Option<u32>,
    /// `UNIT_FIELD_HEALTH <= 0` — the reference's live-object arm for return 9, and *only* that:
    /// deliberately `unit_is_dead` rather than `unit_reads_dead`, so a feigning hunter does not
    /// take the arm a health test would not.
    pub(super) dead: bool,
}

/// Build `GetRaidRosterInfo`'s array (decision 0434 §6's roster, wow-re
/// `ui/scratch/raid-roster-bindings.md` §2). Empty outside a raid — `GroupState::group_type` is
/// `1` only for one — which is what makes `GetNumRaidMembers()` answer `0` in a party.
///
/// **The player is row 1.** The reference's array contains the local player (it is why
/// `UnitInRaid("player")` answers `1`), and the wire's list does not, so the recipient is spliced
/// back in here. *Where* the real client puts itself is **not derived** — the note carves the
/// binding, not the array's fill order — and nothing observed depends on it: every corpus consumer
/// sweeps `1..GetNumRaidMembers()` (or `1..MAX_RAID_MEMBERS`) and keys the result by name.
///
/// A pure function over plain data so the shape is testable without a second account in a raid:
/// the tuple this fills is the part a live raid could confirm and a unit test cannot, but the
/// *mapping* — rank, subgroup, the online/dead bit pair — is the part that can be, and is below.
fn raid_roster(
    group: &GroupState,
    me: Option<&RaidSelf>,
    names: &NameCache,
    zone_name: &dyn Fn(u32) -> Option<String>,
) -> Vec<RaidMemberInfo> {
    if group.group_type != GROUPTYPE_RAID {
        return Vec::new();
    }
    // rank: 2 leader · 1 assistant · 0 member. The binding exposes `[edi+0xc]` unadjusted and
    // wow-re could not derive the convention from its bytes; the corpus can and does —
    // `ChatLog.lua:351-353` prints `@` for 2 and `*` for 1.
    let rank_of = |guid: u64, flags: u8| {
        if guid == group.leader {
            2
        } else if flags & GROUP_MEMBER_ASSISTANT != 0 {
            1
        } else {
            0
        }
    };
    let row = |guid: u64,
               name: String,
               flags: u8,
               level: u32,
               zone: Option<String>,
               online: bool,
               ninth: bool| {
        let class = names
            .player_traits(guid)
            .and_then(|(_, class, _)| crate::ui_unit::class_names(class));
        RaidMemberInfo {
            name,
            guid,
            rank: rank_of(guid, flags),
            // Stored 0-based; the binding adds the `0x4bb61a inc`.
            subgroup: u32::from(flags & GROUP_MEMBER_SUBGROUP),
            level,
            class: class.map(|(n, _)| n.to_string()),
            class_file: class.map(|(_, f)| f.to_string()),
            zone,
            online,
            ninth,
        }
    };
    let mut roster = Vec::with_capacity(group.members.len() + 1);
    if let Some(me) = me {
        roster.push(row(
            me.guid,
            // An unresolved name takes the reference's name-cache-miss arm at the binding: the
            // whole 9-tuple becomes the fixed miss tuple, not a half-filled row.
            names.peek(me.guid).unwrap_or_default().to_string(),
            me.flags,
            me.level,
            me.area.and_then(zone_name),
            true,
            me.dead,
        ));
    }
    for m in &group.members {
        let stats = group.stats.get(&m.guid);
        let online = m.status & member_status::ONLINE != 0;
        roster.push(row(
            m.guid,
            m.name.clone(),
            m.flags,
            stats.and_then(|s| s.level).map_or(0, u32::from),
            stats
                .and_then(|s| s.zone)
                .and_then(|z| zone_name(u32::from(z))),
            online,
            // The reference's cached arm: `[edi+0x18]` must carry BOTH `0x4` and `0x1`. Our wire
            // spells those `member_status::DEAD` and `ONLINE` (vmangos `Group.cpp:45-63`), which
            // is the corroboration recorded on `RaidMemberInfo::ninth` — an offline dead member
            // answers nil there, and reproducing the conjunction is the point.
            online && m.status & member_status::DEAD != 0,
        ));
    }
    roster
}

/// One member's merged-view unit snapshot (see [`feed_party`]).
fn member_unit_state(
    m: &GroupMemberEntry,
    stats: Option<&PartyMemberStatsInfo>,
    group: &GroupState,
    index: &GuidIndex,
    stores: &Query<&ObjectStore>,
    own_group: Option<String>,
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
    // The group PvP icon's faction (decision 0646 §1 — closes 0434's named party-icon deferral).
    s.faction_group = own_group;
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

    /// The raid roster mapping — the half a unit test can actually settle. The nine-value *shape*
    /// is pinned in `benilla-ui`'s own tests; what is pinned here is which wire fact lands in
    /// which slot, because that is what a live raid would otherwise be the only witness to.
    #[test]
    fn the_raid_roster_maps_the_wire_to_get_raid_roster_info() {
        let mut names = NameCache::default();
        names.insert_player(0x5E1F, "Me".into(), Some((4, 11, 0))); // Night Elf DRUID
        names.insert_player(0xA11CE, "Alice".into(), Some((1, 1, 1))); // Human WARRIOR
        names.insert_player(0xB0B, "Bob".into(), None); // no traits yet

        let member = |guid: u64, name: &str, status: u8, flags: u8| GroupMemberEntry {
            name: name.into(),
            guid,
            status,
            flags,
        };
        let mut group = GroupState {
            group_type: GROUPTYPE_RAID,
            own_flags: 0,
            leader: 0x5E1F,
            members: vec![
                // Alice: subgroup 2 (stored), an assistant, online and DEAD.
                member(
                    0xA11CE,
                    "Alice",
                    member_status::ONLINE | member_status::DEAD,
                    2 | GROUP_MEMBER_ASSISTANT,
                ),
                // Bob: subgroup 0, offline — and offline-AND-dead, which must NOT set return 9.
                member(0xB0B, "Bob", member_status::DEAD, 0),
            ],
            ..Default::default()
        };
        group.stats.insert(
            0xA11CE,
            PartyMemberStatsInfo {
                level: Some(58),
                zone: Some(1537),
                ..Default::default()
            },
        );
        let zone = |id: u32| (id == 1537).then(|| "Ironforge".to_string());
        let me = RaidSelf {
            guid: 0x5E1F,
            flags: 0,
            level: 60,
            area: Some(1537),
            dead: false,
        };

        let roster = raid_roster(&group, Some(&me), &names, &zone);
        assert_eq!(roster.len(), 3, "the player is spliced back in");

        // Row 1 — us. Leader (rank 2), subgroup stored 0, class from our own name-query traits.
        assert_eq!(roster[0].name, "Me");
        assert_eq!(roster[0].rank, 2);
        assert_eq!(
            roster[0].subgroup, 0,
            "stored 0-based; the BINDING adds one"
        );
        assert_eq!(roster[0].level, 60);
        assert_eq!(roster[0].class_file.as_deref(), Some("DRUID"));
        assert_eq!(roster[0].zone.as_deref(), Some("Ironforge"));
        assert!(roster[0].online && !roster[0].ninth);

        // Row 2 — an assistant, and the online+dead pair the reference's cached arm tests.
        assert_eq!((roster[1].rank, roster[1].subgroup), (1, 2));
        assert_eq!(roster[1].level, 58);
        assert_eq!(roster[1].class_file.as_deref(), Some("WARRIOR"));
        assert!(roster[1].online && roster[1].ninth, "online AND dead");

        // Row 3 — offline. Return 9 needs BOTH bits, so a dead-but-offline member is nil there,
        // and an unresolved class is nil rather than a guess.
        assert!(!roster[2].online, "offline");
        assert!(!roster[2].ninth, "0x4 without 0x1 is not the arm");
        assert_eq!(roster[2].class, None);
        assert_eq!(roster[2].zone, None, "no stats packet, no zone");

        // A PARTY is not a raid: the whole list is empty, so `GetNumRaidMembers()` answers 0
        // while `IsRaidLeader()` still answers 1 — the pair that surprises people.
        group.group_type = 0;
        assert!(raid_roster(&group, Some(&me), &names, &zone).is_empty());
    }
}
