//! The chat arc's internal currency (decision 0288 §1): [`ChatEvent`] mirrors the reference
//! client's `CHAT_MSG_*` event + `arg1..argN` shape as typed fields, so every source — the wire
//! (`SMSG_MESSAGECHAT`, `SMSG_CHANNEL_NOTIFY`, whisper-fail errors, `SMSG_TEXT_EMOTE`) and the
//! client-composed feeds (loot receive lines, played time, rolls) — speaks one vocabulary, and
//! the router/composer ([`super::frames`]) is the single place lines are formatted, colored, and
//! fanned across the docked windows.
//!
//! The kind set is the renderable subset of the reference's `ChatTypeInfo` (its `COMBAT_*`/
//! `SPELL_*` block is the combat-log content arc, deliberately out — 0288 §3, except
//! `COMBAT_XP_GAIN`, pulled in by the ding arc 0304: the XP line is part of leveling feedback);
//! the group tables are `ChatTypeGroup` transcribed; the colors are the complete shipped default
//! table (the ref client's own `chat-cache.txt` COLORS block ≡ wow-re's byte-verified
//! `.rdata 0x804710` table, double-sourced in 0288's pin).

/// The renderable chat-event kinds — `ChatTypeInfo`'s keys, minus the combat-log block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChatEventKind {
    Say,
    Party,
    Raid,
    Guild,
    Officer,
    Yell,
    Whisper,
    WhisperInform,
    Emote,
    TextEmote,
    System,
    MonsterSay,
    MonsterYell,
    MonsterEmote,
    MonsterWhisper,
    Channel,
    ChannelJoin,
    ChannelLeave,
    ChannelNotice,
    ChannelNoticeUser,
    ChannelList,
    Afk,
    Dnd,
    Ignored,
    Skill,
    Loot,
    Money,
    /// The client-composed XP line (`SMSG_LOG_XPGAIN`, decision 0304) — the one `COMBAT_*`
    /// family member modeled (0288 §3 keeps the rest for the combat-log content arc).
    CombatXpGain,
    RaidLeader,
    RaidWarning,
    RaidBossEmote,
    Battleground,
    BattlegroundLeader,
    BgSystemNeutral,
    BgSystemAlliance,
    BgSystemHorde,
}

/// One chat event — the reference's `CHAT_MSG_*` fire, typed. Field ↔ arg mapping (the 1.12
/// handler's positional args, constrained by `ChatFrame_OnEvent`'s reads):
/// `text`=arg1 · `sender`=arg2 · `language`=arg3 (already a *name*, empty = no header) ·
/// `channel`=arg4 (the display form, "N. Name - Zone" when numbered) · `target`=arg5 (the second
/// name of a two-name channel notice) · `flag`=arg6 ("AFK"/"DND"/"GM", empty none) ·
/// `channel_number`=arg8 · `channel_base`=arg9 (the number-less name). `notice`=arg1 of the
/// CHANNEL_NOTICE family (the token selecting the `CHAT_<X>_NOTICE` string).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ChatEvent {
    pub kind: Option<ChatEventKind>,
    pub text: String,
    pub sender: String,
    pub language: String,
    pub channel: String,
    pub target: String,
    pub flag: String,
    pub channel_number: u32,
    pub channel_base: String,
    pub notice: String,
}

impl ChatEvent {
    /// A bare kind+text event (SYSTEM lines, loot receive lines, TEXT_EMOTE sentences).
    pub(crate) fn text_only(kind: ChatEventKind, text: String) -> Self {
        ChatEvent {
            kind: Some(kind),
            text,
            ..Default::default()
        }
    }
}

/// The message groups a window registers — `ChatTypeGroup`'s keys (transcribed; the chat-cache
/// WINDOW blocks list these names). Only the groups the current kind set can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChatGroup {
    System,
    Say,
    Yell,
    Whisper,
    Party,
    Guild,
    Creature,
    Channel,
    Skill,
    Loot,
    Money,
    /// `COMBAT_XP_GAIN` — one kind, its own group (ref ChatFrame.lua l.235-237).
    CombatXpGain,
}

/// `ChatTypeGroup` transcribed (ref ChatFrame.lua l.116-174): which kinds a group registration
/// subscribes a window to. `MONEY` rides both the LOOT group (l.171-174) and window 2's explicit
/// MONEY registration (chat-cache), hence its own group here.
pub(crate) fn group_kinds(group: ChatGroup) -> &'static [ChatEventKind] {
    use ChatEventKind as K;
    match group {
        ChatGroup::System => &[
            K::System,
            K::Afk,
            K::Dnd,
            K::Ignored,
            K::ChannelList,
            K::BgSystemNeutral,
            K::BgSystemAlliance,
            K::BgSystemHorde,
        ],
        ChatGroup::Say => &[K::Say, K::Emote, K::TextEmote],
        ChatGroup::Yell => &[K::Yell],
        ChatGroup::Whisper => &[K::Whisper, K::WhisperInform],
        ChatGroup::Party => &[
            K::Party,
            K::Raid,
            K::RaidLeader,
            K::RaidWarning,
            K::Battleground,
            K::BattlegroundLeader,
        ],
        ChatGroup::Guild => &[K::Guild, K::Officer],
        ChatGroup::Creature => &[
            K::MonsterSay,
            K::MonsterYell,
            K::MonsterEmote,
            K::MonsterWhisper,
            K::RaidBossEmote,
        ],
        ChatGroup::Channel => &[
            K::ChannelJoin,
            K::ChannelLeave,
            K::ChannelNotice,
            K::ChannelNoticeUser,
        ],
        ChatGroup::Skill => &[K::Skill],
        ChatGroup::Loot => &[K::Loot, K::Money],
        ChatGroup::Money => &[K::Money],
        ChatGroup::CombatXpGain => &[K::CombatXpGain],
    }
}

/// The kind's default color — the complete shipped table (chat-cache COLORS ≡ wow-re
/// `chat-color-table.md`, both quoted in 0288's pin; entries this kind set carries). The numbered
/// CHANNEL1..10 rows all ship the CHANNEL pink until per-number recolor exists (settings).
pub(crate) fn default_color(kind: ChatEventKind) -> [u8; 3] {
    use ChatEventKind as K;
    match kind {
        K::Say => [255, 255, 255],
        K::Party => [170, 170, 255],
        K::Raid => [255, 127, 0],
        K::Guild => [64, 255, 64],
        K::Officer => [64, 192, 64],
        K::Yell => [255, 64, 64],
        K::Whisper | K::WhisperInform | K::Afk | K::Dnd => [255, 128, 255],
        K::Emote | K::TextEmote => [255, 128, 64],
        K::System => [255, 255, 0],
        K::MonsterSay => [255, 255, 159],
        K::MonsterYell => [255, 64, 64],
        K::MonsterEmote => [255, 128, 64],
        K::MonsterWhisper => [179, 179, 179],
        K::Channel => [255, 192, 192],
        K::ChannelJoin | K::ChannelLeave | K::ChannelList => [192, 128, 128],
        K::ChannelNotice | K::ChannelNoticeUser => [192, 192, 192],
        K::Ignored => [255, 0, 0],
        K::Skill => [85, 85, 255],
        K::Loot => [0, 170, 0],
        K::Money => [255, 255, 0],
        K::CombatXpGain => [111, 111, 255],
        K::RaidLeader | K::RaidWarning | K::RaidBossEmote | K::BattlegroundLeader => {
            [255, 219, 183]
        }
        K::Battleground => [255, 127, 0],
        K::BgSystemNeutral => [255, 120, 10],
        K::BgSystemAlliance => [0, 174, 239],
        K::BgSystemHorde => [255, 0, 0],
    }
}

/// Map a wire `ChatMsg` byte (`SMSG_MESSAGECHAT.chat_type`) to its event kind. `None` = a type
/// vmangos never emits as wire chat (the combat-log block) or one we don't model — the router
/// drops it loudly.
pub(crate) fn kind_of_wire(chat_type: u8) -> Option<ChatEventKind> {
    use benilla_protocol::messages as m;
    use ChatEventKind as K;
    Some(match chat_type {
        m::CHAT_MSG_SAY => K::Say,
        m::CHAT_MSG_PARTY => K::Party,
        m::CHAT_MSG_RAID => K::Raid,
        m::CHAT_MSG_GUILD => K::Guild,
        m::CHAT_MSG_OFFICER => K::Officer,
        m::CHAT_MSG_YELL => K::Yell,
        m::CHAT_MSG_WHISPER => K::Whisper,
        m::CHAT_MSG_WHISPER_INFORM => K::WhisperInform,
        m::CHAT_MSG_EMOTE => K::Emote,
        m::CHAT_MSG_SYSTEM => K::System,
        m::CHAT_MSG_MONSTER_SAY => K::MonsterSay,
        m::CHAT_MSG_MONSTER_YELL => K::MonsterYell,
        m::CHAT_MSG_MONSTER_EMOTE => K::MonsterEmote,
        m::CHAT_MSG_CHANNEL => K::Channel,
        m::CHAT_MSG_AFK => K::Afk,
        m::CHAT_MSG_DND => K::Dnd,
        m::CHAT_MSG_IGNORED => K::Ignored,
        m::CHAT_MSG_MONSTER_WHISPER | m::CHAT_MSG_RAID_BOSS_WHISPER => K::MonsterWhisper,
        m::CHAT_MSG_RAID_LEADER => K::RaidLeader,
        m::CHAT_MSG_RAID_WARNING => K::RaidWarning,
        m::CHAT_MSG_RAID_BOSS_EMOTE => K::RaidBossEmote,
        m::CHAT_MSG_BATTLEGROUND => K::Battleground,
        m::CHAT_MSG_BATTLEGROUND_LEADER => K::BattlegroundLeader,
        _ => return None,
    })
}

/// The `<AFK>`/`<DND>`/`<GM>` flag token for a wire chat-tag byte (`Player::GetChatTag`) — the
/// event's `flag` field (the ref's arg6), consumed as `CHAT_FLAG_<flag>`.
pub(crate) fn flag_of_tag(chat_tag: u8) -> &'static str {
    use benilla_protocol::messages::chat_tag as t;
    match chat_tag {
        t::AFK => "AFK",
        t::DND => "DND",
        t::GM => "GM",
        _ => "",
    }
}

/// The 1.12 language names by wire id (vmangos `SharedDefines.h` `LANG_*` — the small racial
/// set), for the `[Language]` header when a line isn't Universal/our default. Unknown ids render
/// no header (v1: benilla speaks Common; the picker is a later arc).
pub(crate) fn language_name(id: u32) -> &'static str {
    match id {
        1 => "Orcish",
        2 => "Darnassian",
        3 => "Taurahe",
        6 => "Dwarvish",
        7 => "Common",
        8 => "Demonic",
        9 => "Titan",
        10 => "Thalassian",
        11 => "Draconic",
        12 => "Kalimag",
        13 => "Gnomish",
        14 => "Troll",
        33 => "Gutterspeak",
        _ => "", // 0 = Universal (no header), unknowns likewise
    }
}
