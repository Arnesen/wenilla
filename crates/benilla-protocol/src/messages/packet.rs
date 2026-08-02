//! The decoded [`ServerPacket`] — one variant per server opcode benilla models, with the wire
//! provenance on each, plus the logging [`ServerPacket::name`]. The opcode→variant dispatch lives
//! in the sibling `parse` module; the per-domain payload readers in the domain children.

use crate::wire::Vector3d;

use super::{
    ActionButton, AttackerState, CastOutcome, ChannelNotify, Character, ChatMessage,
    CorpseLocation, DamageShield, EnvironmentalDamageLog, ExplorationXp, FriendEntry,
    FriendStatusUpdate, GameObjectQueryInfo, GossipOption, GroupLootInfo, GroupMemberEntry,
    InitWorldStates, ItemInfo, ItemPushResult, JumpInfo, LevelUpInfo, LootAllPassed, LootItem,
    LootRoll, LootRollWon, LootStartRoll, MailListEntry, MirrorTimerStart, MoveMode, Object,
    PartyMemberStatsInfo, PeriodicAuraLog, QuestComplete, QuestDetails, QuestGiverList,
    QuestOfferReward, QuestOption, QuestRequestItems, QuestTemplate, ResurrectRequestBody,
    SpeedKind, SpellCooldown, SpellDamageLog, SpellEnergizeLog, SpellGo, SpellHealLog,
    SpellLogMiss, SpellStart, TaxiMask, TradeStatus, TradeStatusExtended, TrainerSpell,
    TransportPose, VendorItem, WhoResults, XpGain,
};

/// The **final facing** a `SMSG_MONSTER_MOVE` dictates (its `moveType`): the unit snaps to face this
/// when the move is applied — the real client stores it straight into the unit's movement facing, a
/// hard snap, not a smooth turn (wow-re object-layer: `0x6018f0` → `0x7c6f30 mov [esi+0x1c],eax`).
/// This is how a creature re-orients without walking (a scripted/emote/aggro face), and benilla used
/// to **discard** it (the "mob won't turn to face me" gap). `Angle` is a raw WoW orientation; `Spot`
/// / `Target` resolve to an angle in the app (atan2 from the unit to the point, or to the target
/// unit's live position — the client snapshots the target's position at receipt, it doesn't track).
#[derive(Debug, Clone, Copy)]
pub enum MonsterMoveFacing {
    /// A plain move (`moveType` 0) or a stop — no dictated facing; the unit faces its travel direction.
    None,
    /// `moveType 2` — face a world point (raw WoW coords).
    Spot([f32; 3]),
    /// `moveType 3` — face a unit by guid (resolved to its position when applied).
    Target(u64),
    /// `moveType 4` — face a raw orientation (radians, WoW convention).
    Angle(f32),
}

/// One creature template's UI-visible head (see [`ServerPacket::CreatureQueryResponse`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatureQueryInfo {
    pub name: String,
    /// The NPC subtitle ("Stable Master") — the unit tooltip's second line.
    pub subname: String,
    /// `CreatureType.dbc` id (Beast/Humanoid/… — the level line's type word + the TAB filter).
    pub creature_type: u32,
    /// Elite rank: 0 normal, 1 elite, 2 rare-elite, 3 world boss, 4 rare — the unit tooltip's
    /// rank word `{"", Elite, Elite, Boss, ""}` (decision 0276's byte-verified table).
    pub rank: u32,
    /// The template type flags. Bit `0x10` (HIDE_FACTION_TOOLTIP) suppresses the unit tooltip's
    /// faction-name line (the client's `0x612610` gate on the cached record's `+0x14`).
    pub type_flags: u32,
    /// The civilian flag (the tooltip's green CIVILIAN line; dishonorable-kill marking).
    pub civilian: bool,
    /// The racial-leader flag (the tooltip's white LEADER line; `0x6125c0` on the record's `+0x31`).
    pub racial_leader: bool,
}

/// A decoded server packet (only the opcodes benilla handles; everything else is [`Self::Other`]).
pub enum ServerPacket {
    AuthChallenge {
        server_seed: u32,
    },
    AuthResponse {
        result: u8,
    },
    CharEnum {
        characters: Vec<Character>,
    },
    CharCreate {
        result: u8,
    },
    CharDelete {
        result: u8,
    },
    UpdateObject {
        objects: Vec<Object>,
    },
    /// `SMSG_COMPRESSED_MOVES` — a **batch** of whole movement packets in one zlib envelope, each
    /// already parsed into its own [`ServerPacket`] (decision 0624).
    ///
    /// vmangos flips a session onto this container once it has been sent
    /// `Compression.Movement.Count` (300) movement packets inside a ten-second window
    /// (`WorldSession::SendMovementPacket`), and flips back when the rate drops — so it is not an
    /// exotic case but the **normal** carrier for any nearby player moving at frame cadence, which
    /// since decision 0617 is what our own sender produces. Body: `u32` uncompressed size, then a
    /// deflate stream of `[u8 size][u16 opcode][body]` records where `size` counts the opcode's two
    /// bytes (`MovementData::AddPacket`).
    CompressedMoves {
        packets: Vec<ServerPacket>,
    },
    /// `SMSG_DESTROY_OBJECT` — one object ceased to exist server-side (a corpse decaying ahead of
    /// respawn, a despawn), body a plain `u64` guid (vmangos `DestroyObject::AppendBodyTo`). Distinct
    /// from the update-object `OutOfRange` block (*we* left its range; the real client keeps those
    /// staged for cheap re-create): destroy frees the object outright (handler `0xaa → 0x4674a0`).
    DestroyObject {
        guid: u64,
    },
    /// `SMSG_TRIGGER_CINEMATIC` — play the cinematic sequence (body: the `CinematicSequences.dbc`
    /// id, `u32`). Sent on a character's first-ever login (the race intro,
    /// `HandleCharEnum`'s played-time-0 branch) and by GameObject type-13 cameras. While one runs
    /// unacked, vmangos anchors visibility to the flying camera (see [`super::opcode`]'s note) — the
    /// client must answer with `CMSG_COMPLETE_CINEMATIC` (empty body) when done (or to skip).
    TriggerCinematic {
        cinematic_id: u32,
    },
    MonsterMove {
        guid: u64,
        start: Vector3d,
        /// The server's per-move spline counter — echoed back in `CMSG_MOVE_SPLINE_DONE` when the
        /// spline drives our own player (Charge/knockback/taxi); ignored for a creature's walk.
        spline_id: u32,
        /// The **full** ordered path the unit traverses, in travel order `[start, …waypoints…, endpoint]`
        /// (absolute WoW coords) — every intermediate waypoint decoded, not just the endpoint. The client
        /// walks all of them at constant (arc-length) speed; a straight hop is the two-point degenerate.
        /// Empty for a `Stop`/degenerate move. (Ground paths pack the waypoints as `¼`-yd offsets from the
        /// endpoint; a flying `Mask_CatmullRom` path sends them absolute — decoded in `monster_move`.)
        path: Vec<Vector3d>,
        /// The dictated final facing (`moveType` 2/3/4), applied as a snap. [`MonsterMoveFacing::None`]
        /// for a plain move — the unit then faces its travel direction along the path.
        facing: MonsterMoveFacing,
        stop: bool,
        duration_ms: u32,
        /// The path is a 3-D **flight** (`SPLINE_FLAG_FLYING` = `Mask_CatmullRom`): keep the spline's own Z.
        /// Clear ⇒ a ground walk whose Z the client re-derives from the terrain (the app terrain-clamps it).
        flying: bool,
    },
    /// A relayed *player* movement packet (`MSG_MOVE_*`, see [`super::parse::is_movement_relay`]): the mover's
    /// authoritative pose plus its live CMovement `moveFlags`. The app snaps the entity to `position`
    /// and extrapolates from `flags` between packets. `opcode` is kept for logging/diagnostics.
    /// `fall_time` + `jump` carry the airborne clock + ballistic launch params when `JUMPING` is set, so
    /// a jump replays as one arc (decision 0053). `transport` is `Some` while `MOVEFLAG_ON_TRANSPORT` is
    /// set: the mover's local pose on the named transport (decision 0438 — an observed rider's boarding
    /// tail, composed through the transport's live matrix rather than read as world coordinates).
    PlayerMove {
        guid: u64,
        opcode: u16,
        flags: u32,
        position: Vector3d,
        orientation: f32,
        /// The swim pitch (radians, +up) from the `MovementInfo` tail — the mover's 3D travel
        /// angle while `MOVEFLAG_SWIMMING` is set (`0.0` otherwise), so observers integrate the
        /// vertical between packets the way the client's swim velocity basis does (`0x7c5880`).
        pitch: f32,
        /// The `MovementInfo` time word. vmangos stamps it with **its own ms clock at receipt**
        /// (`MovementInfo::stime`, `Object.cpp` `Write`), so every relayed move shares one server
        /// clock — the scheduling basis for the reference's deferred replay (decision 0601).
        time: u32,
        fall_time: u32,
        jump: Option<JumpInfo>,
        transport: Option<TransportPose>,
    },
    Teleport {
        guid: u64,
        counter: u32,
        position: Vector3d,
        orientation: f32,
    },
    NewWorld {
        map: u32,
        position: Vector3d,
        orientation: f32,
    },
    /// The far-teleport preamble (`SMSG_TRANSFER_PENDING`): the destination map and, when the
    /// player is riding a transport through the transfer, `(transportEntry, oldMapId)`. The
    /// block's presence decides how the follow-up [`Self::NewWorld`] coordinates read —
    /// boat-local vs world (decision 0455).
    TransferPending {
        map: u32,
        transport: Option<(u32, u32)>,
    },
    /// `SMSG_TRANSFER_ABORTED`: the announced transfer will not happen (map full, no instance).
    TransferAborted {
        reason: u8,
    },
    LoginVerifyWorld {
        map: u32,
        position: Vector3d,
        orientation: f32,
    },
    TimeSpeed {
        hours: u8,
        minutes: u8,
        /// Monotonic day count flattened from the packed date (`year·372 + month·31 + day`) —
        /// the celestial layer's moon-phase precession input.
        day_serial: u32,
        timescale: f32,
    },
    /// The player's hearthstone bind point (`SMSG_BINDPOINTUPDATE`, at login + on re-bind):
    /// position + map + the AreaTable id the `$z` token names.
    BindPoint {
        position: Vector3d,
        map: u32,
        area: u32,
    },
    /// `SMSG_SET_PROFICIENCY` (at login + on train): the subclass bitmask the player can equip
    /// for one item class (2 weapons / 4 armor) — the client's `0xc4d4a0[class]` store, the item
    /// tooltip's slot-line proficiency red (vmangos `Skill.h`: u8 itemClass + u32 mask).
    SetProficiency {
        item_class: u8,
        subclass_mask: u32,
    },
    /// `SMSG_INITIALIZE_FACTIONS` — the player's reputation store, sent once at login: `(flags,
    /// standing)` per reputation-list slot (vanilla sends all 64), indexed by `Faction.dbc`'s
    /// `reputationIndex`. The wire standing **excludes** the DBC base value — the client adds the
    /// race/class-matched `BaseRepValue` before ranking (hated → exalted).
    InitializeFactions {
        standings: Vec<(u8, i32)>,
    },
    /// `SMSG_SET_FACTION_STANDING` — mid-session standing deltas: `(reputationListId, standing)`
    /// per changed slot (vmangos `ReputationMgr::SendState` batches every `needSend` faction).
    /// Same standing convention as [`Self::InitializeFactions`] (DBC base excluded).
    SetFactionStanding {
        standings: Vec<(u32, i32)>,
    },
    /// `SMSG_NAME_QUERY_RESPONSE` — a player character's identity, answering `CMSG_NAME_QUERY`:
    /// guid, name, realm (1.12.1 carries it, empty on a single realm), race/gender/class as `u32`s
    /// (VERIFIED vmangos `NameQueryResponse::AppendBodyTo`). An unknown guid answers with an empty
    /// name (`SendNameQueryOpcodeFromDBCallBack` with no DB row).
    NameQueryResponse {
        guid: u64,
        name: String,
        race: u32,
        gender: u32,
        class: u32,
    },
    /// `SMSG_CREATURE_QUERY_RESPONSE` — a creature template's display data, answering
    /// `CMSG_CREATURE_QUERY`. We surface what the UI consumes ([`CreatureQueryInfo`]: name,
    /// subname, `CreatureType.dbc` id, the elite **rank**, the civilian flag); the rest of the
    /// tail (type flags/family/pet spells/display id/racial leader) is parsed for alignment and
    /// dropped until a consumer exists. A **miss** (unknown entry) is a lone `u32` of
    /// `entry | 0x8000_0000` → `None` (VERIFIED vmangos `HandleCreatureQueryOpcode`, both
    /// branches).
    CreatureQueryResponse {
        entry: u32,
        /// `Some` on a hit; `None` when the server flagged the entry unknown.
        info: Option<CreatureQueryInfo>,
    },
    /// `SMSG_PET_NAME_QUERY_RESPONSE` — a pet's display name, answering `CMSG_PET_NAME_QUERY`.
    /// Keyed by **pet number**, not a template entry (a pet guid carries none — see
    /// [`crate::guid::pet_number`]); that is also how the real client's own pet-name cache is keyed.
    /// The `nameTimestamp` tail is parsed for alignment and dropped: it exists to age out that
    /// on-disk cache, which we have no equivalent of.
    PetNameQueryResponse {
        pet_number: u32,
        name: String,
    },
    /// `SMSG_GAMEOBJECT_QUERY_RESPONSE` — a GameObject template's type/display/name/`data[24]` head,
    /// answering `CMSG_GAMEOBJECT_QUERY` (decision 0236's ask-once template lookup, the GO twin of
    /// [`Self::CreatureQueryResponse`]). `data[24]` is the raw type-specific tail (e.g. a chest's
    /// lockId) — parsed verbatim, resolved by whichever later consumer knows the GO's type. A miss
    /// (unknown entry) is a lone `u32` of `entry | 0x8000_0000` → `None`, same shape as the
    /// creature/item miss.
    GameObjectQueryResponse {
        entry: u32,
        info: Option<GameObjectQueryInfo>,
    },
    /// `SMSG_PLAY_SOUND` — a 2D sound-kit id, map/zone-wide (BG events, scripts). Payload
    /// VERIFIED vmangos `Map::PlayDirectSoundToMap`: one `u32 soundId` (SoundEntries).
    PlaySound {
        sound_id: u32,
    },
    /// `SMSG_PLAY_MUSIC` — a music kit for the music channel (scripts, e.g. Karazhan opera).
    /// Payload VERIFIED vmangos: one `u32 musicId` (SoundEntries).
    PlayMusic {
        music_id: u32,
    },
    /// `SMSG_PLAY_OBJECT_SOUND` — a 3D kit at a source object (fishing-bobber splash 3355,
    /// distance-dependent scripts). Payload VERIFIED vmangos `WorldObject::PlayDistanceSound`:
    /// `u32 soundId, u64 sourceGuid`.
    PlayObjectSound {
        sound_id: u32,
        guid: u64,
    },
    /// `SMSG_WEATHER` — the zone's weather state. Payload VERIFIED vmangos
    /// `Weather::SendWeatherForPlayersInZone` (1.12 shape): `u32 type, f32 grade,
    /// u32 soundId (a SoundEntries kit — 8533..8558, the rain/snow/sandstorm loops; 0 = clear),
    /// u8 instant`. `type`/`grade`/`instant` also feed the weather *visuals* when they exist.
    Weather {
        weather_type: u32,
        grade: f32,
        sound_id: u32,
        instant: bool,
    },
    /// `SMSG_TEXT_EMOTE` — a nearby unit performed a chat emote (`/wave`). Payload VERIFIED
    /// vmangos `EmoteChatBuilder`: `u64 guid, u32 textEmote (EmotesText.dbc), u32 emoteNum,
    /// u32 namelen, char name[namelen]` (the target's name; namelen ≥ 1). The name is parsed
    /// past and dropped (the chat line renders from it later; audio keys on ids).
    TextEmote {
        guid: u64,
        text_emote: u32,
    },
    /// `SMSG_EMOTE` — a unit plays an anim emote. Payload VERIFIED vmangos
    /// `Unit::HandleEmote`: `u32 emoteId (Emotes.dbc), u64 guid`.
    Emote {
        guid: u64,
        emote_id: u32,
    },
    /// `SMSG_ITEM_QUERY_SINGLE_RESPONSE` — an item template's display head, answering
    /// `CMSG_ITEM_QUERY_SINGLE` (layout in [`super::items`]); `None` info = the miss shape. `ItemInfo` now
    /// carries the full 1.12.1 item template (decision 0274 P1) — boxed so this one wide-but-rare
    /// variant doesn't inflate every other (tiny, hot) `ServerPacket` (`clippy::large_enum_variant`).
    ItemQueryResponse {
        entry: u32,
        info: Option<Box<ItemInfo>>,
    },
    /// `SMSG_MESSAGECHAT` — one inbound chat line (say/yell/system/NPC/channel; layout in
    /// [`super::chat`]). System lines (type `0x0A`) are how GM dot-commands answer.
    MessageChat(ChatMessage),
    /// `SMSG_CHANNEL_NOTIFY` — a channel join/leave/error/moderation notice (layout in
    /// [`super::channel::read_channel_notify`], decision 0288).
    ChannelNotify(ChannelNotify),
    /// `SMSG_CHANNEL_LIST` — a channel's member roster, answering our `CMSG_CHANNEL_LIST` (layout in
    /// [`super::channel::read_channel_list`]): `(guid, memberFlags)` per row.
    ChannelList {
        channel: String,
        flags: u8,
        members: Vec<(u64, u8)>,
    },
    /// `SMSG_CHAT_PLAYER_NOT_FOUND` — a whisper target wasn't found online (vmangos
    /// `Server/Packets/Chat.cpp:26-29`).
    ChatPlayerNotFound {
        name: String,
    },
    /// `SMSG_CHAT_WRONG_FACTION` — a cross-faction whisper was refused; empty body (vmangos
    /// `Server/Packets/Chat.cpp:16-18`).
    ChatWrongFaction,
    /// `SMSG_NOTIFICATION` — a server notice ("You do not know that language", …); one cstring
    /// (vmangos `WorldSession::SendNotification`, `Server/WorldSession.cpp:900-915`).
    Notification {
        text: String,
    },
    /// `SMSG_AREA_TRIGGER_MESSAGE` — why an area trigger refused us ("You must be at least level 58
    /// to enter…"); `u32 length` + one cstring (vmangos `WorldSession::SendAreaTriggerMessage`,
    /// `Server/WorldSession.cpp:882-898`).
    AreaTriggerMessage {
        text: String,
    },
    /// `SMSG_PLAYED_TIME` — answers our `CMSG_PLAYED_TIME` (`/played`): total played time + time
    /// since the last level-up, both in seconds (layout in [`super::chat::read_played_time`]).
    PlayedTime {
        total: u32,
        level: u32,
    },
    /// `MSG_RANDOM_ROLL` — the server's `/random` broadcast (layout in
    /// [`super::chat::read_random_roll`]): the rolled range, the result, and the roller's guid.
    RandomRoll {
        min: u32,
        max: u32,
        roll: u32,
        guid: u64,
    },
    /// `SMSG_INVENTORY_CHANGE_FAILURE` — the server refused an inventory operation
    /// (equip/store/split; layout in [`super::items::read_inventory_change_failure`]). `reason` is the
    /// `InventoryResult` code; `required_level` rides only on the level refusal (reason 1).
    InventoryChangeFailure {
        reason: u8,
        required_level: Option<u32>,
        item_guid: u64,
    },
    /// `SMSG_INITIAL_SPELLS` — the player's spell book + active cooldowns, once at login
    /// (layout in [`super::spellbook::read_initial_spells`]).
    InitialSpells {
        spell_ids: Vec<u16>,
        cooldowns: Vec<SpellCooldown>,
    },
    /// `SMSG_ACTION_BUTTONS` — the player's saved action bar, once at login: the occupied slots
    /// of the 120-slot wire array (layout in [`super::action_bar::read_action_buttons`]).
    ActionButtons {
        buttons: Vec<ActionButton>,
    },
    /// `SMSG_LEARNED_SPELL` — a spell was added to the book after login (trainer/quest/level-up;
    /// layout in [`super::spellbook::read_learned_spell`]). The first post-login spell-book mutation (0237).
    LearnedSpell {
        spell_id: u16,
    },
    /// `SMSG_SUPERCEDED_SPELL` — a rank-up replaced its predecessor in the book + action bar
    /// (layout in [`super::spellbook::read_superceded_spell`]).
    SupercededSpell {
        old_spell_id: u16,
        new_spell_id: u16,
    },
    /// `SMSG_CAST_RESULT` — the server's verdict on our `CMSG_CAST_SPELL`.
    CastResult {
        spell_id: u32,
        outcome: CastOutcome,
    },
    /// `SMSG_ATTACKSTART` — a unit began melee auto-attack (including our own echo).
    AttackStart {
        attacker: u64,
        victim: u64,
    },
    /// `SMSG_ATTACKSTOP` — a unit stopped melee auto-attack.
    AttackStop {
        attacker: u64,
        victim: u64,
    },
    /// `SMSG_ATTACKERSTATEUPDATE` — one completed melee swing (decision 0073: the attacker's swing
    /// animation trigger).
    AttackerState(AttackerState),
    /// `SMSG_AI_REACTION` — a creature flared aggro (2 HOSTILE) or a stealth pre-aggro alert
    /// (0 ALERT) at someone (layout in [`super::attack::read_ai_reaction`]; decision 0277).
    AiReaction {
        unit: u64,
        reaction: u32,
    },
    /// `SMSG_SPELL_START` — a non-triggered cast began, instants included (decision 0099 phase 1:
    /// the precast trigger; layout in [`super::spells::read_spell_start`]).
    SpellStart(SpellStart),
    /// `SMSG_SPELL_GO` — the cast launched: hit/miss lists + (for a ranged spell) the ammo block.
    /// The server schedules impact itself off `Spell.dbc` Speed — nothing about missile travel
    /// rides this packet (layout in [`super::spells::read_spell_go`]).
    SpellGo(SpellGo),
    /// `SMSG_SPELL_FAILED_OTHER` — an observed cast was interrupted/cancelled (vmangos
    /// `Spell::SendInterrupted`); our own cast's failure is [`Self::CastResult`] instead.
    SpellFailedOther {
        caster: u64,
        spell_id: u32,
    },
    /// `SMSG_SPELL_DELAYED` — pushback: our own cast took damage and the server extended its timer
    /// by `delay_ms` (vmangos `Spell::Delayed`). The cast bar slides its window out by the same, so
    /// a hit pushes the bar back instead of letting it finish early (decision 0256).
    SpellDelayed {
        caster: u64,
        delay_ms: u32,
    },
    /// `SMSG_CANCEL_AUTO_REPEAT` — stop our own ranged auto-repeat visual; self-only, empty body
    /// (vmangos `WorldPackets::Misc::CancelAutoRepeat`). Consumed by the local cancel funnel
    /// (`net/apply/spells.rs::cancel_auto_repeat`, decision 0406) — dormant against vmangos,
    /// which never sends it (zero send sites; the cast-result fail is the live stop path).
    CancelAutoRepeat,
    /// `SMSG_SPELL_COOLDOWN` — server-pushed cooldowns for the player or pet, by caster guid
    /// (layout + the `cooldown_ms == 0` "use Spell.dbc" fork in [`super::spellbook::read_spell_cooldown`];
    /// decision 0137 phase 4).
    SpellCooldownList {
        caster: u64,
        /// `(spell_id, cooldown_ms)` — `0` ms = the spell's own DBC recovery/category times.
        cooldowns: Vec<(u32, u32)>,
    },
    /// `SMSG_ITEM_COOLDOWN` — put an item (by instance guid) on the client's fixed 30 s use
    /// cooldown for its on-use spell (layout in [`super::spellbook::read_item_cooldown`]).
    ItemCooldown {
        item_guid: u64,
        spell_id: u32,
    },
    /// `SMSG_COOLDOWN_EVENT` — start an on-hold (`SPELL_ATTR_COOLDOWN_ON_EVENT`) cooldown's
    /// parked timers now (layout in [`super::spellbook::read_cooldown_event`]).
    CooldownEvent {
        spell_id: u32,
        caster: u64,
    },
    /// `SMSG_CLEAR_COOLDOWN` — remove one spell's cooldown record outright (same body shape as
    /// [`Self::CooldownEvent`]).
    ClearCooldown {
        spell_id: u32,
        caster: u64,
    },
    /// `SMSG_COOLDOWN_CHEAT` — wipe every cooldown for the named unit (the GM reset).
    CooldownCheat {
        caster: u64,
    },
    /// `MSG_CHANNEL_START` — our own channeled cast opened (self-only; no guid on the wire). The
    /// cast bar's channel-open edge (decision 0137).
    ChannelStart {
        spell_id: u32,
        duration_ms: u32,
    },
    /// `MSG_CHANNEL_UPDATE` — our own channel's time left; `0` = over (natural end and interrupt
    /// alike). Self-only (decision 0137).
    ChannelUpdate {
        remaining_ms: u32,
    },
    /// `SMSG_UPDATE_AURA_DURATION` — how long one of **our own** auras has left, keyed by its
    /// `UNIT_FIELD_AURA` slot. Self-only, never sent for a permanent aura, and it arrives *before*
    /// the descriptor delta that says which spell occupies the slot — so a consumer buffers it by
    /// slot and joins on `(slot, spell_id)` (decision 0255).
    UpdateAuraDuration {
        slot: u8,
        remaining_ms: u32,
    },
    /// `SMSG_PLAY_SPELL_VISUAL` — play a spell-visual kit on a unit outside the normal cast
    /// sequence, at the client's hardcoded stage 0 (the eat/drink kit cadence, mid-channel kit
    /// swaps — decision 0280).
    PlaySpellVisual {
        unit: u64,
        kit_id: u32,
    },
    /// `SMSG_SPELLNONMELEEDAMAGELOG` — non-melee (spell) damage dealt (decision 0137 phase 2's
    /// floating-combat-text data feed; layout in [`super::combat_log::read_spell_damage_log`]).
    SpellDamageLog(SpellDamageLog),
    /// `SMSG_PERIODICAURALOG` — periodic (DoT/HoT/regen) aura ticks (decision 0137 phase 2; layout
    /// in [`super::combat_log::read_periodic_aura_log`]).
    PeriodicAuraLog(PeriodicAuraLog),
    /// `SMSG_SPELLHEALLOG` — a direct heal landing (decision 0578's center-combat-text feed;
    /// layout in [`super::combat_log::read_spell_heal_log`]).
    SpellHealLog(SpellHealLog),
    /// `SMSG_SPELLENERGIZELOG` — an instant power gain (decision 0578; layout in
    /// [`super::combat_log::read_spell_energize_log`]).
    SpellEnergizeLog(SpellEnergizeLog),
    /// `SMSG_SPELLDAMAGESHIELD` — a damage-shield (Thorns-style) return hit (decision 0137 phase 2;
    /// layout in [`super::combat_log::read_damage_shield`]).
    DamageShield(DamageShield),
    /// `SMSG_ENVIRONMENTALDAMAGELOG` — environmental damage taken (fall/drowning/…; layout in
    /// [`super::combat_log::read_environmental_damage_log`]).
    EnvironmentalDamageLog(EnvironmentalDamageLog),
    /// `SMSG_SPELLLOGMISS` — a spell cast's per-target miss list (decision 0137 phase 2; layout in
    /// [`super::combat_log::read_spell_log_miss`]).
    SpellLogMiss(SpellLogMiss),
    /// `SMSG_LOG_XPGAIN` — an XP award, kill or non-kill (decision 0137 phase 2; layout in
    /// [`super::progression::read_xp_gain`]).
    XpGain(XpGain),
    /// `SMSG_EXPLORATION_EXPERIENCE` — a first visit to an area: the discovered area id + its XP
    /// award (decision 0828; layout in [`super::progression::read_exploration_xp`]).
    ExplorationXp(ExplorationXp),
    /// `SMSG_LEVELUP_INFO` — our own ding, self-addressed only (decision 0304; layout in
    /// [`super::progression::read_level_up_info`]).
    LevelUp(LevelUpInfo),
    /// `SMSG_QUESTGIVER_STATUS` — the questgiver dialog status for one NPC (`!`/`?` marker), a
    /// [`super::quest::dialog_status`] value. A world-marker concern (out of the panel slice, decision
    /// 0088): parsed + surfaced per guid now, rendered later.
    QuestGiverStatus {
        npc: u64,
        status: u32,
    },
    /// `SMSG_QUESTGIVER_QUEST_LIST` — the greeting panel: an NPC's offered/active quest rows
    /// (layout in [`super::quest::read_questgiver_quest_list`]).
    QuestGiverQuestList(QuestGiverList),
    /// `SMSG_QUESTGIVER_QUEST_DETAILS` — the accept panel: full quest text + rewards on offer
    /// (layout in [`super::quest::read_questgiver_quest_details`]).
    QuestGiverDetails(QuestDetails),
    /// `SMSG_QUESTGIVER_REQUEST_ITEMS` — the progress panel: the "bring me these" text + required
    /// items and the completability flag (layout in [`super::quest::read_questgiver_request_items`]).
    QuestGiverRequestItems(QuestRequestItems),
    /// `SMSG_QUESTGIVER_OFFER_REWARD` — the reward panel: turn-in text + rewards to grant (layout in
    /// [`super::quest::read_questgiver_offer_reward`]).
    QuestGiverOfferReward(QuestOfferReward),
    /// `SMSG_QUESTGIVER_QUEST_COMPLETE` — the turn-in result: XP/money granted + fixed items
    /// (layout in [`super::quest::read_questgiver_quest_complete`]).
    QuestGiverComplete(QuestComplete),
    /// `SMSG_QUESTGIVER_QUEST_INVALID` (vmangos `Quest.cpp:126`) — the accept/query attempt was
    /// rejected before a details/failed panel could be built; `msg` is a client message code.
    QuestGiverInvalid {
        msg: u32,
    },
    /// `SMSG_QUESTGIVER_QUEST_FAILED` (vmangos `Quest.cpp:110`) — a `CMSG_QUESTGIVER_ACCEPT_QUEST`
    /// (or an in-progress requirement) failed; `reason` is a client `QuestFailedReason` code.
    QuestGiverFailed {
        quest_id: u32,
        reason: u32,
    },
    /// `SMSG_QUEST_QUERY_RESPONSE` — the full quest template, answering `CMSG_QUEST_QUERY`; feeds
    /// the quest-log detail view (layout + wire-trap notes on [`QuestTemplate`]). Boxed: at 400+
    /// bytes (four fixed-count arrays plus five strings) it would otherwise dwarf every other
    /// `ServerPacket` variant and bloat the whole enum.
    QuestQueryResponse(Box<QuestTemplate>),
    /// `SMSG_QUESTLOG_FULL` — the log has no free slot for a new quest; empty body (vmangos
    /// `Quest.cpp:87`).
    QuestLogFull,
    /// `SMSG_QUESTUPDATE_COMPLETE` — every objective on this quest is now complete (vmangos
    /// `Quest.cpp:91`); the log slot's state byte gets `QUEST_STATE_COMPLETE`.
    QuestUpdateComplete {
        quest_id: u32,
    },
    /// `SMSG_QUESTUPDATE_FAILED` — the quest failed outright (vmangos `Quest.cpp:116`).
    QuestUpdateFailed {
        quest_id: u32,
    },
    /// `SMSG_QUESTUPDATE_FAILEDTIMER` — a timed quest's clock ran out (vmangos `Quest.cpp:121`).
    QuestUpdateFailedTimer {
        quest_id: u32,
    },
    /// `SMSG_QUESTUPDATE_ADD_KILL` — a kill/use toast for a creature-or-gameobject objective
    /// (vmangos `Quest.cpp:144`); `entry` carries the same raw creature/GO encoding as
    /// [`super::quest::QuestObjective::creature_or_go`], and mirrors (doesn't replace) the durable
    /// `PLAYER_QUEST_LOG` counter field.
    QuestUpdateAddKill {
        quest_id: u32,
        entry: u32,
        count: u32,
        required: u32,
        guid: u64,
    },
    /// `SMSG_QUESTUPDATE_ADD_ITEM` — an item-collection toast (vmangos `Quest.cpp:138`).
    QuestUpdateAddItem {
        item_id: u32,
        count: u32,
    },
    /// `SMSG_GOSSIP_MESSAGE` — a gossip menu opened on an NPC, answering our `CMSG_GOSSIP_HELLO` (or
    /// riding a `CMSG_GOSSIP_SELECT_OPTION` reply that re-opens the menu). Payload VERIFIED vmangos
    /// `GossipDef.cpp:180-225` (the 1.12 shape — no box-money field, that's TBC+); `text_id` drives a
    /// follow-up `CMSG_NPC_TEXT_QUERY` for the greeting body. `quests` carries the quest-option rows
    /// riding the same packet (wired to the questgiver panels — decision 0088).
    GossipMessage {
        npc: u64,
        text_id: u32,
        options: Vec<GossipOption>,
        quests: Vec<QuestOption>,
    },
    /// `SMSG_GOSSIP_COMPLETE` — the gossip window closes (vmangos `Npc.cpp:90`); an empty body.
    GossipComplete,
    /// `SMSG_NPC_TEXT_UPDATE` — answers `CMSG_NPC_TEXT_QUERY`: always 8 weighted text blocks
    /// (vmangos `GossipDef.cpp:298-369`), carried here **undecided**. Which line greets you needs
    /// the NPC's gender and a die roll, so it is drawn when the frame opens, not when the packet
    /// lands — [`super::gossip::select_greeting`].
    NpcText {
        text_id: u32,
        blocks: Vec<super::gossip::NpcTextBlock>,
    },
    /// `SMSG_LIST_INVENTORY` — a vendor's stock, answering `CMSG_LIST_INVENTORY` (vmangos
    /// `ItemHandler.cpp:741-810`). Empty stock sends `count = 0` plus a trailing error byte the
    /// parser leaves unconsumed.
    VendorList {
        vendor: u64,
        items: Vec<VendorItem>,
    },
    /// `SMSG_BUY_ITEM` — the vendor stock update after a purchase (vmangos `Item.cpp:190-196`); the
    /// purchased item itself arrives via `UPDATE_OBJECT` + `SMSG_ITEM_PUSH_RESULT`, already handled.
    BuyItem {
        vendor: u64,
        slot: u32,
        new_count: u32,
        purchase_count: u32,
    },
    /// `SMSG_SELL_ITEM` — the **error** path only (vmangos `Item.cpp:183-188`, `SendSellError`
    /// `Player.cpp:11723`); a successful sell sends nothing, visible only as the item vanishing +
    /// coinage rising via `UPDATE_OBJECT`. `reason` is a [`super::vendor::sell_result`] code.
    SellItemResult {
        vendor: u64,
        item_guid: u64,
        reason: u8,
    },
    /// `SMSG_BUY_FAILED` — the server refused a purchase (vmangos `Item.h:277`). `reason` is a
    /// [`super::vendor::buy_result`] code.
    BuyFailed {
        vendor: u64,
        item_entry: u32,
        reason: u8,
    },
    /// `SMSG_SHOW_BANK` — the bank window opens, answering our `CMSG_BANKER_ACTIVATE` (vmangos
    /// `Npc.cpp:94`) — or arriving unprompted for the `GOSSIP_OPTION_BANKER` gossip option
    /// (`SendShowBank`, `Player.cpp:12426`); the handler must not assume it always follows our own
    /// activate. The vault itself is already streamed via the ordinary player descriptor
    /// (decision 0604) — this only opens the window.
    ShowBank {
        banker: u64,
    },
    /// `SMSG_BUY_BANK_SLOT_RESULT` — a bank-slot purchase was refused (vmangos `Item.cpp:137-140`).
    /// `result` is a [`super::bank::bank_slot_result`] code; a *successful* buy sends no packet at
    /// all, visible only as the `PLAYER_BYTES_2` bank-bag-count byte advancing + the coinage drop
    /// (decision 0604).
    BuyBankSlotResult {
        result: u32,
    },
    /// `SMSG_TRAINER_LIST` — a class/profession trainer's service list, reached through the gossip
    /// trainer option (layout in [`super::trainer::read_trainer_list`], decision 0237). `trainer_type` is
    /// the window-framing kind (0 class · 1 mount · 2 tradeskill · 3 pet); `title` is the greeting.
    TrainerList {
        trainer: u64,
        trainer_type: u32,
        services: Vec<TrainerSpell>,
        title: String,
    },
    /// `SMSG_TRAINER_BUY_SUCCEEDED` — the trainer taught the service (vmangos `SendTrainingSuccess`);
    /// confirmation + sound only — the learned spell arrives via `SMSG_LEARNED_SPELL`, and the
    /// green→gray repaint needs a `CMSG_TRAINER_LIST` re-request.
    TrainerBuySucceeded {
        trainer: u64,
        spell_id: u32,
    },
    /// `SMSG_TRAINER_BUY_FAILED` — the trainer refused (vmangos `SendTrainingFailure`). `error` is a
    /// [`super::trainer::train_fail`] code (0 unavailable · 1 not-enough-money · 2 not-enough-skill).
    TrainerBuyFailed {
        trainer: u64,
        spell_id: u32,
        error: u32,
    },
    /// `SMSG_LOOT_RESPONSE`, normal shape — a loot window opened, answering `CMSG_LOOT` (layout in
    /// [`super::loot::read_loot_response`]). `loot_type` is a [`super::loot::loot_type`] code; `items` includes
    /// any quest-item rows riding the same list.
    LootResponse {
        guid: u64,
        loot_type: u8,
        gold: u32,
        items: Vec<LootItem>,
    },
    /// `SMSG_LOOT_RESPONSE`, error shape — the server refused to open the loot window (didn't
    /// kill it, too far, not standing, …). `error` is a [`super::loot::loot_error`] code.
    LootError {
        guid: u64,
        error: u8,
    },
    /// `SMSG_LOOT_RELEASE_RESPONSE` — the loot window closes, answering `CMSG_LOOT_RELEASE`.
    /// `result` is always `1` (vmangos never sends another value).
    LootReleaseResponse {
        guid: u64,
        result: u8,
    },
    /// `SMSG_LOOT_REMOVED` — one loot-window row was taken, by anyone.
    LootRemoved {
        slot: u8,
    },
    /// `SMSG_LOOT_MONEY_NOTIFY` — our share of the loot's coin pile, answering `CMSG_LOOT_MONEY`.
    LootMoneyNotify {
        amount: u32,
    },
    /// `SMSG_LOOT_CLEAR_MONEY` — the coin line disappears for every current looter; empty body.
    LootClearMoney,
    /// `SMSG_LOOT_START_ROLL` — a group roll opened on one drop (layout in [`LootStartRoll`]);
    /// drives a `GroupLootFrame`.
    LootStartRoll(LootStartRoll),
    /// `SMSG_LOOT_ROLL` — one roller's vote or dice result (layout, and the overloaded
    /// `(roll_number, roll_type)` pair, in [`LootRoll`]).
    LootRoll(LootRoll),
    /// `SMSG_LOOT_ROLL_WON` — a group roll resolved (layout in [`LootRollWon`]).
    LootRollWon(LootRollWon),
    /// `SMSG_LOOT_ALL_PASSED` — everyone passed; the roll closes and the item returns to the
    /// corpse for ordinary looting (layout in [`LootAllPassed`]).
    LootAllPassed(LootAllPassed),
    /// `SMSG_ITEM_PUSH_RESULT` — an item landed in our bags (looted or received from an NPC);
    /// drives the "You receive loot: …" chat line (layout in [`ItemPushResult`]).
    ItemPushResult(ItemPushResult),
    /// `MSG_CORPSE_QUERY`'s answer — where our corpse is (layout + the two-map split in
    /// [`CorpseLocation`]); also pushed unprompted as not-found at bones-conversion.
    CorpseQuery(CorpseLocation),
    /// `SMSG_CORPSE_RECLAIM_DELAY` — ms until the corpse can be reclaimed (sent at release +
    /// at login-while-dead).
    CorpseReclaimDelay {
        delay_ms: u32,
    },
    /// `SMSG_DURABILITY_DAMAGE_DEATH` — the 10% natural-death durability loss happened (empty
    /// body); the red error-line cue.
    DurabilityDamageDeath,
    /// `SMSG_RESURRECT_REQUEST` — a resurrection offer (layout in [`ResurrectRequestBody`]).
    ResurrectRequest(ResurrectRequestBody),
    /// `SMSG_SPIRIT_HEALER_CONFIRM` — the spirit healer asks for the XP_LOSS-style confirm;
    /// carries the NPC's guid (the eventual `CMSG_SPIRIT_HEALER_ACTIVATE` target).
    SpiritHealerConfirm {
        npc: u64,
    },
    /// **A granted mover mode changed** — the ack'd movement-mode family (decision 0866): root,
    /// water-walk, feather-fall or hover, granted or revoked on our mover. `apply` is the direction.
    /// Must be acked with the echoed `counter` ([`MoveMode::ack_opcode`]) or the server never applies
    /// the change and observers never see it.
    MoveMode {
        guid: u64,
        counter: u32,
        mode: MoveMode,
        apply: bool,
    },
    /// `SMSG_LOGOUT_COMPLETE` — the world session is over; we are back at character select.
    LogoutComplete,
    /// `SMSG_LOGOUT_RESPONSE` — the server's answer to `CMSG_LOGOUT_REQUEST`, and the ONLY thing
    /// that decides whether logging out is instant or a countdown. Body `{u32 reason, u8 instant}`
    /// (vmangos `WorldPackets::Misc::LogoutResponse`; the real client reads the same pair — wow-re
    /// `system/net/ledger.tsv` 0x5b4630 `Handle(0x4c) — {u32, u8}`).
    ///
    /// `reason` non-zero is a REFUSAL and no logout starts (vmangos `HandleLogoutRequestOpcode`:
    /// 1 in combat, 3 jumping/falling, 2 GM-frozen). `instant` is set when the server logs you out
    /// on the spot — resting (an inn or a city), on a taxi, or a GM account — in which case
    /// `LogoutComplete` follows immediately; otherwise a 20-second server-side timer runs, which is
    /// what the client's CAMP/QUIT dialog counts down.
    LogoutResponse {
        reason: u32,
        instant: bool,
    },
    /// `SMSG_LOGOUT_CANCEL_ACK` — the server dropped a pending logout at our `CMSG_LOGOUT_CANCEL`.
    /// Empty body; it is what takes the countdown dialog back down (`LOGOUT_CANCEL`).
    LogoutCancelAck,
    /// `SMSG_PONG` — the echo of our `CMSG_PING`'s sequence number (the keepalive's return leg;
    /// the io layer matches it against the ping clock to measure the round-trip).
    Pong {
        sequence: u32,
    },
    /// A `SMSG_FORCE_*_SPEED_CHANGE` — the server changed one of our mover's six speeds (aura,
    /// mount, GM `.modify speed`) and awaits the matching `CMSG_FORCE_*_SPEED_CHANGE_ACK` carrying
    /// this `counter` + the exact `speed` back (see the opcode block's protocol note). `guid` is
    /// the mover (packed on the wire); `speed` is flat yd/s (rad/s for the turn rate).
    ForceSpeedChange {
        guid: u64,
        kind: SpeedKind,
        counter: u32,
        speed: f32,
    },
    /// `SMSG_GROUP_INVITE` — someone invited us to their group (layout in
    /// [`super::group::read_group_invite`]).
    GroupInvite {
        inviter: String,
    },
    /// `SMSG_GROUP_DECLINE` — an invite we sent was declined.
    GroupDecline {
        name: String,
    },
    /// `SMSG_GROUP_UNINVITE` — we were removed from our group (kicked or left); empty body.
    GroupUninvited,
    /// `SMSG_GROUP_SET_LEADER` — the group's leader changed.
    GroupLeaderChanged {
        name: String,
    },
    /// `SMSG_GROUP_DESTROYED` — the group disbanded outright; empty body.
    GroupDestroyed,
    /// `SMSG_GROUP_LIST` — the full roster, sent on every membership change (layout in
    /// [`super::group::read_group_list`]). `members` excludes the recipient's own row (`own_flags`
    /// carries theirs); `loot` is `None` for the empty "you left" shape and whenever the list has
    /// no other members.
    GroupList {
        group_type: u8,
        own_flags: u8,
        members: Vec<GroupMemberEntry>,
        leader: u64,
        loot: Option<GroupLootInfo>,
    },
    /// `SMSG_PARTY_COMMAND_RESULT` — the server's verdict on a group command we issued
    /// (invite/leave): `operation` a [`super::group::party_operation`] code, `result` a
    /// [`super::group::party_result`] code.
    PartyCommandResult {
        operation: u32,
        member: String,
        result: u32,
    },
    /// `SMSG_PARTY_MEMBER_STATS` / `_FULL` — a party/raid member's live stats for the frame
    /// (layout in [`super::group::read_party_member_stats`]); `full` distinguishes the delta form
    /// from the ask-once full form (our own [`super::group::request_party_member_stats`], or the
    /// offline-miss reply). Boxed: [`PartyMemberStatsInfo`]'s ~20 optional fields would otherwise
    /// dwarf every other variant here.
    PartyMemberStats {
        guid: u64,
        full: bool,
        info: Box<PartyMemberStatsInfo>,
    },
    /// `MSG_MINIMAP_PING` — someone pinged the minimap for the group.
    MinimapPing {
        guid: u64,
        x: f32,
        y: f32,
    },
    /// `MSG_RAID_TARGET_UPDATE`, delta shape — one raid-target icon changed; `guid == 0` clears it.
    RaidTargetSet {
        icon: u8,
        guid: u64,
    },
    /// `MSG_RAID_TARGET_UPDATE`, full-list shape — the whole current icon set (only currently-set
    /// icons are present).
    RaidTargetList {
        entries: Vec<(u8, u64)>,
    },
    /// `MSG_RAID_READY_CHECK`, empty body — the raid leader started a ready check.
    ReadyCheckRequest,
    /// `MSG_RAID_READY_CHECK`, non-empty body — one member's answer, forwarded to the leader only.
    ReadyCheckAnswer {
        guid: u64,
        ready: u8,
    },
    /// `SMSG_DUEL_REQUESTED` — a duel challenge. Sent to challenger and challenged alike; which
    /// one we are is `challenger == our guid` (decision 0633).
    DuelRequested {
        arbiter: u64,
        challenger: u64,
    },
    /// `SMSG_DUEL_OUTOFBOUNDS` — we left the 75 yd bubble around the duel flag; 10 s to return.
    DuelOutOfBounds,
    /// `SMSG_DUEL_INBOUNDS` — we came back inside (70 yd, the hysteresis edge).
    DuelInBounds,
    /// `SMSG_DUEL_COMPLETE` — the duel is over. `started` is false only when it ended before it
    /// began (declined/cancelled), which is what earns the "Duel cancelled." line.
    DuelComplete {
        started: bool,
    },
    /// `SMSG_DUEL_WINNER` — the outcome line, broadcast to everyone nearby.
    DuelWinner {
        fled: bool,
        winner: String,
        loser: String,
    },
    /// `SMSG_DUEL_COUNTDOWN` — start the "Duel starting: N" tick. Already converted from the
    /// wire's milliseconds to whole seconds ([`super::duel::read_duel_countdown`]).
    DuelCountdown {
        seconds: u32,
    },
    /// `SMSG_START_MIRROR_TIMER` — start or wholly re-state one mirror timer (the breath /
    /// fatigue bars, decision 0874). The family has no update opcode: every change re-sends this.
    MirrorTimerStart(MirrorTimerStart),
    /// `SMSG_PAUSE_MIRROR_TIMER` — freeze/unfreeze a running timer. vmangos never sends it.
    MirrorTimerPause {
        kind: u32,
        paused: bool,
    },
    /// `SMSG_STOP_MIRROR_TIMER` — that timer is over; hide its bar.
    MirrorTimerStop {
        kind: u32,
    },
    /// `SMSG_FRIEND_LIST` — the whole friend list, guids + presence (decision 0668). Pushed
    /// unasked at login and on every `CMSG_FRIEND_LIST`; always complete, never a delta.
    FriendList {
        friends: Vec<FriendEntry>,
    },
    /// `SMSG_IGNORE_LIST` — the whole ignore list: guids and nothing else.
    IgnoreList {
        guids: Vec<u64>,
    },
    /// `SMSG_FRIEND_STATUS` — one result about one player: the ack for an add/remove, or the
    /// login/logout broadcast every friend-lister receives.
    FriendStatus(FriendStatusUpdate),
    /// `SMSG_WHO` — the `/who` answer: up to 49 rows plus the true match total.
    WhoResults(WhoResults),
    /// A `SMSG_SPLINE_SET_*_SPEED` — a speed change on a unit we don't control (a creature, or a
    /// player mid-spline): `[packed guid][f32 speed]`, no counter, no ack (decision 0441 — how an
    /// observed unit's mounted speed reaches us).
    SplineSpeedChange {
        guid: u64,
        kind: SpeedKind,
        speed: f32,
    },
    /// A `MSG_MOVE_SET_*_SPEED` — a freely-moving *player's* speed change (the common observer
    /// case: someone mounts up nearby): `[packed guid][MovementInfo][f32 speed]` — a speed change
    /// AND a fresh authoritative pose in one packet (decision 0441).
    MoveSetSpeed {
        guid: u64,
        kind: SpeedKind,
        flags: u32,
        position: Vector3d,
        orientation: f32,
        /// The swim pitch from the `MovementInfo` tail (see [`Self::PlayerMove::pitch`]).
        pitch: f32,
        /// The `MovementInfo` time word (see [`Self::PlayerMove::time`]).
        time: u32,
        fall_time: u32,
        jump: Option<JumpInfo>,
        /// The rider's platform frame when ON_TRANSPORT is set (a speed change aboard a boat
        /// carries the same MovementInfo as any relay — decision 0438's frame law applies).
        transport: Option<TransportPose>,
        speed: f32,
    },
    /// `SMSG_MOUNTRESULT` / `SMSG_DISMOUNTRESULT` — the server's answer to a (dis)mount attempt
    /// (`mount` distinguishes them): one raw result code (vmangos `UnitMountResult` /
    /// `UnitDismountResult`; OK = 10 / 3). Decision 0441; error lines are a P2 trimming.
    MountResult {
        mount: bool,
        code: u32,
    },
    /// `SMSG_MOUNTSPECIAL_ANIM` — a nearby mounted player hit the flourish (one raw u64 guid;
    /// VERIFIED vmangos `HandleMountSpecialAnimOpcode`, `MovementHandler.cpp:969-970`). The
    /// sender is excluded from the broadcast, so this only ever names someone else's mount.
    MountSpecialAnim {
        guid: u64,
    },
    /// `SMSG_SHOWTAXINODES` — the taxi map (vmangos `SendTaxiMenu`, `TaxiHandler.cpp:82-96`;
    /// layout in [`super::taxi::read_show_taxi_nodes`], decision 0484). `window` is the
    /// window-framing constant vmangos always writes `1`; `flightmaster` names the NPC the menu
    /// opened on (its taxi mount resolves through the `benilla-formats` `TaxiNodes` catalog);
    /// `nearest_node` is the node the flight master itself sits at; `known` is the full
    /// known-node bitmask.
    ShowTaxiNodes {
        window: u32,
        flightmaster: u64,
        nearest_node: u32,
        known: TaxiMask,
    },
    /// `SMSG_TAXINODE_STATUS` — answers `CMSG_TAXINODE_STATUS_QUERY`, and also rides a
    /// first-visit "learn" (vmangos `SendLearnNewTaxiNode`, `TaxiHandler.cpp:117-138`): `guid`
    /// names the flight master asked about (plain, not packed); `known` is whether the nearest
    /// node to it is in our taxi mask.
    TaxiNodeStatus {
        guid: u64,
        known: bool,
    },
    /// `SMSG_ACTIVATETAXIREPLY` — answers `CMSG_ACTIVATETAXI`/`CMSG_ACTIVATETAXIEXPRESS`: `code`
    /// is a [`super::taxi_reply`] value (`0` OK, everything else a refusal — no flight starts).
    ActivateTaxiReply {
        code: u32,
    },
    /// `SMSG_NEW_TAXI_PATH` — empty body. Rides a first-visit "learn" alongside
    /// [`Self::TaxiNodeStatus`] (vmangos `SendLearnNewTaxiNode`); the client has nothing to read
    /// from it, but the wire is modelled rather than silently dropped.
    NewTaxiPath,
    /// `SMSG_MAIL_LIST_RESULT` — the inbox page, answering `CMSG_GET_MAIL_LIST` (layout in
    /// [`super::mail::read_mail_list_result`], decision 0544 P0).
    MailList {
        mails: Vec<MailListEntry>,
    },
    /// `SMSG_SEND_MAIL_RESULT` — the verdict on a mail action (send/take-money/take-item/return/
    /// delete), keyed by `action` ([`super::mail::mail_action`]) and `error`
    /// ([`super::mail::mail_error`]); `equip_error`/`item` are the two mutually-exclusive
    /// conditional tails (layout in [`super::mail::read_send_mail_result`]).
    SendMailResult {
        mail_id: u32,
        action: u32,
        error: u32,
        equip_error: Option<u32>,
        item: Option<(u32, u32)>,
    },
    /// `SMSG_ITEM_TEXT_QUERY_RESPONSE` — a letter's body text, answering `CMSG_ITEM_TEXT_QUERY`
    /// (the ask-once fetch a mail's nonzero `item_text_id` triggers).
    ItemTextQueryResponse {
        text_id: u32,
        text: String,
    },
    /// `SMSG_RECEIVED_MAIL` — a mail arrived (instant for text-only, on the delivery timer's
    /// expiry otherwise); the wire's lone `u32` is always 0 on vmangos, so nothing rides this.
    ReceivedMail,
    /// `MSG_QUERY_NEXT_MAIL_TIME`'s reply (same opcode as our empty-body request): `0.0` = unread
    /// mail waiting, `-86400.0` = none.
    NextMailTime {
        seconds: f32,
    },
    /// `SMSG_TRADE_STATUS` — one pulse of the trade state machine (open/accept/cancel/complete/
    /// the refusal reasons); the tail-carrying statuses hold their payload in [`TradeStatus`]
    /// (layout in [`super::trade::read_trade_status`], decision 0592 P0).
    TradeStatus {
        status: TradeStatus,
    },
    /// `SMSG_TRADE_STATUS_EXTENDED` — the item/gold snapshot for one window side, pushed whenever
    /// that side's offer changes (layout in [`super::trade::read_trade_status_extended`]). Boxed:
    /// the seven-slot item array is ~460 bytes, and it would otherwise bloat every `ServerPacket`
    /// (the same reason `QuestQueryResponse`/`PartyMemberStats` box their payloads).
    TradeStatusExtended {
        state: Box<TradeStatusExtended>,
    },
    /// `SMSG_INIT_WORLD_STATES` — the whole world-state table for a zone, pushed on login and on
    /// every zone change (layout in [`super::world_state::read_init_world_states`]).
    InitWorldStates(InitWorldStates),
    /// `SMSG_UPDATE_WORLD_STATE` — one `(id, value)` write into that table.
    UpdateWorldState {
        id: u32,
        value: u32,
    },
    Other {
        opcode: u16,
    },
}

impl ServerPacket {
    /// A short human name for logging/tallying (the opcode in hex for unmodelled packets).
    pub fn name(&self) -> String {
        match self {
            ServerPacket::AuthChallenge { .. } => "SMSG_AUTH_CHALLENGE".into(),
            ServerPacket::AuthResponse { .. } => "SMSG_AUTH_RESPONSE".into(),
            ServerPacket::CharEnum { .. } => "SMSG_CHAR_ENUM".into(),
            ServerPacket::CharCreate { .. } => "SMSG_CHAR_CREATE".into(),
            ServerPacket::CharDelete { .. } => "SMSG_CHAR_DELETE".into(),
            ServerPacket::UpdateObject { .. } => "SMSG_UPDATE_OBJECT".into(),
            ServerPacket::CompressedMoves { .. } => "SMSG_COMPRESSED_MOVES".into(),
            ServerPacket::DestroyObject { .. } => "SMSG_DESTROY_OBJECT".into(),
            ServerPacket::TriggerCinematic { .. } => "SMSG_TRIGGER_CINEMATIC".into(),
            ServerPacket::MonsterMove { .. } => "SMSG_MONSTER_MOVE".into(),
            ServerPacket::PlayerMove { opcode, .. } => format!("MSG_MOVE relay ({opcode:#06x})"),
            ServerPacket::Teleport { .. } => "MSG_MOVE_TELEPORT_ACK".into(),
            ServerPacket::NewWorld { .. } => "SMSG_NEW_WORLD".into(),
            ServerPacket::TransferPending { .. } => "SMSG_TRANSFER_PENDING".into(),
            ServerPacket::TransferAborted { .. } => "SMSG_TRANSFER_ABORTED".into(),
            ServerPacket::LoginVerifyWorld { .. } => "SMSG_LOGIN_VERIFY_WORLD".into(),
            ServerPacket::TimeSpeed { .. } => "SMSG_LOGIN_SETTIMESPEED".into(),
            ServerPacket::BindPoint { .. } => "SMSG_BINDPOINTUPDATE".into(),
            ServerPacket::SetProficiency { .. } => "SMSG_SET_PROFICIENCY".into(),
            ServerPacket::InitializeFactions { .. } => "SMSG_INITIALIZE_FACTIONS".into(),
            ServerPacket::SetFactionStanding { .. } => "SMSG_SET_FACTION_STANDING".into(),
            ServerPacket::NameQueryResponse { .. } => "SMSG_NAME_QUERY_RESPONSE".into(),
            ServerPacket::CreatureQueryResponse { .. } => "SMSG_CREATURE_QUERY_RESPONSE".into(),
            ServerPacket::PetNameQueryResponse { .. } => "SMSG_PET_NAME_QUERY_RESPONSE".into(),
            ServerPacket::GameObjectQueryResponse { .. } => "SMSG_GAMEOBJECT_QUERY_RESPONSE".into(),
            ServerPacket::PlaySound { .. } => "SMSG_PLAY_SOUND".into(),
            ServerPacket::PlayMusic { .. } => "SMSG_PLAY_MUSIC".into(),
            ServerPacket::PlayObjectSound { .. } => "SMSG_PLAY_OBJECT_SOUND".into(),
            ServerPacket::Weather { .. } => "SMSG_WEATHER".into(),
            ServerPacket::TextEmote { .. } => "SMSG_TEXT_EMOTE".into(),
            ServerPacket::Emote { .. } => "SMSG_EMOTE".into(),
            ServerPacket::ItemQueryResponse { .. } => "SMSG_ITEM_QUERY_SINGLE_RESPONSE".into(),
            ServerPacket::InventoryChangeFailure { .. } => "SMSG_INVENTORY_CHANGE_FAILURE".into(),
            ServerPacket::MessageChat(_) => "SMSG_MESSAGECHAT".into(),
            ServerPacket::ChannelNotify(_) => "SMSG_CHANNEL_NOTIFY".into(),
            ServerPacket::ChannelList { .. } => "SMSG_CHANNEL_LIST".into(),
            ServerPacket::ChatPlayerNotFound { .. } => "SMSG_CHAT_PLAYER_NOT_FOUND".into(),
            ServerPacket::ChatWrongFaction => "SMSG_CHAT_WRONG_FACTION".into(),
            ServerPacket::Notification { .. } => "SMSG_NOTIFICATION".into(),
            ServerPacket::AreaTriggerMessage { .. } => "SMSG_AREA_TRIGGER_MESSAGE".into(),
            ServerPacket::PlayedTime { .. } => "SMSG_PLAYED_TIME".into(),
            ServerPacket::RandomRoll { .. } => "MSG_RANDOM_ROLL".into(),
            ServerPacket::InitialSpells { .. } => "SMSG_INITIAL_SPELLS".into(),
            ServerPacket::ActionButtons { .. } => "SMSG_ACTION_BUTTONS".into(),
            ServerPacket::LearnedSpell { .. } => "SMSG_LEARNED_SPELL".into(),
            ServerPacket::SupercededSpell { .. } => "SMSG_SUPERCEDED_SPELL".into(),
            ServerPacket::CastResult { .. } => "SMSG_CAST_RESULT".into(),
            ServerPacket::AttackStart { .. } => "SMSG_ATTACKSTART".into(),
            ServerPacket::AttackStop { .. } => "SMSG_ATTACKSTOP".into(),
            ServerPacket::AttackerState(_) => "SMSG_ATTACKERSTATEUPDATE".into(),
            ServerPacket::AiReaction { .. } => "SMSG_AI_REACTION".into(),
            ServerPacket::SpellStart(_) => "SMSG_SPELL_START".into(),
            ServerPacket::SpellGo(_) => "SMSG_SPELL_GO".into(),
            ServerPacket::SpellFailedOther { .. } => "SMSG_SPELL_FAILED_OTHER".into(),
            ServerPacket::SpellDelayed { .. } => "SMSG_SPELL_DELAYED".into(),
            ServerPacket::CancelAutoRepeat => "SMSG_CANCEL_AUTO_REPEAT".into(),
            ServerPacket::SpellCooldownList { .. } => "SMSG_SPELL_COOLDOWN".into(),
            ServerPacket::ItemCooldown { .. } => "SMSG_ITEM_COOLDOWN".into(),
            ServerPacket::CooldownEvent { .. } => "SMSG_COOLDOWN_EVENT".into(),
            ServerPacket::ClearCooldown { .. } => "SMSG_CLEAR_COOLDOWN".into(),
            ServerPacket::CooldownCheat { .. } => "SMSG_COOLDOWN_CHEAT".into(),
            ServerPacket::ChannelStart { .. } => "MSG_CHANNEL_START".into(),
            ServerPacket::ChannelUpdate { .. } => "MSG_CHANNEL_UPDATE".into(),
            ServerPacket::UpdateAuraDuration { .. } => "SMSG_UPDATE_AURA_DURATION".into(),
            ServerPacket::PlaySpellVisual { .. } => "SMSG_PLAY_SPELL_VISUAL".into(),
            ServerPacket::SpellDamageLog(_) => "SMSG_SPELLNONMELEEDAMAGELOG".into(),
            ServerPacket::PeriodicAuraLog(_) => "SMSG_PERIODICAURALOG".into(),
            ServerPacket::SpellHealLog(_) => "SMSG_SPELLHEALLOG".into(),
            ServerPacket::SpellEnergizeLog(_) => "SMSG_SPELLENERGIZELOG".into(),
            ServerPacket::DamageShield(_) => "SMSG_SPELLDAMAGESHIELD".into(),
            ServerPacket::EnvironmentalDamageLog(_) => "SMSG_ENVIRONMENTALDAMAGELOG".into(),
            ServerPacket::SpellLogMiss(_) => "SMSG_SPELLLOGMISS".into(),
            ServerPacket::XpGain(_) => "SMSG_LOG_XPGAIN".into(),
            ServerPacket::ExplorationXp(_) => "SMSG_EXPLORATION_EXPERIENCE".into(),
            ServerPacket::LevelUp(_) => "SMSG_LEVELUP_INFO".into(),
            ServerPacket::QuestGiverStatus { .. } => "SMSG_QUESTGIVER_STATUS".into(),
            ServerPacket::QuestGiverQuestList(_) => "SMSG_QUESTGIVER_QUEST_LIST".into(),
            ServerPacket::QuestGiverDetails(_) => "SMSG_QUESTGIVER_QUEST_DETAILS".into(),
            ServerPacket::QuestGiverRequestItems(_) => "SMSG_QUESTGIVER_REQUEST_ITEMS".into(),
            ServerPacket::QuestGiverOfferReward(_) => "SMSG_QUESTGIVER_OFFER_REWARD".into(),
            ServerPacket::QuestGiverComplete(_) => "SMSG_QUESTGIVER_QUEST_COMPLETE".into(),
            ServerPacket::QuestGiverInvalid { .. } => "SMSG_QUESTGIVER_QUEST_INVALID".into(),
            ServerPacket::QuestGiverFailed { .. } => "SMSG_QUESTGIVER_QUEST_FAILED".into(),
            ServerPacket::QuestQueryResponse(_) => "SMSG_QUEST_QUERY_RESPONSE".into(),
            ServerPacket::QuestLogFull => "SMSG_QUESTLOG_FULL".into(),
            ServerPacket::QuestUpdateComplete { .. } => "SMSG_QUESTUPDATE_COMPLETE".into(),
            ServerPacket::QuestUpdateFailed { .. } => "SMSG_QUESTUPDATE_FAILED".into(),
            ServerPacket::QuestUpdateFailedTimer { .. } => "SMSG_QUESTUPDATE_FAILEDTIMER".into(),
            ServerPacket::QuestUpdateAddKill { .. } => "SMSG_QUESTUPDATE_ADD_KILL".into(),
            ServerPacket::QuestUpdateAddItem { .. } => "SMSG_QUESTUPDATE_ADD_ITEM".into(),
            ServerPacket::GossipMessage { .. } => "SMSG_GOSSIP_MESSAGE".into(),
            ServerPacket::GossipComplete => "SMSG_GOSSIP_COMPLETE".into(),
            ServerPacket::NpcText { .. } => "SMSG_NPC_TEXT_UPDATE".into(),
            ServerPacket::VendorList { .. } => "SMSG_LIST_INVENTORY".into(),
            ServerPacket::BuyItem { .. } => "SMSG_BUY_ITEM".into(),
            ServerPacket::SellItemResult { .. } => "SMSG_SELL_ITEM".into(),
            ServerPacket::BuyFailed { .. } => "SMSG_BUY_FAILED".into(),
            ServerPacket::ShowBank { .. } => "SMSG_SHOW_BANK".into(),
            ServerPacket::BuyBankSlotResult { .. } => "SMSG_BUY_BANK_SLOT_RESULT".into(),
            ServerPacket::TrainerList { .. } => "SMSG_TRAINER_LIST".into(),
            ServerPacket::TrainerBuySucceeded { .. } => "SMSG_TRAINER_BUY_SUCCEEDED".into(),
            ServerPacket::TrainerBuyFailed { .. } => "SMSG_TRAINER_BUY_FAILED".into(),
            ServerPacket::LootResponse { .. } => "SMSG_LOOT_RESPONSE".into(),
            ServerPacket::LootError { .. } => "SMSG_LOOT_RESPONSE (error)".into(),
            ServerPacket::LootReleaseResponse { .. } => "SMSG_LOOT_RELEASE_RESPONSE".into(),
            ServerPacket::LootRemoved { .. } => "SMSG_LOOT_REMOVED".into(),
            ServerPacket::LootMoneyNotify { .. } => "SMSG_LOOT_MONEY_NOTIFY".into(),
            ServerPacket::LootClearMoney => "SMSG_LOOT_CLEAR_MONEY".into(),
            ServerPacket::LootStartRoll(_) => "SMSG_LOOT_START_ROLL".into(),
            ServerPacket::LootRoll(_) => "SMSG_LOOT_ROLL".into(),
            ServerPacket::LootRollWon(_) => "SMSG_LOOT_ROLL_WON".into(),
            ServerPacket::LootAllPassed(_) => "SMSG_LOOT_ALL_PASSED".into(),
            ServerPacket::ItemPushResult(_) => "SMSG_ITEM_PUSH_RESULT".into(),
            ServerPacket::CorpseQuery(_) => "MSG_CORPSE_QUERY".into(),
            ServerPacket::CorpseReclaimDelay { .. } => "SMSG_CORPSE_RECLAIM_DELAY".into(),
            ServerPacket::DurabilityDamageDeath => "SMSG_DURABILITY_DAMAGE_DEATH".into(),
            ServerPacket::ResurrectRequest(_) => "SMSG_RESURRECT_REQUEST".into(),
            ServerPacket::SpiritHealerConfirm { .. } => "SMSG_SPIRIT_HEALER_CONFIRM".into(),
            ServerPacket::MoveMode { mode, apply, .. } => match (mode, apply) {
                (MoveMode::Root, true) => "SMSG_FORCE_MOVE_ROOT".into(),
                (MoveMode::Root, false) => "SMSG_FORCE_MOVE_UNROOT".into(),
                (MoveMode::WaterWalk, true) => "SMSG_MOVE_WATER_WALK".into(),
                (MoveMode::WaterWalk, false) => "SMSG_MOVE_LAND_WALK".into(),
                (MoveMode::FeatherFall, true) => "SMSG_MOVE_FEATHER_FALL".into(),
                (MoveMode::FeatherFall, false) => "SMSG_MOVE_NORMAL_FALL".into(),
                (MoveMode::Hover, true) => "SMSG_MOVE_SET_HOVER".into(),
                (MoveMode::Hover, false) => "SMSG_MOVE_UNSET_HOVER".into(),
            },
            ServerPacket::LogoutComplete => "SMSG_LOGOUT_COMPLETE".into(),
            ServerPacket::LogoutResponse { .. } => "SMSG_LOGOUT_RESPONSE".into(),
            ServerPacket::LogoutCancelAck => "SMSG_LOGOUT_CANCEL_ACK".into(),
            ServerPacket::Pong { .. } => "SMSG_PONG".into(),
            ServerPacket::ForceSpeedChange { kind, .. } => {
                format!("SMSG_FORCE_{kind:?}_SPEED_CHANGE")
            }
            ServerPacket::GroupInvite { .. } => "SMSG_GROUP_INVITE".into(),
            ServerPacket::GroupDecline { .. } => "SMSG_GROUP_DECLINE".into(),
            ServerPacket::GroupUninvited => "SMSG_GROUP_UNINVITE".into(),
            ServerPacket::GroupLeaderChanged { .. } => "SMSG_GROUP_SET_LEADER".into(),
            ServerPacket::GroupDestroyed => "SMSG_GROUP_DESTROYED".into(),
            ServerPacket::GroupList { .. } => "SMSG_GROUP_LIST".into(),
            ServerPacket::PartyCommandResult { .. } => "SMSG_PARTY_COMMAND_RESULT".into(),
            ServerPacket::PartyMemberStats { full: true, .. } => {
                "SMSG_PARTY_MEMBER_STATS_FULL".into()
            }
            ServerPacket::PartyMemberStats { full: false, .. } => "SMSG_PARTY_MEMBER_STATS".into(),
            ServerPacket::MinimapPing { .. } => "MSG_MINIMAP_PING".into(),
            ServerPacket::RaidTargetSet { .. } | ServerPacket::RaidTargetList { .. } => {
                "MSG_RAID_TARGET_UPDATE".into()
            }
            ServerPacket::ReadyCheckRequest | ServerPacket::ReadyCheckAnswer { .. } => {
                "MSG_RAID_READY_CHECK".into()
            }
            ServerPacket::DuelRequested { .. } => "SMSG_DUEL_REQUESTED".into(),
            ServerPacket::DuelOutOfBounds => "SMSG_DUEL_OUTOFBOUNDS".into(),
            ServerPacket::DuelInBounds => "SMSG_DUEL_INBOUNDS".into(),
            ServerPacket::DuelComplete { .. } => "SMSG_DUEL_COMPLETE".into(),
            ServerPacket::DuelWinner { .. } => "SMSG_DUEL_WINNER".into(),
            ServerPacket::DuelCountdown { .. } => "SMSG_DUEL_COUNTDOWN".into(),
            ServerPacket::MirrorTimerStart(..) => "SMSG_START_MIRROR_TIMER".into(),
            ServerPacket::MirrorTimerPause { .. } => "SMSG_PAUSE_MIRROR_TIMER".into(),
            ServerPacket::MirrorTimerStop { .. } => "SMSG_STOP_MIRROR_TIMER".into(),
            ServerPacket::FriendList { .. } => "SMSG_FRIEND_LIST".into(),
            ServerPacket::IgnoreList { .. } => "SMSG_IGNORE_LIST".into(),
            ServerPacket::FriendStatus(..) => "SMSG_FRIEND_STATUS".into(),
            ServerPacket::WhoResults(..) => "SMSG_WHO".into(),
            ServerPacket::SplineSpeedChange { kind, .. } => {
                format!("SMSG_SPLINE_SET_{kind:?}_SPEED")
            }
            ServerPacket::MoveSetSpeed { kind, .. } => format!("MSG_MOVE_SET_{kind:?}_SPEED"),
            ServerPacket::MountResult { mount: true, .. } => "SMSG_MOUNTRESULT".into(),
            ServerPacket::MountResult { mount: false, .. } => "SMSG_DISMOUNTRESULT".into(),
            ServerPacket::MountSpecialAnim { .. } => "SMSG_MOUNTSPECIAL_ANIM".into(),
            ServerPacket::ShowTaxiNodes { .. } => "SMSG_SHOWTAXINODES".into(),
            ServerPacket::TaxiNodeStatus { .. } => "SMSG_TAXINODE_STATUS".into(),
            ServerPacket::ActivateTaxiReply { .. } => "SMSG_ACTIVATETAXIREPLY".into(),
            ServerPacket::NewTaxiPath => "SMSG_NEW_TAXI_PATH".into(),
            ServerPacket::MailList { .. } => "SMSG_MAIL_LIST_RESULT".into(),
            ServerPacket::SendMailResult { .. } => "SMSG_SEND_MAIL_RESULT".into(),
            ServerPacket::ItemTextQueryResponse { .. } => "SMSG_ITEM_TEXT_QUERY_RESPONSE".into(),
            ServerPacket::ReceivedMail => "SMSG_RECEIVED_MAIL".into(),
            ServerPacket::NextMailTime { .. } => "MSG_QUERY_NEXT_MAIL_TIME".into(),
            ServerPacket::TradeStatus { .. } => "SMSG_TRADE_STATUS".into(),
            ServerPacket::TradeStatusExtended { .. } => "SMSG_TRADE_STATUS_EXTENDED".into(),
            ServerPacket::InitWorldStates(_) => "SMSG_INIT_WORLD_STATES".into(),
            ServerPacket::UpdateWorldState { .. } => "SMSG_UPDATE_WORLD_STATE".into(),
            ServerPacket::Other { opcode } => format!("opcode {opcode:#06x}"),
        }
    }
}
