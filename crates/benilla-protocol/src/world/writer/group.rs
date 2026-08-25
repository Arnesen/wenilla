//! The group/party family's `WorldWriter` sends — invite/accept/decline/kick/leader/disband, loot
//! method, party-stats asks, raid subgroup management, minimap pings, raid-target icons, and ready
//! checks (bodies in [`crate::messages`]'s `group_*`/`loot_method`/`minimap_ping`/`raid_target_*`/
//! `ready_check_*` builders; layout VERIFIED against vmangos `Server/Packets/Group.{h,cpp}`,
//! `Handlers/GroupHandler.cpp`). Split out of `writer/mod.rs`, mirroring [`super::channel`] — the
//! group commands are one clearly separable concern among the writer's many domains.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Invite a player to group (`CMSG_GROUP_INVITE`, layout in [`messages::group_invite`]).
    /// Answered by `SMSG_PARTY_COMMAND_RESULT`; the invited player sees `SMSG_GROUP_INVITE`.
    pub fn group_invite(&mut self, member_name: &str) -> Result<()> {
        self.send(
            opcode::CMSG_GROUP_INVITE,
            &messages::group_invite(member_name),
        )
    }

    /// Accept a pending group invite (`CMSG_GROUP_ACCEPT`, empty body).
    pub fn group_accept(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GROUP_ACCEPT, &messages::group_accept())
    }

    /// Decline a pending group invite (`CMSG_GROUP_DECLINE`, empty body); the inviter sees
    /// `SMSG_GROUP_DECLINE`.
    pub fn group_decline(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GROUP_DECLINE, &messages::group_decline())
    }

    /// Kick a group member by name (`CMSG_GROUP_UNINVITE`, layout in [`messages::group_uninvite`]).
    pub fn group_uninvite(&mut self, member_name: &str) -> Result<()> {
        self.send(
            opcode::CMSG_GROUP_UNINVITE,
            &messages::group_uninvite(member_name),
        )
    }

    /// Kick a group member by guid (`CMSG_GROUP_UNINVITE_GUID`) — the raid-frame right-click kick.
    pub fn group_uninvite_guid(&mut self, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_GROUP_UNINVITE_GUID,
            &messages::group_uninvite_guid(guid),
        )
    }

    /// Hand off group leadership (`CMSG_GROUP_SET_LEADER`, a full guid — the 1.12 wire form).
    pub fn group_set_leader(&mut self, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_GROUP_SET_LEADER,
            &messages::group_set_leader(guid),
        )
    }

    /// Set the group's loot method (`CMSG_LOOT_METHOD`, layout in [`messages::loot_method`]):
    /// `method` is `0` free-for-all / `1` round-robin / `2` master / `3` group / `4`
    /// need-before-greed; `loot_master` is the master looter's guid (ignored unless `method ==
    /// 2`); `threshold` is an `ItemQualities` value.
    pub fn loot_method(&mut self, method: u32, loot_master: u64, threshold: u32) -> Result<()> {
        self.send(
            opcode::CMSG_LOOT_METHOD,
            &messages::loot_method(method, loot_master, threshold),
        )
    }

    /// Disband the group (`CMSG_GROUP_DISBAND`, empty body).
    pub fn group_disband(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GROUP_DISBAND, &messages::group_disband())
    }

    /// Ask a party/raid member's full live stats (`CMSG_REQUEST_PARTY_MEMBER_STATS`, a full guid).
    /// Answered by `SMSG_PARTY_MEMBER_STATS_FULL` (or its offline-miss shape).
    pub fn request_party_member_stats(&mut self, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_REQUEST_PARTY_MEMBER_STATS,
            &messages::request_party_member_stats(guid),
        )
    }

    /// Move a raid member to a different subgroup (`CMSG_GROUP_CHANGE_SUB_GROUP`, layout in
    /// [`messages::group_change_sub_group`]) — raid-only drag-drop between subgroup columns.
    pub fn group_change_sub_group(&mut self, name: &str, group_nr: u8) -> Result<()> {
        self.send(
            opcode::CMSG_GROUP_CHANGE_SUB_GROUP,
            &messages::group_change_sub_group(name, group_nr),
        )
    }

    /// Swap two raid members' subgroups (`CMSG_GROUP_SWAP_SUB_GROUP`, layout in
    /// [`messages::group_swap_sub_group`]).
    pub fn group_swap_sub_group(&mut self, name: &str, swap_with: &str) -> Result<()> {
        self.send(
            opcode::CMSG_GROUP_SWAP_SUB_GROUP,
            &messages::group_swap_sub_group(name, swap_with),
        )
    }

    /// Convert the party into a raid (`CMSG_GROUP_RAID_CONVERT`, empty body) — leader-only,
    /// one-way.
    pub fn group_raid_convert(&mut self) -> Result<()> {
        self.send(
            opcode::CMSG_GROUP_RAID_CONVERT,
            &messages::group_raid_convert(),
        )
    }

    /// Grant or revoke raid-assistant on a member (`CMSG_GROUP_ASSISTANT_LEADER`, layout in
    /// [`messages::group_assistant_leader`]).
    pub fn group_assistant_leader(&mut self, guid: u64, grant: bool) -> Result<()> {
        self.send(
            opcode::CMSG_GROUP_ASSISTANT_LEADER,
            &messages::group_assistant_leader(guid, grant),
        )
    }

    /// Ping the minimap for the group (`MSG_MINIMAP_PING`, layout in [`messages::minimap_ping`]) —
    /// the server stamps our guid and relays it to the rest of the group.
    pub fn minimap_ping(&mut self, x: f32, y: f32) -> Result<()> {
        self.send(opcode::MSG_MINIMAP_PING, &messages::minimap_ping(x, y))
    }

    /// Set (or clear, `guid == 0`) one raid-target icon (`MSG_RAID_TARGET_UPDATE`, layout in
    /// [`messages::raid_target_set`]).
    pub fn raid_target_set(&mut self, icon: u8, guid: u64) -> Result<()> {
        self.send(
            opcode::MSG_RAID_TARGET_UPDATE,
            &messages::raid_target_set(icon, guid),
        )
    }

    /// Ask the current raid-target icon set (`MSG_RAID_TARGET_UPDATE`, layout in
    /// [`messages::raid_target_request`]). Answered by the server's full-list shape.
    pub fn raid_target_request(&mut self) -> Result<()> {
        self.send(
            opcode::MSG_RAID_TARGET_UPDATE,
            &messages::raid_target_request(),
        )
    }

    /// Start a raid ready check (`MSG_RAID_READY_CHECK`, empty body) — leader-only.
    pub fn ready_check_start(&mut self) -> Result<()> {
        self.send(opcode::MSG_RAID_READY_CHECK, &messages::ready_check_start())
    }

    /// Answer a raid ready check (`MSG_RAID_READY_CHECK`, layout in
    /// [`messages::ready_check_answer`]).
    pub fn ready_check_answer(&mut self, ready: bool) -> Result<()> {
        self.send(
            opcode::MSG_RAID_READY_CHECK,
            &messages::ready_check_answer(ready),
        )
    }

    /// Ask for our saved-instance list (`CMSG_REQUEST_RAID_INFO`, empty body — decision 1549's
    /// Raid Info panel). Answered by `SMSG_RAID_INSTANCE_INFO`.
    pub fn request_raid_info(&mut self) -> Result<()> {
        self.send(
            opcode::CMSG_REQUEST_RAID_INFO,
            &messages::request_raid_info(),
        )
    }
}
