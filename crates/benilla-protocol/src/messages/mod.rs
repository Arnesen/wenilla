//! The world (`mangosd`) message layer — in-repo replacement for `wow_world_messages` (decision
//! 0021), scoped to the opcodes benilla sends and receives.
//!
//! World packets are framed by a header-encrypted size+opcode (see [`crate::world`]); this module owns
//! only the *bodies*: parsing the server packets benilla decodes into [`ServerPacket`], and building
//! the client packet bodies benilla sends. The one genuinely complex packet is `SMSG_UPDATE_OBJECT`
//! (and its zlib twin) — an object list where each entry carries a [`MovementBlock`]-shaped position
//! and a sparse [`ObjectFields`] of descriptor fields; its decode lives in [`update_object`], and
//! [`crate::events`] pulls out the handful of fields the renderer uses.
//! The [`ServerPacket`] enum itself lives in `packet`; the opcode→variant dispatch
//! ([`parse_server`]) in `parse`.
//!
//! Proven byte-for-byte against `wow_world_messages` during the decision-0021 migration (oracle test
//! in git history); ongoing coverage is the oracle-free golden/fixture tests in `tests/*.rs` (split by
//! domain — client, movement, items, spells, update_object, simple_packets — sharing fixtures via
//! `tests/common`).

mod action_bar;
mod area_trigger;
mod attack;
mod bank;
mod channel;
mod chat;
mod client;
mod combat_log;
mod death;
mod duel;
mod gameobject;
mod gossip;
mod group;
mod items;
mod loot;
mod mail;
mod monster_move;
mod movement;
pub mod opcode;
mod opcode_names;
mod packet;
mod parse;
mod pose;
mod progression;
mod quest;
mod roster;
mod skills;
mod social;
mod spellbook;
mod spells;
mod taxi;
mod trade;
mod trainer;
mod update_object;
mod vendor;
mod world_state;
pub use action_bar::{
    set_action_button, ActionButton, ACTION_KIND_ITEM, ACTION_KIND_MACRO, ACTION_KIND_SPELL,
};
pub use area_trigger::area_trigger;
pub use attack::{attack_swing, AttackerState};
pub use bank::{
    autobank_item, autostore_bank_item, bank_slot_result, banker_activate, buy_bank_slot,
};
pub use channel::{channel_notice, ChannelNoticeTail, ChannelNotify};
pub use chat::{
    chat_tag, ChatMessage, CHAT_MSG_AFK, CHAT_MSG_BATTLEGROUND, CHAT_MSG_BATTLEGROUND_LEADER,
    CHAT_MSG_CHANNEL, CHAT_MSG_DND, CHAT_MSG_EMOTE, CHAT_MSG_GUILD, CHAT_MSG_IGNORED,
    CHAT_MSG_MONSTER_EMOTE, CHAT_MSG_MONSTER_SAY, CHAT_MSG_MONSTER_WHISPER, CHAT_MSG_MONSTER_YELL,
    CHAT_MSG_OFFICER, CHAT_MSG_PARTY, CHAT_MSG_RAID, CHAT_MSG_RAID_BOSS_EMOTE,
    CHAT_MSG_RAID_BOSS_WHISPER, CHAT_MSG_RAID_LEADER, CHAT_MSG_RAID_WARNING, CHAT_MSG_SAY,
    CHAT_MSG_SYSTEM, CHAT_MSG_WHISPER, CHAT_MSG_WHISPER_INFORM, CHAT_MSG_YELL,
};
pub use client::{
    auth_session, channel_announcements, channel_ban, channel_invite, channel_kick, channel_list,
    channel_moderate, channel_moderator, channel_mute, channel_owner, channel_password,
    channel_set_owner, channel_unban, channel_unmoderator, channel_unmute, char_create,
    creature_query, force_speed_ack, full_guid, join_channel, leave_channel, messagechat,
    messagechat_channel, messagechat_kind, messagechat_whisper, move_flag_ack, move_spline_done,
    movement, pet_name_query, ping, played_time, random_roll, teleport_ack, text_emote,
};
pub use combat_log::{
    DamageShield, EnvironmentalDamageLog, PeriodicAuraLog, PeriodicTick, SpellDamageLog,
    SpellEnergizeLog, SpellHealLog, SpellLogMiss,
};
pub use death::{
    reclaim_corpse, resurrect_response, spirit_healer_activate, CorpseLocation,
    ResurrectRequestBody,
};
pub use duel::{
    duel_accepted, duel_cancelled, read_duel_complete, read_duel_countdown, read_duel_requested,
    read_duel_winner, DuelRequested, DuelWinner,
};
pub use gameobject::{gameobj_use, gameobject_query, GameObjectQueryInfo};
pub use gossip::{
    gossip_hello, gossip_select_option, npc_text_query, select_greeting, GossipOption,
    NpcTextBlock, QuestOption, NPC_TEXT_BLOCKS,
};
pub use group::{
    group_accept, group_assistant_leader, group_change_sub_group, group_decline, group_disband,
    group_invite, group_raid_convert, group_set_leader, group_swap_sub_group, group_uninvite,
    group_uninvite_guid, loot_method, member_status, minimap_ping, party_member_mask,
    party_operation, party_result, raid_target_request, raid_target_set, ready_check_answer,
    ready_check_start, request_party_member_stats, GroupLootInfo, GroupMemberEntry,
    PartyMemberStatsInfo, RaidTargetUpdate, ReadyCheck, GROUP_MEMBER_ASSISTANT,
};
pub use items::{
    auto_equip_item, auto_store_bag_item, destroy_item, item_query, set_ammo, split_item,
    swap_inv_item, swap_item, use_item, ItemDamage, ItemInfo, ItemSpellEntry, ItemUseSpell,
    BAG_PLAYER_INVENTORY, SLOT_BAG_FIRST, SLOT_PACK_FIRST,
};
pub use loot::{
    autostore_loot_item, loot, loot_error, loot_money, loot_release, loot_roll, loot_type,
    roll_vote, slot_type, ItemPushResult, LootAllPassed, LootItem, LootResponseBody, LootRoll,
    LootRollWon, LootStartRoll,
};
pub use mail::{
    get_mail_list, item_text_query, mail_action, mail_create_text_item, mail_delete, mail_error,
    mail_mark_as_read, mail_message_type, mail_return_to_sender, mail_take_item, mail_take_money,
    send_mail, MailAttachment, MailListEntry,
};
pub use movement::{JumpInfo, MovementInfo, SpeedKind, TransportPose};
pub use opcode_names::opcode_name;
pub use packet::{CreatureQueryInfo, MonsterMoveFacing, ServerPacket};
pub use parse::parse_server;
pub use pose::{set_sheathed, stand_state_change};
pub use progression::{learn_talent, LevelUpInfo, XpGain};
pub use quest::{
    dialog_status, quest_query, questgiver_accept_quest, questgiver_choose_reward,
    questgiver_complete_quest, questgiver_hello, questgiver_query_quest, questgiver_request_reward,
    questgiver_status_query, questlog_remove_quest, questlog_swap_quest, QuestComplete,
    QuestDetails, QuestGiverList, QuestListEntry, QuestObjective, QuestOfferReward,
    QuestRequestItems, QuestRequiredItem, QuestRewardItem, QuestTemplate, QUEST_EMOTE_COUNT,
    QUEST_OBJECTIVES_COUNT, QUEST_REWARDS_COUNT, QUEST_REWARD_CHOICES_COUNT,
};
pub use roster::{
    CharCreateReq, CharEnumItem, Character, CHARACTER_FLAG_GHOST, CHARACTER_FLAG_HIDE_CLOAK,
    CHARACTER_FLAG_HIDE_HELM, CHARACTER_FLAG_RENAME, CHAR_CREATE_NAME_IN_USE,
    CHAR_CREATE_SERVER_LIMIT, CHAR_CREATE_SUCCESS, CHAR_DELETE_SUCCESS, CLASS_WARRIOR, GENDER_MALE,
    RACE_HUMAN,
};
pub use skills::unlearn_skill;
pub use social::{
    add_friend, add_ignore, del_friend, del_ignore, friend_list, friend_result, friend_status,
    read_friend_list, read_friend_status, read_ignore_list, read_who, who, FriendEntry,
    FriendOnline, FriendStatusUpdate, WhoEntry, WhoRequest, WhoResults, WHO_MAX_SEARCH_TERMS,
    WHO_MAX_ZONES,
};
pub use spellbook::SpellCooldown;
pub use spells::{
    cancel_aura, cast_spell, cast_spell_gameobject, cast_spell_item, CastOutcome, SpellCastTargets,
    SpellGo, SpellStart,
};
pub use taxi::{
    activate_taxi, activate_taxi_express, taxi_node_status_query, taxi_query_available_nodes,
    taxi_reply, TaxiMask,
};
pub use trade::{
    accept_trade, clear_trade_item, initiate_trade, set_trade_gold, set_trade_item, TradeItem,
    TradeStatus, TradeStatusExtended, TRADE_SLOT_COUNT, TRADE_SLOT_NONTRADED,
    TRADE_SLOT_TRADED_COUNT,
};
pub use trainer::{train_fail, trainer_buy_spell, trainer_list, trainer_spell_state, TrainerSpell};
pub use update_object::{
    quest_slot_state, CreateSpline, MovementBlock, Object, ObjectFields, ObjectType,
    PlayerSkillSlot, QuestLogSlot, UnitAuraSlot, AURA_FLAG_CANCELABLE, AURA_FLAG_EFF_INDEX_MASK,
    PLAYER_EXPLORED_ZONES_SLOTS, PLAYER_QUEST_LOG_SLOTS, PLAYER_SKILL_SLOTS,
    UNIT_AURA_POSITIVE_SLOTS, UNIT_AURA_SLOTS,
};
pub use vendor::{
    buy_item, buy_result, buyback_item, list_inventory, repair_item, sell_item, sell_result,
    VendorItem,
};
pub use world_state::InitWorldStates;

/// `SMSG_AUTH_RESPONSE` AuthOk result.
pub const AUTH_OK: u8 = 0x0C;
/// `LogoutResult::Success` (`SMSG_LOGOUT_RESPONSE`).
pub const LOGOUT_SUCCESS: u32 = 0x0;
/// Chat `Language` wire ids (VERIFIED vmangos `SharedDefines.h:256-261`): the faction tongues.
/// `Universal` (0) is server-reserved — vmangos rejects it from clients for ordinary chat.
pub const LANGUAGE_COMMON: u32 = 0x7;
pub const LANGUAGE_ORCISH: u32 = 0x1;

/// The language a character speaks by default — its faction tongue. Every send must carry a
/// language the character *knows*: vmangos drops the whole message (including a `.command`
/// payload, which is intercepted downstream of the check) with a "not learned" notification
/// otherwise (`HandleChatMessageOpcode`'s `KnowsLanguage` gate, `Handlers/ChatHandler.cpp`).
/// Race → tongue VERIFIED against the live world DB (`playercreateinfo_spell`): races 1/3/4/7
/// (Alliance) learn spell 668 Language Common, races 2/5/6/8 (Horde) learn 669 Language Orcish.
pub fn faction_language(race: u8) -> u32 {
    match race {
        2 | 5 | 6 | 8 => LANGUAGE_ORCISH, // orc, undead, tauren, troll
        _ => LANGUAGE_COMMON,             // human, dwarf, night elf, gnome
    }
}
/// `ChatMsg` wire values benilla sends, widened to `u32` (VERIFIED vmangos `SharedDefines.h:
/// 1191-1301`) — `CMSG_MESSAGECHAT`'s `type` field is a `u32` on the wire
/// (`WorldPackets::Chat::ChatMessage::type`, `Server/Packets/Chat.h:12`), unlike the inbound
/// `SMSG_MESSAGECHAT` decode's `u8` (hence the separate, narrower [`CHAT_MSG_SAY`] etc. set there):
/// two constant sets for the same enum because the two wire fields are different widths.
pub const CHAT_TYPE_SAY: u32 = 0x0;
pub const CHAT_TYPE_PARTY: u32 = 0x1;
pub const CHAT_TYPE_RAID: u32 = 0x2;
pub const CHAT_TYPE_GUILD: u32 = 0x3;
pub const CHAT_TYPE_OFFICER: u32 = 0x4;
pub const CHAT_TYPE_YELL: u32 = 0x5;
pub const CHAT_TYPE_WHISPER: u32 = 0x6;
pub const CHAT_TYPE_EMOTE: u32 = 0x8;
pub const CHAT_TYPE_CHANNEL: u32 = 0xE;
pub const CHAT_TYPE_AFK: u32 = 0x14;
pub const CHAT_TYPE_DND: u32 = 0x15;
/// `#if SUPPORTED_CLIENT_BUILD > CLIENT_BUILD_1_10_2` — active for 5875.
pub const CHAT_TYPE_RAID_LEADER: u32 = 0x57;
pub const CHAT_TYPE_RAID_WARNING: u32 = 0x58;
/// `#if SUPPORTED_CLIENT_BUILD > CLIENT_BUILD_1_11_2` — active for 5875.
pub const CHAT_TYPE_BATTLEGROUND: u32 = 0x5C;
pub const CHAT_TYPE_BATTLEGROUND_LEADER: u32 = 0x5D;
