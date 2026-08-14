//! The per-frame wire→ECS bridge systems: [`apply_net_updates`] drains the inbound
//! [`SessionEvent`] channel into real entities (spawn/move/despawn, descriptor merges, splines,
//! teleports, clock), and [`tag_self_player`] marks our own streamed entity. The parent module
//! owns the channel/type surface; this module owns the event application.

use std::collections::HashMap;

use benilla_protocol::{ObjectFields, SessionEvent};
use bevy::prelude::*;

use super::{
    AiReactionMessage, CharActionResultMessage, CharListMessage, EmoteMessage, EnteredWorldMessage,
    Guid, GuidIndex, LoggedOutMessage, NetCommands, NetEvents, NetStatus, ObjectStore,
    PendingTransfer, RemoteMotion, Reputations, SelfGuid, SelfPlayer, ServerSoundMessage,
    ServerTime, ServerWallClock, TeleportMessage, WorldportMessage,
};
use benilla_world::weather::WeatherMessage;

mod anim;
mod chat;
mod combat;
mod combat_log;
mod death;
mod group;
mod loot;
mod mail;
mod mount;
mod names;
mod npc;
mod objects;
mod pet;
mod quests;
mod session;
mod spells;
mod trade;
mod world;

// The arm families, split out of the dispatch match below (each `pub(super)` fn is one arm's
// body; the match stays the dispatcher, one call per arm — see the child modules).
use loot::{
    inventory_failure, item_push_result, item_template, loot_all_passed, loot_clear_money,
    loot_error, loot_money_notify, loot_release_response, loot_removed, loot_response, loot_roll,
    loot_roll_won, loot_start_roll,
};
use quests::{
    quest_complete, quest_detail, quest_failed, quest_giver_failed, quest_giver_invalid,
    quest_giver_status, quest_greeting, quest_log_full, quest_objective_item, quest_objective_kill,
    quest_objectives_complete, quest_offer, quest_progress, quest_template,
};
use spells::{
    action_buttons, aura_duration, cancel_auto_repeat, cast_result, channel_start, channel_update,
    clear_cooldown, cooldown_cheat, cooldown_event, item_cooldown, learned_spell, spell_book,
    spell_chain_targets, spell_cooldowns, spell_delayed, spell_failed_other, spell_go, spell_start,
    superceded_spell,
};

/// Which unit's cooldown store a wire cooldown packet addresses (decision 0982).
///
/// All four of them (`SMSG_SPELL_COOLDOWN`, `_COOLDOWN_EVENT`, `_CLEAR_COOLDOWN`,
/// `_COOLDOWN_CHEAT`) carry a caster guid, and until the pet bar existed all four answered it the
/// same way: "is it us? then apply, else drop" — four copies of a self-only assumption, each
/// inside its own arm. Since the server sends a pet's cooldowns on the pet's guid, that
/// assumption silently discarded every one of them. Resolving the guid ONCE, here, is what let the
/// pet bar sweep for real without a second copy of any arm; it also matches the reference, whose
/// `SMSG_COOLDOWN_CHEAT` handler wipes "the self/pet cooldown list" off exactly this test.
///
/// `None` = a guid we hold no store for (another player's pet, a stale packet): dropped, as the
/// client drops an unknown guid.
fn addressed_store<'a>(
    caster: u64,
    self_guid: &SelfGuid,
    player: &'a mut crate::cooldowns::Cooldowns,
    pet: &'a mut crate::ui_pet::PetBar,
) -> Option<&'a mut crate::cooldowns::Cooldowns> {
    if self_guid.0 == Some(caster) {
        Some(player)
    } else if pet.has_bar() && pet.spells.pet_guid == caster {
        Some(&mut pet.cooldowns)
    } else {
        None
    }
}

// ── The per-frame bridge systems ─────────────────────────────────────────────────────────────────

/// Drain the inbound event channel and mutate real ECS entities: spawn on create, move existing,
/// despawn on remove, attach/clear movement splines, and surface teleport/worldport/clock changes.
// The tuple params below batch resources to stay under Bevy's 16-SystemParam ceiling; clippy reads
// the 5-element ResMut tuples as "very complex types", but a named alias per tuple would be less
// legible than the inline, commented groups.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn apply_net_updates(
    mut commands: Commands,
    events: Res<NetEvents>,
    mut index: ResMut<GuidIndex>,
    mut self_guid: ResMut<SelfGuid>,
    mut status: ResMut<NetStatus>,
    // The two server clocks, paired in one param (the 16-SystemParam ceiling this signature
    // already lives against): the in-game day/night clock the lighting reads, and the wall clock
    // the absolute descriptor stamps are dated in (decision 1150). Different quantities, same
    // handler file.
    mut clocks: (ResMut<ServerTime>, ResMut<ServerWallClock>),
    mut reputations: ResMut<Reputations>,
    mut transforms: Query<&mut Transform>,
    mut stores: Query<&mut ObjectStore>,
    mut remote_motion: Query<&mut RemoteMotion>,
    // One tuple param (the 16-SystemParam ceiling): the session-lifecycle one-shot writers — the
    // player's teleport/worldport snaps + the glue-screen edges (decision 0193).
    session_msgs: (
        MessageWriter<TeleportMessage>,
        MessageWriter<WorldportMessage>,
        MessageWriter<CharListMessage>,
        MessageWriter<CharActionResultMessage>,
        MessageWriter<EnteredWorldMessage>,
        MessageWriter<LoggedOutMessage>,
        MessageWriter<super::SpeedChangeMessage>,
        // The ack'd movement-mode family (decisions 0308, 0866): a mode the server granted our
        // mover, which the player controller applies and answers with its live pose.
        MessageWriter<super::MoveModeMessage>,
        // The login screen's dialog + reconnect-policy feed (decision 0539).
        MessageWriter<super::LoginStageMessage>,
        MessageWriter<super::LoginFailedMessage>,
        MessageWriter<super::DisconnectedMessage>,
        // The server's own answer to a GM dot-command, readable rather than only logged — the
        // probe shield's confirmation channel (decision 0677).
        MessageWriter<super::ServerSaidMessage>,
        // A `MSG_MOVE_*` the server addressed to OUR mover (decision 0725): a pose it wrote, with
        // no handshake — the controller applies it in `player::wire_in`.
        MessageWriter<super::SelfMoveMessage>,
    ),
    // One tuple param (the 16-SystemParam ceiling): the ask-once query caches + the gossip/merchant
    // state the net drain fills for the NPC-interaction windows (decision 0081).
    caches: (
        ResMut<crate::names::NameCache>,
        ResMut<crate::items::Items>,
        ResMut<crate::ui_gossip::GossipState>,
        ResMut<crate::ui_merchant::MerchantOpen>,
        ResMut<crate::ui_trainer::TrainerOpen>,
        // Nested triple (the tuple is at the 16-param ceiling): the loot window state, the
        // client-local loot-target latch (the kneel's self trigger, decision 0515), and the open
        // group-loot rolls (decision 0591).
        (
            ResMut<crate::ui_loot::LootState>,
            ResMut<crate::ui_loot::LootLatch>,
            ResMut<crate::ui_loot_roll::LootRolls>,
        ),
        ResMut<crate::ui_chat::ChatLog>,
        ResMut<crate::ui_quest::QuestGiver>,
        ResMut<crate::ui_quest_log::QuestLog>,
        ResMut<crate::go_templates::GameObjectTemplates>,
        ResMut<crate::net::HomeBind>,
        ResMut<crate::net::Proficiencies>,
        ResMut<crate::net::DroppedOpcodes>,
        // The death arc's wire-fed store (decision 0308): reclaim expiry, corpse location,
        // resurrect offer, the spirit-healer confirm.
        ResMut<crate::death::DeathNet>,
        // The party/raid roster mirror + its composed system lines (decision 0434).
        ResMut<crate::ui_party::GroupState>,
        // The taxi-map session (decision 0484 phase 1) + the mailbox session + its login-scoped
        // arrival countdown (decision 0544) + the player-trade session (decision 0592) + the bank
        // session and its purchase-refusal queue (decision 0604) + the world-state table the
        // NPC-text `$<n>w` tokens read + the duel session (decision 0633), grouped to stay under
        // Bevy's 16-SystemParam ceiling (this tuple's 16th and last slot).
        (
            ResMut<crate::ui_taxi::TaxiState>,
            ResMut<crate::ui_mail::MailOpen>,
            ResMut<crate::ui_mail::MailPending>,
            ResMut<crate::ui_trade::TradeSession>,
            ResMut<crate::ui_bank::BankOpen>,
            ResMut<crate::ui_bank::BankErrors>,
            ResMut<crate::world_state::WorldStates>,
            ResMut<crate::ui_duel::DuelState>,
            ResMut<crate::ui_social::SocialState>,
            // The pending logout/quit (decision 0674): the server's response and cancel-ack land
            // here, and `crate::ui_logout` turns them into the countdown dialog.
            ResMut<crate::ui_logout::LogoutState>,
            // The shared AreaTable catalog — the exploration arm's area-id → name resolve
            // (decision 0828) — and the race-keyed discovery-jingle catalog (decision 0829).
            // Either absent if its DBC failed to load.
            Option<Res<crate::area::AreaTableRes>>,
            Option<Res<crate::sound::ExplorationSounds>>,
            // The mirror-timer queue (decision 0874): the breath/fatigue START/PAUSE/STOP edges,
            // drained into the FrameXML bars by `crate::ui_mirror`.
            ResMut<crate::ui_mirror::MirrorTimerFeed>,
            // The pet action bar's server-authoritative state + its own cooldown store
            // (decision 0982), replaced wholesale on every `SMSG_PET_SPELLS`; and the by-key red
            // error queue its refused-order feedback rides (the `DisplayError` route, resolved
            // through the VM's own GlobalStrings by `ui_action::feed_actions`).
            ResMut<crate::ui_pet::PetBar>,
            ResMut<crate::ui_action::UiErrorKeys>,
            // The ask-once book-page cache (decision 1105) — every readable's text, keyed by
            // `PageText` id; the reader session repaints off it.
            // Paired into one slot: the group is itself at Bevy's 16-param ceiling, so a 17th

            // entry stops the whole system implementing `SystemParam` — nesting is the same

            // escape this tuple already is.
            (
                ResMut<crate::ui_item_text::PageTexts>,
                ResMut<crate::net::PlayedTimeAnswer>,
            ),
        ),
    ),
    // One tuple param (the 16-SystemParam ceiling again): the action-bar- + merchant-facing errors
    // and the cast-bar feed (decision 0137), plus the item-lock bookkeeping the inventory-failure
    // arm also drains (decision 0216 §4 / 0218 §3 — this apply site has no `UiScript` to fire
    // `ITEM_LOCK_CHANGED` through, so the transitioned slots queue in `LockClearedByFailure` for
    // the container feed to pick up).
    mut ui_actions: (
        ResMut<crate::ui_action::PlayerActions>,
        // Nested pair (the tuple is at the 16-param ceiling): the cast + mount error queues,
        // both drained into the red error line by `ui_action::feed_actions`.
        (
            ResMut<crate::ui_action::CastErrors>,
            ResMut<crate::ui_action::MountErrors>,
        ),
        ResMut<crate::ui_items::EquipErrors>,
        ResMut<crate::ui_merchant::MerchantErrors>,
        ResMut<crate::ui_loot::LootErrors>,
        ResMut<crate::ui_cast::CastBarFeed>,
        ResMut<crate::pending_item_ops::PendingItemOps>,
        ResMut<crate::pending_item_ops::LockClearedByFailure>,
        ResMut<crate::ui_trainer::TrainerErrors>,
        ResMut<crate::ui_cast::PendingCast>,
        // The cooldown store + the Spell.dbc catalog its wire laws read, and the live
        // auto-repeat state the bar's flash rides (decision 0137 phase 4).
        ResMut<crate::cooldowns::Cooldowns>,
        Option<Res<crate::ui_action::Spells>>,
        ResMut<crate::ui_action::AutoRepeatActive>,
        // Our own running channel (the IsCurrentAction channel leg, decision 0137 phase 4).
        ResMut<crate::ui_cast::ActiveChannel>,
        // Pre-formatted red error lines (the death durability notice — drained by the
        // container feed beside EquipErrors).
        ResMut<crate::ui_items::UiErrorLines>,
        // The queued on-next-swing strike (the melee-slot half of the cast tracking) — the
        // wire resolves it here: GO fires it, a failing result/interrupt kills it.
        ResMut<crate::ui_cast::QueuedMeleeSpell>,
    ),
    net_commands: Res<NetCommands>,
    // One tuple param (Bevy's 16-SystemParam ceiling): the audio + combat/cast bridge writers, the
    // cast-state read the spell-id-keyed `Casting` reap needs (decision 0107), and the floating
    // combat-text feed (decision 0137 phase 2).
    mut audio: (
        MessageWriter<ServerSoundMessage>,
        MessageWriter<WeatherMessage>,
        MessageWriter<EmoteMessage>,
        MessageWriter<crate::creature_anim::SwingMessage>,
        Query<&crate::creature_anim::Casting>,
        MessageWriter<crate::creature_anim::CastEvent>,
        MessageWriter<crate::creature_anim::SpellGoTargets>,
        MessageWriter<crate::combat_text::CombatTextSpawn>,
        MessageWriter<crate::creature_anim::SwingImpact>,
        MessageWriter<crate::creature_anim::SwingFlush>,
        MessageWriter<crate::go_anim::GoLidOpen>,
        // The aggro/alert vocal flare + the pushed-kit play (decision 0280).
        MessageWriter<AiReactionMessage>,
        MessageWriter<crate::creature_anim::KitPush>,
        // The remote landing predictor's report (decision 0415): a relayed FALL_LAND fires the
        // grunt + dust puff for an observed mover, the way the self controller does for us.
        MessageWriter<crate::creature_anim::HardLanding>,
        // An observed rider's flourish (`SMSG_MOUNTSPECIAL_ANIM`, decision 0441 P2) — the
        // unit → mount-child hop happens in `creature_anim::flourish_to_anim`.
        MessageWriter<crate::creature_anim::MountFlourish>,
        // The UNIT_COMBAT event feed (the portrait hit indicator, decision 0576) + the
        // COMBAT_TEXT_UPDATE feed (the center combat text, decision 0578) — the spell arms'
        // self-facing twins of the floating-text spawn — and the sheath setter's queue, which the
        // ATTACKERSTATEUPDATE arm's melee auto-draw writes into. Nested: the tuple is at the
        // 16-param ceiling.
        (
            MessageWriter<crate::ui_unit::UnitCombatFeedback>,
            MessageWriter<crate::ui_unit::CombatTextEvent>,
            MessageWriter<crate::creature_anim::SheathRequest>,
            // The GO one-shot Custom play (`SMSG_GAMEOBJECT_CUSTOM_ANIM` — the bobber's bite
            // splash, decision 1086), the step-8 sibling of the `GoLidOpen` writer above.
            MessageWriter<crate::go_anim::GoCustomAnim>,
        ),
    ),
    // The aura feed's duration side-table + the clock to stamp arrivals (decisions 0255/0257): the
    // self-only `SMSG_UPDATE_AURA_DURATION` lands here keyed by raw slot, timestamped for the
    // `ui_aura` slot-join — plus the ping clock the Pong arm measures round trips against.
    // Grouped as a tuple to stay under Bevy's 16-SystemParam ceiling.
    //
    // The stamp clock is `Time<Real>`, NOT the default virtual one, and both its readers
    // (`aura_duration`, `corpse_reclaim_delay`) are the reason: a span the SERVER sends us counts
    // down in real seconds, so it must be stamped and read on a real clock. `Time<Virtual>` clamps
    // at 250 ms per frame (`max_delta`), which is right for simulation — a 2 s hitch must not
    // teleport animations — and wrong here: every long frame would quietly ADD that much to every
    // buff timer, so an aura would vanish with seconds still showing on its clock. Measured on a
    // hitchy run: the virtual clock lost 20 s against real in 33 s (decision 0846).
    mut aura: (
        ResMut<crate::ui_aura::AuraDurations>,
        Res<Time<Real>>,
        Res<super::PingShared>,
        Query<&mut super::UnitSpeeds>,
        // The PlayAnimation call-order counter (`creature_anim::PlaySeq`): every
        // animation-bearing message this drain emits stamps `next()`, in packet order.
        ResMut<crate::creature_anim::PlaySeq>,
        // The EnvironmentalDamage.dbc 6-slot table (damage type → SpellVisualKit) the
        // `SMSG_ENVIRONMENTALDAMAGELOG` arm reads — the fall-landing dust puff.
        Option<Res<crate::creature_anim::EnvDamageTable>>,
        // The far-teleport latch + the armed-transport lens for the worldport's spare
        // predicate (decision 0455: a boat whose path touches the destination map survives
        // the purge; the TRANSFER_PENDING transport block routes NEW_WORLD's coordinates).
        ResMut<PendingTransfer>,
        Query<&crate::transport::Transport>,
        // The REAL-time clock the relayed-move replay runs on (decisions 0601/0615): a remote's
        // fire-time is stamped against this, and `drain_pending_moves`/`extrapolate_remote_units`
        // read the same clock. Virtual time's `max_delta` clamp falls behind real time under
        // occlusion throttling and would displace the whole replay schedule.
        Res<Time<bevy::time::Real>>,
    ),
) {
    // A `&mut` to the counter itself (deref-coerced through the `ResMut`), so the arms that stamp
    // it *conditionally* can take it by reference and only advance it when they emit.
    let play_seq: &mut crate::creature_anim::PlaySeq = &mut aura.4;
    let (
        mut names,
        mut items,
        mut gossip,
        mut merchant,
        mut trainer_open,
        (mut loot, mut loot_latch, mut loot_rolls),
        mut chat_log,
        mut quest,
        mut quest_log,
        mut go_templates,
        mut home_bind,
        mut proficiencies,
        mut dropped,
        mut death_net,
        mut group,
        (
            mut taxi,
            mut mail_open,
            mut mail_pending,
            mut trade_session,
            mut bank_open,
            mut bank_errors,
            mut world_states,
            mut duel,
            mut social,
            mut logout,
            area_table,
            exploration_sounds,
            mut mirror_timers,
            mut pet_bar,
            mut ui_error_keys,
            (mut page_texts, mut played_time_answer),
        ),
    ) = caches;
    let (
        mut teleports,
        mut worldports,
        mut char_lists,
        mut char_actions,
        mut entered_world,
        mut logged_out,
        mut speed_changes,
        mut move_modes,
        mut login_stages,
        mut login_failures,
        mut disconnects,
        mut server_said,
        mut self_moves,
    ) = session_msgs;
    // Descriptor seeds/deltas for objects created *earlier in this same drain* can't land on their
    // entities yet (the spawn `Command` hasn't run), so they accumulate here and flush once at the end.
    // This also removes a latent clobber: a plain per-delta `insert` on a not-yet-spawned entity would
    // overwrite an earlier partial rather than merge it (decision 0061).
    let mut pending: HashMap<u64, ObjectFields> = HashMap::new();
    for ev in events.0.try_iter() {
        match ev {
            SessionEvent::LoginStage { stage } => session::login_stage(stage, &mut login_stages),
            SessionEvent::LoginFailed {
                code,
                reason,
                terminal,
            } => session::login_failed(code, reason, terminal, &mut login_failures),
            SessionEvent::CharacterList { characters, realm } => {
                session::character_list(characters, realm, &mut status, &mut char_lists)
            }
            SessionEvent::CharActionResult { action, code } => {
                session::char_action_result(action, code, &mut char_actions)
            }
            SessionEvent::CinematicTriggered { cinematic_id } => {
                session::cinematic_triggered(cinematic_id, &net_commands)
            }
            SessionEvent::Connected {
                self_guid: guid,
                name,
            } => session::connected(
                guid,
                name,
                &mut self_guid,
                &mut status,
                &mut names,
                &mut entered_world,
            ),
            SessionEvent::LoggedOut => {
                session::logged_out(&mut commands, &mut index, &mut self_guid, &mut logged_out)
            }
            // The logout arc's two narration packets (decision 0674) — `crate::ui_logout` owns the
            // decision table; this is only the hand-off.
            SessionEvent::LogoutResponse { reason, instant } => {
                logout.apply_response(reason, instant)
            }
            SessionEvent::LogoutCancelled => logout.apply_cancelled(),
            SessionEvent::Disconnected { reason } => {
                session::disconnected(
                    reason,
                    &mut commands,
                    &mut index,
                    &self_guid,
                    &mut status,
                    &mut names,
                    &mut items,
                    &mut gossip,
                    &mut merchant,
                    &mut trainer_open,
                    &mut loot,
                    &mut loot_latch,
                    &mut loot_rolls,
                    &mut chat_log,
                    &mut quest,
                    &mut quest_log,
                    &mut death_net,
                    &mut group,
                    &mut taxi,
                    &mut mail_open,
                    &mut mail_pending,
                    &mut trade_session,
                    &mut bank_open,
                    &mut duel,
                    &mut social,
                    &mut aura.6,
                    &mut disconnects,
                );
            }
            SessionEvent::ObjectCreate {
                guid,
                kind,
                display_id,
                position,
                orientation,
                scale,
                speeds,
                transport_progress,
                transport,
                spline,
                fields,
            } => {
                death::note_corpse(guid, kind, &fields, &self_guid, &mut death_net);
                objects::object_create(
                    guid,
                    kind,
                    display_id,
                    position,
                    orientation,
                    scale,
                    speeds,
                    transport_progress,
                    transport,
                    spline,
                    fields,
                    &mut commands,
                    &mut index,
                    &mut transforms,
                    &mut stores,
                    &mut pending,
                    &mut names,
                    &mut go_templates,
                    &net_commands,
                )
            }
            SessionEvent::ItemCreate {
                guid,
                container,
                fields,
            } => objects::item_create(guid, container, fields, &mut items),
            SessionEvent::ObjectMove {
                guid,
                position,
                orientation,
            } => objects::object_move(
                guid,
                position,
                orientation,
                &mut commands,
                &index,
                &mut transforms,
            ),
            SessionEvent::UnitMove {
                guid,
                position,
                orientation,
                flags,
                pitch,
                time,
                heartbeat,
                fall_time,
                jump,
                transport,
            } => {
                // The scheduled-replay law (decisions 0601/0615): `unit_move` runs the mover's own
                // replay chain over this packet's wire stamp to get its client fire-time, then
                // applies it now if due, else queues it on the unit for `drain_pending_moves`.
                let now_ms = aura.8.elapsed_secs_f64() * 1000.0;
                objects::unit_move(
                    guid,
                    crate::net::motion::RelayMove {
                        wire_ms: time,
                        position,
                        orientation,
                        flags,
                        pitch,
                        fall_time,
                        jump,
                        transport,
                        heartbeat,
                    },
                    now_ms,
                    &mut commands,
                    &index,
                    &self_guid,
                    &mut remote_motion,
                    &mut transforms,
                    &mut audio.13,
                    &mut self_moves,
                );
            }
            SessionEvent::ObjectValues { guid, fields } => {
                objects::object_values(guid, fields, &index, &mut stores, &mut pending, &mut items)
            }
            SessionEvent::ObjectDestroyed(guid) => {
                death::forget_corpse(guid, &mut death_net);
                objects::object_destroyed(guid, &mut commands, &mut index, &mut items)
            }
            SessionEvent::ObjectsRemoved(guids) => {
                objects::objects_removed(guids, &mut commands, &mut index)
            }
            SessionEvent::MonsterMove {
                guid,
                start,
                spline_id,
                path,
                facing,
                stop,
                duration_ms,
                flying,
            } => objects::monster_move(
                guid,
                start,
                spline_id,
                path,
                facing,
                stop,
                duration_ms,
                flying,
                &mut commands,
                &index,
                &mut transforms,
            ),
            SessionEvent::Teleport {
                guid,
                counter,
                position,
                orientation,
            } => session::teleport(
                guid,
                counter,
                position,
                orientation,
                &self_guid,
                &mut teleports,
            ),
            SessionEvent::Worldport {
                map_id,
                position,
                orientation,
                needs_ack,
            } => session::worldport(
                map_id,
                position,
                orientation,
                needs_ack,
                &mut commands,
                &mut index,
                &mut aura.6,
                &aura.7,
                &mut worldports,
            ),
            SessionEvent::TransferPending {
                map_id,
                transport_entry,
            } => session::transfer_pending(map_id, transport_entry, &mut aura.6),
            SessionEvent::TransferAborted { reason } => {
                session::transfer_aborted(reason, &mut aura.6)
            }
            SessionEvent::TimeSpeed {
                hours,
                minutes,
                day_serial,
                timescale,
            } => session::time_speed(hours, minutes, day_serial, timescale, &mut clocks.0),
            SessionEvent::ServerUnixTime { unix_time } => {
                session::server_unix_time(unix_time, &mut clocks.1)
            }
            SessionEvent::Reputations { standings } => {
                session::reputations(standings, &mut reputations)
            }
            SessionEvent::ReputationDelta { standings } => {
                session::reputation_delta(standings, &mut reputations, &mut quest)
            }
            SessionEvent::ReputationVisible { list_id } => {
                session::reputation_visible(list_id, &mut reputations)
            }
            SessionEvent::BindPoint { area } => home_bind.0 = Some(area),
            SessionEvent::Proficiency {
                item_class,
                subclass_mask,
            } => {
                proficiencies.0.insert(item_class, subclass_mask);
            }
            SessionEvent::PlayerName {
                guid,
                name,
                race,
                class,
                gender,
            } => names::player_name(guid, name, race, class, gender, &mut names),
            SessionEvent::PetName { pet_number, name } => {
                names::pet_name(pet_number, name, &mut names)
            }
            SessionEvent::CreatureName {
                entry,
                name,
                subname,
                creature_type,
                pet_family,
                rank,
                type_flags,
                civilian,
                racial_leader,
            } => names::creature_name(
                entry,
                name,
                subname,
                creature_type,
                pet_family,
                rank,
                type_flags,
                civilian,
                racial_leader,
                &mut names,
            ),
            SessionEvent::GameObjectInfo {
                entry,
                type_id,
                display_id,
                name,
                data,
            } => {
                objects::gameobject_info(entry, type_id, display_id, name, &data, &mut go_templates)
            }
            SessionEvent::GameObjectCustomAnim { guid, anim_id } => {
                objects::gameobject_custom_anim(guid, anim_id, &mut audio.15 .3)
            }
            SessionEvent::FishNotHooked => loot::fish_verdict(false, &mut ui_error_keys),
            SessionEvent::FishEscaped => loot::fish_verdict(true, &mut ui_error_keys),
            SessionEvent::PlaySound { sound_id } => world::play_sound(sound_id, &mut audio.0),
            SessionEvent::PlayMusic { music_id } => world::play_music(music_id, &mut audio.0),
            SessionEvent::PlayObjectSound { sound_id, guid } => {
                world::play_object_sound(sound_id, guid, &index, &mut audio.0)
            }
            SessionEvent::Weather {
                weather_type,
                grade,
                sound_id,
                instant,
            } => world::weather(weather_type, grade, sound_id, instant, &mut audio.1),
            SessionEvent::TextEmote { guid, text_emote } => {
                anim::text_emote(guid, text_emote, &index, &mut audio.2)
            }
            SessionEvent::Emote { guid, emote_id } => {
                anim::emote(guid, emote_id, &index, &mut audio.2)
            }
            // The spell-book/action-bar pair → the action store the UI feed reads
            // (`crate::ui_action`), sent once at login (and the bar again on server-side edits).
            SessionEvent::SpellBook {
                spell_ids,
                cooldowns,
            } => spell_book(spell_ids, cooldowns, &mut ui_actions.0, &mut ui_actions.10),
            SessionEvent::ActionButtons { buttons } => action_buttons(buttons, &mut ui_actions.0),
            SessionEvent::SpellLearned { spell_id } => learned_spell(spell_id, &mut ui_actions.0),
            SessionEvent::SpellSuperceded {
                old_spell_id,
                new_spell_id,
            } => superceded_spell(old_spell_id, new_spell_id, &mut ui_actions.0),
            SessionEvent::CastResult {
                spell_id,
                success,
                reason,
            } => cast_result(
                spell_id,
                success,
                reason,
                &mut commands,
                &self_guid,
                &index,
                &mut ui_actions.1 .0,
                &audio.4,
                &mut audio.5,
                &mut ui_actions.5,
                &mut ui_actions.9,
                &mut ui_actions.15,
                &mut ui_actions.10,
                &mut ui_actions.12,
                ui_actions.11.as_deref(),
                &net_commands,
                play_seq.next(),
            ),
            SessionEvent::InventoryFailure {
                reason,
                required_level,
                item_guid,
                bag_slot,
            } => inventory_failure(
                reason,
                required_level,
                item_guid,
                bag_slot,
                &mut ui_actions.2,
                &mut ui_actions.6,
                &mut ui_actions.7,
            ),
            SessionEvent::Chat(m) => {
                chat::chat(m, &mut chat_log, &social, &net_commands, &mut server_said)
            }
            SessionEvent::ChannelNotify {
                notice,
                channel,
                tail,
            } => chat_log.push_channel_notice(notice, channel, &tail),
            SessionEvent::ChannelList {
                channel, members, ..
            } => chat::channel_list(channel, &members, &mut chat_log),
            SessionEvent::ChatPlayerNotFound { name } => {
                chat::chat_player_not_found(&name, &mut chat_log)
            }
            SessionEvent::ChatWrongFaction => chat::chat_wrong_faction(&mut chat_log),
            SessionEvent::Notification { text } => chat::notification(text, &mut chat_log),
            SessionEvent::AreaTriggerMessage { text } => {
                chat::area_trigger_message(text, &mut chat_log)
            }
            SessionEvent::PlayedTime { total, level } => {
                // BOTH halves, and they are not redundant. The chat breakdown is our stand-in for
                // the reference's `ChatFrame_DisplayTimePlayed`, which we do not ship; the mailbox
                // is what becomes `TIME_PLAYED_MSG(total, level)` for an addon that asked.
                played_time_answer.0 = Some((total, level));
                chat::played_time(total, level, &mut chat_log)
            }
            SessionEvent::RandomRoll {
                min,
                max,
                roll,
                guid,
            } => chat_log.push_roll(min, max, roll, guid),
            // ── The group/party family (decision 0434 §D2, superseded by 0440) — arm bodies in
            // `group` ──
            SessionEvent::GroupInvite { inviter } => {
                group::invited(&mut group, &mut chat_log, &inviter)
            }
            SessionEvent::GroupDecline { name } => {
                group::declined(&mut group, &mut chat_log, &name)
            }
            SessionEvent::GroupUninvited => group::uninvited(&mut group, &mut chat_log),
            SessionEvent::GroupLeaderChanged { name } => group::leader_changed(
                &mut group,
                &mut chat_log,
                &name,
                &self_guid,
                &mut names,
                &net_commands,
            ),
            SessionEvent::GroupDestroyed => group::destroyed(&mut group, &mut chat_log),
            SessionEvent::GroupList {
                group_type,
                own_flags,
                members,
                leader,
                loot,
            } => group::list(
                &mut group,
                &mut chat_log,
                &mut quest,
                group_type,
                own_flags,
                members,
                leader,
                loot,
            ),
            SessionEvent::PartyCommandResult {
                operation,
                member,
                result,
            } => group::command_result(&mut group, &mut chat_log, operation, &member, result),
            SessionEvent::PartyMemberStats { guid, full, info } => {
                group.apply_stats(guid, full, *info)
            }
            SessionEvent::RaidTargetSet { icon, guid } => group.apply_raid_target(icon, guid),
            SessionEvent::RaidTargetList { entries } => group.apply_raid_target_list(&entries),
            // Ping + ready-check are removed for now (decision 0460); the protocol still decodes
            // the wire, but the client ignores it until those features return.
            SessionEvent::MinimapPing { .. }
            | SessionEvent::ReadyCheckRequest
            | SessionEvent::ReadyCheckAnswer { .. } => {}
            // ── The duel family (decision 0633): the session mirror + the two DisplayError
            // lines the handlers emit inline; the Era events fire off the mirror's edges in
            // `ui_duel::feed_duel`, and the countdown ticks in its own system ──
            SessionEvent::DuelRequested {
                arbiter,
                challenger,
            } => crate::ui_duel::apply::requested(
                &mut duel,
                &mut chat_log,
                &net_commands,
                arbiter,
                challenger,
                self_guid.0,
                social.is_ignored(challenger),
            ),
            SessionEvent::DuelOutOfBounds => crate::ui_duel::apply::bounds(&mut duel, true),
            SessionEvent::DuelInBounds => crate::ui_duel::apply::bounds(&mut duel, false),
            SessionEvent::DuelComplete { started } => {
                crate::ui_duel::apply::complete(&mut duel, &mut chat_log, started);
            }
            SessionEvent::DuelWinner {
                fled,
                winner,
                loser,
            } => crate::ui_duel::apply::winner(&mut chat_log, fled, &winner, &loser),
            SessionEvent::DuelCountdown { seconds } => {
                crate::ui_duel::apply::countdown(&mut duel, seconds);
            }
            // ── The mirror timers (decision 0874): breath / fatigue / feign-death. Pure queue
            // arms — every meaning (which bar, what colour, what caption, how fast it drains)
            // is resolved at the UI seam in `ui_mirror`, and the countdown itself is the
            // FrameXML's own OnUpdate integration ──────────────────────────────────────────────
            SessionEvent::MirrorTimerStart(start) => mirror_timers
                .0
                .push(crate::ui_mirror::MirrorTimerEdge::Start(start)),
            SessionEvent::MirrorTimerPause { kind, paused } => mirror_timers
                .0
                .push(crate::ui_mirror::MirrorTimerEdge::Pause { kind, paused }),
            SessionEvent::MirrorTimerStop { kind } => mirror_timers
                .0
                .push(crate::ui_mirror::MirrorTimerEdge::Stop { kind }),
            // ── The social family (decision 0668): the friend/ignore lists, the `/who`
            // answer, and the result codes that print their own chat lines. The lines and the
            // Era events fire off the mirror in `ui_social::feed_social` — every one of them
            // needs a NAME the drain has no cache handle for.
            SessionEvent::FriendList { friends } => {
                crate::ui_social::apply::friend_list(&mut social, friends)
            }
            SessionEvent::IgnoreList { guids } => {
                crate::ui_social::apply::ignore_list(&mut social, guids)
            }
            SessionEvent::FriendStatus(update) => {
                crate::ui_social::apply::friend_status(&mut social, update)
            }
            SessionEvent::WhoResults(results) => crate::ui_social::apply::who(&mut social, results),
            SessionEvent::LootResponse {
                guid,
                loot_type,
                gold,
                items,
            } => loot_response(guid, loot_type, gold, items, &mut loot),
            SessionEvent::LootError { guid, error } => {
                loot_error(guid, error, &mut ui_actions.4, &mut loot_latch)
            }
            SessionEvent::LootRemoved { slot } => loot_removed(slot, &mut loot),
            SessionEvent::LootMoneyNotify { amount } => loot_money_notify(amount),
            SessionEvent::LootClearMoney => loot_clear_money(&mut loot),
            SessionEvent::LootReleaseResponse { guid } => {
                loot_release_response(guid, &mut loot, &mut loot_latch)
            }
            SessionEvent::ItemPushResult(p) => item_push_result(p, &self_guid, &mut loot),
            // ── The group-loot roll family (decision 0591) — the GroupLootFrame feed ───────────
            SessionEvent::LootStartRoll(p) => loot_start_roll(p, &mut loot_rolls),
            SessionEvent::LootRoll(p) => loot_roll(p, &mut loot_rolls),
            SessionEvent::LootRollWon(p) => loot_roll_won(p, &mut loot_rolls),
            SessionEvent::LootAllPassed(p) => loot_all_passed(p, &mut loot_rolls),
            // ── The death arc (decision 0308) — arm bodies in `death` ─────────────────────────
            SessionEvent::CorpseQuery {
                found,
                display_map,
                position,
                corpse_map,
            } => death::corpse_query(found, display_map, position, corpse_map, &mut death_net),
            SessionEvent::CorpseReclaimDelay { delay_ms } => {
                death::corpse_reclaim_delay(delay_ms, aura.1.elapsed_secs_f64(), &mut death_net)
            }
            SessionEvent::ResurrectRequest {
                caster,
                name,
                sickness,
                has_timer,
            } => death::resurrect_request(caster, name, sickness, has_timer, &mut death_net),
            SessionEvent::SpiritHealerConfirm { npc } => {
                death::spirit_healer_confirm(npc, &mut death_net)
            }
            SessionEvent::DurabilityDamageDeath => {
                death::durability_damage_death(&mut ui_actions.14)
            }
            SessionEvent::MoveMode {
                guid,
                counter,
                mode,
                apply,
            } => death::move_mode(
                guid,
                counter,
                mode,
                apply,
                &self_guid,
                &mut death_net,
                &mut move_modes,
            ),
            SessionEvent::ItemTemplate { entry, info } => {
                item_template(entry, info.map(|b| *b), &mut items)
            }
            SessionEvent::AttackStart { attacker, victim } => {
                combat::attack_start(attacker, victim, &mut commands, &index)
            }
            SessionEvent::AttackStop { attacker, victim } => {
                combat::attack_stop(attacker, victim, &mut commands, &index, &mut audio.9)
            }
            SessionEvent::AiReaction { unit, reaction } => {
                combat::ai_reaction(unit, reaction, &index, &mut audio.11)
            }
            SessionEvent::AttackerState(s) => combat::attacker_state(
                s,
                &index,
                &self_guid,
                &mut audio.3,
                &mut audio.8,
                &mut audio.15 .1,
                &mut audio.15 .2,
                play_seq.next(),
            ),
            SessionEvent::SpellDamageLog(s) => combat_log::spell_damage_log(
                s,
                &index,
                &self_guid,
                &stores,
                ui_actions.11.as_deref(),
                &mut audio.7,
                &mut audio.15 .0,
                &mut audio.15 .1,
            ),
            SessionEvent::PeriodicAuraLog(s) => combat_log::periodic_aura_log(
                s,
                &index,
                &self_guid,
                &stores,
                ui_actions.11.as_deref(),
                &mut audio.7,
                &mut audio.15 .0,
                &mut audio.15 .1,
                &mut names,
                &net_commands,
            ),
            SessionEvent::SpellHealLog(s) => combat_log::spell_heal_log(
                s,
                &index,
                &self_guid,
                &mut audio.15 .0,
                &mut audio.15 .1,
                &mut names,
                &net_commands,
            ),
            SessionEvent::SpellEnergizeLog(s) => {
                combat_log::spell_energize_log(s, &self_guid, &mut audio.15 .1)
            }
            SessionEvent::DamageShield(s) => combat_log::damage_shield(
                s,
                &index,
                &self_guid,
                &stores,
                &mut audio.7,
                &mut audio.15 .0,
            ),
            SessionEvent::SpellLogMiss(s) => combat_log::spell_log_miss(
                s,
                &index,
                &self_guid,
                &stores,
                &mut audio.7,
                &mut audio.15 .0,
                &mut audio.15 .1,
            ),
            SessionEvent::XpGain(x) => {
                combat_log::xp_gain(x, &index, &self_guid, &mut audio.7, &mut chat_log)
            }
            SessionEvent::ExplorationXp(x) => combat_log::exploration_xp(
                x,
                area_table.as_deref(),
                exploration_sounds.as_deref(),
                &index,
                &self_guid,
                &stores,
                &mut audio.0,
                &mut chat_log,
            ),
            SessionEvent::LevelUp(l) => combat_log::level_up(l, &mut chat_log),
            SessionEvent::SpellStart {
                caster,
                spell_id,
                cast_flags,
                cast_time_ms,
                target,
                ammo_display_id,
            } => spell_start(
                caster,
                spell_id,
                cast_flags,
                cast_time_ms,
                target,
                ammo_display_id,
                &mut commands,
                &index,
                &mut audio.5,
                &self_guid,
                &mut ui_actions.5,
                &mut ui_actions.9,
                ui_actions.11.as_deref(),
                play_seq.next(),
            ),
            SessionEvent::SpellGo {
                caster,
                spell_id,
                cast_flags,
                hits,
                misses,
                target,
                go_target,
                dest,
                ammo_display_id,
                item_caster,
            } => spell_go(
                caster,
                spell_id,
                cast_flags,
                hits,
                misses,
                target,
                go_target,
                dest,
                ammo_display_id,
                item_caster,
                &mut commands,
                &index,
                &audio.4,
                &mut audio.5,
                &mut audio.6,
                &self_guid,
                &stores,
                &mut ui_actions.5,
                &mut ui_actions.9,
                &mut ui_actions.15,
                &mut audio.7,
                &mut audio.10,
                (
                    &mut ui_actions.10,
                    ui_actions.11.as_deref(),
                    &mut items,
                    &net_commands,
                    &mut pet_bar,
                ),
                play_seq.next(),
            ),
            SessionEvent::SpellChainTargets {
                caster,
                spell_id,
                targets,
            } => spell_chain_targets(caster, spell_id, targets, &mut commands, &index),
            SessionEvent::SpellFailedOther { caster, spell_id } => spell_failed_other(
                caster,
                spell_id,
                &mut commands,
                &index,
                &audio.4,
                &mut audio.5,
                &self_guid,
                &mut ui_actions.5,
                &mut ui_actions.9,
                &mut ui_actions.15,
                play_seq.next(),
            ),
            SessionEvent::SpellDelayed { caster, delay_ms } => spell_delayed(
                caster,
                delay_ms,
                &self_guid,
                &mut ui_actions.5,
                &mut ui_actions.9,
            ),
            SessionEvent::CancelAutoRepeat => cancel_auto_repeat(
                &mut ui_actions.12,
                &self_guid,
                &index,
                &mut commands,
                &net_commands,
            ),
            SessionEvent::SpellCooldowns { caster, cooldowns } => {
                if let Some(store) =
                    addressed_store(caster, &self_guid, &mut ui_actions.10, &mut pet_bar)
                {
                    spell_cooldowns(caster, cooldowns, ui_actions.11.as_deref(), store);
                }
            }
            SessionEvent::ItemCooldown {
                item_guid,
                spell_id,
            } => item_cooldown(item_guid, spell_id, &items, &mut ui_actions.10),
            // The temporary-enchant countdown's ONLY feed (decision 0920): park the deadline on the
            // item store, which every tooltip surface reads back through `enchant_remaining_ms`.
            SessionEvent::ItemEnchantTime {
                item_guid,
                slot,
                seconds,
            } => items.set_enchant_deadline(item_guid, slot, seconds),
            SessionEvent::CooldownEvent { spell_id, caster } => {
                if let Some(store) =
                    addressed_store(caster, &self_guid, &mut ui_actions.10, &mut pet_bar)
                {
                    cooldown_event(spell_id, caster, store);
                }
            }
            SessionEvent::ClearCooldown { spell_id, caster } => {
                if let Some(store) =
                    addressed_store(caster, &self_guid, &mut ui_actions.10, &mut pet_bar)
                {
                    clear_cooldown(spell_id, caster, store);
                }
            }
            SessionEvent::CooldownCheat { caster } => {
                if let Some(store) =
                    addressed_store(caster, &self_guid, &mut ui_actions.10, &mut pet_bar)
                {
                    cooldown_cheat(caster, store);
                }
            }
            // The pet action bar (decision 0982) — server-authoritative, so PET_SPELLS is a
            // wholesale replace and its zero-guid form is the teardown.
            SessionEvent::PetSpells(spells) => {
                pet::pet_spells(*spells, ui_actions.11.as_deref(), &mut pet_bar)
            }
            SessionEvent::PetMode(mode) => pet::pet_mode(mode, &mut pet_bar),
            SessionEvent::PetActionFeedback { reason } => {
                pet::pet_action_feedback(reason, &mut ui_error_keys)
            }
            SessionEvent::PetCastFailed { spell_id, reason } => {
                pet::pet_cast_failed(spell_id, reason, &mut ui_actions.1 .0)
            }
            SessionEvent::ChannelStart {
                spell_id,
                duration_ms,
            } => channel_start(spell_id, duration_ms, &mut ui_actions.13, &mut ui_actions.5),
            SessionEvent::ChannelUpdate { remaining_ms } => {
                channel_update(remaining_ms, &mut ui_actions.13, &mut ui_actions.5)
            }
            SessionEvent::AuraDuration { slot, remaining_ms } => {
                aura_duration(slot, remaining_ms, &mut aura.0, aura.1.elapsed_secs_f64())
            }
            SessionEvent::PlaySpellVisual { unit, kit_id } => {
                anim::play_spell_visual(unit, kit_id, &index, play_seq, &mut audio.12)
            }
            SessionEvent::EnvironmentalDamageLog(e) => anim::environmental_damage_log(
                e,
                &index,
                aura.5.as_deref(),
                play_seq,
                &mut audio.12,
            ),
            // The gossip/vendor/trainer NPC-interaction family — arm bodies in `npc`.
            SessionEvent::GossipMenu {
                npc,
                text_id,
                options,
                quests,
            } => npc::gossip_menu(
                npc,
                text_id,
                options,
                quests,
                &mut gossip,
                &net_commands,
                &index,
                &stores,
            ),
            SessionEvent::NpcGreeting { text_id, blocks } => {
                npc::npc_greeting(text_id, blocks, &mut gossip, &index, &stores)
            }
            SessionEvent::GossipComplete => npc::gossip_complete(&mut gossip, &mut quest),
            // Questgiver panels (decision 0088): fill the `QuestGiver` the quest feed
            // (`crate::ui_quest`) reads. Each panel packet replaces the open view; the greeting/gossip
            // quest-row clicks and the panel buttons flow back out through the quest/gossip drains.
            SessionEvent::QuestGiverStatus { npc, status } => {
                quest_giver_status(npc, status, &mut quest)
            }
            SessionEvent::QuestGreeting(list) => quest_greeting(list, &mut quest),
            SessionEvent::QuestDetail(d) => quest_detail(d, &mut quest),
            SessionEvent::QuestProgress(p) => quest_progress(p, &mut quest),
            SessionEvent::QuestOffer(o) => quest_offer(o, &mut quest),
            SessionEvent::QuestComplete(c) => quest_complete(c, &mut quest),
            // Quest log (decision 0088's deferred second slice): the full template feeds the log
            // window's ask-once detail cache; the `SMSG_QUESTUPDATE_*` toasts have no dedicated
            // window of their own on this server (no ErrorsFrame-style transient panel yet), so they
            // route through the chat window's system-line seam ([`crate::ui_chat::ChatLog`]) — the
            // same seam the loot feed's refusal/receive lines use — colored SYSTEM yellow, the
            // GM-feedback color.
            SessionEvent::QuestTemplate(t) => quest_template(t, &mut quest_log),
            SessionEvent::QuestObjectiveKill {
                quest_id: _,
                entry,
                count,
                required,
            } => quest_objective_kill(entry, count, required, &mut quest),
            SessionEvent::QuestObjectiveItem { item_id, count } => {
                quest_objective_item(item_id, count, &mut quest)
            }
            SessionEvent::QuestObjectivesComplete { quest_id } => {
                quest_objectives_complete(quest_id, &mut quest)
            }
            SessionEvent::QuestFailed { quest_id, timed } => quest_failed(
                quest_id,
                timed,
                &mut quest_log,
                &net_commands,
                &mut chat_log,
                &mut quest,
            ),
            SessionEvent::QuestLogFull => quest_log_full(&mut quest),
            SessionEvent::QuestGiverInvalid { reason } => quest_giver_invalid(reason, &mut quest),
            SessionEvent::QuestGiverFailed { quest_id, reason } => {
                quest_giver_failed(quest_id, reason, &mut quest, &mut quest_log, &net_commands)
            }
            SessionEvent::VendorInventory { vendor, items } => {
                npc::vendor_inventory(vendor, items, &mut merchant)
            }
            SessionEvent::ShowBank { banker } => {
                npc::show_bank(banker, &mut bank_open, &mut gossip, &mut quest)
            }
            SessionEvent::BuyBankSlotResult { result } => {
                npc::bank_buy_slot_result(result, &mut bank_errors)
            }
            SessionEvent::TrainerList {
                trainer,
                trainer_type,
                services,
                greeting,
            } => npc::trainer_list(trainer, trainer_type, services, greeting, &mut trainer_open),
            SessionEvent::TrainerBuySucceeded { trainer, spell_id } => {
                npc::trainer_buy_succeeded(trainer, spell_id, &trainer_open, &net_commands)
            }
            SessionEvent::TrainerBuyFailed { error, .. } => {
                npc::trainer_buy_failed(error, &mut ui_actions.8)
            }
            SessionEvent::TaxiNodesShown {
                flightmaster,
                nearest_node,
                known_mask,
            } => npc::taxi_nodes_shown(flightmaster, nearest_node, known_mask, &mut taxi),
            SessionEvent::TaxiNodeStatus { guid, known } => {
                npc::taxi_node_status(guid, known, &mut commands, &index)
            }
            SessionEvent::ActivateTaxiReply { code } => npc::taxi_activate_reply(code, &mut taxi),
            SessionEvent::NewTaxiPath => npc::taxi_new_path(&mut taxi),
            SessionEvent::VendorBuyResult {
                vendor,
                slot,
                new_count,
                ..
            } => npc::vendor_buy_result(vendor, slot, new_count, &mut merchant),
            SessionEvent::VendorBuyFailed { reason, .. } => {
                npc::vendor_buy_failed(reason, &mut ui_actions.3)
            }
            SessionEvent::VendorSellFailed { reason, .. } => {
                npc::vendor_sell_failed(reason, &mut ui_actions.3)
            }
            SessionEvent::ForceSpeedChange {
                guid,
                kind,
                counter,
                speed,
            } => objects::force_speed_change(
                guid,
                kind,
                counter,
                speed,
                &index,
                &mut aura.3,
                &self_guid,
                &mut speed_changes,
            ),
            SessionEvent::SpeedChanged { guid, kind, speed } => {
                objects::speed_changed(guid, kind, speed, &index, &mut aura.3)
            }
            // ── The mount arc (decision 0441) — arm bodies in `mount` ─────────────────────────
            SessionEvent::MountResult { mount, code } => {
                mount::mount_result(mount, code, &mut ui_actions.1 .1)
            }
            SessionEvent::MountSpecial { guid } => {
                mount::mount_special(guid, &self_guid, &index, &mut audio.14)
            }
            SessionEvent::Pong { sequence } => session::pong(sequence, &aura.2, &mut status),
            SessionEvent::PacketDropped {
                opcode,
                unparseable,
            } => session::packet_dropped(opcode, unparseable, &mut dropped),
            // The mail arc (decision 0544 P1/P2/P3): the inbox/body/send-result arms fill the
            // mailbox session the feed reads (`crate::ui_mail`); the arrival pair feeds
            // `MailPending` (`HasNewMail()`/the minimap icon).
            SessionEvent::MailList { mails } => {
                mail::mail_list(mails, &mut mail_open, &net_commands)
            }
            SessionEvent::SendMailResult {
                mail_id,
                action,
                error,
                equip_error,
                item,
            } => mail::send_mail_result(
                mail_id,
                action,
                error,
                equip_error,
                item,
                &mut mail_open,
                &net_commands,
                &mut ui_actions.2,
            ),
            SessionEvent::MailItemText { text_id, text } => {
                mail::mail_item_text(text_id, text, &mut mail_open)
            }
            // The book-page cache (decision 1105) — one page per packet, the whole chain in
            // answer to the first ask; the reader repaints off it on the next feed.
            SessionEvent::PageText {
                page_id,
                text,
                next_page_id,
            } => page_texts.insert(page_id, text, next_page_id),
            SessionEvent::ReceivedMail { seconds } => {
                mail::received_mail(seconds, &mut mail_pending, &mail_open, &net_commands)
            }
            SessionEvent::NextMailTime { seconds } => {
                mail::next_mail_time(seconds, &mut mail_pending)
            }
            // The player-trade arc (decision 0592 P1): the status packet drives the open/accept/close
            // state machine, the extended snapshot replaces one side's item/gold — both into the
            // `TradeSession` the trade feed (`crate::ui_trade`) reads.
            SessionEvent::TradeStatus { status } => {
                trade::trade_status(status, &mut trade_session, &net_commands)
            }
            SessionEvent::TradeStatusExtended { state } => {
                trade::trade_status_extended(&state, &mut trade_session)
            }
            SessionEvent::WorldStates { scope, states } => {
                world::world_states(scope, states, &mut world_states)
            }
        }
    }
    // Flush the staged descriptor seeds/deltas onto the entities born this drain (now spawned by the
    // above Commands) — one insert each, fully merged, so no partial delta clobbers another.
    for (guid, fields) in pending {
        if let Some(&e) = index.0.get(&guid) {
            commands.entity(e).insert(ObjectStore(fields));
        }
    }
}

/// Tag our own player's streamed entity with [`SelfPlayer`] once we know our guid — by matching the
/// [`Guid`] component against [`SelfGuid`]. The renderer skips this entity (the controller owns our
/// avatar); the controller reads its transform to take control. Done as its own pass (rather than at
/// spawn) so it's robust to the order our guid and our create packet arrive in.
///
/// The controller's animation motion source (`MovementState`) rides the tag: a cross-map worldport
/// despawns every tracked entity — our avatar included — and the new map re-streams it, so any
/// per-entity state attached only at the one-shot take-control edge is lost on transfer. That was
/// the ".tele to another continent" bug: the re-tagged avatar had no `MovementState`, the anim
/// selector read it as stationary, and it slid around in the Stand pose.
pub(super) fn tag_self_player(
    mut commands: Commands,
    self_guid: Res<SelfGuid>,
    untagged: Query<(Entity, &Guid), Without<SelfPlayer>>,
) {
    let Some(me) = self_guid.0 else {
        return;
    };
    for (entity, guid) in &untagged {
        if guid.0 == me {
            commands
                .entity(entity)
                .insert((SelfPlayer, crate::creature_anim::MovementState::default()));
        }
    }
}
