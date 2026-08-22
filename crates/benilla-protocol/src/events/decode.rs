//! The [`ServerPacket`]→[`SessionEvent`] mapping: [`decode`]'s per-packet fan-out plus the
//! object-list interpretation ([`decode_objects`] and the spawn-identity readers). Pure functions
//! of the packet — no I/O, no state; the running world model stays the app's.

use crate::messages::{CastOutcome, MovementBlock, Object, ObjectFields, ObjectType, ServerPacket};
use crate::wire::Vector3d;

use super::{EntityKind, MoveSpeeds, SessionEvent};

fn v3(v: Vector3d) -> [f32; 3] {
    [v.x, v.y, v.z]
}

/// Decode one server packet into zero or more [`SessionEvent`]s. Pure: no I/O, no state. Packets the
/// client doesn't model yield an empty list.
pub fn decode(packet: ServerPacket) -> Vec<SessionEvent> {
    match packet {
        ServerPacket::UpdateObject { objects } => decode_objects(objects),
        // A batch is only a transport: unwrap it in order and decode each move exactly as if it had
        // arrived on its own (decision 0624). Order matters — the replay queue in
        // `net::motion` reconstructs a mover's arc from packet order.
        ServerPacket::CompressedMoves { packets } => packets.into_iter().flat_map(decode).collect(),
        ServerPacket::CharEnum { characters } => vec![SessionEvent::CharacterList {
            characters,
            realm: None,
        }],
        ServerPacket::LogoutComplete => vec![SessionEvent::LoggedOut],
        ServerPacket::LogoutResponse { reason, instant } => {
            vec![SessionEvent::LogoutResponse { reason, instant }]
        }
        ServerPacket::LogoutCancelAck => vec![SessionEvent::LogoutCancelled],
        ServerPacket::PlaySound { sound_id } => vec![SessionEvent::PlaySound { sound_id }],
        ServerPacket::PlayMusic { music_id } => vec![SessionEvent::PlayMusic { music_id }],
        ServerPacket::PlayObjectSound { sound_id, guid } => {
            vec![SessionEvent::PlayObjectSound { sound_id, guid }]
        }
        ServerPacket::Weather {
            weather_type,
            grade,
            sound_id,
            instant,
        } => vec![SessionEvent::Weather {
            weather_type,
            grade,
            sound_id,
            instant,
        }],
        ServerPacket::TextEmote {
            guid,
            text_emote,
            target_name,
        } => {
            vec![SessionEvent::TextEmote {
                guid,
                text_emote,
                target_name,
            }]
        }
        ServerPacket::Emote { guid, emote_id } => vec![SessionEvent::Emote { guid, emote_id }],
        ServerPacket::InitialSpells {
            spell_ids,
            cooldowns,
        } => vec![SessionEvent::SpellBook {
            spell_ids: spell_ids.into_iter().map(u32::from).collect(),
            cooldowns,
        }],
        ServerPacket::ActionButtons { buttons } => vec![SessionEvent::ActionButtons { buttons }],
        ServerPacket::LearnedSpell { spell_id } => vec![SessionEvent::SpellLearned {
            spell_id: u32::from(spell_id),
        }],
        ServerPacket::SupercededSpell {
            old_spell_id,
            new_spell_id,
        } => vec![SessionEvent::SpellSuperceded {
            old_spell_id: u32::from(old_spell_id),
            new_spell_id: u32::from(new_spell_id),
        }],
        ServerPacket::CastResult { spell_id, outcome } => vec![SessionEvent::CastResult {
            spell_id,
            success: outcome == CastOutcome::Ok,
            reason: match outcome {
                CastOutcome::Ok => None,
                CastOutcome::Failed { reason, .. } => Some(reason),
            },
            arg: match outcome {
                CastOutcome::Ok => None,
                CastOutcome::Failed { arg, .. } => arg,
            },
        }],
        ServerPacket::PetSpells(spells) => vec![SessionEvent::PetSpells(Box::new(spells))],
        ServerPacket::PetMode(mode) => vec![SessionEvent::PetMode(mode)],
        ServerPacket::PetActionFeedback { reason } => {
            vec![SessionEvent::PetActionFeedback { reason }]
        }
        ServerPacket::PetCastFailed { spell_id, outcome } => vec![SessionEvent::PetCastFailed {
            spell_id,
            reason: match outcome {
                CastOutcome::Ok => None,
                CastOutcome::Failed { reason, .. } => Some(reason),
            },
        }],
        ServerPacket::ItemQueryResponse { entry, info } => {
            vec![SessionEvent::ItemTemplate { entry, info }]
        }
        ServerPacket::MessageChat(m) => vec![SessionEvent::Chat(m)],
        ServerPacket::ChannelNotify(n) => vec![SessionEvent::ChannelNotify {
            notice: n.notice,
            channel: n.channel,
            tail: n.tail,
        }],
        ServerPacket::ChannelList {
            channel,
            flags,
            members,
        } => vec![SessionEvent::ChannelList {
            channel,
            flags,
            members,
        }],
        ServerPacket::ChatPlayerNotFound { name } => {
            vec![SessionEvent::ChatPlayerNotFound { name }]
        }
        ServerPacket::ChatWrongFaction => vec![SessionEvent::ChatWrongFaction],
        ServerPacket::Notification { text } => vec![SessionEvent::Notification { text }],
        ServerPacket::AreaTriggerMessage { text } => {
            vec![SessionEvent::AreaTriggerMessage { text }]
        }
        ServerPacket::PlayedTime { total, level } => {
            vec![SessionEvent::PlayedTime { total, level }]
        }
        ServerPacket::RandomRoll {
            min,
            max,
            roll,
            guid,
        } => vec![SessionEvent::RandomRoll {
            min,
            max,
            roll,
            guid,
        }],
        ServerPacket::InventoryChangeFailure {
            reason,
            required_level,
            item_guid,
            bag_slot,
        } if reason != 0 => {
            vec![SessionEvent::InventoryFailure {
                reason,
                required_level,
                item_guid,
                bag_slot,
            }]
        }
        ServerPacket::AttackStart { attacker, victim } => {
            vec![SessionEvent::AttackStart { attacker, victim }]
        }
        ServerPacket::AttackStop { attacker, victim } => {
            vec![SessionEvent::AttackStop { attacker, victim }]
        }
        ServerPacket::AttackerState(s) => vec![SessionEvent::AttackerState(s)],
        ServerPacket::AiReaction { unit, reaction } => {
            vec![SessionEvent::AiReaction { unit, reaction }]
        }
        ServerPacket::SpellStart(s) => vec![SessionEvent::SpellStart {
            caster: s.caster,
            spell_id: s.spell_id,
            cast_flags: s.cast_flags,
            cast_time_ms: s.cast_time_ms,
            target: s.targets.unit_target,
            ammo_display_id: s.ammo_display_id,
        }],
        ServerPacket::SpellGo(s) => vec![SessionEvent::SpellGo {
            caster: s.caster,
            spell_id: s.spell_id,
            cast_flags: s.cast_flags,
            hits: s.hits,
            misses: s.misses,
            target: s.targets.unit_target,
            go_target: s.targets.go_target,
            dest: s.targets.dest.map(|d| [d.x, d.y, d.z]),
            ammo_display_id: s.ammo_display_id,
            item_caster: (s.item_or_caster != s.caster).then_some(s.item_or_caster),
        }],
        ServerPacket::SpellChainTargets(c) => vec![SessionEvent::SpellChainTargets {
            caster: c.caster,
            spell_id: c.spell_id,
            targets: c.targets,
        }],
        ServerPacket::SpellDelayed { caster, delay_ms } => {
            vec![SessionEvent::SpellDelayed { caster, delay_ms }]
        }
        ServerPacket::SpellFailedOther { caster, spell_id } => {
            vec![SessionEvent::SpellFailedOther { caster, spell_id }]
        }
        ServerPacket::CancelAutoRepeat => vec![SessionEvent::CancelAutoRepeat],
        ServerPacket::SpellCooldownList { caster, cooldowns } => {
            vec![SessionEvent::SpellCooldowns { caster, cooldowns }]
        }
        ServerPacket::ItemCooldown {
            item_guid,
            spell_id,
        } => vec![SessionEvent::ItemCooldown {
            item_guid,
            spell_id,
        }],
        ServerPacket::ItemEnchantTime {
            item_guid,
            slot,
            seconds,
        } => vec![SessionEvent::ItemEnchantTime {
            item_guid,
            slot,
            seconds,
        }],
        ServerPacket::CooldownEvent { spell_id, caster } => {
            vec![SessionEvent::CooldownEvent { spell_id, caster }]
        }
        ServerPacket::ClearCooldown { spell_id, caster } => {
            vec![SessionEvent::ClearCooldown { spell_id, caster }]
        }
        ServerPacket::CooldownCheat { caster } => vec![SessionEvent::CooldownCheat { caster }],
        ServerPacket::ChannelStart {
            spell_id,
            duration_ms,
        } => vec![SessionEvent::ChannelStart {
            spell_id,
            duration_ms,
        }],
        ServerPacket::ChannelUpdate { remaining_ms } => {
            vec![SessionEvent::ChannelUpdate { remaining_ms }]
        }
        ServerPacket::UpdateAuraDuration { slot, remaining_ms } => {
            vec![SessionEvent::AuraDuration { slot, remaining_ms }]
        }
        ServerPacket::PlaySpellVisual { unit, kit_id } => {
            vec![SessionEvent::PlaySpellVisual { unit, kit_id }]
        }
        ServerPacket::SpellDamageLog(s) => vec![SessionEvent::SpellDamageLog(s)],
        ServerPacket::PeriodicAuraLog(s) => vec![SessionEvent::PeriodicAuraLog(s)],
        ServerPacket::SpellHealLog(s) => vec![SessionEvent::SpellHealLog(s)],
        ServerPacket::SpellEnergizeLog(s) => vec![SessionEvent::SpellEnergizeLog(s)],
        ServerPacket::DamageShield(s) => vec![SessionEvent::DamageShield(s)],
        ServerPacket::EnvironmentalDamageLog(s) => vec![SessionEvent::EnvironmentalDamageLog(s)],
        ServerPacket::SpellLogMiss(s) => vec![SessionEvent::SpellLogMiss(s)],
        ServerPacket::XpGain(s) => vec![SessionEvent::XpGain(s)],
        ServerPacket::ExplorationXp(s) => vec![SessionEvent::ExplorationXp(s)],
        ServerPacket::LevelUp(l) => vec![SessionEvent::LevelUp(l)],
        ServerPacket::GossipMessage {
            npc,
            text_id,
            options,
            quests,
        } => vec![SessionEvent::GossipMenu {
            npc,
            text_id,
            options,
            quests: quests
                .into_iter()
                .map(|q| (q.quest_id, q.icon, q.title))
                .collect(),
        }],
        ServerPacket::QuestGiverStatus { npc, status } => {
            vec![SessionEvent::QuestGiverStatus { npc, status }]
        }
        ServerPacket::QuestGiverQuestList(l) => vec![SessionEvent::QuestGreeting(l)],
        ServerPacket::QuestGiverDetails(d) => vec![SessionEvent::QuestDetail(d)],
        ServerPacket::QuestGiverRequestItems(p) => vec![SessionEvent::QuestProgress(p)],
        ServerPacket::QuestGiverOfferReward(o) => vec![SessionEvent::QuestOffer(o)],
        ServerPacket::QuestGiverComplete(c) => vec![SessionEvent::QuestComplete(c)],
        ServerPacket::QuestQueryResponse(t) => vec![SessionEvent::QuestTemplate(t)],
        ServerPacket::QuestUpdateAddKill {
            quest_id,
            entry,
            count,
            required,
            guid: _, // the killed unit's guid — no consumer (the toast keys on the entry)
        } => vec![SessionEvent::QuestObjectiveKill {
            quest_id,
            entry,
            count,
            required,
        }],
        ServerPacket::QuestUpdateAddItem { item_id, count } => {
            vec![SessionEvent::QuestObjectiveItem { item_id, count }]
        }
        ServerPacket::QuestUpdateComplete { quest_id } => {
            vec![SessionEvent::QuestObjectivesComplete { quest_id }]
        }
        ServerPacket::QuestUpdateFailed { quest_id } => vec![SessionEvent::QuestFailed {
            quest_id,
            timed: false,
        }],
        ServerPacket::QuestUpdateFailedTimer { quest_id } => vec![SessionEvent::QuestFailed {
            quest_id,
            timed: true,
        }],
        ServerPacket::QuestLogFull => vec![SessionEvent::QuestLogFull],
        ServerPacket::QuestGiverInvalid { msg } => {
            vec![SessionEvent::QuestGiverInvalid { reason: msg }]
        }
        ServerPacket::QuestGiverFailed { quest_id, reason } => {
            vec![SessionEvent::QuestGiverFailed { quest_id, reason }]
        }
        ServerPacket::GossipComplete => vec![SessionEvent::GossipComplete],
        ServerPacket::GossipPoi(poi) => vec![SessionEvent::GossipPoi(poi)],
        ServerPacket::NpcText { text_id, blocks } => {
            vec![SessionEvent::NpcGreeting { text_id, blocks }]
        }
        ServerPacket::VendorList { vendor, items } => {
            vec![SessionEvent::VendorInventory { vendor, items }]
        }
        ServerPacket::TrainerList {
            trainer,
            trainer_type,
            services,
            title,
        } => vec![SessionEvent::TrainerList {
            trainer,
            trainer_type,
            services,
            greeting: title,
        }],
        ServerPacket::TrainerBuySucceeded { trainer, spell_id } => {
            vec![SessionEvent::TrainerBuySucceeded { trainer, spell_id }]
        }
        ServerPacket::TrainerBuyFailed {
            trainer,
            spell_id,
            error,
        } => vec![SessionEvent::TrainerBuyFailed {
            trainer,
            spell_id,
            error,
        }],
        ServerPacket::BuyItem {
            vendor,
            slot,
            new_count,
            purchase_count,
        } => vec![SessionEvent::VendorBuyResult {
            vendor,
            slot,
            new_count,
            purchase_count,
        }],
        ServerPacket::SellItemResult {
            vendor,
            item_guid,
            reason,
        } => vec![SessionEvent::VendorSellFailed {
            vendor,
            item_guid,
            reason,
        }],
        ServerPacket::BuyFailed {
            vendor,
            item_entry,
            reason,
        } => vec![SessionEvent::VendorBuyFailed {
            vendor,
            item_entry,
            reason,
        }],
        ServerPacket::ShowBank { banker } => vec![SessionEvent::ShowBank { banker }],
        ServerPacket::BuyBankSlotResult { result } => {
            vec![SessionEvent::BuyBankSlotResult { result }]
        }
        ServerPacket::LootResponse {
            guid,
            loot_type,
            gold,
            items,
        } => vec![SessionEvent::LootResponse {
            guid,
            loot_type,
            gold,
            items,
        }],
        ServerPacket::LootError { guid, error } => vec![SessionEvent::LootError { guid, error }],
        ServerPacket::LootReleaseResponse { guid, .. } => {
            vec![SessionEvent::LootReleaseResponse { guid }]
        }
        ServerPacket::LootRemoved { slot } => vec![SessionEvent::LootRemoved { slot }],
        ServerPacket::LootMoneyNotify { amount } => {
            vec![SessionEvent::LootMoneyNotify { amount }]
        }
        ServerPacket::LootClearMoney => vec![SessionEvent::LootClearMoney],
        ServerPacket::LootStartRoll(p) => vec![SessionEvent::LootStartRoll(p)],
        ServerPacket::LootRoll(p) => vec![SessionEvent::LootRoll(p)],
        ServerPacket::LootRollWon(p) => vec![SessionEvent::LootRollWon(p)],
        ServerPacket::LootAllPassed(p) => vec![SessionEvent::LootAllPassed(p)],
        ServerPacket::ItemPushResult(p) => vec![SessionEvent::ItemPushResult(p)],
        ServerPacket::CorpseQuery(loc) => vec![SessionEvent::CorpseQuery {
            found: loc.found,
            display_map: loc.display_map,
            position: loc.position,
            corpse_map: loc.corpse_map,
        }],
        ServerPacket::CorpseReclaimDelay { delay_ms } => {
            vec![SessionEvent::CorpseReclaimDelay { delay_ms }]
        }
        ServerPacket::DurabilityDamageDeath => vec![SessionEvent::DurabilityDamageDeath],
        ServerPacket::ResurrectRequest(r) => vec![SessionEvent::ResurrectRequest {
            caster: r.caster,
            name: r.name,
            sickness: r.sickness,
            has_timer: r.has_timer,
        }],
        ServerPacket::SpiritHealerConfirm { npc } => {
            vec![SessionEvent::SpiritHealerConfirm { npc }]
        }
        ServerPacket::MoveMode {
            guid,
            counter,
            mode,
            apply,
        } => vec![SessionEvent::MoveMode {
            guid,
            counter,
            mode,
            apply,
        }],
        ServerPacket::GroupInvite { inviter } => vec![SessionEvent::GroupInvite { inviter }],
        ServerPacket::GroupDecline { name } => vec![SessionEvent::GroupDecline { name }],
        ServerPacket::GroupUninvited => vec![SessionEvent::GroupUninvited],
        ServerPacket::GroupLeaderChanged { name } => {
            vec![SessionEvent::GroupLeaderChanged { name }]
        }
        ServerPacket::GroupDestroyed => vec![SessionEvent::GroupDestroyed],
        ServerPacket::GroupList {
            group_type,
            own_flags,
            members,
            leader,
            loot,
        } => vec![SessionEvent::GroupList {
            group_type,
            own_flags,
            members,
            leader,
            loot,
        }],
        ServerPacket::PartyCommandResult {
            operation,
            member,
            result,
        } => vec![SessionEvent::PartyCommandResult {
            operation,
            member,
            result,
        }],
        ServerPacket::PartyMemberStats { guid, full, info } => {
            vec![SessionEvent::PartyMemberStats { guid, full, info }]
        }
        ServerPacket::MinimapPing { guid, x, y } => {
            vec![SessionEvent::MinimapPing { guid, x, y }]
        }
        ServerPacket::RaidTargetSet { icon, guid } => {
            vec![SessionEvent::RaidTargetSet { icon, guid }]
        }
        ServerPacket::RaidTargetList { entries } => {
            vec![SessionEvent::RaidTargetList { entries }]
        }
        ServerPacket::ReadyCheckRequest => vec![SessionEvent::ReadyCheckRequest],
        ServerPacket::ReadyCheckAnswer { guid, ready } => {
            vec![SessionEvent::ReadyCheckAnswer { guid, ready }]
        }
        ServerPacket::DuelRequested {
            arbiter,
            challenger,
        } => vec![SessionEvent::DuelRequested {
            arbiter,
            challenger,
        }],
        ServerPacket::DuelOutOfBounds => vec![SessionEvent::DuelOutOfBounds],
        ServerPacket::DuelInBounds => vec![SessionEvent::DuelInBounds],
        ServerPacket::DuelComplete { started } => vec![SessionEvent::DuelComplete { started }],
        ServerPacket::DuelWinner {
            fled,
            winner,
            loser,
        } => vec![SessionEvent::DuelWinner {
            fled,
            winner,
            loser,
        }],
        ServerPacket::DuelCountdown { seconds } => vec![SessionEvent::DuelCountdown { seconds }],
        ServerPacket::InspectHonorStats(stats) => vec![SessionEvent::InspectHonorStats(stats)],
        ServerPacket::PvpCredit(credit) => vec![SessionEvent::PvpCredit(credit)],
        ServerPacket::MirrorTimerStart(start) => vec![SessionEvent::MirrorTimerStart(start)],
        ServerPacket::MirrorTimerPause { kind, paused } => {
            vec![SessionEvent::MirrorTimerPause { kind, paused }]
        }
        ServerPacket::MirrorTimerStop { kind } => vec![SessionEvent::MirrorTimerStop { kind }],
        ServerPacket::FriendList { friends } => vec![SessionEvent::FriendList { friends }],
        ServerPacket::IgnoreList { guids } => vec![SessionEvent::IgnoreList { guids }],
        ServerPacket::FriendStatus(status) => vec![SessionEvent::FriendStatus(status)],
        ServerPacket::WhoResults(results) => vec![SessionEvent::WhoResults(results)],
        ServerPacket::GuildQueryResponse(response) => {
            vec![SessionEvent::GuildQueryResponse(response)]
        }
        ServerPacket::GuildRoster(roster) => vec![SessionEvent::GuildRoster(roster)],
        ServerPacket::GuildEvent(notice) => vec![SessionEvent::GuildEvent(notice)],
        ServerPacket::GuildCommandResult(result) => vec![SessionEvent::GuildCommandResult(result)],
        ServerPacket::GuildInvite { inviter, guild } => {
            vec![SessionEvent::GuildInvite { inviter, guild }]
        }
        ServerPacket::GuildDecline { name } => vec![SessionEvent::GuildDecline { name }],
        ServerPacket::GuildInfo(info) => vec![SessionEvent::GuildInfo(info)],
        ServerPacket::DestroyObject { guid } => vec![SessionEvent::ObjectDestroyed(guid)],
        ServerPacket::TriggerCinematic { cinematic_id } => {
            vec![SessionEvent::CinematicTriggered { cinematic_id }]
        }
        ServerPacket::MonsterMove {
            guid,
            start,
            spline_id,
            path,
            facing,
            stop,
            duration_ms,
            flying,
        } => vec![SessionEvent::MonsterMove {
            guid,
            start: v3(start),
            spline_id,
            path: path.into_iter().map(v3).collect(),
            facing,
            stop,
            duration_ms,
            flying,
        }],
        ServerPacket::PlayerMove {
            guid,
            opcode,
            flags,
            position,
            orientation,
            pitch,
            time,
            fall_time,
            jump,
            transport,
        } => vec![SessionEvent::UnitMove {
            guid,
            position: v3(position),
            orientation,
            flags,
            pitch,
            time,
            heartbeat: opcode == crate::messages::opcode::MSG_MOVE_HEARTBEAT,
            fall_time,
            jump,
            transport,
        }],
        ServerPacket::Teleport {
            guid,
            counter,
            position,
            orientation,
        } => vec![SessionEvent::Teleport {
            guid,
            counter,
            position: v3(position),
            orientation,
        }],
        ServerPacket::NewWorld {
            map,
            position,
            orientation,
        } => vec![SessionEvent::Worldport {
            map_id: map,
            position: v3(position),
            orientation,
            needs_ack: true,
        }],
        ServerPacket::LoginVerifyWorld {
            map,
            position,
            orientation,
        } => vec![SessionEvent::Worldport {
            map_id: map,
            position: v3(position),
            orientation,
            needs_ack: false,
        }],
        ServerPacket::TransferPending { map, transport } => vec![SessionEvent::TransferPending {
            map_id: map,
            transport_entry: transport.map(|(entry, _old_map)| entry),
        }],
        ServerPacket::TransferAborted { reason } => {
            vec![SessionEvent::TransferAborted { reason }]
        }
        ServerPacket::TimeSpeed {
            hours,
            minutes,
            day_serial,
            timescale,
        } => vec![SessionEvent::TimeSpeed {
            hours,
            minutes,
            day_serial,
            timescale,
        }],
        ServerPacket::QueryTimeResponse { unix_time } => {
            vec![SessionEvent::ServerUnixTime { unix_time }]
        }
        ServerPacket::BindPoint { area, .. } => vec![SessionEvent::BindPoint { area }],
        ServerPacket::BinderConfirm { binder } => vec![SessionEvent::BinderConfirm { binder }],
        ServerPacket::PlayerBound { binder, area } => {
            vec![SessionEvent::PlayerBound { binder, area }]
        }
        ServerPacket::SetProficiency {
            item_class,
            subclass_mask,
        } => vec![SessionEvent::Proficiency {
            item_class: u32::from(item_class),
            subclass_mask,
        }],
        ServerPacket::InitializeFactions { standings } => {
            vec![SessionEvent::Reputations { standings }]
        }
        ServerPacket::SetFactionStanding { standings } => {
            vec![SessionEvent::ReputationDelta { standings }]
        }
        ServerPacket::SetFactionVisible { list_id } => {
            vec![SessionEvent::ReputationVisible { list_id }]
        }
        ServerPacket::NameQueryResponse {
            guid,
            name,
            race,
            gender,
            class,
        } => vec![SessionEvent::PlayerName {
            guid,
            name,
            race,
            gender,
            class,
        }],
        ServerPacket::PetNameQueryResponse { pet_number, name } => {
            vec![SessionEvent::PetName { pet_number, name }]
        }
        ServerPacket::CreatureQueryResponse { entry, info } => {
            let (
                name,
                subname,
                creature_type,
                pet_family,
                rank,
                type_flags,
                civilian,
                racial_leader,
            ) = match info {
                Some(i) => (
                    Some(i.name),
                    // An EMPTY wire subname is NO subname (vmangos sends "" for creatures
                    // without one; the client renders no line for it — the nameplate
                    // builder's verified shape, 0x608f50). Mapped here, once, so no consumer
                    // ever sees a present-but-empty tooltip/nameplate line.
                    Some(i.subname).filter(|s| !s.is_empty()),
                    Some(i.creature_type),
                    // NOT wrapped in an `Option` like the type beside it: family `0` already
                    // means "this template has no family", which is exactly what a miss means
                    // too, so both answer the same `0` rather than two shapes for one state
                    // (`CreatureFamily.dbc` has no row 0 — decision 1062).
                    i.pet_family,
                    i.rank,
                    i.type_flags,
                    i.civilian,
                    i.racial_leader,
                ),
                None => (None, None, None, 0, 0, 0, false, false),
            };
            vec![SessionEvent::CreatureName {
                entry,
                name,
                subname,
                creature_type,
                pet_family,
                rank,
                type_flags,
                civilian,
                racial_leader,
            }]
        }
        ServerPacket::PageTextQueryResponse {
            page_id,
            text,
            next_page_id,
        } => vec![SessionEvent::PageText {
            page_id,
            text,
            next_page_id,
        }],
        ServerPacket::GameObjectQueryResponse { entry, info } => {
            let event = match info {
                Some(i) => SessionEvent::GameObjectInfo {
                    entry,
                    type_id: i.type_id,
                    display_id: i.display_id,
                    name: i.name,
                    data: i.data,
                },
                None => SessionEvent::GameObjectInfo {
                    entry,
                    type_id: 0,
                    display_id: 0,
                    name: String::new(),
                    data: [0; 24],
                },
            };
            vec![event]
        }
        ServerPacket::GameObjectCustomAnim { guid, anim_id } => {
            vec![SessionEvent::GameObjectCustomAnim { guid, anim_id }]
        }
        ServerPacket::GameObjectDespawnAnim { guid } => {
            vec![SessionEvent::GameObjectDespawnAnim { guid }]
        }
        ServerPacket::FishNotHooked => vec![SessionEvent::FishNotHooked],
        ServerPacket::FishEscaped => vec![SessionEvent::FishEscaped],
        // The keepalive echo: the io layer matches the sequence against its ping clock to compute
        // the round-trip time (the codec is stateless, so the timing lives with the sender).
        ServerPacket::Pong { sequence } => vec![SessionEvent::Pong { sequence }],
        ServerPacket::ForceSpeedChange {
            guid,
            kind,
            counter,
            speed,
        } => vec![SessionEvent::ForceSpeedChange {
            guid,
            kind,
            counter,
            speed,
        }],
        ServerPacket::SplineSpeedChange { guid, kind, speed } => {
            vec![SessionEvent::SpeedChanged { guid, kind, speed }]
        }
        // The MOVE_SET flavour is a speed change AND a fresh authoritative pose in one packet
        // (decision 0441) — surface both; they land in the same drain, so extrapolation resumes
        // from the new pose at the new speed either way.
        ServerPacket::MoveSetSpeed {
            guid,
            kind,
            flags,
            position,
            orientation,
            pitch,
            time,
            fall_time,
            jump,
            transport,
            speed,
        } => vec![
            SessionEvent::UnitMove {
                guid,
                position: v3(position),
                orientation,
                flags,
                pitch,
                time,
                heartbeat: false,
                fall_time,
                jump,
                transport,
            },
            SessionEvent::SpeedChanged { guid, kind, speed },
        ],
        ServerPacket::MountResult { mount, code } => {
            vec![SessionEvent::MountResult { mount, code }]
        }
        ServerPacket::MountSpecialAnim { guid } => {
            vec![SessionEvent::MountSpecial { guid }]
        }
        ServerPacket::ClientControlUpdate { mover, allow_move } => {
            vec![SessionEvent::ClientControl { mover, allow_move }]
        }
        ServerPacket::ShowTaxiNodes {
            window,
            flightmaster,
            nearest_node,
            known,
        } => {
            // The leading word is the client parser's own conditional gate (0496 §claim-1): a
            // zero-gated body carries no menu — no event. vmangos always sends 1.
            if window == 0 {
                vec![]
            } else {
                vec![SessionEvent::TaxiNodesShown {
                    flightmaster,
                    nearest_node,
                    known_mask: known,
                }]
            }
        }
        ServerPacket::TaxiNodeStatus { guid, known } => {
            vec![SessionEvent::TaxiNodeStatus { guid, known }]
        }
        ServerPacket::ActivateTaxiReply { code } => {
            vec![SessionEvent::ActivateTaxiReply { code }]
        }
        ServerPacket::NewTaxiPath => vec![SessionEvent::NewTaxiPath],
        ServerPacket::MailList { mails } => vec![SessionEvent::MailList { mails }],
        ServerPacket::SendMailResult {
            mail_id,
            action,
            error,
            equip_error,
            item,
        } => vec![SessionEvent::SendMailResult {
            mail_id,
            action,
            error,
            equip_error,
            item,
        }],
        ServerPacket::ItemTextQueryResponse { text_id, text } => {
            vec![SessionEvent::MailItemText { text_id, text }]
        }
        ServerPacket::ReceivedMail { seconds } => vec![SessionEvent::ReceivedMail { seconds }],
        ServerPacket::NextMailTime { seconds } => {
            vec![SessionEvent::NextMailTime { seconds }]
        }
        ServerPacket::AuctionHello {
            auctioneer,
            house_id,
        } => vec![SessionEvent::AuctionHello {
            auctioneer,
            house_id,
        }],
        ServerPacket::AuctionCommandResult {
            auction_id,
            action,
            error,
            tail,
        } => vec![SessionEvent::AuctionCommandResult {
            auction_id,
            action,
            error,
            tail,
        }],
        ServerPacket::AuctionListResult {
            auctions,
            total_count,
        } => vec![SessionEvent::AuctionListResult {
            auctions,
            total_count,
        }],
        ServerPacket::AuctionOwnerListResult {
            auctions,
            total_count,
        } => vec![SessionEvent::AuctionOwnerListResult {
            auctions,
            total_count,
        }],
        ServerPacket::AuctionBidderListResult {
            auctions,
            total_count,
        } => vec![SessionEvent::AuctionBidderListResult {
            auctions,
            total_count,
        }],
        ServerPacket::AuctionBidderNotification(n) => {
            vec![SessionEvent::AuctionBidderNotification(n)]
        }
        ServerPacket::AuctionOwnerNotification(n) => {
            vec![SessionEvent::AuctionOwnerNotification(n)]
        }
        ServerPacket::AuctionRemovedNotification {
            auction_id,
            item_entry,
            random_property_id,
        } => vec![SessionEvent::AuctionRemovedNotification {
            auction_id,
            item_entry,
            random_property_id,
        }],
        ServerPacket::TradeStatus { status } => vec![SessionEvent::TradeStatus { status }],
        ServerPacket::TradeStatusExtended { state } => {
            vec![SessionEvent::TradeStatusExtended { state }]
        }
        ServerPacket::InitWorldStates(w) => vec![SessionEvent::WorldStates {
            scope: Some((w.map, w.zone)),
            states: w.states,
        }],
        ServerPacket::UpdateWorldState { id, value } => vec![SessionEvent::WorldStates {
            scope: None,
            states: vec![(id, value)],
        }],
        // An opcode with NO parse arm at all — surface it so the app can tally the coverage gap
        // (the debug panel's dropped-opcode instrument); the silent fall-through hid whole wire
        // families. Parsed-but-unmodelled packets (the `_` below) stay deliberate no-ops.
        ServerPacket::Other { opcode } => vec![SessionEvent::PacketDropped {
            opcode,
            unparseable: false,
        }],
        _ => Vec::new(),
    }
}

/// Fan one object-update packet's object list out into create/move/remove/values events. Consumes the
/// list so each object's descriptor `mask` moves into its event as the ECS store seed/delta (decision
/// 0061) — no clone.
fn decode_objects(objects: Vec<Object>) -> Vec<SessionEvent> {
    let mut out = Vec::new();
    for object in objects {
        match object {
            Object::Create {
                guid,
                object_type,
                movement,
                mask,
            } => {
                // Items/containers are descriptor-only creates — no pose to interpret, no scene
                // presence. Route them whole into the item event before the placement gate below
                // (which would otherwise silently drop a position-less create, fields and all).
                if matches!(object_type, ObjectType::Item | ObjectType::Container) {
                    out.push(SessionEvent::ItemCreate {
                        guid,
                        container: object_type == ObjectType::Container,
                        fields: mask,
                    });
                    continue;
                }
                // Interpret the spawn identity + placement from the mask (borrow), then move the whole
                // mask into the create event as the object's initial descriptor store.
                if let Some((position, orientation)) =
                    create_placement(object_type, &mask, &movement)
                {
                    let display = display_id(object_type, &mask);
                    let scale = object_scale(object_type, &mask);
                    let speeds = movement.speeds.map(MoveSpeeds::from_wire);
                    out.push(SessionEvent::ObjectCreate {
                        guid,
                        kind: entity_kind(object_type),
                        display_id: display,
                        position: v3(position),
                        orientation,
                        scale,
                        speeds,
                        transport_progress: movement.transport_progress,
                        transport: movement.transport,
                        spline: movement.spline,
                        fields: mask,
                    });
                }
            }
            Object::Movement { guid, movement } => {
                if let Some((position, orientation)) = movement.position {
                    out.push(SessionEvent::ObjectMove {
                        guid,
                        position: v3(position),
                        orientation,
                    });
                }
            }
            Object::OutOfRange { guids } => {
                out.push(SessionEvent::ObjectsRemoved(guids));
            }
            // `Values` carries UpdateFields-only changes (no position) — descriptor deltas (health on a
            // hit, an appearance byte, a door's state). Emit the whole non-empty delta for the ECS to
            // merge into the object's store; interpretation is the consumer's, per named accessor.
            Object::Values { guid, mask } => {
                if !mask.is_empty() {
                    out.push(SessionEvent::ObjectValues { guid, fields: mask });
                }
            }
            // `Near` is a guid pre-announce; no state.
            Object::Near { .. } => {}
        }
    }
    out
}

/// Coarse classification of a decoded object from its `TypeId`.
fn entity_kind(t: ObjectType) -> EntityKind {
    match t {
        ObjectType::Player => EntityKind::Player,
        ObjectType::Unit => EntityKind::Unit,
        ObjectType::GameObject => EntityKind::GameObject,
        ObjectType::DynamicObject => EntityKind::DynamicObject,
        _ => EntityKind::Other,
    }
}

/// The display id from a create packet — units, **players**, and GameObjects all carry one. A player's
/// body model resolves through the same CreatureDisplayInfo→CreatureModelData chain as a creature's:
/// the 1.12 client has no race/sex→path resolver, CGPlayer inherits CGUnit's displayId model-build
/// (decision 0041), so we route `Player` alongside `Unit`. Cast from the wire `i32`; `0`/absent →
/// `None`.
fn display_id(t: ObjectType, mask: &ObjectFields) -> Option<u32> {
    let raw = match t {
        ObjectType::Unit | ObjectType::Player => mask.unit_displayid(),
        ObjectType::GameObject => mask.gameobject_displayid(),
        _ => None,
    }?;
    (raw > 0).then_some(raw as u32)
}

/// The per-object scale (`OBJECT_FIELD_SCALE_X`) units/players/GameObjects carry — the *complete*
/// render scale that multiplies the raw model's native size. The real client sizes any object by this
/// field alone (verified `world_model_scale` `0x613ef0`); for a unit the server has already folded the
/// CreatureDisplayInfo/CreatureModelData scale into it (vmangos `Unit::GetScaleForDisplayId`), so it is
/// *not* multiplied again client-side. Defaults to `1.0` when absent or non-positive (the server always
/// sends a positive value on create).
fn object_scale(t: ObjectType, mask: &ObjectFields) -> f32 {
    let raw = match t {
        ObjectType::Unit | ObjectType::Player | ObjectType::GameObject => mask.object_scale_x(),
        _ => None,
    };
    raw.filter(|s| *s > 0.0).unwrap_or(1.0)
}

/// Where a freshly-created object sits: GameObjects from UpdateFields (`GAMEOBJECT_POS_*`) — falling
/// back to the movement block's `HAS_POSITION` pose when those fields were never sent at all — everything
/// else straight from the movement block.
///
/// The fallback exists for transports: vmangos never sets `GAMEOBJECT_POS_*` on a boat/zeppelin/elevator
/// (it sends the stationary spawn point in the movement block's `HAS_POSITION` slot instead — decision
/// 0438 "the wire"). Gated on [`ObjectFields::gameobject_pos_sent`] rather than
/// `gameobject_position().or(..)`: a create block's absent-is-zero fold
/// (`ObjectFields`'s "created" semantics) means `gameobject_position()` reads `Some((0,0,0), 0)` even
/// when nothing was sent, so a plain `.or` never falls through. An ordinary GameObject sends at least
/// one non-zero `GAMEOBJECT_POS_*` axis virtually always, so this never changes its placement.
fn create_placement(
    object_type: ObjectType,
    mask: &ObjectFields,
    movement: &MovementBlock,
) -> Option<(Vector3d, f32)> {
    if object_type == ObjectType::GameObject && mask.gameobject_pos_sent() {
        mask.gameobject_position()
    } else {
        movement.position
    }
}
