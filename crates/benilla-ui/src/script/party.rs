//! The party/raid **Era API surface** (decision 0434 §2, phase 2) — the engine-free seam mirroring
//! [`super::unit`]: the app pushes a roster **snapshot** ([`UiScript::set_party`]) built from its own
//! `GroupState` wire mirror, and the `GetNumPartyMembers`/`GetPartyLeaderIndex`/`GetLootMethod`/…
//! globals here read that plain data. The invite/uninvite/promote/loot-config calls are the outbound
//! half: they queue a [`PartyRequest`] the app drains ([`UiScript::take_party_requests`]) and turns
//! into the matching `CMSG_GROUP_*`/`CMSG_LOOT_METHOD` send — no ECS/net reach from the engine
//! (decision 0068 §3), exactly [`super::unit`]'s split.
//!
//! Per-member game state (health/mana/level/reaction/…) does **not** live here — it rides the
//! existing per-unit snapshots under the `"party1"`..`"party4"` tokens (decision 0434 §3), the same
//! feed `"player"`/`"target"` use ([`super::unit::UnitState`]). This module owns only the
//! roster-level facts a unit snapshot can't carry: how many members, who leads, the loot
//! configuration. `PartyState::default()` is "not in a group" — every getter then answers the
//! solo-player shape a fresh client reports (`GetNumPartyMembers()` `0`, `GetLootMethod()`
//! `("group", nil)`, …).
//!
//! v1 gap, stated not hidden: raid membership is a bare count ([`PartyState::raid_members`]) feeding
//! `GetNumRaidMembers` only — the 40-member raid roster/grid is a later arc (decision 0434 §6).

use mlua::{Lua, Value};

use super::Model;

/// One roster member's engine-owned facts (decision 0434 §2/§3) — deliberately thin: everything else
/// (health, class, reaction, …) is the unit snapshot under its `"partyN"` token.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartyMemberInfo {
    pub name: String,
    /// The member's GUID — the identity `UnitInParty` matches arbitrary tokens (the target)
    /// against (decision 0434 §5's popup menu pick). `0` = unknown, never matches.
    pub guid: u64,
}

/// The party/raid roster snapshot, pushed whole by the app each frame it changes
/// ([`UiScript::set_party`]) — the `GroupState` merged view's roster-level facts (decision 0434 §2).
/// `PartyState::default()` = not in a group.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartyState {
    /// The other party members, `"party1"`..`"party4"` order (the recipient never appears in its own
    /// list, matching `SMSG_GROUP_LIST`); empty = not in a group. Never more than 4 — a raid's extra
    /// members are [`Self::raid_members`] only (module doc's v1 gap).
    pub members: Vec<PartyMemberInfo>,
    /// The party leader, on the Lua `GetPartyLeaderIndex` scale: `0` the player leads, `1..=4` that
    /// `members` slot (1-based) leads.
    pub leader_index: u32,
    /// The raid roster's member count (`GetNumRaidMembers`) — `0` outside a raid. The 40-member
    /// roster itself is a later arc (module doc); this only feeds the one getter that already has a
    /// stable Era shape.
    pub raid_members: u32,
    /// `GetLootMethod`'s method string: `"freeforall"` | `"roundrobin"` | `"master"` | `"group"` |
    /// `"needbeforegreed"`. `Default::default()` is `""` — the *native* reports it as `"group"` (the
    /// live shape for a solo/fresh player) when empty; the app is expected to always push a real
    /// method once grouped.
    pub loot_method: String,
    /// The master looter, as a party index (`0` the player, `1..=4` that `members` slot) — the loot
    /// method's second return, `masterlooterPartyID`. `None` = no master looter (any method but
    /// `"master"`, or a master-loot group with none assigned yet).
    pub master_looter: Option<u32>,
    /// `GetLootThreshold`'s quality floor (`2..=4`) below which non-leader loot isn't round-robin/
    /// master gated. `0` (the default) is fine while ungrouped — the getter has nothing to floor.
    pub loot_threshold: u32,
}

/// Outbound party/loot intents queued by the Era API's action calls, drained by the app
/// ([`UiScript::take_party_requests`]) into the matching `CMSG_*` send. Plain data — no mlua/ECS
/// types, [`super::unit::UnitState`]'s `TargetUnit` seam's twin.
#[derive(Clone, Debug, PartialEq)]
pub enum PartyRequest {
    /// `AcceptGroup()` — accept the pending invite.
    Accept,
    /// `DeclineGroup()` — decline the pending invite.
    Decline,
    /// `LeaveParty()` — leave the current group (no confirmation popup, decision 0434 §4).
    Leave,
    /// `InviteByName(name)` — invite by character name.
    InviteName(String),
    /// `InviteToParty(unit)` — invite by unit TOKEN (e.g. `"target"`); the app resolves it to a name.
    InviteUnit(String),
    /// `UninviteFromParty(unit)` — kick a roster member, addressed by unit token (e.g. `"party2"`).
    UninviteUnit(String),
    /// `PromoteToPartyLeader(unit)` — hand leadership to a roster member, by unit token.
    PromoteUnit(String),
    /// `SetLootMethod(method[, masterName])` — the master-looter argument is a character NAME (the
    /// reference's own shape); the app resolves it to a roster member for the send.
    LootMethod {
        method: String,
        master_name: Option<String>,
    },
    /// `SetLootThreshold(n)` — the new quality floor.
    LootThreshold(u32),
    /// `SetRaidTargetIcon(unit, index)` — mark (1..=8) or clear (0) the raid-target icon on a
    /// unit, addressed by token; the app resolves the token to a guid for the
    /// `MSG_RAID_TARGET_UPDATE` send (decision 0434 §5's submenu, §6's board law).
    SetRaidTarget { unit: String, index: u8 },
}

impl super::UiScript {
    /// Push the roster snapshot, replacing whatever was there. A bare setter (the `spellbook`/
    /// `action` shape) — firing any `PARTY_*`/roster-changed event is the app's own diff-and-fire
    /// job, never auto-fired here.
    pub fn set_party(&mut self, state: PartyState) {
        self.model_mut().party = state;
    }

    /// Drain the party/loot intents queued since the last call.
    pub fn take_party_requests(&mut self) -> Vec<PartyRequest> {
        std::mem::take(&mut self.model_mut().party_requests)
    }

    /// Drain the whisper targets `ChatFrame_SendTell` queued since the last call — the app opens
    /// its chat edit box prefilled `/w <name> ` for each (in practice the popup queues one).
    pub fn take_tell_requests(&mut self) -> Vec<String> {
        std::mem::take(&mut self.model_mut().tell_requests)
    }
}

/// Register the party/raid globals reading the roster snapshot store (the same style/place `unit`
/// registers the `Unit*` globals).
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // GetNumPartyMembers() → the roster's member count (0 = not in a group; never counts the player
    // themself, matching SMSG_GROUP_LIST's recipient-excluded array).
    g.set(
        "GetNumPartyMembers",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.party.members.len() as i64)
        })?,
    )?;

    // GetNumRaidMembers() → the raid roster's count (0 outside a raid — module doc's v1 gap).
    g.set(
        "GetNumRaidMembers",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(model.party.raid_members))
        })?,
    )?;

    // GetPartyMember(id) → 1 if id is a live 1-based roster slot, else nil (era 1/nil shape).
    g.set(
        "GetPartyMember",
        lua.create_function(|lua, id: i64| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let n = model.party.members.len() as i64;
            Ok(if id >= 1 && id <= n {
                Value::Integer(1)
            } else {
                Value::Nil
            })
        })?,
    )?;

    // GetPartyLeaderIndex() → 0 (the player leads) or 1..4 (that party slot leads).
    g.set(
        "GetPartyLeaderIndex",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(model.party.leader_index))
        })?,
    )?;

    // IsPartyLeader() → 1 iff we're grouped AND lead it, else nil (a solo player doesn't "lead").
    g.set(
        "IsPartyLeader",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let leads = !model.party.members.is_empty() && model.party.leader_index == 0;
            Ok(if leads { Value::Integer(1) } else { Value::Nil })
        })?,
    )?;

    // GetLootMethod() → lootmethod, masterlooterPartyID (the era 2-tuple return; later clients add
    // a raid index third return we don't carry). An unset method (never pushed) reports the
    // fresh-player shape: "group", nil.
    g.set(
        "GetLootMethod",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let method = if model.party.loot_method.is_empty() {
                "group"
            } else {
                model.party.loot_method.as_str()
            };
            let master = match model.party.master_looter {
                Some(idx) => Value::Integer(i64::from(idx)),
                None => Value::Nil,
            };
            Ok((Value::String(lua.create_string(method)?), master))
        })?,
    )?;

    // GetLootThreshold() → the quality floor (0 while ungrouped is fine — nothing to floor).
    g.set(
        "GetLootThreshold",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(model.party.loot_threshold))
        })?,
    )?;

    // The outbound half: each call queues a PartyRequest, the app drains and sends. No-return, era
    // shape (fire-and-forget, like TargetUnit/CastSpell).
    g.set(
        "AcceptGroup",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.party_requests.push(PartyRequest::Accept);
            Ok(())
        })?,
    )?;
    g.set(
        "DeclineGroup",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.party_requests.push(PartyRequest::Decline);
            Ok(())
        })?,
    )?;
    g.set(
        "LeaveParty",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.party_requests.push(PartyRequest::Leave);
            Ok(())
        })?,
    )?;
    g.set(
        "InviteByName",
        lua.create_function(|lua, name: String| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.party_requests.push(PartyRequest::InviteName(name));
            Ok(())
        })?,
    )?;
    g.set(
        "InviteToParty",
        lua.create_function(|lua, unit: String| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.party_requests.push(PartyRequest::InviteUnit(unit));
            Ok(())
        })?,
    )?;
    g.set(
        "UninviteFromParty",
        lua.create_function(|lua, unit: String| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.party_requests.push(PartyRequest::UninviteUnit(unit));
            Ok(())
        })?,
    )?;
    g.set(
        "PromoteToPartyLeader",
        lua.create_function(|lua, unit: String| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.party_requests.push(PartyRequest::PromoteUnit(unit));
            Ok(())
        })?,
    )?;
    g.set(
        "SetLootMethod",
        lua.create_function(|lua, (method, master_name): (String, Option<String>)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.party_requests.push(PartyRequest::LootMethod {
                method,
                master_name,
            });
            Ok(())
        })?,
    )?;
    g.set(
        "SetLootThreshold",
        lua.create_function(|lua, n: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.party_requests.push(PartyRequest::LootThreshold(n));
            Ok(())
        })?,
    )?;
    g.set(
        "SetRaidTargetIcon",
        lua.create_function(|lua, (unit, index): (String, u8)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model
                .party_requests
                .push(PartyRequest::SetRaidTarget { unit, index });
            Ok(())
        })?,
    )?;

    // IsRaidOfficer() → nil, always: a 1.12 PARTY has no officer rank (the assistant flag is a
    // raid concept), and the raid roster is a later arc (module doc's v1 gap). The popup's
    // leader-or-assistant gates read this and fall back to the leader half.
    g.set(
        "IsRaidOfficer",
        lua.create_function(|_, ()| Ok(Value::Nil))?,
    )?;

    // ChatFrame_SendTell(name) — the popup's WHISPER action. In the ref this is ChatFrame.lua
    // filling the edit box with "/w name "; our chat edit is app-side (ui_chat), so the call
    // queues the name for the app to open the edit box prefilled (UiScript::take_tell_requests
    // drains — the PartyRequest seam's chat sibling).
    g.set(
        "ChatFrame_SendTell",
        lua.create_function(|lua, name: String| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.tell_requests.push(name);
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::script::{PartyMemberInfo, PartyRequest, PartyState, UiScript};

    fn two_member_party() -> PartyState {
        PartyState {
            members: vec![
                PartyMemberInfo {
                    name: "Alice".into(),
                    guid: 0xA11CE,
                },
                PartyMemberInfo {
                    name: "Bob".into(),
                    guid: 0xB0B,
                },
            ],
            leader_index: 1, // Alice (party1) leads
            raid_members: 0,
            loot_method: "group".into(),
            master_looter: None,
            loot_threshold: 2,
        }
    }

    #[test]
    fn read_natives_report_the_pushed_roster() {
        let mut s = UiScript::new().unwrap();
        s.set_party(two_member_party());

        assert_eq!(s.eval::<i64>("return GetNumPartyMembers()").unwrap(), 2);
        assert_eq!(s.eval::<i64>("return GetNumRaidMembers()").unwrap(), 0);
        assert_eq!(s.eval::<i64>("return GetPartyMember(1)").unwrap(), 1);
        assert_eq!(s.eval::<i64>("return GetPartyMember(2)").unwrap(), 1);
        assert!(s.eval::<bool>("return GetPartyMember(3) == nil").unwrap());
        assert!(s.eval::<bool>("return GetPartyMember(0) == nil").unwrap());
        assert_eq!(s.eval::<i64>("return GetPartyLeaderIndex()").unwrap(), 1);
        // Alice (party1) leads, not us.
        assert!(s.eval::<bool>("return IsPartyLeader() == nil").unwrap());
        let (method, master) = s
            .eval::<(String, Option<i64>)>("return GetLootMethod()")
            .unwrap();
        assert_eq!(method, "group");
        assert_eq!(master, None);
        assert_eq!(s.eval::<i64>("return GetLootThreshold()").unwrap(), 2);
    }

    #[test]
    fn is_party_leader_reports_when_the_player_leads() {
        let mut s = UiScript::new().unwrap();
        let mut party = two_member_party();
        party.leader_index = 0; // the player leads
        s.set_party(party);
        assert_eq!(s.eval::<i64>("return IsPartyLeader()").unwrap(), 1);
    }

    #[test]
    fn get_loot_method_reports_the_assigned_master() {
        let mut s = UiScript::new().unwrap();
        let mut party = two_member_party();
        party.loot_method = "master".into();
        party.master_looter = Some(2); // Bob (party2)
        s.set_party(party);
        let (method, master) = s.eval::<(String, i64)>("return GetLootMethod()").unwrap();
        assert_eq!(method, "master");
        assert_eq!(master, 2);
    }

    #[test]
    fn empty_state_reports_the_solo_player_shape() {
        let s = UiScript::new().unwrap();
        assert_eq!(s.eval::<i64>("return GetNumPartyMembers()").unwrap(), 0);
        assert!(s.eval::<bool>("return GetPartyMember(1) == nil").unwrap());
        assert!(s.eval::<bool>("return IsPartyLeader() == nil").unwrap());
        let (method, master) = s
            .eval::<(String, Option<i64>)>("return GetLootMethod()")
            .unwrap();
        assert_eq!(method, "group");
        assert_eq!(master, None);
    }

    #[test]
    fn intent_natives_queue_the_exact_request_sequence() {
        let mut s = UiScript::new().unwrap();
        // Nothing queued until a call lands.
        assert!(s.take_party_requests().is_empty());

        s.run("AcceptGroup()").unwrap();
        s.run("DeclineGroup()").unwrap();
        s.run("LeaveParty()").unwrap();
        s.run(r#"InviteByName("Bob")"#).unwrap();
        s.run(r#"InviteToParty("target")"#).unwrap();
        s.run(r#"UninviteFromParty("party2")"#).unwrap();
        s.run(r#"PromoteToPartyLeader("party2")"#).unwrap();
        s.run(r#"SetLootMethod("master", "Bob")"#).unwrap();
        s.run(r#"SetLootThreshold(3)"#).unwrap();

        assert_eq!(
            s.take_party_requests(),
            vec![
                PartyRequest::Accept,
                PartyRequest::Decline,
                PartyRequest::Leave,
                PartyRequest::InviteName("Bob".into()),
                PartyRequest::InviteUnit("target".into()),
                PartyRequest::UninviteUnit("party2".into()),
                PartyRequest::PromoteUnit("party2".into()),
                PartyRequest::LootMethod {
                    method: "master".into(),
                    master_name: Some("Bob".into()),
                },
                PartyRequest::LootThreshold(3),
            ]
        );
        // The drain is a take — a second read is empty.
        assert!(s.take_party_requests().is_empty());
    }

    #[test]
    fn set_loot_method_without_a_master_name_queues_none() {
        let mut s = UiScript::new().unwrap();
        s.run(r#"SetLootMethod("freeforall")"#).unwrap();
        assert_eq!(
            s.take_party_requests(),
            vec![PartyRequest::LootMethod {
                method: "freeforall".into(),
                master_name: None,
            }]
        );
    }

    #[test]
    fn set_raid_target_icon_queues_the_token_and_index() {
        let mut s = UiScript::new().unwrap();
        s.run(r#"SetRaidTargetIcon("target", 8)"#).unwrap();
        s.run(r#"SetRaidTargetIcon("party2", 0)"#).unwrap();
        assert_eq!(
            s.take_party_requests(),
            vec![
                PartyRequest::SetRaidTarget {
                    unit: "target".into(),
                    index: 8,
                },
                PartyRequest::SetRaidTarget {
                    unit: "party2".into(),
                    index: 0,
                },
            ]
        );
    }

    #[test]
    fn is_raid_officer_is_nil_until_the_raid_arc() {
        let s = UiScript::new().unwrap();
        assert!(s.eval::<bool>("return IsRaidOfficer() == nil").unwrap());
    }

    #[test]
    fn chat_frame_send_tell_queues_the_name() {
        let mut s = UiScript::new().unwrap();
        assert!(s.take_tell_requests().is_empty());
        s.run(r#"ChatFrame_SendTell("Alice")"#).unwrap();
        assert_eq!(s.take_tell_requests(), vec!["Alice".to_string()]);
        assert!(s.take_tell_requests().is_empty());
    }

    // ── The identity predicates (decision 0434 §5 — the popup's menu pick + gating) ─────────────

    fn unit(exists: bool, guid: u64) -> crate::script::UnitState {
        crate::script::UnitState {
            exists,
            guid,
            ..Default::default()
        }
    }

    #[test]
    fn unit_is_unit_compares_guids_and_tokens() {
        let mut s = UiScript::new().unwrap();
        s.set_unit("player", Some(unit(true, 0x10)));
        s.set_unit("target", Some(unit(true, 0x10)));
        s.set_unit("party1", Some(unit(true, 0x20)));
        // Same guid across tokens; same token trivially; different guids nil.
        assert_eq!(
            s.eval::<i64>(r#"return UnitIsUnit("target", "player")"#)
                .unwrap(),
            1
        );
        assert_eq!(
            s.eval::<i64>(r#"return UnitIsUnit("player", "player")"#)
                .unwrap(),
            1
        );
        assert!(s
            .eval::<bool>(r#"return UnitIsUnit("party1", "player") == nil"#)
            .unwrap());
        // Zero guids never match across tokens (unknown identity is not identity).
        s.set_unit("target", Some(unit(true, 0)));
        s.set_unit("mouseover", Some(unit(true, 0)));
        assert!(s
            .eval::<bool>(r#"return UnitIsUnit("target", "mouseover") == nil"#)
            .unwrap());
        // A missing token is nil.
        assert!(s
            .eval::<bool>(r#"return UnitIsUnit("pet", "player") == nil"#)
            .unwrap());
    }

    #[test]
    fn unit_in_party_matches_roster_guids() {
        let mut s = UiScript::new().unwrap();
        s.set_party(two_member_party());
        s.set_unit("player", Some(unit(true, 0x10)));
        s.set_unit("party1", Some(unit(true, 0xA11CE)));
        // The target IS Alice (guid match through an arbitrary token).
        s.set_unit("target", Some(unit(true, 0xA11CE)));
        assert_eq!(s.eval::<i64>(r#"return UnitInParty("target")"#).unwrap(), 1);
        assert_eq!(s.eval::<i64>(r#"return UnitInParty("party1")"#).unwrap(), 1);
        // A stranger's guid is nil.
        s.set_unit("target", Some(unit(true, 0xDEAD)));
        assert!(s
            .eval::<bool>(r#"return UnitInParty("target") == nil"#)
            .unwrap());
        // Ungrouped: everything is nil, the player included.
        s.set_party(crate::script::PartyState::default());
        assert!(s
            .eval::<bool>(r#"return UnitInParty("player") == nil"#)
            .unwrap());
    }

    #[test]
    fn unit_can_cooperate_needs_a_friendly_player() {
        let mut s = UiScript::new().unwrap();
        let mut friendly = unit(true, 0x30);
        friendly.is_player = true;
        friendly.reaction = 5;
        s.set_unit("target", Some(friendly.clone()));
        assert_eq!(
            s.eval::<i64>(r#"return UnitCanCooperate("player", "target")"#)
                .unwrap(),
            1
        );
        // A hostile player, and a friendly NPC, both fail the gate.
        let mut hostile = friendly.clone();
        hostile.reaction = 2;
        s.set_unit("target", Some(hostile));
        assert!(s
            .eval::<bool>(r#"return UnitCanCooperate("player", "target") == nil"#)
            .unwrap());
        let mut npc = friendly;
        npc.is_player = false;
        s.set_unit("target", Some(npc));
        assert!(s
            .eval::<bool>(r#"return UnitCanCooperate("player", "target") == nil"#)
            .unwrap());
    }

    #[test]
    fn get_raid_target_index_reads_the_fed_mark() {
        let mut s = UiScript::new().unwrap();
        let mut marked = unit(true, 0x40);
        marked.raid_target = 8;
        s.set_unit("target", Some(marked));
        assert_eq!(
            s.eval::<i64>(r#"return GetRaidTargetIndex("target")"#)
                .unwrap(),
            8
        );
        s.set_unit("target", Some(unit(true, 0x40)));
        assert!(s
            .eval::<bool>(r#"return GetRaidTargetIndex("target") == nil"#)
            .unwrap());
    }
}
