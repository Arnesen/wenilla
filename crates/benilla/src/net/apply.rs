//! The per-frame wire→ECS bridge systems: [`apply_net_updates`] drains the inbound
//! [`SessionEvent`] channel into real entities (spawn/move/despawn, descriptor merges, splines,
//! teleports, clock), and [`tag_self_player`] marks our own streamed entity. The parent module
//! owns the channel/type surface; this module owns the event application.

use std::collections::HashMap;

use benilla_protocol::{ObjectFields, SessionEvent};
use bevy::prelude::*;

use super::{
    AiReactionMessage, CharActionResultMessage, CharListMessage, EmoteKind, EmoteMessage,
    EnteredWorldMessage, Guid, GuidIndex, LoggedOutMessage, NetCommands, NetEvents, NetStatus,
    ObjectStore, PendingTransfer, RemoteMotion, Reputations, SelfGuid, SelfPlayer, ServerSoundKind,
    ServerSoundMessage, ServerTime, TeleportMessage, WeatherMessage, WorldportMessage,
};

mod chat;
mod combat;
mod combat_log;
mod group;
mod loot;
mod mail;
mod npc;
mod objects;
mod quests;
mod session;
mod spells;
mod trade;

// The large arm families, split out of the dispatch match below (each `pub(super)` fn is
// one arm's body; the match stays the dispatcher, one call per arm — see the child modules).
use group::push_group_lines;
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
    action_buttons, cancel_auto_repeat, cast_result, clear_cooldown, cooldown_cheat,
    cooldown_event, item_cooldown, learned_spell, spell_book, spell_cooldowns, spell_delayed,
    spell_failed_other, spell_go, spell_start, superceded_spell,
};

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
    mut server_time: ResMut<ServerTime>,
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
        // The death arc's controller-facing acks (decision 0308): the server root/water-walk
        // changes the player controller answers with its live pose.
        MessageWriter<crate::death::MoveRootMessage>,
        MessageWriter<crate::death::WaterWalkMessage>,
        // The login screen's dialog + reconnect-policy feed (decision 0539).
        MessageWriter<super::LoginStageMessage>,
        MessageWriter<super::LoginFailedMessage>,
        MessageWriter<super::DisconnectedMessage>,
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
        // self-facing twins of the floating-text spawn. Nested pair: the tuple is at the
        // 16-param ceiling.
        (
            MessageWriter<crate::ui_unit::UnitCombatFeedback>,
            MessageWriter<crate::ui_unit::CombatTextEvent>,
        ),
    ),
    // The aura feed's duration side-table + the clock to stamp arrivals (decisions 0255/0257): the
    // self-only `SMSG_UPDATE_AURA_DURATION` lands here keyed by raw slot, timestamped for the
    // `ui_aura` slot-join — plus the ping clock the Pong arm measures round trips against.
    // Grouped as a tuple to stay under Bevy's 16-SystemParam ceiling.
    mut aura: (
        ResMut<crate::ui_aura::AuraDurations>,
        Res<Time>,
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
    let play_seq = &mut aura.4;
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
        mut move_roots,
        mut water_walks,
        mut login_stages,
        mut login_failures,
        mut disconnects,
    ) = session_msgs;
    // Descriptor seeds/deltas for objects created *earlier in this same drain* can't land on their
    // entities yet (the spawn `Command` hasn't run), so they accumulate here and flush once at the end.
    // This also removes a latent clobber: a plain per-delta `insert` on a not-yet-spawned entity would
    // overwrite an earlier partial rather than merge it (decision 0061).
    let mut pending: HashMap<u64, ObjectFields> = HashMap::new();
    for ev in events.0.try_iter() {
        match ev {
            SessionEvent::LoginStage { stage } => {
                login_stages.write(super::LoginStageMessage { stage });
            }
            SessionEvent::LoginFailed {
                code,
                reason,
                terminal,
            } => {
                login_failures.write(super::LoginFailedMessage {
                    code,
                    reason,
                    terminal,
                });
            }
            SessionEvent::CharacterList { characters, realm } => {
                session::character_list(characters, realm, &mut status, &mut char_lists)
            }
            SessionEvent::CharActionResult { action, code } => {
                char_actions.write(CharActionResultMessage { action, code });
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
                disconnects.write(super::DisconnectedMessage {
                    reason: reason.clone(),
                });
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
                fields,
            } => {
                // OUR corpse streaming into range (a TYPEID_CORPSE create whose owner is us):
                // remember its guid for the reclaim send (decision 0308 §5). Corpses classify as
                // EntityKind::Other; the owner field is corpse-only, so the filter is exact.
                if kind == benilla_protocol::EntityKind::Other
                    && fields.corpse_owner() == self_guid.0
                {
                    death_net.corpse_guid = Some(guid);
                }
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
                );
            }
            SessionEvent::ObjectValues { guid, fields } => {
                objects::object_values(guid, fields, &index, &mut stores, &mut pending, &mut items)
            }
            SessionEvent::ObjectDestroyed(guid) => {
                // The corpse-to-bones swap destroys the corpse object under its guid (0308 §1);
                // a stale guid must not ride a later reclaim.
                if death_net.corpse_guid == Some(guid) {
                    death_net.corpse_guid = None;
                }
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
            } => session::time_speed(hours, minutes, day_serial, timescale, &mut server_time),
            SessionEvent::Reputations { standings } => {
                session::reputations(standings, &mut reputations)
            }
            SessionEvent::ReputationDelta { standings } => {
                // A standing change is a questgiver-status input (`SatisfyQuestReputation`, and
                // the reaction gate): the reference sweeps from this handler too (0654).
                session::reputation_delta(standings, &mut reputations);
                quest.bump_reask();
            }
            SessionEvent::BindPoint { area } => home_bind.0 = Some(area),
            SessionEvent::Proficiency {
                item_class,
                subclass_mask,
            } => {
                proficiencies.0.insert(item_class, subclass_mask);
            }
            // Name-query answers → the cache (asked by NameCache::resolve; race/gender/class ride
            // the player answer but have no consumer yet — the cache stays name-only until one does).
            SessionEvent::PlayerName { guid, name, .. } => {
                names.insert_player(guid, name);
            }
            SessionEvent::PetName { pet_number, name } => {
                names.insert_pet(pet_number, name);
            }
            SessionEvent::CreatureName {
                entry,
                name,
                subname,
                creature_type,
                rank,
                type_flags,
                civilian,
                racial_leader,
            } => {
                names.insert_creature(
                    entry,
                    name.map(|n| crate::names::CreatureRecord {
                        name: n,
                        subname,
                        creature_type: creature_type.unwrap_or(0),
                        rank,
                        type_flags,
                        civilian,
                        racial_leader,
                    }),
                );
            }
            SessionEvent::GameObjectInfo {
                entry,
                type_id,
                display_id,
                name,
                data,
            } => {
                // The ask-once GameObject template (decision 0239): cache it and resolve the lockId
                // from the type-specific `data[]` slot — the interact routing reads it to choose
                // use-vs-cast; the hover tooltip reads the name (decision 0276's GO law).
                debug!(
                    "net: gameobject template {entry} type {type_id} display {display_id} {name:?}"
                );
                go_templates.insert(entry, type_id, name, &data);
            }
            SessionEvent::PlaySound { sound_id } => {
                audio.0.write(ServerSoundMessage {
                    kind: ServerSoundKind::Sound2d,
                    sound_id,
                    source: None,
                });
            }
            SessionEvent::PlayMusic { music_id } => {
                audio.0.write(ServerSoundMessage {
                    kind: ServerSoundKind::Music,
                    sound_id: music_id,
                    source: None,
                });
            }
            SessionEvent::PlayObjectSound { sound_id, guid } => {
                audio.0.write(ServerSoundMessage {
                    kind: ServerSoundKind::ObjectSound,
                    sound_id,
                    source: index.0.get(&guid).copied(),
                });
            }
            SessionEvent::Weather {
                weather_type,
                grade,
                sound_id,
                instant,
            } => {
                audio.1.write(WeatherMessage {
                    weather_type,
                    grade,
                    sound_id,
                    instant,
                });
            }
            SessionEvent::TextEmote { guid, text_emote } => {
                audio.2.write(EmoteMessage {
                    source: index.0.get(&guid).copied(),
                    kind: EmoteKind::Text(text_emote),
                });
            }
            SessionEvent::Emote { guid, emote_id } => {
                audio.2.write(EmoteMessage {
                    source: index.0.get(&guid).copied(),
                    kind: EmoteKind::Anim(emote_id),
                });
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
                &net_commands,
                play_seq.next(),
            ),
            SessionEvent::InventoryFailure {
                reason,
                required_level,
                item_guid,
            } => inventory_failure(
                reason,
                required_level,
                item_guid,
                &mut ui_actions.2,
                &mut ui_actions.6,
                &mut ui_actions.7,
            ),
            SessionEvent::Chat(m) => {
                // The chat window (decision 0084): the feed ([`crate::ui_chat`]) formats + colors
                // per type, resolves the sender name ask-once, and AddMessages it into ChatFrame1.
                // System lines (`CHAT_MSG_SYSTEM` 0x0A, vmangos `SharedDefines.h`) are the SERVER'S
                // ANSWER to a GM dot-command — "Premade gear template N applied", "No matching
                // premade player template found", "There is no such command". For a headless probe
                // that is the only channel the server has to say *why* something did not happen, so
                // it rides at `info!`: at `debug!` it was in the log but invisible at the default
                // level, and a refused command read exactly like an applied one (decision 0651 —
                // the rig's whole batch silently no-op'd on a too-low GM level and nothing said so).
                // Ordinary chat stays at `debug!`: conversation, not diagnosis, and high volume.
                if m.chat_type == 0x0A {
                    info!("net: server says — {}", m.text);
                } else {
                    debug!("net: chat [{:#04x}] {}", m.chat_type, m.text);
                }
                // …and on the trace clock too (decision 0624). A GM dot-command is the only way to
                // ask the SERVER what it believes — `.gps` reads back the server-side position of a
                // mover whose packets may or may not be reaching it — and its answer is only usable
                // if it lands on the same timeline as the `snd`/`rly`/`run` lines it must be read
                // against. `debug!` timestamps are wall-clock in a different format; this is one
                // clock, one file.
                if crate::dbg_trace::enabled() {
                    crate::dbg_trace::line(
                        "sys",
                        &format!("[{:#04x}] {}", m.chat_type, m.text.replace('\n', " ⏎ ")),
                    );
                }
                // The ignore gate (decision 0668): an ignored speaker is dropped SILENTLY —
                // no line at all — which is the client's own `FriendList::IsIgnored 0x5ae5a0`
                // check, VERIFIED for the sibling text-emote path (wow-re
                // `system/ui/scratch/text-emote-composition.md`). A dropped WHISPER additionally
                // tells the server, so the sender gets the "is ignoring you" answer: that is what
                // `CMSG_CHAT_IGNORED` is for, and only the client can send it.
                if social.is_ignored(m.sender_guid) {
                    if m.chat_type == benilla_protocol::messages::CHAT_MSG_WHISPER {
                        let _ = net_commands.0.send(crate::net::ClientCommand::ChatIgnored {
                            guid: m.sender_guid,
                        });
                    }
                    continue;
                }
                chat_log.push_wire(m);
            }
            SessionEvent::ChannelNotify {
                notice,
                channel,
                tail,
            } => {
                chat_log.push_channel_notice(notice, channel, &tail);
            }
            SessionEvent::ChannelList {
                channel, members, ..
            } => chat::channel_list(channel, &members, &mut chat_log),
            SessionEvent::ChatPlayerNotFound { name } => {
                chat::chat_player_not_found(&name, &mut chat_log)
            }
            SessionEvent::ChatWrongFaction => chat::chat_wrong_faction(&mut chat_log),
            SessionEvent::Notification { text } => chat::notification(text, &mut chat_log),
            SessionEvent::PlayedTime { total, level } => {
                chat::played_time(total, level, &mut chat_log)
            }
            SessionEvent::RandomRoll {
                min,
                max,
                roll,
                guid,
            } => {
                chat_log.push_roll(min, max, roll, guid);
            }
            // ── The group/party family (decision 0434 §D2, superseded by 0440): `GroupState`
            // mirrors the wire; its composed lines ride CHAT_MSG_SYSTEM, the way the reference's
            // engine-side errorId→GlobalStrings display does (mapping byte-verified, decision
            // 0440's §5 fold-back) ──
            SessionEvent::GroupInvite { inviter } => {
                push_group_lines(&mut chat_log, group.apply_invited(&inviter));
            }
            SessionEvent::GroupDecline { name } => {
                push_group_lines(&mut chat_log, group.apply_declined(&name));
            }
            SessionEvent::GroupUninvited => {
                push_group_lines(&mut chat_log, group.apply_uninvited());
            }
            SessionEvent::GroupLeaderChanged { name } => {
                // Our own name is cache-seeded at login (session::connected), so this never asks.
                let own = self_guid
                    .0
                    .and_then(|g| names.resolve(g, &net_commands).map(str::to_string));
                push_group_lines(
                    &mut chat_log,
                    group.apply_leader_changed(&name, own.as_deref()),
                );
            }
            SessionEvent::GroupDestroyed => {
                push_group_lines(&mut chat_log, group.apply_destroyed());
            }
            SessionEvent::GroupList {
                group_type,
                own_flags,
                members,
                leader,
                loot,
            } => {
                let lines = group.apply_list(group_type, own_flags, members, leader, loot);
                push_group_lines(&mut chat_log, lines);
                // Roster changes move shared-quest availability — the reference sweeps here (0654).
                quest.bump_reask();
            }
            SessionEvent::PartyCommandResult {
                operation,
                member,
                result,
            } => {
                push_group_lines(
                    &mut chat_log,
                    group.apply_command_result(operation, &member, result),
                );
            }
            SessionEvent::PartyMemberStats { guid, full, info } => {
                group.apply_stats(guid, full, *info);
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
            SessionEvent::ItemPushResult(p) => item_push_result(p, &mut loot),
            // ── The group-loot roll family (decision 0591) — the GroupLootFrame feed ───────────
            SessionEvent::LootStartRoll(p) => loot_start_roll(p, &mut loot_rolls),
            SessionEvent::LootRoll(p) => loot_roll(p, &mut loot_rolls),
            SessionEvent::LootRollWon(p) => loot_roll_won(p, &mut loot_rolls),
            SessionEvent::LootAllPassed(p) => loot_all_passed(p, &mut loot_rolls),
            // ── The death arc (decision 0308) — the wire-fed stores + the controller acks ──────
            SessionEvent::CorpseQuery {
                found,
                display_map,
                position,
                corpse_map,
            } => {
                // A not-found (reactive or the unprompted bones-conversion push) drops the marker.
                death_net.corpse = found.then_some(crate::death::CorpsePoint {
                    display_map,
                    position,
                    corpse_map,
                });
            }
            SessionEvent::CorpseReclaimDelay { delay_ms } => {
                death_net.reclaim_at =
                    Some(aura.1.elapsed_secs_f64() + f64::from(delay_ms) / 1000.0);
                // The client's 0x269 handler re-fires the corpse-range events through its latch
                // (wow-re death-ui.md §4) — the feed re-announces on this bump.
                death_net.reclaim_generation = death_net.reclaim_generation.wrapping_add(1);
            }
            SessionEvent::ResurrectRequest {
                caster,
                name,
                sickness,
                has_timer,
            } => {
                death_net.resurrect = Some(crate::death::ResurrectOffer {
                    caster,
                    name,
                    sickness,
                    has_timer,
                });
            }
            SessionEvent::SpiritHealerConfirm { npc } => death_net.spirit_healer = Some(npc),
            SessionEvent::DurabilityDamageDeath => {
                // The red line, verbatim GlobalStrings DURABILITYDAMAGE_DEATH (the %% unescaped).
                ui_actions
                    .14
                     .0
                    .push("Your equipped items suffer a 10% durability loss.".to_string());
            }
            SessionEvent::MoveRoot {
                guid,
                counter,
                rooted,
            } => {
                // The server only addresses our own mover; the guard keeps a stray relay harmless.
                if self_guid.0 == Some(guid) {
                    move_roots.write(crate::death::MoveRootMessage {
                        guid,
                        counter,
                        rooted,
                    });
                }
            }
            SessionEvent::WaterWalk { guid, counter, on } => {
                if self_guid.0 == Some(guid) {
                    death_net.water_walk = on;
                    water_walks.write(crate::death::WaterWalkMessage { guid, counter, on });
                }
            }
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
                combat_log::xp_gain(x, &index, &self_guid, &mut audio.7);
                chat_log.push_xp_gain(&x);
            }
            SessionEvent::LevelUp(l) => {
                // The ding's chat lines (decision 0304). The talent-count arg is not on the
                // wire — the client computes `(newLevel >= 10) ? 1 : 0` (byte-verified
                // `0x5e407c`, wow-re levelup-ding.md — the 0305 fold-back), exactly this. The
                // ding's VISUAL is deliberately absent here: it rides the UNIT_FIELD_LEVEL
                // change-watcher (`entities` spell_fx::level_up_flash), never this packet.
                let talent_points = u32::from(l.level >= 10);
                chat_log.push_level_up(&l, talent_points);
            }
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
                ),
                play_seq.next(),
            ),
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
            SessionEvent::SpellCooldowns { caster, cooldowns } => spell_cooldowns(
                caster,
                cooldowns,
                &self_guid,
                ui_actions.11.as_deref(),
                &mut ui_actions.10,
            ),
            SessionEvent::ItemCooldown {
                item_guid,
                spell_id,
            } => item_cooldown(item_guid, spell_id, &items, &mut ui_actions.10),
            SessionEvent::CooldownEvent { spell_id, caster } => {
                cooldown_event(spell_id, caster, &self_guid, &mut ui_actions.10)
            }
            SessionEvent::ClearCooldown { spell_id, caster } => {
                clear_cooldown(spell_id, caster, &self_guid, &mut ui_actions.10)
            }
            SessionEvent::CooldownCheat { caster } => {
                cooldown_cheat(caster, &self_guid, &mut ui_actions.10)
            }
            // The channel pair is self-only on the wire (no guid) — straight to the cast bar;
            // the channel *animation* state rides the unit-field pair instead (decision 0137).
            SessionEvent::ChannelStart {
                spell_id,
                duration_ms,
            } => {
                let now = std::time::Instant::now();
                ui_actions.13.start(spell_id, duration_ms, now);
                ui_actions
                    .5
                     .0
                    .push(crate::ui_cast::CastBarEdge::ChannelStart {
                        spell_id,
                        duration_ms,
                    })
            }
            SessionEvent::ChannelUpdate { remaining_ms } => {
                ui_actions
                    .13
                    .update(remaining_ms, std::time::Instant::now());
                ui_actions
                    .5
                     .0
                    .push(crate::ui_cast::CastBarEdge::ChannelUpdate { remaining_ms })
            }
            // One of our own auras' remaining time (decisions 0255/0257) — keyed by raw slot,
            // stamped with the receive time. The `ui_aura` feed joins it to the aura in that slot
            // by arrival order; it arrives *before* the descriptor delta that names the slot.
            SessionEvent::AuraDuration { slot, remaining_ms } => {
                aura.0.set(slot, remaining_ms, aura.1.elapsed_secs_f64());
            }
            SessionEvent::PlaySpellVisual { unit, kit_id } => {
                // The kit-push opcode (decision 0280): stage-0 play on the unit — the eat/drink
                // kit cadence and mid-channel swaps. Consumer: `creature_anim::spell_visual`.
                if let Some(&e) = index.0.get(&unit) {
                    audio.12.write(crate::creature_anim::KitPush {
                        entity: e,
                        kit_id,
                        seq: play_seq.next(),
                    });
                }
            }
            SessionEvent::EnvironmentalDamageLog(e) => {
                // The 0x1FC consequence (wow-re `sound/scratch/uisound-tables.md`: reader
                // `0x624fcc` inside `0x624f30`): the EnvironmentalDamage.dbc 6-slot table picks
                // the damage type's SpellVisualKit — fall's is the DustCloud_Land puff — played
                // on the victim through the ordinary discrete kit play (`0x60edf0`), the same
                // leg the kit-push opcode rides. The pain vocal's exact trigger is a dispatched
                // wow-re §5 (in flight) — it folds in as its own edge when the verdict lands.
                if let Some(&ent) = index.0.get(&e.victim) {
                    if let Some(kit_id) = aura.5.as_ref().and_then(|t| t.0.kit_id(e.damage_type)) {
                        debug!(
                            "net: environmental damage on {:#x} (type {}, {} dmg) → kit {kit_id}",
                            e.victim, e.damage_type, e.damage
                        );
                        audio.12.write(crate::creature_anim::KitPush {
                            entity: ent,
                            kit_id,
                            seq: play_seq.next(),
                        });
                    }
                }
            }
            // The gossip/vendor/trainer NPC-interaction family — arm bodies in `npc`.
            SessionEvent::GossipMenu {
                npc,
                text_id,
                options,
                quests,
            } => {
                let gender = npc_gender(npc, &index, &stores);
                npc::gossip_menu(
                    npc,
                    gender,
                    text_id,
                    options,
                    quests,
                    &mut gossip,
                    &net_commands,
                );
            }
            SessionEvent::NpcGreeting { text_id, blocks } => {
                // The record answers a query we sent for the OPEN menu, so its NPC is the one whose
                // gender picks the column (decision 0081's ask-once flow).
                let gender = gossip.npc.map_or(0, |npc| npc_gender(npc, &index, &stores));
                npc::npc_greeting(text_id, gender, blocks, &mut gossip)
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
            SessionEvent::QuestComplete(c) => {
                // The turn-in result — the `SMSG_QUESTGIVER_*` demux the reference sweeps from
                // (0654): every other giver's `!`/`?` can move the moment a quest is handed in.
                quest_complete(c, &mut quest);
                quest.bump_reask();
            }
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
            } => {
                quest_objective_kill(entry, count, required);
                quest.bump_reask();
            }
            SessionEvent::QuestObjectiveItem { item_id, count } => {
                quest_objective_item(item_id, count);
                quest.bump_reask();
            }
            SessionEvent::QuestObjectivesComplete { quest_id } => {
                // The `SMSG_QUESTUPDATE_*` family: the turn-in `?` can go gold with no quest-log
                // field change of its own, so the reference sweeps from these handlers (0654).
                quest_objectives_complete(quest_id);
                quest.bump_reask();
            }
            SessionEvent::QuestFailed { quest_id, timed } => {
                quest_failed(
                    quest_id,
                    timed,
                    &mut quest_log,
                    &net_commands,
                    &mut chat_log,
                );
                quest.bump_reask();
            }
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
            // The (dis)mount attempt's result code (decision 0441): OK is silent in the reference
            // (10 mounting / 3 dismounting); a failure queues the red error line
            // (`ui_action::mount_result_key` — resolved against the VM's GlobalStrings at drain).
            SessionEvent::MountResult { mount, code } => {
                let ok = if mount { code == 10 } else { code == 3 };
                if !ok {
                    info!(
                        "net: {}mount refused (code {code})",
                        if mount { "" } else { "dis" }
                    );
                    ui_actions.1 .1 .0.push((mount, code));
                }
            }
            // A nearby rider's flourish: rear their mount (MountSpecial 94 on the mount child —
            // the hop happens in `creature_anim::flourish_to_anim`). Our OWN guid is dropped:
            // we played it locally at send time, and whether the sender gets the SMSG echoed
            // back is a server-config detail (LIVE-VERIFIED 2026-07-17, double-flourish probe:
            // vmangos's `SendMovementMessageToSet(.., false)` only cheat-logs on the flag — the
            // non-broadcaster delivery hardcodes self=true, so our deployment echoes; the
            // optional per-player broadcaster honors it and would not). Self-suppression on
            // receive is correct under both configs.
            SessionEvent::MountSpecial { guid } => {
                if self_guid.0 != Some(guid) {
                    if let Some(&e) = index.0.get(&guid) {
                        audio
                            .14
                            .write(crate::creature_anim::MountFlourish { unit: e });
                    }
                }
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
                mail::mail_list(mails, &mut mail_open, &net_commands, &mut mail_pending)
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
            SessionEvent::ReceivedMail => {
                mail::received_mail(&mut mail_pending, &mail_open, &net_commands)
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
            // The world-state table (`SMSG_INIT_WORLD_STATES` / `SMSG_UPDATE_WORLD_STATE`) — both
            // wires funnel into the one setter, as the reference's own handler does. An init does
            // NOT clear first: what its `(map, zone)` dwords drive is unrecorded, so we log the
            // scope rather than act on it (rationale on `crate::world_state`).
            SessionEvent::WorldStates { scope, states } => {
                if let Some((map, zone)) = scope {
                    debug!(
                        "world states: map {map} zone {zone}, {} entries",
                        states.len()
                    );
                }
                world_states.write(&states);
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

/// A streamed unit's gender (`UNIT_FIELD_BYTES_0` byte 2) by guid — the gossip greeting's column
/// selector (wow-re `gossip-npctext-law.md`: tested `== 1` for female, so genderless `2` reads as
/// male). `0` when the guid isn't streamed in or carries no descriptor yet, which is the same
/// column the reference takes for a gossip target that isn't a unit at all.
fn npc_gender(guid: u64, index: &GuidIndex, stores: &Query<&mut ObjectStore>) -> u8 {
    index
        .0
        .get(&guid)
        .and_then(|&e| stores.get(e).ok())
        .and_then(|s| s.0.unit_gender())
        .unwrap_or(0)
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
