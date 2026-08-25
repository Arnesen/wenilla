//! Decoded session events — the codec's typed output.
//!
//! `benilla-protocol` decodes the world stream into a flat list of [`SessionEvent`]s; the running
//! world model (which entities exist, where they are, the clock) is the **app's** job (the ECS), not
//! the codec's. One `SMSG_(COMPRESSED_)UPDATE_OBJECT` fans out into many create/move/remove events; a
//! `SMSG_MONSTER_MOVE` becomes a path the app turns into a spline; the clock packet becomes a time
//! sample the app advances. Only primitives + the coarse [`EntityKind`] classification cross this
//! boundary.
//!
//! Coordinates stay **raw WoW** (the `benilla` boundary applies `bevy = (-y, z, -x)`).

use crate::messages::{
    ActionButton, AttackerState, AuctionBidderNotification, AuctionCommandTail, AuctionListEntry,
    AuctionOwnerNotification, ChannelNoticeTail, Character, CreateSpline, DamageShield,
    EnvironmentalDamageLog, ExplorationXp, FriendEntry, FriendStatusUpdate, GossipOption,
    GroupLootInfo, GroupMemberEntry, GuildCommandResult, GuildEventNotice, GuildInfo,
    GuildQueryResponse, GuildRoster, InspectHonorStats, ItemInfo, ItemPushResult, JumpInfo,
    LevelUpInfo, LootAllPassed, LootItem, LootRoll, LootRollWon, LootStartRoll, MailListEntry,
    MirrorTimerStart, MonsterMoveFacing, ObjectFields, PartyMemberStatsInfo, PeriodicAuraLog,
    PetMode, PetSpells, PvpCredit, QuestComplete, QuestDetails, QuestGiverList, QuestOfferReward,
    QuestRequestItems, QuestTemplate, SpellDamageLog, SpellEnergizeLog, SpellHealLog, SpellLogMiss,
    TaxiMask, TradeStatus, TradeStatusExtended, TrainerSpell, TransportPose, VendorItem,
    WhoResults, XpGain,
};

/// Coarse entity classification, free of wire types so the app can branch on it without depending on
/// the message layer. Part of the codec's output vocabulary (carried on [`SessionEvent::ObjectCreate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityKind {
    Player,
    Unit,
    GameObject,
    /// A `TYPEID_DYNAMICOBJECT` (6) create — the invisible anchor a ground-targeted spell's
    /// area effect hangs on (Blizzard's storm, Flamestrike's burn). Carries no display id; its
    /// visual resolves through the spell chain (`DYNAMICOBJECT_SPELLID`), not a model field.
    DynamicObject,
    Other,
}

/// Which character-management verb produced a [`SessionEvent::CharActionResult`] — so the screen can
/// phrase the outcome ("created"/"deleted") and map the `WorldResult` code against the right family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharAction {
    Create,
    Delete,
}

/// A unit's 6 movement speeds (yd/s), decoded from its `LIVING` block. The animation selector keys its
/// walk-vs-run boundary on `walk` (2× it → run, RF-0057); the net bridge extrapolates a remote mover
/// between movement packets at `run`/`run_back`/`swim` and turns it in place at `turn_rate` (rad/s).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MoveSpeeds {
    pub walk: f32,
    pub run: f32,
    pub run_back: f32,
    pub swim: f32,
    pub swim_back: f32,
    pub turn_rate: f32,
}

impl MoveSpeeds {
    /// From the wire order `[walk, run, run_back, swim, swim_back, turn_rate]` (RF-0058).
    fn from_wire(s: [f32; 6]) -> Self {
        Self {
            walk: s[0],
            run: s[1],
            run_back: s[2],
            swim: s[3],
            swim_back: s[4],
            turn_rate: s[5],
        }
    }
}

/// Where a login attempt currently is — the IO thread emits these as it walks the pre-roster
/// sequence (decision 0539), and the login screen's connecting dialog quotes the matching
/// `LOGIN_STATE_*` glue string for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStage {
    /// Dialing the auth server (`LOGIN_STATE_CONNECTING`).
    Connecting,
    /// SRP6 challenge/proof in flight (`LOGIN_STATE_AUTHENTICATING`).
    Authenticating,
    /// Realm picked; the world-socket handshake + char enum (`LOGIN_STATE_HANDSHAKING`).
    Handshaking,
}

/// **How a world session ended** — the fact [`SessionEvent::Disconnected`] carries alongside its
/// reason string, and the one the client's whole post-session behaviour hangs on (decision 1262).
///
/// The two are not shades of the same thing. A logout is a door the player walked through, and the
/// roster on the other side of it is the character-select screen they asked for. A loss is a door
/// that slammed: the reference's engine fires `DISCONNECTED_FROM_SERVER`, and its `GlueParent.lua`
/// answers `SetGlueScreen("login")` + `GlueDialog_Show("DISCONNECTED")` — back to the account
/// screen, one Okay button, no retry. Before this the app told them apart by comparing the reason
/// *string* to `"logged out"`, which is how "the session died" and "the session ended" came to share
/// one code path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    /// A clean, client-initiated logout the server confirmed (`SMSG_LOGOUT_COMPLETE`). The IO
    /// thread cycles the connection and a fresh [`SessionEvent::CharacterList`] follows — the
    /// teardown here is bookkeeping between two halves of one continuous session.
    LoggedOut,
    /// The stream died under us: a socket error, an EOF, or a post-roster handshake failure. A
    /// **displacement kick is exactly this** and carries nothing else — vmangos's
    /// `WorldSession::KickPlayer` is a bare `CloseSocket()`, no packet (verified in
    /// `vmangos-core/src/game/Server/WorldSession.cpp`), so "another client took the account" and
    /// "the server went away" are the same event on the wire and get the same answer.
    Lost,
}

/// One decoded event from the world stream. Carries only primitives + the coarse [`EntityKind`]
/// classification — no wire types leak to the app, and no running state lives here.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// A login attempt progressed to `stage` (decision 0539) — IO-thread-emitted, like
    /// [`Self::CharacterList`], never wire-decoded.
    LoginStage { stage: LoginStage },
    /// A login attempt failed before the roster (decision 0539): `code` is the server's auth
    /// result byte ([`crate::auth::AuthReject`]) when the server *refused* us, `None` for a
    /// transport failure (dial/handshake error). The IO thread is re-parked pre-logon; the app
    /// decides whether to resubmit (transport, with intent) or surface the dialog (refusal).
    /// `terminal` marks a failure that *retrying cannot fix* even though it carries no auth code —
    /// the server is simply unusable by this client (e.g. [`crate::WardenRequired`]). The app must
    /// surface `reason` and stop, never fold it into the paced-resubmit path.
    LoginFailed {
        code: Option<u8>,
        reason: String,
        terminal: bool,
    },
    /// The account's character roster (`SMSG_CHAR_ENUM`): the world socket is authenticated and
    /// parked at character select. The IO thread emits this each connection cycle, then waits for
    /// the app's pick (its pick channel) before `CMSG_PLAYER_LOGIN` moves the session into the world.
    /// `realm` is the auth realm-list entry this session connected to (the select screen's realm
    /// name + type banner) — `None` only when the packet arrives outside the IO thread's own emit
    /// (the raw [`crate::decode`] path has no auth context).
    CharacterList {
        characters: Vec<Character>,
        realm: Option<crate::RealmInfo>,
    },
    /// The result of a character create or delete the IO thread serviced in place while parked at
    /// select (`SMSG_CHAR_CREATE` / `SMSG_CHAR_DELETE` result byte — decision 0423). On a successful
    /// create/delete a fresh [`Self::CharacterList`] is emitted *before* this, so the roster is
    /// already up to date when the screen shows the outcome. `code` is the raw `WorldResult`
    /// (`CHAR_CREATE_SUCCESS`, a `CHAR_NAME_*` code, …) for the app to map to its status text.
    CharActionResult { action: CharAction, code: u8 },
    /// We are in the world: `self_guid` is our player, `name` its character name. The IO thread emits
    /// this first, before any object updates.
    Connected { self_guid: u64, name: String },
    /// The server confirmed our logout (`SMSG_LOGOUT_COMPLETE`) — we are back at character select.
    /// The IO thread cycles the connection immediately; a fresh [`Self::CharacterList`] follows.
    LoggedOut,
    /// The server answered `CMSG_LOGOUT_REQUEST` (`SMSG_LOGOUT_RESPONSE`) — and this is where the
    /// camp countdown comes from (decision 0674). `reason` non-zero is a refusal (vmangos: 1 in
    /// combat, 3 jumping/falling, 2 GM-frozen) and nothing further happens; `reason == 0` starts a
    /// logout that is either `instant` — [`Self::LoggedOut`] follows at once (resting, on a taxi,
    /// or a GM account) — or a 20-second server-side timer the client narrates.
    LogoutResponse { reason: u32, instant: bool },
    /// The server dropped a pending logout at our `CMSG_LOGOUT_CANCEL` (`SMSG_LOGOUT_CANCEL_ACK`).
    LogoutCancelled,
    /// The session ended; carries a human-readable reason and **how** it ended ([`SessionEnd`]).
    Disconnected { reason: String, end: SessionEnd },
    /// An object entered range / was created: a unit, player, or GameObject at a raw-WoW pose. Carries
    /// the interpreted spawn identity (`display_id`/`scale`, decoded per object type here so the app
    /// needn't know the wire layout) plus the full descriptor `fields` mask the ECS seeds its per-object
    /// store from (decision 0061).
    ObjectCreate {
        guid: u64,
        kind: EntityKind,
        /// The model display id (`UNIT_FIELD_DISPLAYID` / `GAMEOBJECT_DISPLAYID`), interpreted per type;
        /// `None` when absent/zero. Present raw in `fields` too, but decoded here (unit-vs-GameObject).
        display_id: Option<u32>,
        position: [f32; 3],
        orientation: f32,
        /// `OBJECT_FIELD_SCALE_X` — the per-object size multiplier (see [`object_scale`]). `1.0` when
        /// absent; for a GameObject this *is* its size (no DBC scale column in 5875).
        scale: f32,
        /// The unit's 6 movement speeds from its `LIVING` block; `None` for a GameObject. Seeds the
        /// animation selector's walk-vs-run boundary (RF-0057) and the net bridge's remote-mover
        /// extrapolation. (Movement-block data, not a descriptor field — hence not in `fields`.)
        speeds: Option<MoveSpeeds>,
        /// A transport GameObject's (`UPDATE_FLAG_TRANSPORT`) path-progress anchor — `Some` only for a
        /// type-15/type-11 transport create (a boat/zeppelin/elevator). Decision 0438's cycle anchor:
        /// `anchor = this value`, `t₀ = Instant::now()` on create/re-create; per frame `progress =
        /// anchor + elapsed_ms`, position = `timetable(progress % period)`.
        transport_progress: Option<u32>,
        /// This object's own rider pose on a transport (`MOVEFLAG_ON_TRANSPORT` in its `LIVING` block) —
        /// `Some` when a freshly-created unit/player is standing on a boat/zeppelin/elevator at create
        /// time, local to that transport's frame (decision 0438).
        transport: Option<TransportPose>,
        /// The path this unit was **already walking** when it streamed into view
        /// (`MOVEFLAG_SPLINE_ENABLED` in its `LIVING` block) — the same travel-order polyline as
        /// [`Self::MonsterMove`], plus the milliseconds of it the server has already ridden, so the app
        /// can join the walk mid-path. Dropping it is what froze every creature that happened to be in
        /// motion at first sight, until its next `MonsterMove` teleported it forward (decision 0708).
        spline: Option<CreateSpline>,
        /// The object's full descriptor field set from the create mask — health/level/appearance/
        /// customization and everything else. The ECS seeds its `ObjectStore` from this, then merges
        /// later [`Self::ObjectValues`] deltas into it (decision 0061).
        fields: ObjectFields,
    },
    /// An item or container entered our view: our own inventory streaming in at login, a looted drop,
    /// a traded bag. A pure **descriptor** object — its create block carries no pose (wire-verified:
    /// vmangos sends `UPDATEFLAG_ALL` + a constant, nothing spatial), so it never becomes a scene
    /// entity. The app seeds its item store from `fields` and merges later [`Self::ObjectValues`] /
    /// removes by guid, exactly like the scene store. Which *slot* holds the item is not here — that
    /// lives in the owning player/bag descriptor (`PLAYER_FIELD_INV_SLOT`/`PACK_SLOT`,
    /// `CONTAINER_FIELD_SLOT`) as this guid.
    ItemCreate {
        guid: u64,
        /// `true` for a container (a bag — its fields carry `CONTAINER_FIELD_SLOT_*` contents),
        /// `false` for a plain item.
        container: bool,
        /// The item's full descriptor set: `OBJECT_FIELD_ENTRY` (→ the template for name/icon via
        /// `ITEM_QUERY_SINGLE`), stack count, durability, enchants; container slots when `container`.
        fields: ObjectFields,
    },
    /// An existing object moved to a new authoritative pose (supersedes any active path). From an
    /// `SMSG_UPDATE_OBJECT` movement block — a one-off correction/relocation, not live locomotion.
    ObjectMove {
        guid: u64,
        position: [f32; 3],
        orientation: f32,
    },
    /// A relayed *player* movement packet (`MSG_MOVE_*`): the mover's authoritative pose plus its live
    /// CMovement `moveFlags`. This is how another player's continuous walking/turning/strafing reaches
    /// us (creatures move via [`Self::MonsterMove`] splines instead). The app snaps to `position` and
    /// extrapolates from `flags` between packets so motion stays smooth at the ~2 Hz relay rate.
    /// `fall_time` (ms airborne) + `jump` (the ballistic launch) are present while `JUMPING` is set, so
    /// the app replays a jump as one arc instead of stair-stepping its height (decision 0053).
    /// `transport` is `Some` while `MOVEFLAG_ON_TRANSPORT` is set: the mover's local pose on the named
    /// transport, for an observed rider to re-anchor through that transport's live matrix (decision
    /// 0438) rather than snap to `position` as a world coordinate.
    UnitMove {
        guid: u64,
        position: [f32; 3],
        orientation: f32,
        flags: u32,
        /// The swim pitch (radians, +up) — the mover's 3D travel angle while `MOVEFLAG_SWIMMING`
        /// is set (`0.0` otherwise): the wire tail exists so observers can integrate a swimmer's
        /// vertical between packets (the client's swim velocity basis adds pitch, `0x7c5880`).
        pitch: f32,
        /// The `MovementInfo` time word — vmangos's own ms clock stamped at receipt (`stime`
        /// = `WorldTimer::getMSTime()` in `MovementInfo::Read`, relayed verbatim by `Write`), one
        /// coherent server clock across all movers. The reference schedules a remote's apply off the
        /// *deltas* between consecutive stamps — replay paced by the sender's own cadence, wow-re
        /// `remote-apply-timing.md`; the app mirrors that per unit (decisions 0601/0615).
        time: u32,
        /// True for `MSG_MOVE_HEARTBEAT` — the periodic mid-move pulse. The reference's reconcile
        /// lerp is armed only for NON-heartbeat events (`0x619090` excludes tag `0x26`); a
        /// heartbeat applies as an outright snap (decision 0601).
        heartbeat: bool,
        fall_time: u32,
        jump: Option<JumpInfo>,
        transport: Option<TransportPose>,
    },
    /// A descriptor `Values` update: the fields this packet changed (`UpdateFields`; WoW resends only
    /// what changed, so a health-only hit carries just that), for the ECS to merge into the object's
    /// store (decision 0061). Emitted from `Object::Values`; a create's *initial* fields ride on
    /// [`Self::ObjectCreate`] instead. Never empty (the codec drops an empty delta).
    ObjectValues { guid: u64, fields: ObjectFields },
    /// Objects that left our view range (the update-object `OutOfRange` block). They still exist
    /// server-side — the real client demotes them to a staging table for cheap re-create.
    ObjectsRemoved(Vec<u64>),
    /// One object destroyed outright (`SMSG_DESTROY_OBJECT` — a corpse decaying ahead of respawn, a
    /// despawn): it ceases to exist. The real client frees it on the spot — an **instant pop**, no
    /// fade-out; the only lifecycle fade is the appear-fade on a fresh create (wow-re object-layer,
    /// selection-death-clear RE). The corpse → destroy → fresh-create-on-respawn sequence is why a
    /// respawn never stands the old corpse up.
    ObjectDestroyed(u64),
    /// The server asked us to play a cinematic (`SMSG_TRIGGER_CINEMATIC`): a character's
    /// first-ever login (the race intro) or a GameObject camera. The app must answer
    /// `CMSG_COMPLETE_CINEMATIC` when it ends — or immediately, to skip — because vmangos anchors
    /// object visibility to the flying cinematic camera while one runs unacked
    /// (`Player::UpdateCinematic`): the world around the body despawns until the ack arrives.
    CinematicTriggered { cinematic_id: u32 },
    /// A server-dictated movement path: the unit traverses `path` — the full travel-order polyline
    /// `[start, …waypoints…, endpoint]` — at constant (arc-length) speed over `duration_ms`. Every
    /// waypoint is carried, so a curved patrol reads as its real path, not a straight `start → endpoint`
    /// shortcut. A `Stop` / zero-duration / <2-point move clears the path (`path` empty).
    MonsterMove {
        guid: u64,
        start: [f32; 3],
        /// The server's per-move spline counter — echoed in `CMSG_MOVE_SPLINE_DONE` when this spline
        /// drives our own player (Charge/knockback/taxi); ignored for a creature's walk.
        spline_id: u32,
        path: Vec<[f32; 3]>,
        /// The dictated final facing (`moveType` 2/3/4), applied as a snap. [`MonsterMoveFacing::None`]
        /// for a plain move — the unit faces its travel direction along the path. This is how a
        /// creature re-faces **without walking** (aggro/scripted/emote), the "won't turn to face me" fix.
        facing: MonsterMoveFacing,
        stop: bool,
        duration_ms: u32,
        /// `true` ⇒ a 3-D flight path (keep the spline's Z); `false` ⇒ a ground walk whose Z the app
        /// re-derives from the terrain under the unit (see the renderer's creature ground-clamp).
        flying: bool,
    },
    /// Same-map teleport (`MSG_MOVE_TELEPORT_ACK`): the app snaps our player + echoes the ack.
    Teleport {
        guid: u64,
        counter: u32,
        position: [f32; 3],
        orientation: f32,
    },
    /// Cross-map worldport (`SMSG_NEW_WORLD`, `needs_ack = true`) or the initial-login map
    /// announcement (`SMSG_LOGIN_VERIFY_WORLD`, `needs_ack = false`): load the new map's ADTs.
    /// While a pending transfer named a transport, `position`/`orientation` are **boat-local**
    /// (decision 0455 — vmangos `SendNewWorld` sends `GetTransportPos()` when riding).
    Worldport {
        map_id: u32,
        position: [f32; 3],
        orientation: f32,
        needs_ack: bool,
    },
    /// The far-teleport preamble (`SMSG_TRANSFER_PENDING`): a worldport to `map_id` follows.
    /// `transport_entry` is set iff the player rides that transport through the transfer — the
    /// signal that the coming [`Self::Worldport`] carries boat-local coordinates (decision 0455).
    TransferPending {
        map_id: u32,
        transport_entry: Option<u32>,
    },
    /// `SMSG_TRANSFER_ABORTED`: the announced transfer will not happen (map full, no instance);
    /// clears the pending-transfer latch.
    TransferAborted { reason: u8 },
    /// The server in-game clock (`SMSG_LOGIN_SETTIMESPEED`): drives time-of-day lighting.
    TimeSpeed {
        hours: u8,
        minutes: u8,
        /// Monotonic day count (`year·372 + month·31 + day` of the packed server date) — the
        /// celestial moon-phase precession input.
        day_serial: u32,
        timescale: f32,
    },
    /// The server's **wall clock** in unix-epoch seconds (`SMSG_QUERY_TIME_RESPONSE`, answering our
    /// `CMSG_QUERY_TIME`) — not [`Self::TimeSpeed`]'s in-game day/night clock, a different quantity
    /// with a different consumer. This one dates the absolute stamps the server writes into
    /// descriptor fields: a timed quest's deadline is `time(nullptr) + limitTime`, so the quest
    /// countdown is a subtraction against *this* number (decision 1150).
    ServerUnixTime { unix_time: u32 },
    /// The player's hearthstone bind point (`SMSG_BINDPOINTUPDATE`): the AreaTable id the
    /// `$z` token names ("Returns you to <area>.").
    BindPoint { area: u32 },
    /// An innkeeper is asking whether to make this your home (`SMSG_BINDER_CONFIRM`) — the
    /// question the `CONFIRM_BINDER` dialog puts on screen. `binder` is the innkeeper's guid, and
    /// it must be echoed in `CMSG_BINDER_ACTIVATE` for the bind to happen at all (decision 1331).
    BinderConfirm { binder: u64 },
    /// The bind took (`SMSG_PLAYERBOUND`): `area` is the AreaTable id we are now bound in, the
    /// same one [`Self::BindPoint`] carries in the packet beside it.
    PlayerBound { binder: u64, area: u32 },
    /// The player's equip proficiencies for one item class (`SMSG_SET_PROFICIENCY`, at login +
    /// on train): the subclass bitmask the item tooltip's slot-line red compares against
    /// (the client's `0xc4d4a0[class]` store).
    Proficiency { item_class: u32, subclass_mask: u32 },
    /// The player's reputation store (`SMSG_INITIALIZE_FACTIONS`, once at login): `(flags, standing)`
    /// per reputation-list slot, indexed by `Faction.dbc`'s `reputationIndex`. The standing excludes
    /// the DBC race/class base — consumers add it before ranking. Drives the reputation branch of
    /// unit reaction (a reputation faction's NPCs colour by the player's rank, not faction templates).
    Reputations { standings: Vec<(u8, i32)> },
    /// Mid-session reputation deltas (`SMSG_SET_FACTION_STANDING`): `(reputationListId,
    /// standing)` per changed slot — same standing convention as [`Self::Reputations`].
    ReputationDelta { standings: Vec<(u32, i32)> },
    /// One reputation-list slot just became visible in the pane (`SMSG_SET_FACTION_VISIBLE`) — the
    /// server's "you have now met these people". Carries no standing; it only lifts the slot's
    /// visible flag, which is what decides whether the pane lists the row at all.
    ReputationVisible { list_id: u32 },
    /// A player character's identity (`SMSG_NAME_QUERY_RESPONSE`, answering our `CMSG_NAME_QUERY`).
    /// Unit names are **not** descriptor fields on the 1.12 wire — they arrive only through this
    /// query pair (players) and [`Self::CreatureName`] (creatures); the app caches them by guid/entry
    /// (decision 0068 §3's query-cache seam). An unknown guid answers with an empty `name`.
    PlayerName {
        guid: u64,
        name: String,
        race: u32,
        gender: u32,
        class: u32,
    },
    /// A pet's display name (`SMSG_PET_NAME_QUERY_RESPONSE`), keyed by **pet number** — the third
    /// naming path, alongside [`Self::PlayerName`] and [`Self::CreatureName`], and the only one a
    /// pet can use: its guid carries a pet number where a creature carries its template entry
    /// ([`crate::guid::pet_number`]), so a creature query for it can only miss. The name is the
    /// pet's own — a hunter pet's custom name, or the creature name for anything summoned. There is
    /// no miss shape: the server stays silent when the pet is gone or the number disagrees.
    PetName { pet_number: u32, name: String },
    /// A creature template's display name (`SMSG_CREATURE_QUERY_RESPONSE`). Keyed by template
    /// `entry` (shared by every spawn of that template), not by spawn guid. `name` is `None` when
    /// the server flagged the entry unknown; `subname` is the tooltip line ("Stable Master", …).
    CreatureName {
        entry: u32,
        name: Option<String>,
        subname: Option<String>,
        /// The template's `CreatureType.dbc` id (Beast, Humanoid, Critter, …) — the TAB-target
        /// critter/totem filter's input. `None` on a server miss.
        creature_type: Option<u32>,
        /// The template's `CreatureFamily.dbc` id (Wolf, Cat, Imp, …) — `UnitCreatureFamily`'s
        /// word and, through that row's pet-food mask, the diet tooltip (decision 1062). `0` for
        /// everything that is neither a tameable beast nor a warlock minion, and `0` on a miss.
        pet_family: u32,
        /// Elite rank 0..4 (the unit tooltip's rank word, decision 0276). `0` on a miss.
        rank: u32,
        /// The template type flags — bit `0x10` hides the tooltip's faction-name line. `0` on a miss.
        type_flags: u32,
        /// The civilian flag (the tooltip's green CIVILIAN line). `false` on a miss.
        civilian: bool,
        /// The racial-leader flag (the tooltip's white LEADER line). `false` on a miss.
        racial_leader: bool,
    },
    /// A GameObject template's type/display/name/`data[24]` head (`SMSG_GAMEOBJECT_QUERY_RESPONSE`,
    /// answering our `CMSG_GAMEOBJECT_QUERY`) — the ask-once GO template lookup, decision 0236.
    /// Keyed by template `entry` (shared by every spawn of that template), not spawn guid. `data[24]`
    /// is the type-specific raw tail (e.g. a chest's lockId lives at a type-specific slot); resolving
    /// a slot is a later consumer's job. On a server miss (unknown entry) this still fires, with
    /// `type_id`/`display_id` zeroed, `name` empty, and `data` all zero — the flattened-field twin of
    /// how [`Self::CreatureName`] answers a miss with `None`s.
    GameObjectInfo {
        entry: u32,
        type_id: u32,
        display_id: u32,
        name: String,
        data: [i32; 24],
    },
    /// A GameObject plays a one-shot **Custom** animation (`SMSG_GAMEOBJECT_CUSTOM_ANIM`):
    /// the client arms GO substate `8 + anim_id` — AnimationData ids 153..156 (Custom0..3),
    /// `anim_id >= 4` rejected at the consumer (wow-re `gameobject-anim-arm.md` §step 8).
    /// The load-bearing sender is the fishing bobber's bite (`anim_id 0`, the splash;
    /// decision 1086).
    GameObjectCustomAnim { guid: u64, anim_id: u32 },
    /// An object plays its one-shot **Despawn** animation, then goes away
    /// (`SMSG_GAMEOBJECT_DESPAWN_ANIM`): the client arms substate 12 — AnimationData **157
    /// Despawn** — and holds the object alive across the `SMSG_DESTROY_OBJECT` that follows in
    /// the same server tick, for the length of that play (decision 1404). UBRS's Rookery Eggs
    /// hatch on this packet; a totem's death and a DynamicObject's expiry send it too, and those
    /// carry nothing armable, so the consumer falls back to the ordinary instant destroy.
    GameObjectDespawnAnim { guid: u64 },
    /// The fishing channel ended with nothing hooked (`SMSG_FISH_NOT_HOOKED`, empty body):
    /// the red `ERR_FISH_NOT_HOOKED` toast (decision 1086).
    FishNotHooked,
    /// The hooked fish got away — the fishing-skill roll failed on the bobber click
    /// (`SMSG_FISH_ESCAPED`, empty body): the red `ERR_FISH_ESCAPED` toast (decision 1086).
    FishEscaped,
    /// A server-pushed 2D sound kit (`SMSG_PLAY_SOUND`): BG events, quest/zone scripts.
    PlaySound { sound_id: u32 },
    /// A server-pushed music kit for the music channel (`SMSG_PLAY_MUSIC`).
    PlayMusic { music_id: u32 },
    /// A server-pushed 3D sound kit at a source object (`SMSG_PLAY_OBJECT_SOUND`) — e.g. the
    /// fishing-bobber splash. The guid positions the emitter.
    PlayObjectSound { sound_id: u32, guid: u64 },
    /// The zone's weather state (`SMSG_WEATHER`): `sound_id` is a SoundEntries loop kit
    /// (8533..8558 rain/snow/sandstorm, 0 = clear); `weather_type`/`grade`/`instant` also feed
    /// the weather visuals when they exist.
    Weather {
        weather_type: u32,
        grade: f32,
        sound_id: u32,
        instant: bool,
    },
    /// A nearby unit's chat emote (`SMSG_TEXT_EMOTE`): the `EmotesText.dbc` id; the guid names
    /// the performer (voice race/sex resolve from its descriptor). `target_name` is the wire's
    /// target display name, empty when untargeted — the sentence-form selector, decision 1274.
    TextEmote {
        guid: u64,
        text_emote: u32,
        target_name: String,
    },
    /// A unit's anim emote (`SMSG_EMOTE`): the `Emotes.dbc` id (drives the anim + its
    /// `EventSoundID`).
    Emote { guid: u64, emote_id: u32 },
    /// The player's spell book (`SMSG_INITIAL_SPELLS`, once at login): the known spell ids,
    /// widened from the wire's `u16`s, plus the active-cooldown list (the seed of the
    /// cooldown store — decision 0137 phase 4).
    SpellBook {
        spell_ids: Vec<u32>,
        cooldowns: Vec<crate::messages::SpellCooldown>,
    },
    /// The player's saved action bar (`SMSG_ACTION_BUTTONS`, once at login): the occupied slots
    /// (0..119; 0–11 = the main bar). Feeds the FrameXML action bar (decision 0068 slice 1).
    ActionButtons { buttons: Vec<ActionButton> },
    /// A spell added to the book after login (`SMSG_LEARNED_SPELL`): trainer/quest/level-up, widened
    /// from the wire `u16`. The first post-login spell-book mutation (decision 0237).
    SpellLearned { spell_id: u32 },
    /// A rank-up (`SMSG_SUPERCEDED_SPELL`): the new rank replaces the old in the book and on the
    /// action bar. Both ids widened from the wire `u16`.
    SpellSuperceded {
        old_spell_id: u32,
        new_spell_id: u32,
    },
    /// The server's verdict on our cast (`SMSG_CAST_RESULT`): `reason` is the failure code when
    /// `success` is false (the client's error-text table keys on it), and `arg` the reason-specific
    /// argument word that fills that message's `%s` — a `SpellFocusObject.dbc` id for
    /// `REQUIRES_SPELL_FOCUS`, an `AreaTable.dbc` id for `REQUIRES_AREA`
    /// ([`crate::messages::CastOutcome::Failed`]).
    CastResult {
        spell_id: u32,
        success: bool,
        reason: Option<u8>,
        arg: Option<u32>,
    },
    /// The pet action bar's whole state (`SMSG_PET_SPELLS`, decision 0982) — or its **teardown**,
    /// which is the same event carrying a zero `pet_guid`. Server-authoritative: this is not a
    /// delta, it is the bar, and every visible pet-bar change arrives through it.
    PetSpells(Box<PetSpells>),
    /// The pet's react/command state alone (`SMSG_PET_MODE`) — a state change with no bar edit
    /// behind it (the reaction buttons' usual wire).
    PetMode(PetMode),
    /// A refused pet order (`SMSG_PET_ACTION_FEEDBACK`): one reason code for the red error line.
    PetActionFeedback { reason: u8 },
    /// The pet's cast refusal (`SMSG_PET_CAST_FAILED`) — [`Self::CastResult`]'s vocabulary, but
    /// the caster is the pet, so it never touches OUR cast state.
    PetCastFailed { spell_id: u32, reason: Option<u8> },
    /// An item template's display head (`SMSG_ITEM_QUERY_SINGLE_RESPONSE`, answering our
    /// `CMSG_ITEM_QUERY_SINGLE`). Keyed by template entry; `None` = the server doesn't know it
    /// (undiscovered) — cached negative, like an unknown creature entry. Boxed for the same reason
    /// as [`crate::messages::ServerPacket::ItemQueryResponse`]: the full item template is wide, and
    /// this is one variant among many tiny, hot ones.
    ItemTemplate {
        entry: u32,
        info: Option<Box<ItemInfo>>,
    },
    /// One inbound chat line (`SMSG_MESSAGECHAT`) — say/yell/system/NPC/channel. System lines
    /// (type `0x0A`) carry GM command feedback. Carries the sender's `chat_tag` (AFK/DND/GM) for
    /// the `<AFK>`/`<DND>`/`<GM>` name-prefix the real chat frame renders.
    Chat(crate::messages::ChatMessage),
    /// A channel join/leave/error/moderation notice (`SMSG_CHANNEL_NOTIFY`) — `tail` is the payload
    /// selected by `notice` (see [`ChannelNoticeTail`]).
    ChannelNotify {
        notice: u8,
        channel: String,
        tail: ChannelNoticeTail,
    },
    /// A channel's member roster (`SMSG_CHANNEL_LIST`, answering our `CMSG_CHANNEL_LIST`): `(guid,
    /// memberFlags)` per row (owner/moderator/voiced/muted/custom/mic-muted — vmangos
    /// `Chat/Channel.h:119-130`).
    ChannelList {
        channel: String,
        flags: u8,
        members: Vec<(u64, u8)>,
    },
    /// A whisper target wasn't found online (`SMSG_CHAT_PLAYER_NOT_FOUND`).
    ChatPlayerNotFound { name: String },
    /// A cross-faction whisper was refused (`SMSG_CHAT_WRONG_FACTION`); empty body.
    ChatWrongFaction,
    /// A server notice (`SMSG_NOTIFICATION`) — pre-formatted text the real client flashes in the
    /// red UIErrorsFrame ("You do not know that language", trade refusals, and kin).
    Notification { text: String },
    /// An area trigger refused (`SMSG_AREA_TRIGGER_MESSAGE`) — pre-formatted text explaining why a
    /// portal or instance entrance did nothing ("You must be at least level 58 to enter…", "You
    /// cannot enter … while in ghost form."). The reference sends it to the same system-message
    /// sink as [`Self::Notification`].
    AreaTriggerMessage { text: String },
    /// Answers `/played` (`SMSG_PLAYED_TIME`, our `CMSG_PLAYED_TIME`): total played time + time
    /// since the last level-up, both in seconds.
    PlayedTime { total: u32, level: u32 },
    /// The server's `/random` broadcast (`MSG_RANDOM_ROLL`): the rolled range, the result, and the
    /// roller's guid.
    RandomRoll {
        min: u32,
        max: u32,
        roll: u32,
        guid: u64,
    },
    /// The server refused an inventory operation (`SMSG_INVENTORY_CHANGE_FAILURE` — equip level,
    /// proficiency, bag full, …): the UI error line's inventory vocabulary, the equip twin of
    /// [`Self::CastResult`]'s failure path. `required_level` rides only on the level refusal.
    InventoryFailure {
        reason: u8,
        required_level: Option<u32>,
        item_guid: u64,
        /// The destination bag's ABSOLUTE player slot — the `%s` source of reason 16's
        /// "Only Arrows can be placed in that."; 255 = the player's own array (no bag to name).
        bag_slot: u8,
    },
    /// A unit began melee auto-attack (`SMSG_ATTACKSTART` — including the echo of our own
    /// `CMSG_ATTACKSWING`).
    AttackStart { attacker: u64, victim: u64 },
    /// A unit stopped melee auto-attack (`SMSG_ATTACKSTOP`).
    AttackStop { attacker: u64, victim: u64 },
    /// One completed melee swing (`SMSG_ATTACKERSTATEUPDATE`) — the attacker's swing-animation
    /// trigger (decision 0073: one packet = one swing, no client timer).
    AttackerState(AttackerState),
    /// A creature flared at someone (`SMSG_AI_REACTION`): reaction 2 = HOSTILE (sent on every
    /// creature melee-attack start), 0 = ALERT (stealth pre-aggro detection); any other value is
    /// a no-op. Pure audio in the client — the aggro/alert vocals (decision 0280).
    AiReaction { unit: u64, reaction: u32 },
    /// A unit began a non-triggered cast (`SMSG_SPELL_START`), instants included (`cast_time_ms ==
    /// 0`) — the precast trigger the phase-2 casting animation loop builds on (decision 0099 phase
    /// 1). `target` is the explicit unit target, when the spell's target block carries one.
    SpellStart {
        caster: u64,
        spell_id: u32,
        cast_flags: u16,
        cast_time_ms: u32,
        target: Option<u64>,
        /// The nocked-ammo display id (the `CAST_FLAG_AMMO` tail, ranged spells only) — feeds the
        /// caster's worn ammo model (the client's `0x60ba30` @ START `0x6e78b6`, any caster;
        /// wow-re `nocked-ammo-cancel.md`). `None` when the flag is clear.
        ammo_display_id: Option<u32>,
    },
    /// The cast launched (`SMSG_SPELL_GO`): hit/miss lists + (for a ranged spell) the ammo display
    /// id for the projectile visual. The server schedules impact itself off `Spell.dbc` Speed —
    /// nothing about missile travel rides this packet.
    SpellGo {
        caster: u64,
        spell_id: u32,
        cast_flags: u16,
        hits: Vec<u64>,
        misses: Vec<(u64, u8)>,
        target: Option<u64>,
        /// The GameObject an open-lock cast launched at (`TARGET_FLAG_GAMEOBJECT`) — opens a chest lid /
        /// locked door (decision 0250). `None` for a unit spell.
        go_target: Option<u64>,
        /// The ground point a dest-targeted cast launched at (`TARGET_FLAG_DEST_LOCATION`), raw
        /// WoW coords — where a ground AOE's launch-side visual belongs (the B132 follow-up;
        /// the persistent effect anchors to the DynamicObject create instead). `None` otherwise.
        dest: Option<[f32; 3]>,
        ammo_display_id: Option<u32>,
        /// The **cast item's** guid when the packet's first guid names one (an item use — potion,
        /// scroll: `item_or_caster != caster`); `None` for a plain spell cast. The item-use
        /// cooldown keys on it (decision 0137 phase 4 — the client resolves its SPELLCAST item
        /// the same way).
        item_caster: Option<u64>,
    },
    /// The hop list for a caster's **beam** (`SMSG_SPELL_UPDATE_CHAIN_TARGETS`, decision 0955):
    /// the units a chain/beam visual runs through, caster → t1 → t2 → …. The client's own array
    /// (`unit+0xd44`) is filled from this and from nothing else, and is consumed once by the next
    /// chain `CharProc` — so this is an edge, not a state.
    ///
    /// vmangos sends it only for **channeled** spells. The cast-stage chains get their hops from
    /// [`Self::SpellGo`]'s hit list instead — which is the reference's own second producer
    /// (`0x6e800d` inside its SPELL_GO handler fills the same array), not a divergence.
    SpellChainTargets {
        caster: u64,
        spell_id: u32,
        targets: Vec<u64>,
    },
    /// An observed cast was interrupted/cancelled (`SMSG_SPELL_FAILED_OTHER`) — ends the caster's
    /// `Casting` state seam the same as a [`Self::SpellGo`].
    SpellFailedOther { caster: u64, spell_id: u32 },
    /// Our own cast was pushed back by damage (`SMSG_SPELL_DELAYED`) — the cast bar slides its
    /// window out by `delay_ms` instead of finishing early (decision 0256).
    SpellDelayed { caster: u64, delay_ms: u32 },
    /// Stop our own ranged auto-repeat visual (`SMSG_CANCEL_AUTO_REPEAT`, self-only). No
    /// ranged-attack consumer exists yet (decision 0099 phase 5).
    CancelAutoRepeat,
    /// Server-pushed cooldowns (`SMSG_SPELL_COOLDOWN`) for `caster` (the player or pet): pairs of
    /// `(spell_id, cooldown_ms)`, where `0` ms means "use the spell's own Spell.dbc recovery
    /// times" (decision 0137 phase 4; the school-lockout / pet path — a normal cast's cooldown is
    /// client-tracked).
    SpellCooldowns {
        caster: u64,
        cooldowns: Vec<(u32, u32)>,
    },
    /// Put an item instance on the client's fixed 30 s use cooldown (`SMSG_ITEM_COOLDOWN`).
    ItemCooldown { item_guid: u64, spell_id: u32 },
    /// `SMSG_ITEM_ENCHANT_TIME_UPDATE` — the seconds left on one item's TEMPORARY enchant, in the
    /// named enchant slot. The **only** feed for the tooltip's countdown: the item's own
    /// `ITEM_FIELD_ENCHANTMENT` duration field is never read for it (wow-re
    /// `ui/scratch/tooltip-content-law.md` §E3; decision 0920). `seconds == 0` = expired.
    ItemEnchantTime {
        item_guid: u64,
        slot: u32,
        seconds: u32,
    },
    /// Start an on-hold (`SPELL_ATTR_COOLDOWN_ON_EVENT`) cooldown's parked timers now
    /// (`SMSG_COOLDOWN_EVENT`).
    CooldownEvent { spell_id: u32, caster: u64 },
    /// Remove one spell's cooldown record (`SMSG_CLEAR_COOLDOWN`).
    ClearCooldown { spell_id: u32, caster: u64 },
    /// Wipe every cooldown for `caster` (`SMSG_COOLDOWN_CHEAT`, the GM reset).
    CooldownCheat { caster: u64 },
    /// Our own channeled cast opened (`MSG_CHANNEL_START`, self-only — no guid on the wire). The
    /// cast bar's channel-open edge (decision 0137).
    ChannelStart { spell_id: u32, duration_ms: u32 },
    /// Our own channel's remaining time (`MSG_CHANNEL_UPDATE`, self-only); `0` = the channel is
    /// over — natural end and interrupt alike (decision 0137).
    ChannelUpdate { remaining_ms: u32 },
    /// How long one of **our own** auras has left (`SMSG_UPDATE_AURA_DURATION`), keyed by its
    /// `UNIT_FIELD_AURA` slot. Self-only, never sent for a permanent aura, and it arrives *before*
    /// the descriptor delta that names the slot's spell — so the aura feed buffers it by slot and
    /// joins on the spell present when it lands (decisions 0255/0257).
    AuraDuration { slot: u8, remaining_ms: u32 },
    /// Play a spell-visual kit on a unit outside the normal cast sequence (`SMSG_PLAY_SPELL_VISUAL`
    /// — eat/drink kits, a channel-kit refresh workaround). No VFX consumer yet (decision 0099
    /// phase 3).
    PlaySpellVisual { unit: u64, kit_id: u32 },
    /// Non-melee (spell) damage dealt (`SMSG_SPELLNONMELEEDAMAGELOG`) — decision 0137 phase 2's
    /// floating-combat-text data feed.
    SpellDamageLog(SpellDamageLog),
    /// Periodic (DoT/HoT/regen) aura ticks (`SMSG_PERIODICAURALOG`) — decision 0137 phase 2.
    PeriodicAuraLog(PeriodicAuraLog),
    /// A direct heal landing (`SMSG_SPELLHEALLOG`) — decision 0578's center-combat-text feed.
    SpellHealLog(SpellHealLog),
    /// An instant power gain (`SMSG_SPELLENERGIZELOG`) — decision 0578.
    SpellEnergizeLog(SpellEnergizeLog),
    /// A damage-shield (Thorns-style) return hit (`SMSG_SPELLDAMAGESHIELD`) — decision 0137 phase 2.
    DamageShield(DamageShield),
    /// Environmental damage taken — fall/drowning/fatigue/lava/slime/fire
    /// (`SMSG_ENVIRONMENTALDAMAGELOG`): the client-side feedback trigger (the fall-landing dust
    /// kit via `EnvironmentalDamage.dbc`).
    EnvironmentalDamageLog(EnvironmentalDamageLog),
    /// A spell cast's per-target miss list (`SMSG_SPELLLOGMISS`) — decision 0137 phase 2.
    SpellLogMiss(SpellLogMiss),
    /// An XP award, kill or non-kill (`SMSG_LOG_XPGAIN`) — decision 0137 phase 2.
    XpGain(XpGain),
    /// A first visit to an area (`SMSG_EXPLORATION_EXPERIENCE`) — the discovered area id + its
    /// XP award; the "Discovered …" chat line's data (decision 0828).
    ExplorationXp(ExplorationXp),
    /// Our own ding (`SMSG_LEVELUP_INFO`, self-addressed only) — decision 0304.
    LevelUp(LevelUpInfo),
    /// A gossip menu opened on an NPC (`SMSG_GOSSIP_MESSAGE`, answering our `CMSG_GOSSIP_HELLO`):
    /// `text_id` drives a follow-up [`crate::messages::npc_text_query`] for the greeting body;
    /// `options` are the selectable lines (icon + coded flag + label). `quests` are the quest-option
    /// rows riding the same packet — `(quest_id, dialog-status icon, title)`; the gossip window lists
    /// them and a click sends `CMSG_QUESTGIVER_QUERY_QUEST` (decision 0088).
    GossipMenu {
        npc: u64,
        text_id: u32,
        options: Vec<GossipOption>,
        quests: Vec<(u32, u32, String)>,
    },
    /// A questgiver dialog status for one NPC (`SMSG_QUESTGIVER_STATUS`) — the `!`/`?` marker's
    /// [`crate::messages::dialog_status`] value. Stored per guid now; the world marker is a later
    /// slice (decision 0088).
    QuestGiverStatus { npc: u64, status: u32 },
    /// The greeting panel: an NPC's offered/active quest rows (`SMSG_QUESTGIVER_QUEST_LIST`).
    QuestGreeting(QuestGiverList),
    /// The accept panel: full quest text + rewards on offer (`SMSG_QUESTGIVER_QUEST_DETAILS`).
    QuestDetail(QuestDetails),
    /// The progress panel: "bring me these" text + required items/money + completability
    /// (`SMSG_QUESTGIVER_REQUEST_ITEMS`).
    QuestProgress(QuestRequestItems),
    /// The reward panel: turn-in text + rewards to grant (`SMSG_QUESTGIVER_OFFER_REWARD`).
    QuestOffer(QuestOfferReward),
    /// The turn-in result: XP/money granted + fixed items (`SMSG_QUESTGIVER_QUEST_COMPLETE`).
    QuestComplete(QuestComplete),
    /// The full quest template (`SMSG_QUEST_QUERY_RESPONSE`, answering our `CMSG_QUEST_QUERY`) —
    /// the quest log's ask-once detail source, cached by `quest_id`.
    QuestTemplate(Box<QuestTemplate>),
    /// A kill/use objective ticked (`SMSG_QUESTUPDATE_ADD_KILL`) — the "Kobold Vermin slain: 3/10"
    /// toast. `entry` carries the raw creature/GO encoding (GO = `(−id)|0x80000000`); the durable
    /// count also lands in the `PLAYER_QUEST_LOG` slot's counter field, this is the announce line.
    QuestObjectiveKill {
        quest_id: u32,
        entry: u32,
        count: u32,
        required: u32,
    },
    /// An item-collection objective toast (`SMSG_QUESTUPDATE_ADD_ITEM`).
    QuestObjectiveItem { item_id: u32, count: u32 },
    /// Every objective on the quest is complete (`SMSG_QUESTUPDATE_COMPLETE`) — the "Quest
    /// completed" line; the slot's state byte carries the durable fact.
    QuestObjectivesComplete { quest_id: u32 },
    /// The quest failed (`SMSG_QUESTUPDATE_FAILED` / `_FAILEDTIMER` — `timed` picks which).
    QuestFailed { quest_id: u32, timed: bool },
    /// The log refused a new quest — no free slot (`SMSG_QUESTLOG_FULL`).
    QuestLogFull,
    /// The giver won't OFFER the quest (`SMSG_QUESTGIVER_QUEST_INVALID`): one `QuestFailedReason`
    /// msg code and no quest id — vmangos' `SendCanTakeQuestResponse`, the answer to a query or an
    /// accept that fails `CanTakeQuest` ("already on that quest", "not high enough level").
    QuestGiverInvalid { reason: u32 },
    /// A quest the giver DID offer failed on accept (`SMSG_QUESTGIVER_QUEST_FAILED`): the
    /// `{questId, reason}` pair, whose line names the quest. Kept apart from
    /// [`Self::QuestGiverInvalid`] because the reference reads the two packets with two different
    /// handlers and two different message tables (decision 0669).
    QuestGiverFailed { quest_id: u32, reason: u32 },
    /// The gossip window closes (`SMSG_GOSSIP_COMPLETE`) — no menu is open server-side any more.
    GossipComplete,
    /// A guard's directions (`SMSG_GOSSIP_POI`): drop a marker at that spot on the minimap and the
    /// world map. Volunteered by a gossip option carrying an `action_poi_id` — never requested.
    GossipPoi(crate::messages::GossipPoi),
    /// The greeting record for a gossip menu (`SMSG_NPC_TEXT_UPDATE`, answering our
    /// `CMSG_NPC_TEXT_QUERY`) — all 8 blocks, **undrawn**: which line greets you depends on the
    /// NPC's gender and a die roll, so the app draws it when the frame opens
    /// (`messages::gossip::select_greeting`). `$N` and friends are client-substituted tokens still
    /// in the strings.
    NpcGreeting {
        text_id: u32,
        blocks: Vec<crate::messages::NpcTextBlock>,
    },
    /// A vendor's stock (`SMSG_LIST_INVENTORY`, answering our `CMSG_LIST_INVENTORY`): entry, icon,
    /// price, and remaining stock per row (`current_count == 0xFFFF_FFFF` = unlimited).
    VendorInventory { vendor: u64, items: Vec<VendorItem> },
    /// A trainer's service list (`SMSG_TRAINER_LIST`, reached via the gossip trainer option): the
    /// wire services (each a spell id + cost + green/red/gray state + level/skill/ability gates), the
    /// window-framing `trainer_type` (0 class · 1 mount · 2 tradeskill · 3 pet), and the greeting
    /// title. The app resolves each service to name/icon through its spell catalog (decision 0237).
    TrainerList {
        trainer: u64,
        trainer_type: u32,
        services: Vec<TrainerSpell>,
        greeting: String,
    },
    /// A trainer taught a service (`SMSG_TRAINER_BUY_SUCCEEDED`, answering `CMSG_TRAINER_BUY_SPELL`):
    /// confirmation only — the spell itself arrives via `SMSG_LEARNED_SPELL` (already in the book).
    /// The app re-requests `CMSG_TRAINER_LIST` on this to repaint the bought row green→gray (decision
    /// 0237: the server never auto-resends the list on a buy).
    TrainerBuySucceeded { trainer: u64, spell_id: u32 },
    /// A trainer refused a purchase (`SMSG_TRAINER_BUY_FAILED`): `error` is a
    /// [`crate::messages::train_fail`] code (0 unavailable · 1 not-enough-money · 2 not-enough-skill).
    /// Surfaces on the trainer window's error line.
    TrainerBuyFailed {
        trainer: u64,
        spell_id: u32,
        error: u32,
    },
    /// A vendor's stock updated after a purchase (`SMSG_BUY_ITEM`). The purchased item itself
    /// arrives through the normal item-create + inventory-slot update path, already handled.
    VendorBuyResult {
        vendor: u64,
        slot: u32,
        new_count: u32,
        purchase_count: u32,
    },
    /// A sell was refused (`SMSG_SELL_ITEM`'s error path — a success sends no packet at all).
    /// `reason` is a `SellResult` code ([`crate::messages::sell_result`]).
    VendorSellFailed {
        vendor: u64,
        item_guid: u64,
        reason: u8,
    },
    /// A purchase was refused (`SMSG_BUY_FAILED`). `reason` is a `BuyResult` code
    /// ([`crate::messages::buy_result`]).
    VendorBuyFailed {
        vendor: u64,
        item_entry: u32,
        reason: u8,
    },
    /// The bank window opens (`SMSG_SHOW_BANK`), answering our `CMSG_BANKER_ACTIVATE` — or
    /// arriving unprompted for the `GOSSIP_OPTION_BANKER` gossip option, so a handler must not
    /// assume it always follows our own activate. The vault contents are already in the player
    /// descriptor (decision 0604); this only opens the window.
    ShowBank { banker: u64 },
    /// A bank-slot purchase was refused (`SMSG_BUY_BANK_SLOT_RESULT`). `result` is a `BankSlotResult`
    /// code ([`crate::messages::bank_slot_result`]); a successful buy sends no packet, visible only
    /// as the `PLAYER_BYTES_2` bank-bag-count byte advancing + the coinage drop.
    BuyBankSlotResult { result: u32 },
    /// A loot window opened (`SMSG_LOOT_RESPONSE`'s normal shape), answering our `CMSG_LOOT`:
    /// `loot_type` is a `loot::loot_type` code, `items` the row list (quest rows ride the same
    /// list, `slot = items.len() + i`). A row still under a group roll arrives with
    /// `slot_type == ROLL_ONGOING` (decision 0591); master loot stays out of scope.
    LootResponse {
        guid: u64,
        loot_type: u8,
        gold: u32,
        items: Vec<LootItem>,
    },
    /// The server refused to open the loot window (`SMSG_LOOT_RESPONSE`'s error shape — didn't
    /// kill it, too far, not standing, …). `error` is a `loot::loot_error` code.
    LootError { guid: u64, error: u8 },
    /// One loot-window row was taken, by anyone (`SMSG_LOOT_REMOVED`) — the UI clears that row.
    LootRemoved { slot: u8 },
    /// Our share of the loot's coin pile (`SMSG_LOOT_MONEY_NOTIFY`), answering our
    /// `CMSG_LOOT_MONEY`.
    LootMoneyNotify { amount: u32 },
    /// The coin line disappears for every current looter (`SMSG_LOOT_CLEAR_MONEY`).
    LootClearMoney,
    /// The loot window closes (`SMSG_LOOT_RELEASE_RESPONSE`), answering our `CMSG_LOOT_RELEASE`.
    LootReleaseResponse { guid: u64 },
    /// A group roll opened on one drop (`SMSG_LOOT_START_ROLL`) — the app allocates a client-side
    /// `rollID` for it and raises a `GroupLootFrame` (decision 0591).
    LootStartRoll(LootStartRoll),
    /// One roller's vote or dice result (`SMSG_LOOT_ROLL`) — the chat announcement line. The
    /// `(roll_number, roll_type)` pair is overloaded; `LootRoll::is_dice`/`vote` disentangle it.
    LootRoll(LootRoll),
    /// A group roll resolved (`SMSG_LOOT_ROLL_WON`) — closes that roll's frame.
    LootRollWon(LootRollWon),
    /// Everyone passed (`SMSG_LOOT_ALL_PASSED`) — closes that roll's frame; the item returns to
    /// the corpse for ordinary looting.
    LootAllPassed(LootAllPassed),
    /// An item landed in our bags — looted or received from an NPC (`SMSG_ITEM_PUSH_RESULT`);
    /// drives the "You receive loot: …" chat line.
    ItemPushResult(ItemPushResult),
    /// The keepalive echo (`SMSG_PONG`): the sequence number of the `CMSG_PING` it answers. The app
    /// matches it against the ping clock (shared with the write thread's 30 s sender) to measure
    /// the round-trip time shown in the debug panel.
    Pong { sequence: u32 },
    /// The server changed one of our mover's speeds (`SMSG_FORCE_*_SPEED_CHANGE` — an aura
    /// slow/haste, a mount, GM `.modify speed`) and awaits the matching ack. The app must (1) apply
    /// `speed` to the mover's [`MoveSpeeds`] entry for `kind`, and (2) answer
    /// `CMSG_FORCE_*_SPEED_CHANGE_ACK` echoing `counter` + the exact `speed` with a live
    /// `MovementInfo` — unacked, the server force-resolves after ~4 s and flags its anticheat
    /// (`Unit::CheckPendingMovementChanges`), so every speed change desyncs without this.
    ForceSpeedChange {
        guid: u64,
        kind: crate::messages::SpeedKind,
        counter: u32,
        speed: f32,
    },
    /// A speed change on a unit we do NOT control (`SMSG_SPLINE_SET_*_SPEED` /
    /// `MSG_MOVE_SET_*_SPEED` — an observed player mounting up, a hastened creature). Apply to the
    /// unit's [`MoveSpeeds`] entry for `kind`; **no ack** (decision 0441). The MOVE_SET flavour's
    /// accompanying pose arrives as its own [`Self::UnitMove`].
    SpeedChanged {
        guid: u64,
        kind: crate::messages::SpeedKind,
        speed: f32,
    },
    /// The server's answer to our (dis)mount attempt (`SMSG_MOUNTRESULT` /
    /// `SMSG_DISMOUNTRESULT`, split by `mount`): a raw result code (OK = 10 mounting / 3
    /// dismounting; the failure codes are red-error-line material — a P2 trimming, decision 0441).
    MountResult { mount: bool, code: u32 },
    /// A nearby rider hit the mounted flourish (`SMSG_MOUNTSPECIAL_ANIM`): play MountSpecial(94)
    /// on that unit's mount. Our own guid may arrive too — whether the sender gets the echo
    /// is a server-config detail (vmangos's non-broadcaster delivery echoes; the optional
    /// per-player broadcaster does not) — so the app self-suppresses it on receive and plays
    /// its own flourish locally at send time.
    MountSpecial { guid: u64 },
    /// The server handed us — or took from us — the reins of a unit (`SMSG_CLIENT_CONTROL_UPDATE`).
    ///
    /// `mover` is the unit being spoken about, **not** necessarily a new subject: when the server
    /// takes control away it names *us*, with `allow_move = false`. So the two questions the app
    /// asks of this are different — "is this about me?" (`mover == self guid`) and "may that unit
    /// move?" — and conflating them gets the mind-controlled *victim*'s case backwards.
    ///
    /// Answering with `CMSG_SET_ACTIVE_MOVER` is not optional when control is granted: vmangos
    /// discards every `MSG_MOVE_*` for a mover it has not confirmed.
    ClientControl { mover: u64, allow_move: bool },
    /// A packet the codec dropped on the floor: `unparseable = false` means **no parse arm exists**
    /// for the opcode at all (it fell through to `ServerPacket::Other` — a wire-coverage gap);
    /// `true` means a parser exists but errored on this body (the reader skipped it to keep the
    /// stream aligned). The app tallies these per opcode (the debug panel's dropped-opcode
    /// instrument): a silently-ignored opcode is how a whole wire family — a speed change, a
    /// compressed-moves batch — goes unnoticed for months.
    PacketDropped { opcode: u16, unparseable: bool },
    /// Where our corpse is (`MSG_CORPSE_QUERY`'s answer, after our empty-body request): feeds the
    /// map corpse markers and the corpse-run range gate (decision 0308 §5). `found == false` also
    /// arrives UNPROMPTED when the corpse converts to bones — it means "drop the marker".
    /// `display_map`/`position` are where to walk toward (dungeon-entrance-adjusted);
    /// `corpse_map` is the corpse's real map.
    CorpseQuery {
        found: bool,
        display_map: i32,
        position: [f32; 3],
        corpse_map: u32,
    },
    /// Milliseconds until our corpse can be reclaimed (`SMSG_CORPSE_RECLAIM_DELAY` — at release
    /// and at login-while-dead; 30 s base, 60/120 s on repeated deaths). Feeds
    /// `GetCorpseRecoveryDelay` and the RECOVER_CORPSE popup's StartDelay gate.
    CorpseReclaimDelay { delay_ms: u32 },
    /// The 10% natural-death durability loss landed (`SMSG_DURABILITY_DAMAGE_DEATH`, empty
    /// body — sent beside the item-field deltas): the red
    /// "Your equipped items suffer a 10% durability loss." error line's cue.
    DurabilityDamageDeath,
    /// Someone offered to resurrect us (`SMSG_RESURRECT_REQUEST`). `name` is EMPTY for a player
    /// caster (resolve via the guid name cache); `sickness`/`has_timer` pick the reference popup
    /// variant. Answered by `CMSG_RESURRECT_RESPONSE` (accept/decline).
    ResurrectRequest {
        caster: u64,
        name: String,
        sickness: bool,
        has_timer: bool,
    },
    /// The spirit healer asks for the confirm (`SMSG_SPIRIT_HEALER_CONFIRM`, from the server's
    /// gossip spirit-healer option): fires the CONFIRM_XP_LOSS popup; its Accept sends
    /// `CMSG_SPIRIT_HEALER_ACTIVATE` back with this guid.
    SpiritHealerConfirm { npc: u64 },
    /// **A granted mover mode changed** — root, water-walk, feather-fall or hover (the ack'd
    /// movement-mode family, decision 0866). Must be acked with the echoed `counter` (+ our current
    /// `MovementInfo`), or the server never applies it and observers never see the change. The app
    /// also **applies the mode to its own mover**: each one changes how the mover behaves — see
    /// [`crate::messages::MoveMode`] for what each does.
    MoveMode {
        guid: u64,
        counter: u32,
        mode: crate::messages::MoveMode,
        apply: bool,
    },
    /// Someone invited us to their group (`SMSG_GROUP_INVITE`) — the invite popup.
    GroupInvite { inviter: String },
    /// An invite we sent was declined (`SMSG_GROUP_DECLINE`).
    GroupDecline { name: String },
    /// We were removed from our group (`SMSG_GROUP_UNINVITE`, empty body) — kicked or left.
    GroupUninvited,
    /// The group's leader changed (`SMSG_GROUP_SET_LEADER`).
    GroupLeaderChanged { name: String },
    /// The group disbanded outright (`SMSG_GROUP_DESTROYED`, empty body).
    GroupDestroyed,
    /// The full roster (`SMSG_GROUP_LIST`, sent on every membership change): `members` excludes
    /// the recipient's own row (`own_flags` carries theirs); `loot` is `None` for the empty "you
    /// left" shape and whenever the fetched list has no other members.
    GroupList {
        group_type: u8,
        own_flags: u8,
        members: Vec<GroupMemberEntry>,
        leader: u64,
        loot: Option<GroupLootInfo>,
    },
    /// The server's verdict on a group command we issued — invite/leave
    /// (`SMSG_PARTY_COMMAND_RESULT`); `operation` is a [`crate::messages::party_operation`] code,
    /// `result` a [`crate::messages::party_result`] code, `member` the named player (may be empty).
    PartyCommandResult {
        operation: u32,
        member: String,
        result: u32,
    },
    /// A party/raid member's live stats for the frame (`SMSG_PARTY_MEMBER_STATS`/`_FULL` — `full`
    /// distinguishes the delta form from the ask-once full form our own
    /// [`crate::messages::request_party_member_stats`] pulls, and the offline-miss reply).
    PartyMemberStats {
        guid: u64,
        full: bool,
        info: Box<PartyMemberStatsInfo>,
    },
    /// Someone pinged the minimap (`MSG_MINIMAP_PING`) — the group ping marker.
    MinimapPing { guid: u64, x: f32, y: f32 },
    /// One raid-target icon changed (`MSG_RAID_TARGET_UPDATE`, delta shape) — `guid == 0` clears it.
    RaidTargetSet { icon: u8, guid: u64 },
    /// The full current raid-target icon set (`MSG_RAID_TARGET_UPDATE`, full-list shape) — only
    /// currently-set icons are present.
    RaidTargetList { entries: Vec<(u8, u64)> },
    /// The raid leader started a ready check (`MSG_RAID_READY_CHECK`, empty body).
    ReadyCheckRequest,
    /// One member's ready-check answer, forwarded to the leader only (`MSG_RAID_READY_CHECK`,
    /// non-empty body).
    ReadyCheckAnswer { guid: u64, ready: u8 },
    /// Our saved raid lockouts (`SMSG_RAID_INSTANCE_INFO`) — the Raid tab's Raid Info panel
    /// (decision 1549). Replaces the list wholesale; empty is the normal answer.
    RaidInstanceInfo {
        entries: Vec<crate::messages::RaidInstanceEntry>,
    },
    /// A duel challenge (`SMSG_DUEL_REQUESTED`, decision 0633) — delivered to challenger and
    /// challenged alike. `arbiter` is the duel-flag GameObject that identifies the duel and is
    /// echoed on accept/cancel; `challenger` equal to our own guid means we are the one asking.
    DuelRequested { arbiter: u64, challenger: u64 },
    /// We left the duel-flag bubble (`SMSG_DUEL_OUTOFBOUNDS`) — the 10 s forfeit timer runs.
    DuelOutOfBounds,
    /// We are back inside the bubble (`SMSG_DUEL_INBOUNDS`) — the forfeit timer is cleared.
    DuelInBounds,
    /// The duel ended (`SMSG_DUEL_COMPLETE`). `started` is false only when it never began.
    DuelComplete { started: bool },
    /// The duel's outcome line (`SMSG_DUEL_WINNER`), broadcast to everyone nearby: `fled` picks
    /// the retreat template over the knockout one.
    DuelWinner {
        fled: bool,
        winner: String,
        loser: String,
    },
    /// Start the duel countdown (`SMSG_DUEL_COUNTDOWN`) — already in whole seconds.
    DuelCountdown { seconds: u32 },
    /// Another player's honor stats came back (`MSG_INSPECT_HONOR_STATS` reply, decision 1512) —
    /// the inspect window's Honor tab. Arrives only in answer to our own ask, and a *refused* ask
    /// is answered with silence (the server returns without sending), so a consumer must not
    /// treat "no event yet" as "the data is coming".
    InspectHonorStats(InspectHonorStats),
    /// A kill paid out honor (`SMSG_PVP_CREDIT`) — the honor-gain chat line and floating combat
    /// text. Also fires for a **dishonorable** kill, where `honor` is negative.
    PvpCredit(PvpCredit),
    /// One mirror timer started, or was wholly re-stated (`SMSG_START_MIRROR_TIMER`, decision
    /// 0874) — the breath / fatigue bars. There is no separate update opcode: the server re-sends
    /// this whole packet whenever direction, remaining time or frozen state changes, so a
    /// consumer must treat it as idempotent re-statement, not only as a first appearance.
    MirrorTimerStart(MirrorTimerStart),
    /// Freeze/unfreeze one running mirror timer (`SMSG_PAUSE_MIRROR_TIMER`). vmangos never sends
    /// it on purpose — see [`crate::messages::opcode::SMSG_PAUSE_MIRROR_TIMER`].
    MirrorTimerPause { kind: u32, paused: bool },
    /// One mirror timer is over (`SMSG_STOP_MIRROR_TIMER`) — hide its bar. Sent both when the
    /// condition ends *well* (surfacing refills the bar to full, and the server stops it there)
    /// and when it stops applying at all (death, leaving the water).
    MirrorTimerStop { kind: u32 },
    /// The whole friend list (`SMSG_FRIEND_LIST`, decision 0668) — pushed unasked at login and
    /// again for every `CMSG_FRIEND_LIST`. Always the complete list, never a delta, so it
    /// replaces whatever is held. Names are NOT on the wire (see [`crate::messages::social`]);
    /// the consumer resolves them through its name cache.
    FriendList { friends: Vec<FriendEntry> },
    /// The whole ignore list (`SMSG_IGNORE_LIST`) — the same replace-everything shape, guids only.
    IgnoreList { guids: Vec<u64> },
    /// One friend/ignore result (`SMSG_FRIEND_STATUS`): both the ack for an add/remove **and**
    /// the broadcast a friend's login/logout sends to everyone listing them. The
    /// [`friend_result`](crate::messages::friend_result) code says which, and carries the
    /// presence tail for the two online flavours.
    FriendStatus(FriendStatusUpdate),
    /// A `/who` answer (`SMSG_WHO`) — at most 49 rows, plus the true match total.
    WhoResults(WhoResults),
    /// One guild's public identity (`SMSG_GUILD_QUERY_RESPONSE`) — name, its ten rank names,
    /// tabard. The guild twin of the name query: an ask-once cache fill keyed by guild id, so a
    /// consumer holds these by [`GuildQueryResponse::guild_id`] and a roster row or a chat line
    /// can name its guild late rather than never.
    GuildQueryResponse(GuildQueryResponse),
    /// The whole guild (`SMSG_GUILD_ROSTER`) — MOTD, info text, per-rank rights, every member.
    /// Always a complete snapshot, never a delta, so it *replaces* whatever is held; the server
    /// re-sends it for anything worth showing (a note edit, a rank-rights change, a promotion).
    GuildRoster(GuildRoster),
    /// One thing that happened in the guild (`SMSG_GUILD_EVENT`) — a
    /// [`guild_event`](crate::messages::guild_event) id plus its already-formatted arguments. The
    /// arguments are the display text; the optional guid rides only on the sign-on/off pair.
    GuildEvent(GuildEventNotice),
    /// The server's verdict on a guild verb we sent (`SMSG_GUILD_COMMAND_RESULT`). Carry the
    /// command tag through to the display: it is what tells the two meanings of result `0x08`
    /// apart (see [`guild_command_error`](crate::messages::guild_command_error)).
    GuildCommandResult(GuildCommandResult),
    /// Somebody asked us into their guild (`SMSG_GUILD_INVITE`) — the popup. Answered with
    /// `CMSG_GUILD_ACCEPT`/`_DECLINE`, neither of which says *which* invitation it means, so the
    /// pending invitation is the consumer's own state.
    GuildInvite { inviter: String, guild: String },
    /// The player we invited turned it down (`SMSG_GUILD_DECLINE`) — delivered to the inviter only.
    GuildDecline { name: String },
    /// The guild's "founded on / N members / N accounts" summary (`SMSG_GUILD_INFO`) — a separate
    /// ask from the roster, sharing no fields with it.
    GuildInfo(GuildInfo),
    /// The taxi map (`SMSG_SHOWTAXINODES`, decision 0484): `flightmaster` is the NPC the menu
    /// opened on, `nearest_node` the node it sits at, `known_mask` the full known-node bitmask
    /// ([`TaxiMask::is_known`]). The wire's window-framing constant carries no state and is
    /// dropped here.
    TaxiNodesShown {
        flightmaster: u64,
        nearest_node: u32,
        known_mask: TaxiMask,
    },
    /// A taxi node's known status (`SMSG_TAXINODE_STATUS`) — answers
    /// `CMSG_TAXINODE_STATUS_QUERY`, and also rides a first-visit "learn" alongside
    /// [`Self::NewTaxiPath`] (vmangos `SendLearnNewTaxiNode`). `guid` names the flight master
    /// asked about; `known` is whether the nearest node to it is now in our taxi mask.
    TaxiNodeStatus { guid: u64, known: bool },
    /// The server's verdict on `CMSG_ACTIVATETAXI`/`CMSG_ACTIVATETAXIEXPRESS`
    /// (`SMSG_ACTIVATETAXIREPLY`): `code` is a [`crate::messages::taxi_reply`] value — `0` OK,
    /// everything else a refusal (no flight starts, and no mount/spline packets follow).
    ActivateTaxiReply { code: u32 },
    /// `SMSG_NEW_TAXI_PATH` — empty body, rides a first-visit "learn" alongside
    /// [`Self::TaxiNodeStatus`]. No payload to carry; modelled so the wire isn't silently dropped.
    NewTaxiPath,
    /// `SMSG_MAIL_LIST_RESULT` — the inbox page, answering `CMSG_GET_MAIL_LIST` (decision 0544
    /// P0's wire layer; the inbox/open-mail window is P1).
    MailList { mails: Vec<MailListEntry> },
    /// `SMSG_SEND_MAIL_RESULT` — the verdict on a send/take-money/take-item/return/delete action;
    /// `equip_error`/`item` are the two mutually-exclusive conditional tails (decision 0544).
    SendMailResult {
        mail_id: u32,
        action: u32,
        error: u32,
        equip_error: Option<u32>,
        item: Option<(u32, u32)>,
    },
    /// `SMSG_PAGE_TEXT_QUERY_RESPONSE` — one page of a book, answering `CMSG_PAGE_TEXT_QUERY`.
    /// The ask-once page cache both readables reach: a readable item template's `PageText` and a
    /// `GAMEOBJECT_TYPE_TEXT` object's `data[0]` (decision 1105). `next_page_id == 0` ends the
    /// chain; vmangos pushes the whole chain in answer to the first query.
    PageText {
        page_id: u32,
        text: String,
        next_page_id: u32,
    },
    /// `SMSG_ITEM_TEXT_QUERY_RESPONSE` — a letter's body text, answering `CMSG_ITEM_TEXT_QUERY`
    /// (a mail's nonzero `item_text_id` triggers the ask-once fetch; decision 0544).
    MailItemText { text_id: u32, text: String },
    /// `SMSG_RECEIVED_MAIL` — a mail arrived (instant for text-only, on the delivery timer's
    /// expiry otherwise). `seconds` is the delay until it is "waiting", in the pending-mail
    /// countdown's units (vmangos only ever sends `0.0` = now; decision 0913).
    ReceivedMail { seconds: f32 },
    /// `MSG_QUERY_NEXT_MAIL_TIME`'s reply (same opcode as our empty-body request): `0.0` = unread
    /// mail waiting, `-86400.0` = none. Drives `HasNewMail()`/`UPDATE_PENDING_MAIL` (decision 0544
    /// P3).
    NextMailTime { seconds: f32 },
    /// `MSG_AUCTION_HELLO`'s reply — the auctioneer we greeted plus its `AuctionHouse.dbc` row
    /// (1..7, the deposit/cut rates). This packet, not our send, is what opens the auction window
    /// (decision 1511 P0's wire layer; the window itself is a later phase).
    AuctionHello { auctioneer: u64, house_id: u32 },
    /// `SMSG_AUCTION_COMMAND_RESULT` — the verdict on a sell/cancel/bid, keyed by `action`
    /// ([`crate::messages::auction_action`]) and `error` ([`crate::messages::auction_error`]);
    /// `tail` is the conditional payload the error selects. `auction_id` is `0` on most failures,
    /// and several refusals send no packet at all (decision 1511).
    AuctionCommandResult {
        auction_id: u32,
        action: u32,
        error: u32,
        tail: AuctionCommandTail,
    },
    /// `SMSG_AUCTION_LIST_RESULT` — a Browse page (max 50 rows). `total_count` is the pre-cap
    /// match count the pager needs, and it rides at the END of the wire body.
    AuctionListResult {
        auctions: Vec<AuctionListEntry>,
        total_count: u32,
    },
    /// `SMSG_AUCTION_OWNER_LIST_RESULT` — the Auctions tab page (our own listings). Same frame
    /// and record as [`Self::AuctionListResult`].
    AuctionOwnerListResult {
        auctions: Vec<AuctionListEntry>,
        total_count: u32,
    },
    /// `SMSG_AUCTION_BIDDER_LIST_RESULT` — the Bid tab page. Same frame and record as
    /// [`Self::AuctionListResult`]; the server emits the explicitly-refreshed ids first and then
    /// our live bids, so one row can appear twice in a page.
    AuctionBidderListResult {
        auctions: Vec<AuctionListEntry>,
        total_count: u32,
    },
    /// `SMSG_AUCTION_BIDDER_NOTIFICATION` — we won, or we were outbid (`bid_or_zero == 0` means
    /// **won**, not "no bid").
    AuctionBidderNotification(AuctionBidderNotification),
    /// `SMSG_AUCTION_OWNER_NOTIFICATION` — our own auction sold or took a bid. A deliberately
    /// different shape from the bidder notice: no house id, guid fourth, and an all-zero
    /// `bidder_guid` is the "sold" signal.
    AuctionOwnerNotification(AuctionOwnerNotification),
    /// `SMSG_AUCTION_REMOVED_NOTIFICATION` — an auction we had bid on was cancelled by its seller.
    AuctionRemovedNotification {
        auction_id: u32,
        item_entry: u32,
        random_property_id: i32,
    },
    /// `SMSG_TRADE_STATUS` — one pulse of the player-trade state machine (open/accept/cancel/
    /// complete + the refusal reasons). The app drives the trade window and the auto-`BEGIN_TRADE`
    /// reply off this (decision 0592; the window itself is P1).
    TradeStatus { status: TradeStatus },
    /// `SMSG_TRADE_STATUS_EXTENDED` — the item/gold snapshot for one window side (our own or the
    /// partner's, per [`TradeStatusExtended::their_window`]); pushed whenever that side changes.
    /// Boxed (~460 bytes) so it doesn't bloat every `SessionEvent`.
    TradeStatusExtended { state: Box<TradeStatusExtended> },
    /// The world-state table changed. `SMSG_INIT_WORLD_STATES` carries the whole table for a zone
    /// (`states` is the wire run verbatim, terminator pair included); `SMSG_UPDATE_WORLD_STATE`
    /// arrives as a one-entry `states` with `scope: None`, so both wires land on one event and the
    /// app has a single write path — the reference likewise funnels both into the one setter.
    /// `scope` is the init packet's `(map, zone)` dwords; nothing consumes them yet.
    WorldStates {
        scope: Option<(u32, u32)>,
        states: Vec<(u32, u32)>,
    },
}

/// The result of polling the reader for the next packet's events.
pub enum Poll {
    /// A packet decoded into these events. `events` is **empty** for an opcode we parse but do not
    /// model — a third outcome, distinct from both "decoded into something" and [`Poll::Skipped`],
    /// and until decision 0624 it was invisible from outside: no event, no skip, and the inbound
    /// census counted it like any other packet. A relayed move landing in an unmodelled opcode was
    /// therefore indistinguishable from one that never arrived. `opcode` rides along so the net
    /// thread can name what actually came off the wire.
    Events {
        opcode: u16,
        events: Vec<SessionEvent>,
    },
    /// An unparseable packet was skipped — kept the stream aligned, not an error. Carries the
    /// opcode (for the app's dropped-packet tally) and a short description (opcode + error + a hex
    /// preview) so the net thread can log *which* packet was dropped.
    Skipped { opcode: u16, reason: String },
}

mod decode;
pub use decode::decode;
