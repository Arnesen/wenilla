//! The per-frame wire→ECS bridge systems: [`apply_net_updates`] drains the inbound
//! [`SessionEvent`] channel into real entities (spawn/move/despawn, descriptor merges, splines,
//! teleports, clock), and [`tag_self_player`] marks our own streamed entity. The parent module
//! owns the channel/type surface; this module owns the event application.

use std::collections::HashMap;

use benilla_protocol::{ObjectFields, SessionEvent};
use bevy::prelude::*;

use super::{
    Guid, GuidIndex, NetCommands, NetEvents, NetStatus, ObjectStore, Reputations, SelfGuid,
    SelfPlayer,
};

mod anim;
mod chat;
mod combat;
mod combat_chat;
mod combat_log;
mod death;
mod group;
mod loot;
mod mount;
mod names;
mod npc;
mod objects;
mod params;
mod pet;
mod quests;
#[cfg(test)]
mod seam_tests;
mod session;
mod spells;
mod world;

use params::{ActionStores, AnimWriters, Catalogs, Clocks, ObjectQueries, Session, WindowStores};

// The arm families, split out of the dispatch match below (each `pub(super)` fn is one arm's
// body; the match stays the dispatcher, one call per arm — see the child modules).
use loot::{
    inventory_failure, item_push_result, item_template, loot_all_passed, loot_clear_money,
    loot_error, loot_master_list, loot_money_notify, loot_release_response, loot_removed,
    loot_response, loot_roll, loot_roll_won, loot_start_roll,
};
use quests::{
    quest_complete, quest_confirm_accept, quest_detail, quest_failed, quest_giver_failed,
    quest_giver_invalid, quest_giver_status, quest_greeting, quest_log_full, quest_objective_item,
    quest_objective_kill, quest_objectives_complete, quest_offer, quest_progress,
    quest_push_result, quest_template,
};
use spells::{
    action_buttons, aura_duration, cancel_auto_repeat, cast_result, channel_start, channel_update,
    clear_cooldown, cooldown_cheat, cooldown_event, item_cooldown, learned_spell, removed_spell,
    set_spell_modifier, spell_book, spell_chain_targets, spell_cooldowns, spell_delayed,
    spell_failed_other, spell_go, spell_start, superceded_spell,
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

/// The drain: take this frame's events off the channel and run them **in wire order** — a run of
/// kinds the `match` below still owns goes through [`apply_unpeeled`] as one batch, a kind a
/// subsystem has claimed in the handler table ([`super::handlers`]) runs its handlers in place,
/// each a one-shot system over this exclusive world (decisions 2305, 2306). One frame, packet
/// order, before anything else in [`benilla_world::schedule::WorldStage::Net`] runs: the property
/// 0006 built and 2265 said every split must keep.
pub(crate) fn apply_net_updates(world: &mut World) {
    let events: Vec<SessionEvent> = world.resource::<NetEvents>().0.try_iter().collect();
    super::handlers::dispatch(world, events, |world, unclaimed| {
        if let Err(e) = world.run_system_cached_with(apply_unpeeled, unclaimed) {
            panic!("the drain's dispatch match did not run: {e}");
        }
    });
}

/// The dispatch `match` — every kind no subsystem has claimed yet (2305's migration runs one
/// window family at a time out of here into its own module's handlers). Mutates real ECS
/// entities: spawn on create, move existing, despawn on remove, attach/clear movement splines,
/// and surface teleport/worldport/clock changes; parks the rest into window state.
fn apply_unpeeled(
    In(events): In<Vec<SessionEvent>>,
    mut commands: Commands,
    net_commands: Res<NetCommands>,
    mut index: ResMut<GuidIndex>,
    mut self_guid: ResMut<SelfGuid>,
    mut status: ResMut<NetStatus>,
    mut reputations: ResMut<Reputations>,
    // The families, by name (`params`): the drain destructures each one below so every arm
    // reads a plain local.
    clocks: Clocks,
    objects: ObjectQueries,
    session: Session,
    windows: WindowStores,
    actions: ActionStores,
    anim: AnimWriters,
    catalogs: Catalogs,
) {
    let Clocks {
        mut server_time,
        mut wall_clock,
        real: real_clock,
    } = clocks;
    let ObjectQueries {
        mut transforms,
        mut stores,
        remote: mut remote_motion,
        modes: mut unit_modes,
        mut riders,
        speeds: unit_speeds,
        transports,
        casting: casting_units,
        engaged_self,
        mut field_changes,
    } = objects;
    let ActionStores {
        mut player_actions,
        mut cast_errors,
        mut mount_errors,
        mut chain_casts,
        mut pet_tame_failures,
        mut learned_in_tab,
        mut equip_errors,
        mut merchant_errors,
        mut cast_bar,
        mut pending_item_ops,
        mut lock_transitions,
        mut trainer_errors,
        mut stable_errors,
        mut pending_cast,
        cooldowns: mut cooldown_store,
        mut auto_repeat,
        mut active_channel,
        mut ui_error_texts,
        mut queued_melee,
        mut aura_durations,
        mut spell_mods,
    } = actions;
    let AnimWriters {
        mut server_sounds,
        weather: mut weather_msgs,
        mut emotes,
        mut swings,
        mut cast_events,
        mut spell_go_targets,
        combat_text: mut combat_text_spawns,
        mut swing_impacts,
        mut swing_flushes,
        go_lid_open: mut go_lid_opens,
        mut ai_reactions,
        mut kit_pushes,
        mut hard_landings,
        mut mount_flourishes,
        unit_combat: mut unit_combat_feedback,
        mut combat_text_events,
        mut sheath_requests,
        go_custom_anim: mut go_custom_anims,
        pet_talk: mut pet_talks,
        pet_dismiss: mut pet_dismiss_sounds,
        swing_refusals: mut swing_refusal_edges,
        mut play_seq,
    } = anim;
    let Catalogs {
        factions: faction_catalog,
        spells: spell_catalog,
        combat_cvars,
        env_damage: env_damage_table,
        area_table,
        exploration_sounds,
        current_map,
    } = catalogs;
    // A `&mut` to the counter itself (deref-coerced through the `ResMut`), so the arms that stamp
    // it *conditionally* can take it by reference and only advance it when they emit.
    let play_seq: &mut crate::creature_anim::PlaySeq = &mut play_seq;
    let WindowStores {
        mut names,
        mut items,
        mut gossip,
        mut merchant,
        mut trainer_open,
        mut stable_open,
        mut loot,
        mut loot_latch,
        mut loot_rolls,
        mut chat_log,
        mut quest,
        mut quest_log,
        mut quest_share,
        mut go_templates,
        mut home_bind,
        mut proficiencies,
        mut dropped,
        mut death_net,
        mut group,
        mut taxi,
        mut bank_open,
        mut bank_errors,
        mut world_states,
        social,
        mut logout,
        mut mirror_timers,
        mut pet_bar,
        mut ui_error_keys,
        mut page_texts,
        mut played_time_answer,
        mut talent_wipe,
        mut pet_unlearn,
        mut instance_boot,
        mut area_spirit,
        mut battlefield_queue,
        mut meeting_stone,
        mut battlefield_scoreboard,
        mut battlefield,
        mut tutorials,
        mut battlefield_positions,
        mut poi_marker,
        mut inspect_honor,
        mut ping,
    } = windows;
    let Session {
        mut teleports,
        mut worldports,
        mut char_lists,
        mut realm_lists,
        mut char_actions,
        mut char_login_failures,
        mut entered_world,
        mut addon_reply,
        mut logged_out,
        mut speed_changes,
        mut move_modes,
        mut knockbacks,
        mut login_stages,
        mut login_queued,
        mut login_failures,
        mut disconnects,
        mut server_said,
        mut self_moves,
        mut client_control,
        mut cinematics,
        transfer: mut pending_transfer,
    } = session;
    // Descriptor seeds/deltas for objects created *earlier in this same drain* can't land on their
    // entities yet (the spawn `Command` hasn't run), so they accumulate here and flush once at the end.
    // This also removes a latent clobber: a plain per-delta `insert` on a not-yet-spawned entity would
    // overwrite an earlier partial rather than merge it (decision 0061).
    let mut pending: HashMap<u64, ObjectFields> = HashMap::new();
    // The drain's staged [`UnitMoveModes`] grants — see [`objects::StagedModes`]. A grant and the
    // `SMSG_MONSTER_MOVE` it refuses can land in the same drain, and the refusal has to see it.
    let mut staged_modes = objects::StagedModes::new();
    // The same deferral trap for movers' speeds, and the one B213 fell into: a create's
    // `UnitSpeeds` insert is a Command, so a `SMSG_FORCE_*_SPEED_CHANGE` arriving later in this
    // same drain could not land on top of it. Both stage here in packet order (decision 1478).
    let mut speed_stage = objects::SpeedStage::default();
    // The combat log's classification inputs (B297). Built per use rather than once: the arms
    // around these ones take `&mut` to `index`, `group` and `reputations`, so a borrow held across
    // the whole drain would not compile. `macro_rules!` here is hygienic against the locals it
    // names because it is defined after them, so this is one expression in seven call sites rather
    // than seven copies of six fields.
    macro_rules! chat_ctx {
        () => {
            combat_chat::ChatCtx {
                self_guid: &self_guid,
                group: Some(&group),
                index: &index,
                factions: faction_catalog.as_deref(),
                reputations: &reputations,
                spells: spell_catalog.as_deref(),
                ranges: &combat_cvars.ranges,
                periodic: combat_cvars.periodic.0,
            }
        };
    }
    for ev in events {
        match ev {
            SessionEvent::LoginStage { stage } => session::login_stage(stage, &mut login_stages),
            SessionEvent::LoginQueued { position, realm } => {
                login_queued.write(crate::net::LoginQueuedMessage { position, realm });
            }
            SessionEvent::LoginFailed {
                refusal,
                reason,
                terminal,
                dial,
            } => session::login_failed(refusal, reason, terminal, dial, &mut login_failures),
            SessionEvent::RealmList { realms } => {
                realm_lists.write(crate::net::RealmListMessage { realms });
            }
            SessionEvent::CharacterList { characters, realm } => {
                session::character_list(characters, realm, &mut status, &mut char_lists)
            }
            SessionEvent::CharActionResult { action, code } => {
                session::char_action_result(action, code, &mut char_actions)
            }
            SessionEvent::CharacterLoginFailed { result } => {
                session::character_login_failed(result, &mut char_login_failures)
            }
            SessionEvent::CinematicTriggered { cinematic_id } => {
                session::cinematic_triggered(cinematic_id, &mut cinematics)
            }
            SessionEvent::Connected {
                self_guid: guid,
                name,
                billing_time_rested,
                tutorial_flags,
                addon_info,
            } => session::connected(
                guid,
                name,
                billing_time_rested,
                tutorial_flags,
                addon_info,
                &mut addon_reply,
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
            SessionEvent::Disconnected { reason, end } => {
                session::disconnected(
                    reason,
                    end,
                    &mut commands,
                    &mut index,
                    &mut self_guid,
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
                    &mut quest_share,
                    &mut death_net,
                    &mut group,
                    &mut taxi,
                    &mut bank_open,
                    &mut cooldown_store,
                    &mut pending_transfer,
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
                mover,
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
                    mover,
                    transport_progress,
                    transport,
                    spline,
                    fields,
                    &mut commands,
                    &mut index,
                    &mut transforms,
                    &mut stores,
                    &mut pending,
                    &mut field_changes,
                    &mut speed_stage,
                    &names,
                    &go_templates,
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
            // **An observed mover skipped time** (decision 1935). No pose moved — only that
            // unit's clock ran on — so this touches its relay chain and nothing else, which is
            // the whole of what the reference's handler does (`0x603b40` → `0x61ab90`:
            // `[CMovement+0xac] += lag`). A guid we do not hold is dropped, faithfully: the
            // reference resolves under `TYPEMASK_UNIT` and returns on a miss.
            SessionEvent::MoveTimeSkipped { guid, lag_ms } => {
                if let Some(mut m) = index
                    .0
                    .get(&guid)
                    .and_then(|&e| remote_motion.get_mut(e).ok())
                {
                    m.relay.skip_time(lag_ms);
                }
            }
            SessionEvent::UnitMove {
                guid,
                position,
                orientation,
                flags,
                pitch,
                time,
                verb,
                fall_time,
                jump,
                transport,
            } => {
                // The scheduled-replay law (decisions 0601/0615): `unit_move` runs the mover's own
                // replay chain over this packet's wire stamp to get its client fire-time, then
                // applies it now if due, else queues it on the unit for `drain_pending_moves`.
                let now_ms = real_clock.elapsed_secs_f64() * 1000.0;
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
                        verb,
                    },
                    now_ms,
                    &mut commands,
                    &index,
                    &self_guid,
                    &mut remote_motion,
                    &mut transforms,
                    &mut hard_landings,
                    &mut self_moves,
                );
            }
            SessionEvent::ObjectValues { guid, fields } => {
                // Our corpse's own `CORPSE_FIELD_FLAGS` can flip to BONES under a live guid; the
                // reclaim latch is re-asked on that edge, as the reference's `FLAGS` mirror
                // handler `0x5d6d60` does (1729).
                death::recheck_corpse(guid, &fields, &self_guid, &mut death_net);
                objects::object_values(
                    guid,
                    fields,
                    &index,
                    &mut stores,
                    &mut pending,
                    &mut field_changes,
                    &mut items,
                )
            }
            SessionEvent::ObjectDestroyed(guid) => {
                death::forget_corpse(guid, &mut death_net);
                // The party hook runs FIRST and on the same edge the reference takes it: the
                // deactivate virtual reads the descriptor that is about to go (decision 1640).
                let store = index.0.get(&guid).and_then(|e| stores.get(*e).ok());
                group::member_deactivated(guid, &mut group, store, &net_commands);
                objects::object_destroyed(guid, &mut commands, &mut index, &mut items)
            }
            SessionEvent::ObjectsRemoved(guids) => {
                // OUT_OF_RANGE and DESTROY take the same virtual in the reference — so the
                // snapshot + `CMSG_REQUEST_PARTY_MEMBER_STATS` fire here too, which is the edge
                // report B334 is actually about: a member walking over the hill.
                for guid in &guids {
                    let store = index.0.get(guid).and_then(|e| stores.get(*e).ok());
                    group::member_deactivated(*guid, &mut group, store, &net_commands);
                }
                objects::objects_removed(guids, &mut commands, &mut index)
            }
            SessionEvent::MonsterMove {
                guid,
                transport,
                start,
                spline_id,
                path,
                facing,
                stop,
                duration_ms,
                flying,
                run_mode,
            } => objects::monster_move(
                guid,
                transport,
                start,
                spline_id,
                path,
                facing,
                stop,
                duration_ms,
                flying,
                run_mode,
                objects::modes_of(guid, &index, &unit_modes, &staged_modes).rooted(),
                &mut commands,
                &index,
                &mut transforms,
                &mut riders,
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
            } => {
                // Every streamed roster member's object is about to be purged — the same
                // deactivation the reference runs one object at a time (decision 1640).
                group::roster_deactivated(&mut group, &index, &stores, &net_commands);
                session::worldport(
                    map_id,
                    position,
                    orientation,
                    needs_ack,
                    &mut commands,
                    &mut index,
                    &mut pending_transfer,
                    &transports,
                    &mut worldports,
                )
            }
            SessionEvent::TransferPending {
                map_id,
                transport_entry,
            } => session::transfer_pending(map_id, transport_entry, &mut pending_transfer),
            SessionEvent::TransferAborted { reason } => {
                session::transfer_aborted(reason, &mut pending_transfer)
            }
            SessionEvent::TimeSpeed {
                hours,
                minutes,
                day_serial,
                timescale,
            } => session::time_speed(hours, minutes, day_serial, timescale, &mut server_time),
            SessionEvent::ServerUnixTime { unix_time } => {
                session::server_unix_time(unix_time, &mut wall_clock)
            }
            SessionEvent::Reputations { standings } => {
                session::reputations(standings, &mut reputations)
            }
            SessionEvent::ReputationDelta { standings } => {
                // The chat line reads the deltas against the store, so it runs BEFORE the
                // overwrite — after it, every delta is zero.
                combat_chat::faction_standing(
                    &standings,
                    &reputations,
                    faction_catalog.as_deref(),
                    &mut chat_log,
                );
                session::reputation_delta(standings, &mut reputations, &mut quest)
            }
            SessionEvent::ReputationVisible { list_id } => {
                session::reputation_visible(list_id, &mut reputations)
            }
            SessionEvent::BindPoint { area } => home_bind.0 = Some(area),
            // The honor arc's two inbound messages (decision 1512).
            //
            // The inspect reply REPLACES whatever is held, including for a different player: the
            // reference's latch is a single slot, and a pane still showing the last target's
            // kills is the failure keeping the old one produces.
            SessionEvent::InspectHonorStats(stats) => inspect_honor.0 = Some(stats),
            // An honor award: the combat-log line (name-resolved, so it queues) and the floating
            // number, which are two different surfaces of one packet and are both the reference's.
            // A DISHONORABLE kill arrives here too, carrying NEGATIVE honor — the floating text
            // takes it signed, because the shipped `COMBAT_TEXT_HONOR_GAINED` handler prefixes a
            // "+" only when the number is positive and therefore already expects the other case.
            SessionEvent::PvpCredit(credit) => {
                chat_log.push_pvp_credit(
                    credit.honor,
                    credit.victim_guid,
                    u8::try_from(credit.victim_rank).unwrap_or(0),
                );
                combat_text_events.write(crate::ui_unit::CombatTextEvent {
                    message_type: "HONOR_GAINED",
                    data: Some(credit.honor.to_string()),
                    extra: None,
                });
            }
            // A zero trainer guid is vmangos's "you have no talents to reset" refusal, not a
            // question — there is nothing to ask about, so nothing goes on screen (decision 1580;
            // `crate::ui_talent_wipe`'s header carries why the reference instead re-sends here).
            SessionEvent::TalentWipeConfirm { trainer, cost } => {
                if trainer == 0 {
                    debug!("net: talent wipe refused (no talents to reset) — no dialog");
                } else {
                    debug!("net: trainer {trainer:#x} asks to wipe talents for {cost} copper");
                    talent_wipe.ask(trainer, cost);
                }
            }
            // The pet trainer's question (decision 1963) — the talent-wipe twin above; a zero
            // guid is the reference's own `ERR_TALENT_WIPE_ERROR` leg, carried over as observed.
            SessionEvent::PetUnlearnConfirm { trainer, cost } => {
                if trainer == 0 {
                    debug!("net: pet unlearn refused (zero trainer) — no dialog");
                    ui_error_keys
                        .0
                        .push(crate::ui_action::UiError::key("ERR_TALENT_WIPE_ERROR"));
                } else {
                    debug!("net: trainer {trainer:#x} asks to unlearn the pet for {cost} copper");
                    pet_unlearn.ask(trainer, cost);
                }
            }
            SessionEvent::RaidGroupOnly { delay_ms, reason } => {
                instance_boot.apply(delay_ms, reason, std::time::Instant::now());
            }
            SessionEvent::AreaSpiritHealerTime { healer, ms } => {
                area_spirit.on_time(healer, ms, std::time::Instant::now());
            }
            SessionEvent::BattlefieldStatus(status) => battlefield_queue.apply(status),
            SessionEvent::PvpLogData(data) => battlefield_scoreboard.apply(data),
            SessionEvent::BattlefieldList(list) => battlefield.apply_list(list),
            SessionEvent::GroupJoinedBattleground { result } => battlefield.apply_verdict(result),
            SessionEvent::BattlegroundPlayer { guid, joined } => {
                battlefield.apply_player(guid, joined);
            }
            SessionEvent::MeetingStoneSetQueue { area, status } => {
                meeting_stone.apply(area, status);
            }
            SessionEvent::MeetingStoneNotice(notice) => meeting_stone.apply_notice(notice),
            SessionEvent::TutorialFlags(bytes) => tutorials.apply_flags(&bytes),
            SessionEvent::BattlefieldPositions(packet) => battlefield_positions.apply(packet),
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
                display_id,
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
                display_id,
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
                objects::gameobject_custom_anim(guid, anim_id, &mut go_custom_anims)
            }
            SessionEvent::GameObjectDespawnAnim { guid } => {
                objects::gameobject_despawn_anim(guid, &mut commands, &index)
            }
            SessionEvent::FishNotHooked => loot::fish_verdict(false, &mut ui_error_keys),
            SessionEvent::FishEscaped => loot::fish_verdict(true, &mut ui_error_keys),
            SessionEvent::PlaySound { sound_id } => world::play_sound(sound_id, &mut server_sounds),
            SessionEvent::PlayMusic { music_id } => world::play_music(music_id, &mut server_sounds),
            SessionEvent::PlayObjectSound { sound_id, guid } => {
                world::play_object_sound(sound_id, guid, &index, &mut server_sounds)
            }
            SessionEvent::Weather {
                weather_type,
                grade,
                sound_id,
                instant,
            } => world::weather(weather_type, grade, sound_id, instant, &mut weather_msgs),
            SessionEvent::TextEmote {
                guid,
                text_emote,
                target_name,
            } => anim::text_emote(
                guid,
                text_emote,
                target_name,
                &index,
                &mut emotes,
                &mut chat_log,
            ),
            SessionEvent::Emote { guid, emote_id } => {
                anim::emote(guid, emote_id, &index, &mut emotes)
            }
            // The spell-book/action-bar pair → the action store the UI feed reads
            // (`crate::ui_action`), sent once at login (and the bar again on server-side edits).
            SessionEvent::SpellBook {
                spell_ids,
                cooldowns,
            } => spell_book(
                spell_ids,
                cooldowns,
                &mut player_actions,
                &mut cooldown_store,
            ),
            SessionEvent::ActionButtons { buttons } => action_buttons(buttons, &mut player_actions),
            SessionEvent::SpellLearned { spell_id } => learned_spell(
                spell_id,
                &mut player_actions,
                spell_catalog.as_deref(),
                &mut ui_error_keys,
                &mut learned_in_tab,
            ),
            SessionEvent::SpellRemoved { spell_id } => removed_spell(
                spell_id,
                &mut player_actions,
                spell_catalog.as_deref(),
                &mut ui_error_keys,
            ),
            SessionEvent::SpellSuperceded {
                old_spell_id,
                new_spell_id,
            } => superceded_spell(
                old_spell_id,
                new_spell_id,
                &mut player_actions,
                spell_catalog.as_deref(),
                &mut ui_error_keys,
                &mut learned_in_tab,
            ),
            SessionEvent::CastResult {
                spell_id,
                success,
                reason,
                arg,
            } => cast_result(
                spell_id,
                success,
                reason,
                arg,
                &mut commands,
                &self_guid,
                &index,
                &mut cast_errors,
                &casting_units,
                &mut cast_events,
                &mut cast_bar,
                &mut pending_cast,
                &mut queued_melee,
                &mut cooldown_store,
                &mut auto_repeat,
                spell_catalog.as_deref(),
                &net_commands,
                &mut chain_casts,
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
                &mut equip_errors,
                &mut pending_item_ops,
                &mut lock_transitions,
                &mut loot_latch,
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
                chat::chat_player_not_found(&name, &mut ui_error_keys)
            }
            SessionEvent::ChatWrongFaction => chat::chat_wrong_faction(&mut ui_error_keys),
            // The four world broadcasts — parked for `ui_chat::broadcast`'s resolve pass, which
            // owns the AreaTable/ServerMessages lookups and the joined-defense-channel walk.
            SessionEvent::ZoneUnderAttack { area_id } => chat::broadcast(
                crate::ui_chat::Broadcast::ZoneUnderAttack { area_id },
                &mut chat_log,
            ),
            SessionEvent::DefenseMessage { zone_id, text } => chat::broadcast(
                crate::ui_chat::Broadcast::Defense { zone_id, text },
                &mut chat_log,
            ),
            SessionEvent::ServerMessage { message_type, text } => chat::broadcast(
                crate::ui_chat::Broadcast::Server { message_type, text },
                &mut chat_log,
            ),
            SessionEvent::ChatRestricted => {
                chat::broadcast(crate::ui_chat::Broadcast::ChatRestricted, &mut chat_log)
            }
            SessionEvent::Notification { text } => chat::notification(text, &mut ui_error_texts),
            SessionEvent::AreaTriggerMessage { text } => {
                chat::area_trigger_message(text, &mut ui_error_texts)
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
                group::invited(&mut group, &mut ui_error_keys, &inviter)
            }
            SessionEvent::GroupDecline { name } => {
                group::declined(&mut group, &mut ui_error_keys, &name)
            }
            SessionEvent::GroupUninvited => group::uninvited(&mut group, &mut ui_error_keys),
            SessionEvent::GroupLeaderChanged { name } => group::leader_changed(
                &mut group,
                &mut ui_error_keys,
                &name,
                &self_guid,
                &names,
                &net_commands,
            ),
            SessionEvent::GroupDestroyed => group::destroyed(&mut group, &mut ui_error_keys),
            SessionEvent::GroupList {
                group_type,
                own_flags,
                members,
                leader,
                loot,
            } => group::list(
                &mut group,
                &mut ui_error_keys,
                &mut quest,
                group_type,
                own_flags,
                members,
                leader,
                loot,
                &names,
                &index,
                &net_commands,
            ),
            SessionEvent::PartyCommandResult {
                operation,
                member,
                result,
            } => group::command_result(&mut group, &mut ui_error_keys, operation, &member, result),
            SessionEvent::PartyMemberStats { guid, full, info } => {
                group.apply_stats(guid, full, *info)
            }
            SessionEvent::RaidTargetSet { icon, guid } => group.apply_raid_target(icon, guid),
            SessionEvent::RaidTargetList { entries } => group.apply_raid_target_list(&entries),
            // The ready check (decision 1549, completed by 1989): the open form takes the
            // leader arm or the popup arm by our guid; the ANSWER form — forwarded to the leader
            // alone — is logged for the engine's flags, which the timeout tick sums up after 30 s
            // (1.12 has no per-member answer surface, only the AFK summary line).
            SessionEvent::ReadyCheckRequest => {
                group::ready_check_request(&mut group, &mut ui_error_keys, &self_guid)
            }
            SessionEvent::RaidInstanceInfo { entries } => group.apply_raid_instance_info(entries),
            // A group member pinged (decision 1596). The wire carries raw world floats and the
            // relay is stateless in the reference too — we seat them as the pin and the minimap
            // derives the rest. `map` is the map we are standing on: the server only relays a ping
            // between people who are grouped, and a ping from another map would be dropped by the
            // renderer's own map test anyway.
            SessionEvent::MinimapPing { guid, x, y } => {
                ping.seat((x, y), guid);
            }
            SessionEvent::ReadyCheckAnswer { guid, ready } => {
                group.apply_ready_check_answer(guid, ready != 0)
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
            SessionEvent::LootResponse {
                guid,
                loot_type,
                gold,
                items,
            } => loot_response(
                guid,
                loot_type,
                gold,
                items,
                &mut loot,
                &mut loot_latch,
                &net_commands,
            ),
            SessionEvent::LootError { guid, error } => {
                loot_error(guid, error, &mut ui_error_keys, &mut loot_latch)
            }
            SessionEvent::LootRemoved { slot } => loot_removed(slot, &mut loot),
            SessionEvent::LootMoneyNotify { amount } => loot_money_notify(amount),
            SessionEvent::LootClearMoney => loot_clear_money(&mut loot),
            SessionEvent::LootReleaseResponse { guid } => {
                loot_release_response(guid, &mut loot, &mut loot_latch)
            }
            SessionEvent::ItemPushResult(p) => {
                item_push_result(p, &self_guid, &mut loot, &mut tutorials)
            }
            // ── The group-loot roll family (decision 0591) — the GroupLootFrame feed ───────────
            SessionEvent::LootStartRoll(p) => loot_start_roll(p, &mut loot_rolls),
            SessionEvent::LootRoll(p) => loot_roll(p, &mut loot_rolls),
            SessionEvent::LootRollWon(p) => loot_roll_won(p, &mut loot_rolls),
            SessionEvent::LootAllPassed(p) => loot_all_passed(p, &mut loot_rolls),
            // ── Master loot (decision 1675) — the candidate list, ahead of its LootResponse ───
            SessionEvent::LootMasterList { candidates } => loot_master_list(candidates, &mut loot),
            // ── The death arc (decision 0308) — arm bodies in `death` ─────────────────────────
            SessionEvent::CorpseQuery {
                found,
                display_map,
                position,
                corpse_map,
            } => death::corpse_query(found, display_map, position, corpse_map, &mut death_net),
            SessionEvent::CorpseReclaimDelay { delay_ms } => {
                death::corpse_reclaim_delay(delay_ms, real_clock.elapsed_secs_f64(), &mut death_net)
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
            SessionEvent::DurabilityDamageDeath => death::durability_damage_death(&mut chat_log),
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
            // ── The observer movement-mode family (decision 1780) — the same modes, on a body
            //    somebody else is driving. No ack, so this arm ends the packet.
            SessionEvent::SplineMoveMode { guid, mode, apply } => objects::spline_move_mode(
                guid,
                mode,
                apply,
                &mut commands,
                &index,
                &mut unit_modes,
                &mut remote_motion,
                &mut staged_modes,
            ),
            SessionEvent::KnockBack {
                guid,
                counter,
                launch,
            } => session::knock_back(guid, counter, launch, &self_guid, &mut knockbacks),
            SessionEvent::ItemTemplate { entry, info } => {
                item_template(entry, info.map(|b| *b), &mut items)
            }
            SessionEvent::AttackStart { attacker, victim } => {
                combat::attack_start(attacker, victim, &mut commands, &index)
            }
            SessionEvent::AttackStop { attacker, victim } => {
                combat::attack_stop(attacker, victim, &mut commands, &index, &mut swing_flushes)
            }
            SessionEvent::AiReaction { unit, reaction } => {
                combat::ai_reaction(unit, reaction, &index, &mut ai_reactions)
            }
            SessionEvent::AttackerState(s) => {
                combat_chat::attacker_state(s, &chat_ctx!(), &stores, &transforms, &mut chat_log);
                combat::attacker_state(
                    s,
                    &index,
                    &self_guid,
                    &mut swings,
                    &mut swing_impacts,
                    &mut combat_text_events,
                    &mut sheath_requests,
                    &mut swing_refusal_edges,
                    &stores,
                    play_seq.next(),
                )
            }
            SessionEvent::AttackSwingError(e) => {
                combat::attack_swing_error(e, &mut swing_refusal_edges)
            }
            SessionEvent::CancelCombat => combat::cancel_combat(&mut swing_refusal_edges),
            SessionEvent::FeignDeathResisted => combat::feign_death_resisted(&mut ui_error_keys),
            SessionEvent::SpellDamageLog(s) => {
                combat_chat::spell_damage_log(s, &chat_ctx!(), &stores, &transforms, &mut chat_log);
                combat_log::spell_damage_log(
                    s,
                    &index,
                    &self_guid,
                    &stores,
                    spell_catalog.as_deref(),
                    *combat_cvars.damage_text,
                    &mut combat_text_spawns,
                    &mut unit_combat_feedback,
                    &mut combat_text_events,
                )
            }
            SessionEvent::PeriodicAuraLog(s) => {
                // **`CombatLogPeriodicSpells` gates the WHOLE packet body, and this arm is where
                // that is expressible.** The reference's read site `0x626dee` is the first thing
                // the handler `0x626dd0` does, and a zero jumps to the bare epilogue `0x6271b4`:
                // no chat line, no floating tick number, no periodic miss word. Gating inside
                // either half below would model it as two filters; it is one gate over both.
                if !combat_cvars.periodic.0 {
                    continue;
                }
                combat_chat::periodic_aura_log(
                    &s,
                    &chat_ctx!(),
                    &stores,
                    &transforms,
                    &mut chat_log,
                );
                combat_log::periodic_aura_log(
                    s,
                    &index,
                    &self_guid,
                    &stores,
                    spell_catalog.as_deref(),
                    *combat_cvars.damage_text,
                    &mut combat_text_spawns,
                    &mut unit_combat_feedback,
                    &mut combat_text_events,
                    &names,
                    &net_commands,
                )
            }
            SessionEvent::SpellHealLog(s) => {
                combat_chat::spell_heal_log(s, &chat_ctx!(), &stores, &transforms, &mut chat_log);
                combat_log::spell_heal_log(
                    s,
                    &index,
                    &self_guid,
                    &mut unit_combat_feedback,
                    &mut combat_text_events,
                    &names,
                    &net_commands,
                )
            }
            SessionEvent::SpellEnergizeLog(s) => {
                combat_chat::spell_energize_log(
                    s,
                    &chat_ctx!(),
                    &stores,
                    &transforms,
                    &mut chat_log,
                );
                combat_log::spell_energize_log(s, &self_guid, &mut combat_text_events)
            }
            SessionEvent::DamageShield(s) => {
                combat_chat::damage_shield(s, &chat_ctx!(), &stores, &transforms, &mut chat_log);
                combat_log::damage_shield(
                    s,
                    &index,
                    &self_guid,
                    &stores,
                    *combat_cvars.damage_text,
                    &mut combat_text_spawns,
                    &mut unit_combat_feedback,
                )
            }
            SessionEvent::SpellLogMiss(s) => {
                combat_chat::spell_log_miss(&s, &chat_ctx!(), &stores, &transforms, &mut chat_log);
                combat_log::spell_log_miss(
                    s,
                    &index,
                    &self_guid,
                    &stores,
                    spell_catalog.as_deref(),
                    *combat_cvars.damage_text,
                    &mut combat_text_spawns,
                    &mut unit_combat_feedback,
                    &mut combat_text_events,
                )
            }
            // ── the combat log's completeness block (1703) ────────────────────────────────
            // Every one of these is chat-only: they carry no damage number, so unlike their
            // neighbours above they have no floating-text twin to call.
            SessionEvent::PartyKillLog(k) => {
                combat_chat::party_kill_log(k, &chat_ctx!(), &stores, &transforms, &mut chat_log)
            }
            SessionEvent::SpellInstaKillLog(k) => combat_chat::spell_insta_kill_log(
                k,
                &chat_ctx!(),
                &stores,
                &transforms,
                &mut chat_log,
            ),
            SessionEvent::ProcResist(o) => combat_chat::spell_outcome_log(
                o,
                false,
                &chat_ctx!(),
                &stores,
                &transforms,
                &mut chat_log,
            ),
            SessionEvent::SpellOrDamageImmune(o) => combat_chat::spell_outcome_log(
                o,
                true,
                &chat_ctx!(),
                &stores,
                &transforms,
                &mut chat_log,
            ),
            SessionEvent::SpellDispelLog(d) => {
                combat_chat::spell_dispel_log(&d, &chat_ctx!(), &stores, &transforms, &mut chat_log)
            }
            SessionEvent::DispelFailed(d) => {
                combat_chat::dispel_failed(&d, &chat_ctx!(), &stores, &transforms, &mut chat_log)
            }
            SessionEvent::EnchantmentLog(e) => {
                combat_chat::enchantment_log(e, &chat_ctx!(), &stores, &transforms, &mut chat_log)
            }
            SessionEvent::SpellLogExecute(x) => combat_chat::spell_log_execute(
                &x,
                &chat_ctx!(),
                &stores,
                &transforms,
                &mut chat_log,
            ),
            SessionEvent::XpGain(x) => combat_log::xp_gain(
                x,
                &index,
                &self_guid,
                &mut combat_text_spawns,
                &mut chat_log,
            ),
            SessionEvent::ExplorationXp(x) => combat_log::exploration_xp(
                x,
                area_table.as_deref(),
                exploration_sounds.as_deref(),
                &index,
                &self_guid,
                &stores,
                &mut server_sounds,
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
                &mut cast_events,
                &self_guid,
                &mut cast_bar,
                &mut pending_cast,
                spell_catalog.as_deref(),
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
                &casting_units,
                &mut cast_events,
                &mut spell_go_targets,
                &self_guid,
                &stores,
                &mut cast_bar,
                &mut pending_cast,
                &mut queued_melee,
                &mut combat_text_spawns,
                *combat_cvars.damage_text,
                &mut go_lid_opens,
                &mut loot_latch,
                (
                    &mut cooldown_store,
                    spell_catalog.as_deref(),
                    &mut items,
                    &net_commands,
                    &mut pet_bar,
                ),
                (
                    &mut auto_repeat,
                    &mut sheath_requests,
                    !engaged_self.is_empty(),
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
                &casting_units,
                &mut cast_events,
                &self_guid,
                &mut cast_bar,
                &mut pending_cast,
                &mut queued_melee,
                play_seq.next(),
            ),
            SessionEvent::SpellDelayed { caster, delay_ms } => spell_delayed(
                caster,
                delay_ms,
                &self_guid,
                &mut cast_bar,
                &mut pending_cast,
            ),
            SessionEvent::CancelAutoRepeat => cancel_auto_repeat(
                &mut auto_repeat,
                &self_guid,
                &index,
                &mut commands,
                &net_commands,
            ),
            SessionEvent::SpellCooldowns { caster, cooldowns } => {
                if let Some(store) =
                    addressed_store(caster, &self_guid, &mut cooldown_store, &mut pet_bar)
                {
                    spell_cooldowns(caster, cooldowns, spell_catalog.as_deref(), store);
                }
            }
            SessionEvent::ItemCooldown {
                item_guid,
                spell_id,
            } => item_cooldown(item_guid, spell_id, &items, &mut cooldown_store),
            // The item-lifetime countdown's ONLY feed (decision 1933): park the deadline on the
            // item store, exactly as the enchant timer below does — vmangos's own writer says the
            // `ITEM_FIELD_DURATION` field the client also holds is not what it displays from.
            SessionEvent::ItemTime { item_guid, seconds } => {
                items.set_item_duration(item_guid, seconds)
            }
            // The temporary-enchant countdown's ONLY feed (decision 0920): park the deadline on the
            // item store, which every tooltip surface reads back through `enchant_remaining_ms`.
            SessionEvent::ItemEnchantTime {
                item_guid,
                slot,
                seconds,
            } => items.set_enchant_deadline(item_guid, slot, seconds),
            SessionEvent::CooldownEvent { spell_id, caster } => {
                if let Some(store) =
                    addressed_store(caster, &self_guid, &mut cooldown_store, &mut pet_bar)
                {
                    cooldown_event(spell_id, caster, store);
                }
            }
            SessionEvent::ClearCooldown { spell_id, caster } => {
                if let Some(store) =
                    addressed_store(caster, &self_guid, &mut cooldown_store, &mut pet_bar)
                {
                    clear_cooldown(spell_id, caster, store);
                }
            }
            SessionEvent::CooldownCheat { caster } => {
                if let Some(store) =
                    addressed_store(caster, &self_guid, &mut cooldown_store, &mut pet_bar)
                {
                    cooldown_cheat(caster, store);
                }
            }
            // The pet action bar (decision 0982) — server-authoritative, so PET_SPELLS is a
            // wholesale replace and its zero-guid form is the teardown.
            SessionEvent::PetSpells(spells) => {
                pet::pet_spells(*spells, spell_catalog.as_deref(), &mut pet_bar)
            }
            SessionEvent::PetMode(mode) => pet::pet_mode(mode, &mut pet_bar),
            SessionEvent::PetActionFeedback { reason } => {
                pet::pet_action_feedback(reason, &mut ui_error_keys)
            }
            SessionEvent::PetCastFailed { spell_id, reason } => {
                pet::pet_cast_failed(spell_id, reason, &mut cast_errors)
            }
            // The three pet-feedback arms and the pet's voice (decision 2039). Each was
            // name-table-only until then; each is something the reference visibly does.
            SessionEvent::PetTameFailure { reason } => {
                pet::pet_tame_failure(reason, &mut pet_tame_failures)
            }
            SessionEvent::PetNameInvalid => pet::pet_name_invalid(&mut ui_error_keys),
            SessionEvent::PetBroken => pet::pet_broken(&mut ui_error_keys),
            SessionEvent::PetActionSound { pet_guid, talk } => {
                pet::pet_action_sound(pet_guid, talk, &index, &mut pet_talks)
            }
            SessionEvent::PetDismissSound { model_id, position } => {
                pet::pet_dismiss_sound(model_id, position, &mut pet_dismiss_sounds)
            }
            SessionEvent::ChannelStart {
                spell_id,
                duration_ms,
            } => channel_start(spell_id, duration_ms, &mut active_channel, &mut cast_bar),
            SessionEvent::ChannelUpdate { remaining_ms } => {
                channel_update(remaining_ms, &mut active_channel, &mut cast_bar)
            }
            SessionEvent::AuraDuration { slot, remaining_ms } => aura_duration(
                slot,
                remaining_ms,
                &mut aura_durations,
                real_clock.elapsed_secs_f64(),
            ),
            SessionEvent::SpellModifier {
                flat,
                mask_bit,
                op,
                value,
            } => set_spell_modifier(flat, mask_bit, op, value, &mut spell_mods),
            SessionEvent::PlaySpellVisual { unit, kit_id } => {
                anim::play_spell_visual(unit, kit_id, &index, play_seq, &mut kit_pushes)
            }
            SessionEvent::EnvironmentalDamageLog(e) => {
                combat_chat::environmental_damage_log(
                    e,
                    &chat_ctx!(),
                    &stores,
                    &transforms,
                    &mut chat_log,
                );
                anim::environmental_damage_log(
                    e,
                    &index,
                    env_damage_table.as_deref(),
                    play_seq,
                    &mut kit_pushes,
                )
            }
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
            SessionEvent::GossipPoi(poi) => npc::gossip_poi(
                &poi,
                &mut poi_marker,
                current_map.as_ref().map_or(0, |m| m.0),
                real_clock.elapsed_secs_f64(),
            ),
            // Questgiver panels (decision 0088): fill the `QuestGiver` the quest feed
            // (`crate::ui_quest`) reads. Each panel packet replaces the open view; the greeting/gossip
            // quest-row clicks and the panel buttons flow back out through the quest/gossip drains.
            SessionEvent::QuestGiverStatus { npc, status } => {
                quest_giver_status(npc, status, &mut quest)
            }
            SessionEvent::QuestGreeting(list) => quest_greeting(list, &mut quest),
            SessionEvent::QuestDetail(d) => quest_detail(d, &mut quest, &net_commands),
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
            SessionEvent::QuestFailed { quest_id, timed } => {
                quest_failed(quest_id, timed, &mut quest_log, &net_commands, &mut quest)
            }
            SessionEvent::QuestLogFull => quest_log_full(&mut quest),
            // The party quest-share (decision 1733): one member's verdict on a quest we pushed,
            // and the escort-quest confirm. Both park in `QuestShare` for `crate::ui_quest_share`
            // to name and raise — the guid needs a name query the apply pass has no VM to await.
            SessionEvent::QuestPushResult { member, msg } => {
                quest_push_result(member, msg, &mut quest_share)
            }
            SessionEvent::QuestConfirmAccept(c) => quest_confirm_accept(c, &mut quest_share),
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
                npc::trainer_buy_succeeded(trainer, spell_id, &mut trainer_open, &net_commands)
            }
            SessionEvent::TrainerBuyFailed { error, .. } => {
                npc::trainer_buy_failed(error, &mut trainer_errors)
            }
            SessionEvent::InvalidatePlayer { guid } => names::invalidate_player(guid, &mut names),
            SessionEvent::ListStabledPets {
                npc,
                num_stable_slots,
                pets,
            } => npc::list_stabled_pets(npc, num_stable_slots, pets, &mut stable_open, &mut names),
            SessionEvent::StableResult { result } => {
                npc::stable_result(result, &mut stable_open, &mut stable_errors, &net_commands)
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
            SessionEvent::VendorBuyFailed {
                vendor,
                item_entry,
                reason,
            } => npc::vendor_buy_failed(
                vendor,
                item_entry,
                reason,
                &mut merchant,
                &mut merchant_errors,
            ),
            SessionEvent::VendorSellFailed { reason, .. } => {
                npc::vendor_sell_failed(reason, &mut merchant_errors)
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
                &unit_speeds,
                &mut speed_stage,
                &self_guid,
                &mut speed_changes,
            ),
            SessionEvent::SpeedChanged { guid, kind, speed } => {
                objects::speed_changed(guid, kind, speed, &index, &unit_speeds, &mut speed_stage)
            }
            // ── The mount arc (decision 0441) — arm bodies in `mount` ─────────────────────────
            SessionEvent::MountResult { mount, code } => {
                mount::mount_result(mount, code, &mut mount_errors)
            }
            SessionEvent::MountSpecial { guid } => {
                mount::mount_special(guid, &self_guid, &index, &mut mount_flourishes)
            }
            // Possession's control half (B211). Forwarded whole and unjudged: the guid may name us
            // (a revoke) or somebody else (a grant), and only the controller can act on either —
            // it owns the pose to park and the mover claim to send.
            SessionEvent::ClientControl { mover, allow_move } => {
                client_control.write(super::ClientControlMessage { mover, allow_move });
            }
            // **The pong never gets here** — the read thread measures it against the ping clock
            // the instant it lands and stops it, the way the reference's `OnData 0x537b10` hands
            // `SMSG_PONG` to `HandlePong 0x537d60` inline instead of queueing it (`net::io`).
            // Reaching this arm means that bypass was undone and every latency reading is a
            // client frame too slow again, which is B346 exactly — so it says so out loud rather
            // than measuring here and hiding it.
            SessionEvent::Pong { sequence } => {
                warn!("net: pong seq={sequence} reached the drain — the read thread's RTT bypass is gone (B346)");
            }
            SessionEvent::PacketDropped {
                opcode,
                unparseable,
            } => session::packet_dropped(opcode, unparseable, &mut dropped),
            // The book-page cache (decision 1105) — one page per packet, the whole chain in
            // answer to the first ask; the reader repaints off it on the next feed.
            SessionEvent::PageText {
                page_id,
                text,
                next_page_id,
            } => page_texts.insert(page_id, text, next_page_id),
            SessionEvent::WorldStates { scope, states } => {
                world::world_states(scope, states, &mut world_states)
            }
            // A kind with no arm here is one a subsystem has claimed in the handler table
            // (decision 2305) — routed there by the drain, so it never reaches this match — or a
            // new kind nobody handles, which `every_session_event_kind_has_one_owner` names at
            // test time and this names at run time: the reference discards an unregistered
            // opcode in silence; this client says so.
            other => error!(
                "net: {:?} reached the dispatch match with no arm — neither peeled nor handled",
                benilla_protocol::SessionEventKind::from(&other)
            ),
        }
    }
    // Flush the staged descriptor seeds/deltas onto the entities born this drain (now spawned by the
    // above Commands) — one insert each, fully merged, so no partial delta clobbers another.
    for (guid, fields) in pending {
        if let Some(&e) = index.0.get(&guid) {
            commands.entity(e).insert(ObjectStore(fields));
        }
    }
    // And the movers' speed sets, for the same reason (decision 1478) — one insert each, carrying
    // the create block and every force-change this drain saw, folded in the order they arrived.
    speed_stage.flush(&mut commands, &index);
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
            // Identity only. The controller-fed [`crate::creature_anim::MovementState`] used to
            // ride along here, but it belongs to whichever body we are *steering*, which is not
            // always this one — `player::embody` owns it now (decision 1281).
            commands.entity(entity).insert(SelfPlayer);
        }
    }
}
