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
    ServerTime, ServerWallClock, TeleportMessage, UnitMoveModes, WorldportMessage,
};
use benilla_world::weather::WeatherMessage;

mod anim;
mod auction;
mod chat;
mod combat;
mod combat_chat;
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
    spell_book, spell_chain_targets, spell_cooldowns, spell_delayed, spell_failed_other, spell_go,
    spell_start, superceded_spell,
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
    // One tuple param (the 16-SystemParam ceiling this signature already lives against): the two
    // per-unit motion states the drain writes. [`RemoteMotion`] is a relayed player's dead-reckon;
    // [`UnitMoveModes`] is any unit's server-granted movement modes (decision 1780). Different
    // components, one concern — what the wire says about how a body we don't control is moving.
    mut motion: (Query<&mut RemoteMotion>, Query<&mut UnitMoveModes>),
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
        // The two server-authored mover edges the controller both *applies* and *answers*, paired to
        // stay inside Bevy's 16-element tuple limit — and they do belong together: a granted mode
        // (decisions 0308, 0866) and a knockback launch (decision 1702) are the same handshake, an
        // edge the server may not act on until our own live pose comes back.
        (
            MessageWriter<super::MoveModeMessage>,
            MessageWriter<super::KnockBackMessage>,
        ),
        // The login screen's dialog + reconnect-policy feed (decision 0539).
        MessageWriter<super::LoginStageMessage>,
        // The login queue's position feed (decision 1681).
        MessageWriter<super::LoginQueuedMessage>,
        MessageWriter<super::LoginFailedMessage>,
        MessageWriter<super::DisconnectedMessage>,
        // The server's own answer to a GM dot-command, readable rather than only logged — the
        // probe shield's confirmation channel (decision 0677).
        MessageWriter<super::ServerSaidMessage>,
        // A `MSG_MOVE_*` the server addressed to OUR mover (decision 0725): a pose it wrote, with
        // no handshake — the controller applies it in `player::wire_in`.
        MessageWriter<super::SelfMoveMessage>,
        // The possession handoff (`SMSG_CLIENT_CONTROL_UPDATE`): control of a unit granted or
        // revoked. Forwarded verbatim — only the controller knows the pose it would have to park.
        MessageWriter<super::ClientControlMessage>,
        // A cinematic to play (`SMSG_TRIGGER_CINEMATIC`) — `crate::cinematic` owns both the
        // playback and the ack that has to answer it (decision 0196).
        MessageWriter<super::CinematicTriggeredMessage>,
    ),
    // One tuple param (the 16-SystemParam ceiling): the ask-once query caches + the gossip/merchant
    // state the net drain fills for the NPC-interaction windows (decision 0081).
    caches: (
        ResMut<crate::names::NameCache>,
        ResMut<crate::items::Items>,
        ResMut<crate::ui_gossip::GossipState>,
        ResMut<crate::ui_merchant::MerchantOpen>,
        // Nested pair: the two gossip-reached NPC service sessions the net drain fills whose
        // state is a whole open window (the tuple is at the 16-param ceiling).
        (
            ResMut<crate::ui_trainer::TrainerOpen>,
            ResMut<crate::ui_stable::StableOpen>,
        ),
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
        // Nested pair (the tuple is at the 16-param ceiling): the quest-log template cache, and
        // the party quest-share state (decision 1733) — the verdicts on a quest we pushed and the
        // escort confirm. Same family, one slot; unlike most nestings here these two genuinely
        // belong together, since a share is a quest-log verb.
        (
            ResMut<crate::ui_quest_log::QuestLog>,
            ResMut<crate::ui_quest_share::QuestShare>,
        ),
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

            // escape this tuple already is. Third member: the guild session (decision 1257) —
            // the identity/roster mirror the seven `SessionEvent::Guild*` arms below drive.
            (
                ResMut<crate::ui_item_text::PageTexts>,
                ResMut<crate::net::PlayedTimeAnswer>,
                ResMut<crate::ui_guild::GuildState>,
                // The innkeeper's pending bind question (decision 1331) — `SMSG_BINDER_CONFIRM`
                // parks the innkeeper's guid here and `crate::ui_binder` turns it into the
                // CONFIRM_BINDER dialog, whose Accept is the only thing that binds anything.
                ResMut<crate::ui_binder::BinderState>,
                // The class trainer's pending respec question (decision 1580) — the inbound
                // `MSG_TALENT_WIPE_CONFIRM` parks the trainer's guid + cost here and
                // `crate::ui_talent_wipe` turns it into the CONFIRM_TALENT_WIPE dialog, whose
                // Accept is the only thing that unlearns anything. The binder's twin above.
                ResMut<crate::ui_talent_wipe::TalentWipeState>,
                // The guard's directions marker (`SMSG_GOSSIP_POI`) and the map id it has to be
                // stamped with — the wire carries no map field, so "where you were standing when
                // the guard told you" is the client's to remember (`crate::poi_marker`).
                ResMut<crate::poi_marker::PoiMarker>,
                Option<Res<benilla_world::world_map::CurrentMap>>,
                // The inspect-honor reply (decision 1512) — `MSG_INSPECT_HONOR_STATS` is the only
                // source of another player's honor numbers, so the reply parks here and
                // `crate::ui_honor` pushes it into the pane and fires `INSPECT_HONOR_UPDATE`.
                ResMut<crate::ui_honor::InspectHonor>,
                // The auctioneer session (decision 1511) — the hello reply opens it, the three
                // list results fill it, and `crate::ui_auction` feeds it to the window.
                ResMut<crate::ui_auction::AuctionOpen>,
                // Guild Member Alert (decision 1589) — the CVar knob the sign-on/sign-off pair's
                // display condition reads; see `ui_guild::apply::event` for the four conjuncts.
                Res<crate::ui_guild::GuildMemberNotify>,
                // The minimap ping (decision 1596) — a group member's `MSG_MINIMAP_PING` seats the
                // world point here and the minimap renderer derives everything else from it.
                ResMut<crate::minimap::MinimapPing>,
                // The GM ticket (decision 1673) — `SMSG_GMTICKET_GETTICKET` replaces the held
                // ticket here and BUMPS AN ANSWER COUNTER, which is what `crate::ui_gm_ticket`
                // diffs on: the Help window re-polls every 10 minutes and an unchanged answer
                // still has to re-fire `UPDATE_TICKET`.
                ResMut<crate::ui_gm_ticket::GmTicketState>,
                // The guild-charter session (decision 1672) — two resources because only the
                // registrar half is NPC-bound: `SMSG_PETITION_SHOWLIST` opens the registrar, and
                // `SMSG_PETITION_SHOW_SIGNATURES` opens the item-bound charter window, which must
                // survive walking away from the registrar.
                ResMut<crate::ui_petition::GuildRegistrarState>,
                ResMut<crate::ui_petition::PetitionState>,
                // The pending summon question (decision 1747) — `SMSG_SUMMON_REQUEST` parks the
                // summoner's guid, zone and expiry here and `crate::ui_summon` turns it into the
                // CONFIRM_SUMMON dialog, whose Accept is the only packet in the flow.
                ResMut<crate::ui_summon::SummonState>,
                // The instance-lockout bookkeeping (decision 1748) — four of its six packets
                // queue a GlobalStrings-templated chat line here for `crate::ui_instance` to
                // resolve against the VM, and two write the latch behind
                // `CanShowResetInstances()`.
                ResMut<crate::ui_instance::InstanceState>,
            ),
        ),
    ),
    // One tuple param (the 16-SystemParam ceiling again): the action-bar- + merchant-facing errors
    // and the cast-bar feed (decision 0137), plus the item-lock bookkeeping the inventory-failure
    // arm also drains (decision 0216 §4 / 0218 §3 — this apply site has no `UiScript` to fire
    // `ITEM_LOCK_CHANGED` through, so the transitioned slots queue in `LockTransitions` for
    // the container feed to pick up).
    mut ui_actions: (
        ResMut<crate::ui_action::PlayerActions>,
        // Nested group (the tuple is at the 16-param ceiling): the cast + mount error queues,
        // both drained into the red error line by `ui_action::feed_actions`, plus the
        // FactionTemplate catalog the combat log's friend/foe split reads (B297). The catalog has
        // nothing to do with the other two — it rides here purely because this is where the
        // ceiling left room. Absent if the DBC failed to load, which degrades an unresolved unit
        // to the friendly side of the classifier rather than dropping its lines.
        (
            ResMut<crate::ui_action::CastErrors>,
            ResMut<crate::ui_action::MountErrors>,
            Option<Res<crate::target::ring::Factions>>,
            // The `modalNextSpell` chain's outbox (1597) — `cast_result` fills it, the ui_action
            // drain sends it through the one cast path. Rides here for the same reason the
            // catalog does: this is where the ceiling left room.
            ResMut<crate::ui_action::ChainCasts>,
        ),
        ResMut<crate::ui_items::EquipErrors>,
        ResMut<crate::ui_merchant::MerchantErrors>,
        ResMut<crate::ui_loot::LootErrors>,
        ResMut<crate::ui_cast::CastBarFeed>,
        ResMut<crate::pending_item_ops::PendingItemOps>,
        ResMut<crate::pending_item_ops::LockTransitions>,
        // Nested pair (the tuple is at the ceiling): the two NPC-service windows' error queues,
        // each drained onto its window's red line by its own feed.
        (
            ResMut<crate::ui_trainer::TrainerErrors>,
            ResMut<crate::ui_stable::StableErrors>,
        ),
        ResMut<crate::ui_cast::PendingCast>,
        // The cooldown store + the Spell.dbc catalog its wire laws read, and the live
        // auto-repeat state the bar's flash rides (decision 0137 phase 4).
        ResMut<crate::cooldowns::Cooldowns>,
        Option<Res<crate::ui_action::Spells>>,
        ResMut<crate::ui_action::AutoRepeatActive>,
        // Our own running channel (the IsCurrentAction channel leg, decision 0137 phase 4).
        ResMut<crate::ui_cast::ActiveChannel>,
        // Pre-formatted UIErrorsFrame lines — text the wire already resolved, so there is no
        // GlobalStrings key to look up: the death durability notice, `SMSG_NOTIFICATION`, and
        // `SMSG_AREA_TRIGGER_MESSAGE`. Drained by `ui_action::feed_actions` beside UiErrorKeys.
        ResMut<crate::ui_action::UiErrorTexts>,
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
            // Are *we* already swinging? The ref's `[player+0xc48]`, mirrored by the
            // server-echoed [`crate::creature_anim::Engaged`] — read by the GO handler's deferred
            // auto-attack start (`0x6e83e7`, decision 1593). Filter-only, so it conflicts with
            // nothing else in this drain.
            Query<(), (With<crate::creature_anim::Engaged>, With<SelfPlayer>)>,
        ),
    ),
    // The aura feed's duration side-table + the clock to stamp arrivals (decisions 0255/0257): the
    // self-only `SMSG_UPDATE_AURA_DURATION` lands here keyed by raw slot, timestamped for the
    // `ui_aura` slot-join — plus the ping clock, which this drain now only CLEARS on a
    // disconnect: the measuring moved to the read thread with B346's fix.
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
        // READ-only: every write to a mover's speed set goes through this drain's
        // `objects::SpeedStage` and lands in one flush at the end (decision 1478).
        Query<&super::UnitSpeeds>,
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
        (mut trainer_open, mut stable_open),
        (mut loot, mut loot_latch, mut loot_rolls),
        mut chat_log,
        mut quest,
        (mut quest_log, mut quest_share),
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
            (
                mut page_texts,
                mut played_time_answer,
                mut guild,
                mut binder,
                mut talent_wipe,
                mut poi_marker,
                current_map,
                mut inspect_honor,
                mut auction_open,
                guild_notify,
                mut ping,
                mut gm_ticket,
                mut registrar,
                mut petition,
                mut summon,
                mut instances,
            ),
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
        (mut move_modes, mut knockbacks),
        mut login_stages,
        mut login_queued,
        mut login_failures,
        mut disconnects,
        mut server_said,
        mut self_moves,
        mut client_control,
        mut cinematics,
    ) = session_msgs;
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
                factions: ui_actions.1 .2.as_deref(),
                reputations: &reputations,
                spells: ui_actions.11.as_deref(),
            }
        };
    }
    for ev in events.0.try_iter() {
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
            SessionEvent::CharacterList { characters, realm } => {
                session::character_list(characters, realm, &mut status, &mut char_lists)
            }
            SessionEvent::CharActionResult { action, code } => {
                session::char_action_result(action, code, &mut char_actions)
            }
            SessionEvent::CinematicTriggered { cinematic_id } => {
                session::cinematic_triggered(cinematic_id, &mut cinematics)
            }
            SessionEvent::Connected {
                self_guid: guid,
                name,
                billing_time_rested,
            } => session::connected(
                guid,
                name,
                billing_time_rested,
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
                    &mut mail_open,
                    &mut mail_pending,
                    &mut trade_session,
                    &mut auction_open,
                    &mut bank_open,
                    &mut duel,
                    &mut social,
                    &mut guild,
                    &mut gm_ticket,
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
                    &mut speed_stage,
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
                    &mut motion.0,
                    &mut transforms,
                    &mut audio.13,
                    &mut self_moves,
                );
            }
            SessionEvent::ObjectValues { guid, fields } => {
                // Our corpse's own `CORPSE_FIELD_FLAGS` can flip to BONES under a live guid; the
                // reclaim latch is re-asked on that edge, as the reference's `FLAGS` mirror
                // handler `0x5d6d60` does (1729).
                death::recheck_corpse(guid, &fields, &self_guid, &mut death_net);
                objects::object_values(guid, fields, &index, &mut stores, &mut pending, &mut items)
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
                start,
                spline_id,
                path,
                facing,
                stop,
                duration_ms,
                flying,
                run_mode,
                objects::modes_of(guid, &index, &motion.1, &staged_modes).rooted(),
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
                    &mut aura.6,
                    &aura.7,
                    &mut worldports,
                )
            }
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
                // The chat line reads the deltas against the store, so it runs BEFORE the
                // overwrite — after it, every delta is zero.
                combat_chat::faction_standing(
                    &standings,
                    &reputations,
                    ui_actions.1 .2.as_deref(),
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
                audio.15 .1.write(crate::ui_unit::CombatTextEvent {
                    message_type: "HONOR_GAINED",
                    data: Some(credit.honor.to_string()),
                    extra: None,
                });
            }
            SessionEvent::BinderConfirm { binder: npc } => binder.ask(npc),
            // Someone is asking to pull us to them (decision 1747). The reference gates this in
            // the HANDLER, not the dialog: a dead or ghost player's request is dropped before the
            // latch, so it cannot disturb a live question either (`0x5e6194`). The predicate is
            // `0x605f30` — **health ≤ 0 OR (is-player AND `PLAYER_FLAGS` ghost bit)**, which is
            // both of these accessors and not the one `unit_is_dead` alone would give (a ghost's
            // wire health is 1). A self object we have not streamed yet reads as alive: the
            // reference's own default (`0x5e6189` sends a NULL object through to the latch).
            SessionEvent::SummonRequest {
                summoner,
                zone,
                delay_ms,
            } => {
                let dead_or_ghost = self_guid
                    .0
                    .and_then(|g| index.0.get(&g))
                    .and_then(|e| stores.get(*e).ok())
                    .is_some_and(|s| s.0.unit_is_dead() || s.0.player_is_ghost());
                crate::ui_summon::apply::request(
                    summoner,
                    zone,
                    delay_ms,
                    dead_or_ghost,
                    aura.1.elapsed_secs_f64(),
                    &mut summon,
                );
            }
            // The GM ticket answers (decision 1673). The GETTICKET arm takes EVERY answer,
            // including `None` ("you have no ticket") and including an unsolicited one pushed by a
            // GM's `.ticket view`/`escalate`/`complete` — they are indistinguishable on the wire
            // and want identical handling.
            SessionEvent::GmTicket { ticket } => gm_ticket.answer(ticket),
            SessionEvent::GmTicketSystemStatus { status } => gm_ticket.answer_queue(status),
            // A GM touched the ticket. Value 1 makes the reference re-ask (`0x5e7932`), the same
            // leg the create/update success codes take; 2 (closed) and 3 (survey offered) are
            // recorded and not acted on — 3 is the survey trigger and that window is deferred.
            // vmangos never sends this packet at all, so on our server the arm is dead; cmangos
            // makes it the whole notification model, which is why it is parsed rather than dropped.
            SessionEvent::GmTicketStatusUpdate { status } => {
                crate::ui_gm_ticket::apply::status_update(status, &mut gm_ticket)
            }
            // The three response codes have no consumer in the shipped 1.12 UI — no event, no
            // handler. Logged so a refusal is visible in a session log rather than silent; the
            // `ERR_TICKET_*` display path is still unpinned (see `ui_gm_ticket::apply`).
            // Create-ok (2) and update-ok (4) make the ENGINE re-ask for the ticket — the
            // reference's own `0x5e4479` arm, and the reason the shipped UI needs no handler for
            // either opcode. Without it a filed ticket goes unseen until the 10-minute poll.
            SessionEvent::GmTicketCreated { response } => {
                crate::ui_gm_ticket::apply::write_response("create", response, 2, &mut gm_ticket)
            }
            SessionEvent::GmTicketUpdated { response } => {
                crate::ui_gm_ticket::apply::write_response("update", response, 4, &mut gm_ticket)
            }
            SessionEvent::GmTicketDeleted { response } => {
                crate::ui_gm_ticket::apply::response("delete", response)
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
            SessionEvent::PlayerBound { binder: npc, area } => {
                debug!("net: bound to area {area} by {npc:#x}");
                crate::ui_binder::apply::bound(
                    area,
                    &mut binder,
                    &mut chat_log,
                    area_table.as_deref(),
                    &mut audio.0,
                )
            }
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
                objects::gameobject_custom_anim(guid, anim_id, &mut audio.15 .3)
            }
            SessionEvent::GameObjectDespawnAnim { guid } => {
                objects::gameobject_despawn_anim(guid, &mut commands, &index)
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
            SessionEvent::TextEmote {
                guid,
                text_emote,
                target_name,
            } => anim::text_emote(
                guid,
                text_emote,
                target_name,
                &index,
                &mut audio.2,
                &mut chat_log,
            ),
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
            SessionEvent::SpellRemoved { spell_id } => removed_spell(spell_id, &mut ui_actions.0),
            SessionEvent::SpellSuperceded {
                old_spell_id,
                new_spell_id,
            } => superceded_spell(old_spell_id, new_spell_id, &mut ui_actions.0),
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
                &mut ui_actions.1 .3,
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
                chat::chat_player_not_found(&name, &mut chat_log)
            }
            SessionEvent::ChatWrongFaction => chat::chat_wrong_faction(&mut chat_log),
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
            SessionEvent::Notification { text } => chat::notification(text, &mut ui_actions.14),
            SessionEvent::AreaTriggerMessage { text } => {
                chat::area_trigger_message(text, &mut ui_actions.14)
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
                &mut names,
                &index,
                &net_commands,
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
            // The ready check came back with the Raid tab (decision 1549): the open form bumps
            // the ticket the feed turns into a `READY_CHECK` edge. The ANSWER form is still
            // ignored — the reference has no per-member answer surface in 1.12 (the raid pane
            // shows no ready column; only later clients do), so there is nothing to show and
            // storing it would be state with no reader.
            SessionEvent::ReadyCheckRequest => group.apply_ready_check(),
            SessionEvent::RaidInstanceInfo { entries } => group.apply_raid_instance_info(entries),
            // A group member pinged (decision 1596). The wire carries raw world floats and the
            // relay is stateless in the reference too — we seat them as the pin and the minimap
            // derives the rest. `map` is the map we are standing on: the server only relays a ping
            // between people who are grouped, and a ping from another map would be dropped by the
            // renderer's own map test anyway.
            SessionEvent::MinimapPing { guid, x, y } => {
                ping.seat((x, y), current_map.as_ref().map_or(0, |m| m.0), guid);
            }
            SessionEvent::ReadyCheckAnswer { .. } => {}
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
            // ── The instance/raid lockout family (decision 1748): four lines the client
            // composes itself out of GlobalStrings, and the two-packet latch behind the SELF
            // menu's reset row. The lines are QUEUED — resolving them needs the VM, which this
            // drain has no access to (decision 0669's split) ──
            SessionEvent::RaidInstanceMessage { message } => {
                crate::ui_instance::apply::raid_instance_message(&mut instances, message);
            }
            SessionEvent::InstanceSaveCreated { flag } => {
                crate::ui_instance::apply::instance_save_created(&mut instances, flag);
            }
            SessionEvent::InstanceReset { map } => {
                crate::ui_instance::apply::instance_reset(&mut instances, map);
            }
            SessionEvent::InstanceResetFailed { failure } => {
                crate::ui_instance::apply::instance_reset_failed(&mut instances, failure);
            }
            SessionEvent::UpdateLastInstance { map } => {
                crate::ui_instance::apply::update_last_instance(&mut instances, map);
            }
            SessionEvent::UpdateInstanceOwnership { owns } => {
                crate::ui_instance::apply::update_instance_ownership(&mut instances, owns);
            }
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
            // ── The guild family (decision 1257): the identity cache, the roster, and the
            // `ERR_GUILD_*` lines the engine composes. Every arm's law lives in
            // `ui_guild::apply` beside the state it drives; the guild EVENTS fire off the mirror
            // in `ui_guild::feed_guild`, on their edges.
            SessionEvent::GuildQueryResponse(response) => {
                crate::ui_guild::apply::query_response(&mut guild, response)
            }
            SessionEvent::GuildRoster(roster) => crate::ui_guild::apply::roster(&mut guild, roster),
            // The sign-on/sign-off pair's trailing guid exists for exactly one purpose — the
            // four-conjunct display condition on their line — which is why this arm reads
            // `social`, the notify knob and our own guid (decision 1589; the condition and its
            // byte addresses are on `ui_guild::apply::event`).
            SessionEvent::GuildEvent(notice) => crate::ui_guild::apply::event(
                &mut guild,
                &mut chat_log,
                &social,
                &guild_notify,
                self_guid.0,
                notice,
            ),
            SessionEvent::GuildCommandResult(result) => {
                crate::ui_guild::apply::command_result(&mut guild, &mut chat_log, result)
            }
            SessionEvent::GuildInvite { inviter, guild: g } => {
                crate::ui_guild::apply::invite(&mut guild, &mut chat_log, inviter, g)
            }
            SessionEvent::GuildDecline { name } => {
                crate::ui_guild::apply::decline(&mut chat_log, &name)
            }
            SessionEvent::GuildInfo(info) => crate::ui_guild::apply::info(&mut chat_log, info),
            // ── The petition family (decision 1672): founding a guild. The registrar half is an
            // NPC window, the charter half is item-bound, and they are two resources for that
            // reason — see `ui_petition`'s module doc.
            SessionEvent::PetitionShowList(list) => {
                // The registrar's two `UNIT_NPC_FLAGS` gates are on LIVE NPC state rather than on
                // the packet, so the flags are read here — this pass holds the store. An unstreamed
                // guid reads `None` and fails the gate, as the client's own resolve does.
                let flags = index
                    .0
                    .get(&list.npc)
                    .and_then(|e| stores.get(*e).ok())
                    .map(|s| s.0.unit_npc_flags());
                crate::ui_petition::apply::show_list(&mut registrar, list, flags)
            }
            SessionEvent::PetitionShowSignatures(sigs) => {
                // An ignored owner suppresses the ENTIRE update — no record fetch, no list, no
                // event, no error line (`0x5eeefe`). Consulted before anything else happens.
                let ignored = social.is_ignored(sigs.owner);
                crate::ui_petition::apply::show_signatures(
                    &mut petition,
                    sigs,
                    ignored,
                    &net_commands,
                )
            }
            SessionEvent::PetitionQueryResponse(response) => {
                crate::ui_petition::apply::query_response(&mut petition, response)
            }
            SessionEvent::PetitionSignResults(results) => crate::ui_petition::apply::sign_results(
                &mut petition,
                &mut names,
                self_guid.0.unwrap_or(0),
                results,
                &net_commands,
            ),
            SessionEvent::TurnInPetitionResults { result } => {
                crate::ui_petition::apply::turn_in_results(&mut petition, &mut registrar, result)
            }
            SessionEvent::PetitionDeclined { player } => {
                crate::ui_petition::apply::declined(&mut petition, &names, player)
            }
            SessionEvent::PetitionRenamed(rename) => {
                crate::ui_petition::apply::renamed(&mut petition, rename)
            }
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
                &mut motion.1,
                &mut motion.0,
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
                combat::attack_stop(attacker, victim, &mut commands, &index, &mut audio.9)
            }
            SessionEvent::AiReaction { unit, reaction } => {
                combat::ai_reaction(unit, reaction, &index, &mut audio.11)
            }
            SessionEvent::AttackerState(s) => {
                combat_chat::attacker_state(s, &chat_ctx!(), &stores, &transforms, &mut chat_log);
                combat::attacker_state(
                    s,
                    &index,
                    &self_guid,
                    &mut audio.3,
                    &mut audio.8,
                    &mut audio.15 .1,
                    &mut audio.15 .2,
                    play_seq.next(),
                )
            }
            SessionEvent::SpellDamageLog(s) => {
                combat_chat::spell_damage_log(s, &chat_ctx!(), &stores, &transforms, &mut chat_log);
                combat_log::spell_damage_log(
                    s,
                    &index,
                    &self_guid,
                    &stores,
                    ui_actions.11.as_deref(),
                    &mut audio.7,
                    &mut audio.15 .0,
                    &mut audio.15 .1,
                )
            }
            SessionEvent::PeriodicAuraLog(s) => {
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
                    ui_actions.11.as_deref(),
                    &mut audio.7,
                    &mut audio.15 .0,
                    &mut audio.15 .1,
                    &mut names,
                    &net_commands,
                )
            }
            SessionEvent::SpellHealLog(s) => {
                combat_chat::spell_heal_log(s, &chat_ctx!(), &stores, &transforms, &mut chat_log);
                combat_log::spell_heal_log(
                    s,
                    &index,
                    &self_guid,
                    &mut audio.15 .0,
                    &mut audio.15 .1,
                    &mut names,
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
                combat_log::spell_energize_log(s, &self_guid, &mut audio.15 .1)
            }
            SessionEvent::DamageShield(s) => {
                combat_chat::damage_shield(s, &chat_ctx!(), &stores, &transforms, &mut chat_log);
                combat_log::damage_shield(
                    s,
                    &index,
                    &self_guid,
                    &stores,
                    &mut audio.7,
                    &mut audio.15 .0,
                )
            }
            SessionEvent::SpellLogMiss(s) => {
                combat_chat::spell_log_miss(&s, &chat_ctx!(), &stores, &transforms, &mut chat_log);
                combat_log::spell_log_miss(
                    s,
                    &index,
                    &self_guid,
                    &stores,
                    &mut audio.7,
                    &mut audio.15 .0,
                    &mut audio.15 .1,
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
                &mut loot_latch,
                (
                    &mut ui_actions.10,
                    ui_actions.11.as_deref(),
                    &mut items,
                    &net_commands,
                    &mut pet_bar,
                ),
                (
                    &mut ui_actions.12,
                    &mut audio.15 .2,
                    !audio.15 .4.is_empty(),
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
                    aura.5.as_deref(),
                    play_seq,
                    &mut audio.12,
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
                aura.1.elapsed_secs_f64(),
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
            SessionEvent::QuestFailed { quest_id, timed } => quest_failed(
                quest_id,
                timed,
                &mut quest_log,
                &net_commands,
                &mut chat_log,
                &mut quest,
            ),
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
                npc::trainer_buy_failed(error, &mut ui_actions.8 .0)
            }
            SessionEvent::InvalidatePlayer { guid } => names::invalidate_player(guid, &mut names),
            SessionEvent::ListStabledPets {
                npc,
                num_stable_slots,
                pets,
            } => npc::list_stabled_pets(npc, num_stable_slots, pets, &mut stable_open, &mut names),
            SessionEvent::StableResult { result } => npc::stable_result(
                result,
                &mut stable_open,
                &mut ui_actions.8 .1,
                &net_commands,
            ),
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
            } => {
                npc::vendor_buy_failed(vendor, item_entry, reason, &mut merchant, &mut ui_actions.3)
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
                &aura.3,
                &mut speed_stage,
                &self_guid,
                &mut speed_changes,
            ),
            SessionEvent::SpeedChanged { guid, kind, speed } => {
                objects::speed_changed(guid, kind, speed, &index, &aura.3, &mut speed_stage)
            }
            // ── The mount arc (decision 0441) — arm bodies in `mount` ─────────────────────────
            SessionEvent::MountResult { mount, code } => {
                mount::mount_result(mount, code, &mut ui_actions.1 .1)
            }
            SessionEvent::MountSpecial { guid } => {
                mount::mount_special(guid, &self_guid, &index, &mut audio.14)
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
            // The auction house arc (decision 1511 P1): the hello reply OPENS the window — our
            // send does not — and the three list results fill the three tabs.
            SessionEvent::AuctionHello {
                auctioneer,
                house_id,
            } => auction::auction_hello(auctioneer, house_id, &mut auction_open),
            SessionEvent::AuctionCommandResult {
                auction_id,
                action,
                error,
                tail,
            } => {
                auction::auction_command_result(auction_id, action, error, &tail, &mut auction_open)
            }
            SessionEvent::AuctionListResult {
                auctions,
                total_count,
            } => auction::auction_list_result(auctions, total_count, &mut auction_open),
            SessionEvent::AuctionOwnerListResult {
                auctions,
                total_count,
            } => auction::auction_owner_list_result(auctions, total_count, &mut auction_open),
            SessionEvent::AuctionBidderListResult {
                auctions,
                total_count,
            } => auction::auction_bidder_list_result(auctions, total_count, &mut auction_open),
            SessionEvent::AuctionBidderNotification(n) => {
                auction::auction_bidder_notification(&n, &mut auction_open)
            }
            SessionEvent::AuctionOwnerNotification(n) => {
                auction::auction_owner_notification(&n, &mut auction_open)
            }
            SessionEvent::AuctionRemovedNotification { item_entry, .. } => {
                auction::auction_removed_notification(item_entry, &mut auction_open)
            }
            // The player-trade arc (decision 0592 P1): the status packet drives the open/accept/close
            // state machine, the extended snapshot replaces one side's item/gold — both into the
            // `TradeSession` the trade feed (`crate::ui_trade`) reads.
            SessionEvent::TradeStatus { status } => trade::trade_status(status, &mut trade_session),
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
