//! **The drain's parameters, by name** (decision 2279, 2265 §A1a).
//!
//! [`super::apply_net_updates`] takes every resource the 273 arms write, and Bevy caps a system
//! at sixteen parameters (`bevy_ecs` `system_param.rs`, `all_tuples!(…, 0, 16)`). From day one
//! (0006) the escape was tuples nested up to six deep, addressed by coordinate — `audio.15 .6`,
//! `ui_actions.1 .5` — with fifty comments across the tree explaining that a thing rides where it
//! rides "because this is where the ceiling left room". The `SystemParam` derive emits a named
//! struct with no field cap, so each family below is a struct: the body destructures it by field
//! name, every use site reads as what it is, and a new resource joins the family it belongs to
//! rather than the tuple with a slot free.
//!
//! **Grouping is by what the arms do with a thing**, not by what it is: a session-lifecycle edge,
//! a window's parked state, the action bar's queues, an animation-bearing message, a read-only
//! catalog. Two families the tuples had mis-homed for want of room moved to where they belong
//! (the catalogs, and the queries); one parameter the tuples carried nobody read
//! (`PingShared` — the drain stopped clearing it with B346) is gone. Nothing else changed: the
//! same set of resources, the same access, one system.
//!
//! Splitting the drain itself — each family owning a reader system of its own — is 2265 §A1(b),
//! and starts from these names.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::super::{
    AiReactionMessage, CharActionResultMessage, CharListMessage, EmoteMessage, EnteredWorldMessage,
    LoggedOutMessage, ObjectStore, PendingTransfer, PetDismissSoundMessage, PetTalkMessage,
    RemoteMotion, SelfPlayer, ServerSoundMessage, ServerTime, ServerWallClock, TeleportMessage,
    UnitMoveModes, WorldportMessage,
};
use benilla_world::weather::WeatherMessage;

/// The clocks: the server's two, and our real one.
#[derive(SystemParam)]
pub(crate) struct Clocks<'w> {
    /// The in-game day/night clock the lighting reads (`SMSG_LOGIN_SETTIMESPEED`).
    pub server_time: ResMut<'w, ServerTime>,
    /// The wall clock the absolute descriptor stamps are dated in (decision 1150). A different
    /// quantity from the one above, from the same handler file.
    pub wall_clock: ResMut<'w, ServerWallClock>,
    /// `Time<Real>`, NOT the default virtual one, and every reader here is the reason: a span
    /// the SERVER sends us (an aura's remaining duration, the corpse-reclaim delay, a relayed
    /// mover's fire-time) counts down in real seconds, so it must be stamped and read on a real
    /// clock. `Time<Virtual>` clamps at 250 ms per frame (`max_delta`), which is right for
    /// simulation — a 2 s hitch must not teleport animations — and wrong here: every long frame
    /// would quietly ADD that much to every buff timer, so an aura would vanish with seconds
    /// still showing on its clock. Measured on a hitchy run: the virtual clock lost 20 s against
    /// real in 33 s (decision 0846). The relayed-move replay (0601/0615) reads the same clock in
    /// `drain_pending_moves`/`extrapolate_remote_units`.
    pub real: Res<'w, Time<Real>>,
}

/// Every entity query the drain holds — what it reads or writes on streamed objects.
#[derive(SystemParam)]
pub(crate) struct ObjectQueries<'w, 's> {
    pub transforms: Query<'w, 's, &'static mut Transform>,
    pub stores: Query<'w, 's, &'static mut ObjectStore>,
    /// A relayed player's dead-reckon.
    pub remote: Query<'w, 's, &'static mut RemoteMotion>,
    /// Any unit's server-granted movement modes (decision 1780).
    pub modes: Query<'w, 's, &'static mut UnitMoveModes>,
    /// The deck a unit is standing on and its pose there (decision 1936).
    pub riders: Query<'w, 's, &'static mut crate::transport::TransportRider>,
    /// READ-only: every write to a mover's speed set goes through the drain's
    /// `objects::SpeedStage` and lands in one flush at the end (decision 1478).
    pub speeds: Query<'w, 's, &'static super::super::UnitSpeeds>,
    /// The armed-transport lens for the worldport's spare predicate (decision 0455: a boat whose
    /// path touches the destination map survives the purge).
    pub transports: Query<'w, 's, &'static crate::transport::Transport>,
    /// The cast-state read the spell-id-keyed `Casting` reap needs (decision 0107).
    pub casting: Query<'w, 's, &'static crate::creature_anim::Casting>,
    /// Are *we* already swinging? The ref's `[player+0xc48]`, mirrored by the server-echoed
    /// [`crate::creature_anim::Engaged`] — read by the GO handler's deferred auto-attack start
    /// (`0x6e83e7`, decision 1593). Filter-only, so it conflicts with nothing else in the drain.
    pub engaged_self: Query<'w, 's, (), (With<crate::creature_anim::Engaged>, With<SelfPlayer>)>,
    /// The per-field descriptor edges the merge reports (decision 2297) — the reference's
    /// `CMirrorHandler` notify pass, emitted from the one write that holds both sides.
    pub field_changes: MessageWriter<'w, super::super::FieldChanged>,
}

/// The session lifecycle: the glue-screen edges, the player's own teleports and mover
/// handshakes, and the entry/transfer state those edges write (decision 0193).
#[derive(SystemParam)]
pub(crate) struct Session<'w> {
    pub teleports: MessageWriter<'w, TeleportMessage>,
    pub worldports: MessageWriter<'w, WorldportMessage>,
    /// The two park publications: the realm list and the character roster are the same thing
    /// one park apart, each published so the app's policy can answer it.
    pub char_lists: MessageWriter<'w, CharListMessage>,
    pub realm_lists: MessageWriter<'w, crate::net::RealmListMessage>,
    /// The pick park's two verdicts: what a create/delete came back with, and the refusal of the
    /// pick itself.
    pub char_actions: MessageWriter<'w, CharActionResultMessage>,
    pub char_login_failures: MessageWriter<'w, super::super::CharacterLoginFailedMessage>,
    /// The world-entry pair, one edge: the entry message, and the `SMSG_ADDON_INFO` verdict that
    /// the entry's own UI load reads before the first addon's file-scope code asks
    /// `GetNumAddOns()` (2175).
    pub entered_world: MessageWriter<'w, EnteredWorldMessage>,
    pub addon_reply: ResMut<'w, crate::net::AddonInfoReply>,
    pub logged_out: MessageWriter<'w, LoggedOutMessage>,
    pub speed_changes: MessageWriter<'w, super::super::SpeedChangeMessage>,
    /// The two server-authored mover edges the controller both *applies* and *answers*: a
    /// granted mode (decisions 0308, 0866) and a knockback launch (decision 1702) are the same
    /// handshake, an edge the server may not act on until our own live pose comes back.
    pub move_modes: MessageWriter<'w, super::super::MoveModeMessage>,
    pub knockbacks: MessageWriter<'w, super::super::KnockBackMessage>,
    /// The login screen's dialog + reconnect-policy feed (decision 0539).
    pub login_stages: MessageWriter<'w, super::super::LoginStageMessage>,
    /// The login queue's position feed (decision 1681).
    pub login_queued: MessageWriter<'w, super::super::LoginQueuedMessage>,
    pub login_failures: MessageWriter<'w, super::super::LoginFailedMessage>,
    pub disconnects: MessageWriter<'w, super::super::DisconnectedMessage>,
    /// The server's own answer to a GM dot-command, readable rather than only logged — the probe
    /// shield's confirmation channel (decision 0677).
    pub server_said: MessageWriter<'w, super::super::ServerSaidMessage>,
    /// A `MSG_MOVE_*` the server addressed to OUR mover (decision 0725): a pose it wrote, with no
    /// handshake — the controller applies it in `player::wire_in`.
    pub self_moves: MessageWriter<'w, super::super::SelfMoveMessage>,
    /// The possession handoff (`SMSG_CLIENT_CONTROL_UPDATE`): control of a unit granted or
    /// revoked. Forwarded verbatim — only the controller knows the pose it would have to park.
    pub client_control: MessageWriter<'w, super::super::ClientControlMessage>,
    /// A cinematic to play (`SMSG_TRIGGER_CINEMATIC`) — `crate::cinematic` owns both the playback
    /// and the ack that has to answer it (decision 0196).
    pub cinematics: MessageWriter<'w, super::super::CinematicTriggeredMessage>,
    /// The far-teleport latch (decision 0455): `SMSG_TRANSFER_PENDING`'s transport block routes
    /// `NEW_WORLD`'s coordinates; cleared on disconnect.
    pub transfer: ResMut<'w, PendingTransfer>,
}

/// The windows' parked state: the ask-once caches and every `*Open`/`*State`/`*Session` a
/// `feed_*` pushes into the VM later (decision 0081). Where 195 of the 273 arms land.
#[derive(SystemParam)]
pub(crate) struct WindowStores<'w> {
    pub names: ResMut<'w, crate::names::NameCache>,
    pub items: ResMut<'w, crate::items::Items>,
    /// The client-local loot-target latch (the kneel's self trigger, decision 0515). The loot
    /// window's handlers own it since 2319 (`ui_loot::net`); it stays here for the one arm that
    /// still arms it — `SMSG_SPELL_GO` on a chest (decision 1477) — and leaves with the spells.
    pub loot_latch: ResMut<'w, crate::ui_loot::LootLatch>,
    pub chat_log: ResMut<'w, crate::ui_chat::ChatLog>,
    pub quest: ResMut<'w, crate::ui_quest::QuestGiver>,
    pub go_templates: ResMut<'w, crate::go_templates::GameObjectTemplates>,
    pub home_bind: ResMut<'w, crate::net::HomeBind>,
    pub proficiencies: ResMut<'w, crate::net::Proficiencies>,
    pub dropped: ResMut<'w, crate::net::DroppedOpcodes>,
    /// The death arc's wire-fed store (decision 0308): reclaim expiry, corpse location, resurrect
    /// offer, the spirit-healer confirm.
    pub death_net: ResMut<'w, crate::death::DeathNet>,
    /// The party/raid roster mirror + its composed system lines (decision 0434).
    pub group: ResMut<'w, crate::ui_party::GroupState>,
    /// The world-state table the NPC-text `$<n>w` tokens read.
    pub world_states: ResMut<'w, crate::world_state::WorldStates>,
    /// Read-only here since 2312 — the social handlers own it (`ui_social::net`); the chat arm
    /// still reads the ignore list.
    pub social: Res<'w, crate::ui_social::SocialState>,
    /// The pending logout/quit (decision 0674): the server's response and cancel-ack land here,
    /// and `crate::ui_logout` turns them into the countdown dialog.
    pub logout: ResMut<'w, crate::ui_logout::LogoutState>,
    /// The pet action bar's server-authoritative state + its own cooldown store (decision 0982),
    /// replaced wholesale on every `SMSG_PET_SPELLS`.
    pub pet_bar: ResMut<'w, crate::ui_pet::PetBar>,
    /// The by-key red error queue the pet bar's refused-order feedback rides (the `DisplayError`
    /// route, resolved through the VM's own GlobalStrings by `ui_action::feed_actions`).
    pub ui_error_keys: ResMut<'w, crate::ui_action::UiErrorKeys>,
    pub played_time_answer: ResMut<'w, crate::net::PlayedTimeAnswer>,
    pub tutorials: ResMut<'w, crate::tutorial::Tutorials>,
}

/// The action bar's family: the cast/cooldown state and every error queue the red line drains
/// (decision 0137), plus the item-lock bookkeeping the inventory-failure arm also drives
/// (decision 0216 §4 / 0218 §3 — the drain has no `UiScript` to fire `ITEM_LOCK_CHANGED`
/// through, so the transitioned slots queue in `LockTransitions` for the container feed).
#[derive(SystemParam)]
pub(crate) struct ActionStores<'w> {
    pub player_actions: ResMut<'w, crate::ui_action::PlayerActions>,
    /// The cast + mount error queues, both drained into the red error line by
    /// `ui_action::feed_actions`.
    pub cast_errors: ResMut<'w, crate::ui_action::CastErrors>,
    pub mount_errors: ResMut<'w, crate::ui_action::MountErrors>,
    /// The `modalNextSpell` chain's outbox (1597) — `cast_result` fills it, the ui_action drain
    /// sends it through the one cast path.
    pub chain_casts: ResMut<'w, crate::ui_action::ChainCasts>,
    /// The taming-refusal queue (decision 2039) — `SMSG_PET_TAME_FAILURE`'s reason byte. Its own
    /// queue rather than `UiErrorKeys` because its message needs TWO GlobalStrings lookups, and
    /// the inner one is only reachable at the drain (see the resource's doc).
    pub pet_tame_failures: ResMut<'w, crate::ui_action::PetTameFailures>,
    /// Spells learned mid-session, awaiting `LEARNED_SPELL_IN_TAB` (2252). A queue for the same
    /// reason the reference sorts before it fires: the tab index is only correct against the
    /// rebuilt tab list, which is the spellbook feed's, not this apply's.
    pub learned_in_tab: ResMut<'w, crate::ui_spellbook::LearnedInTab>,
    pub cast_bar: ResMut<'w, crate::ui_cast::CastBarFeed>,
    pub pending_cast: ResMut<'w, crate::ui_cast::PendingCast>,
    /// The player's cooldown store (decision 0137 phase 4); the pet's is in its bar, and
    /// `addressed_store` picks between them.
    pub cooldowns: ResMut<'w, crate::cooldowns::Cooldowns>,
    /// The live auto-repeat state the bar's flash rides (decision 0137 phase 4).
    pub auto_repeat: ResMut<'w, crate::ui_action::AutoRepeatActive>,
    /// Our own running channel (the IsCurrentAction channel leg, decision 0137 phase 4).
    pub active_channel: ResMut<'w, crate::ui_cast::ActiveChannel>,
    /// Pre-formatted UIErrorsFrame lines — text the wire already resolved, so there is no
    /// GlobalStrings key to look up: the death durability notice, `SMSG_NOTIFICATION`, and
    /// `SMSG_AREA_TRIGGER_MESSAGE`. Drained by `ui_action::feed_actions` beside `UiErrorKeys`.
    pub ui_error_texts: ResMut<'w, crate::ui_action::UiErrorTexts>,
    /// The queued on-next-swing strike (the melee-slot half of the cast tracking) — the wire
    /// resolves it here: GO fires it, a failing result/interrupt kills it.
    pub queued_melee: ResMut<'w, crate::ui_cast::QueuedMeleeSpell>,
    /// The aura feed's duration side-table (decisions 0255/0257): the self-only
    /// `SMSG_UPDATE_AURA_DURATION` lands here keyed by raw slot, timestamped on
    /// [`Clocks::real`] for the `ui_aura` slot-join.
    pub aura_durations: ResMut<'w, crate::ui_aura::AuraDurations>,
    /// The two talent spell-modifier tables (`crate::spell_mods`) — one wire packet is one
    /// absolute cell, and this is its only writer.
    pub spell_mods: ResMut<'w, crate::spell_mods::SpellModifiers>,
}

/// Every animation-bearing or audible message the drain emits, in packet order, plus the
/// PlayAnimation call-order counter they stamp (decisions 0107, 0137 phase 2, 0280).
#[derive(SystemParam)]
pub(crate) struct AnimWriters<'w> {
    pub server_sounds: MessageWriter<'w, ServerSoundMessage>,
    pub weather: MessageWriter<'w, WeatherMessage>,
    pub emotes: MessageWriter<'w, EmoteMessage>,
    pub swings: MessageWriter<'w, crate::creature_anim::SwingMessage>,
    pub cast_events: MessageWriter<'w, crate::creature_anim::CastEvent>,
    pub spell_go_targets: MessageWriter<'w, crate::creature_anim::SpellGoTargets>,
    /// The floating combat-text feed (decision 0137 phase 2).
    pub combat_text: MessageWriter<'w, crate::combat_text::CombatTextSpawn>,
    pub swing_impacts: MessageWriter<'w, crate::creature_anim::SwingImpact>,
    pub swing_flushes: MessageWriter<'w, crate::creature_anim::SwingFlush>,
    pub go_lid_open: MessageWriter<'w, crate::go_anim::GoLidOpen>,
    /// The aggro/alert vocal flare + the pushed-kit play (decision 0280).
    pub ai_reactions: MessageWriter<'w, AiReactionMessage>,
    pub kit_pushes: MessageWriter<'w, crate::creature_anim::KitPush>,
    /// The remote landing predictor's report (decision 0415): a relayed FALL_LAND fires the grunt
    /// + dust puff for an observed mover, the way the self controller does for us.
    pub hard_landings: MessageWriter<'w, crate::creature_anim::HardLanding>,
    /// An observed rider's flourish (`SMSG_MOUNTSPECIAL_ANIM`, decision 0441 P2) — the unit →
    /// mount-child hop happens in `creature_anim::flourish_to_anim`.
    pub mount_flourishes: MessageWriter<'w, crate::creature_anim::MountFlourish>,
    /// The UNIT_COMBAT event feed (the portrait hit indicator, decision 0576) + the
    /// COMBAT_TEXT_UPDATE feed (the center combat text, decision 0578) — the spell arms'
    /// self-facing twins of the floating-text spawn.
    pub unit_combat: MessageWriter<'w, crate::ui_unit::UnitCombatFeedback>,
    pub combat_text_events: MessageWriter<'w, crate::ui_unit::CombatTextEvent>,
    /// The sheath setter's queue, which the ATTACKERSTATEUPDATE arm's melee auto-draw writes.
    pub sheath_requests: MessageWriter<'w, crate::creature_anim::SheathRequest>,
    /// The GO one-shot Custom play (`SMSG_GAMEOBJECT_CUSTOM_ANIM` — the bobber's bite splash,
    /// decision 1086), the step-8 sibling of `go_lid_open`.
    pub go_custom_anim: MessageWriter<'w, crate::go_anim::GoCustomAnim>,
    /// The pet's bark (`SMSG_PET_ACTION_SOUND`, decision 2039) — `ai_reactions`' sibling: both
    /// are pure audio that resolves a guid and hands the sound layer a state for the SAME
    /// `0x623a40` dispatcher.
    pub pet_talk: MessageWriter<'w, PetTalkMessage>,
    pub pet_dismiss: MessageWriter<'w, PetDismissSoundMessage>,
    /// The swing-refusal seam's two edges (`crate::swing_refusal`, decision 2037): the four
    /// `SMSG_ATTACKSWING_*` refusals, and our own landed swing, which clears their latch. ONE
    /// writer for both so the drain hands them on in packet order.
    pub swing_refusals: MessageWriter<'w, crate::swing_refusal::SwingRefusalEdge>,
    /// The PlayAnimation call-order counter (`creature_anim::PlaySeq`): every animation-bearing
    /// message this drain emits stamps `next()`, in packet order.
    pub play_seq: ResMut<'w, crate::creature_anim::PlaySeq>,
}

/// The read-only reference tables the arms resolve against. Each DBC-backed one is `Option`:
/// absent if its table failed to load, and every reader degrades rather than drops.
#[derive(SystemParam)]
pub(crate) struct Catalogs<'w> {
    /// The FactionTemplate catalog the combat log's friend/foe split reads (B297). Absent, an
    /// unresolved unit lands on the friendly side of the classifier rather than losing its lines.
    pub factions: Option<Res<'w, crate::target::ring::Factions>>,
    /// The Spell.dbc catalog the cooldown/cast wire laws read.
    pub spells: Option<Res<'w, crate::ui_action::Spells>>,
    /// The combat-feedback CVars — the eight display ranges, `CombatLogPeriodicSpells`, and the
    /// three floating-text gates.
    pub combat_cvars: crate::ui_chat::combat::CombatFeedbackCvars<'w>,
    /// The EnvironmentalDamage.dbc 6-slot table (damage type → SpellVisualKit) the
    /// `SMSG_ENVIRONMENTALDAMAGELOG` arm reads — the fall-landing dust puff.
    pub env_damage: Option<Res<'w, crate::creature_anim::EnvDamageTable>>,
    /// The shared AreaTable catalog — the exploration arm's area-id → name resolve (decision
    /// 0828) — and the race-keyed discovery-jingle catalog (decision 0829).
    pub area_table: Option<Res<'w, crate::area::AreaTableRes>>,
    pub exploration_sounds: Option<Res<'w, crate::sound::ExplorationSounds>>,
}
