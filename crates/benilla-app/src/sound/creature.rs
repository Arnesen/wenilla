//! Creature voice — CreatureSoundData-driven vocals (decision 0070 slice 3): the death cry on a
//! live health→0 transition, and the anim-tag vocals (`$FD1..4` fidgets, `$FDX` stand,
//! `$WNG`/`$WGG` wing flap/glide).
//!
//! The swing-driven vocals (exertion/injury) and the `$AH0..3` custom-attack columns live in
//! [`super::combat`] (decisions 0075 + 0525 — the custom attacks are the swing dispatch's
//! natural-weapon impact, not free-standing anim vocals); the
//! aggro/alert flares land here ([`ai_reaction_vocals`], decision 0280), and so does the ambient
//! body loop ([`creature_body_loops`] — the `loop_sound` column, `0x623800`'s alive-gate). Still
//! untriggered — data in the catalog, triggers INFERRED (0280's §5): stun/jump_start/jump_end
//! (offsets verified, live triggers likely M2 tags — unpinned).

use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;

use benilla_formats::CreatureVoiceCatalog;
use benilla_protocol::EntityKind;

use crate::creature_anim::AnimSoundEvent;
use crate::entities::mount::{MountBody, MountChild};
use crate::net::{NetEntity, ObjectStore};
use benilla_assets::{AssetSet, LockRecover, WorldAssets};
use benilla_world::schedule::WorldStage;

use super::kit::{
    play_kit, play_kit_ext, source_kit_playing, stop_source_kit, KitRef, SoundCategory, SoundKits,
};
use super::{AudioListener, SoundConfig, SoundOutput};

/// The display→voice catalog (CreatureDisplayInfo.SoundID → CreatureSoundData).
#[derive(Resource)]
pub(super) struct CreatureVoices(pub(super) CreatureVoiceCatalog);

fn load_creature_voices(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_creature_voice_catalog(&mut chain)
    };
    match loaded {
        Ok(cat) => {
            info!("sound: {} creature voice rows", cat.len());
            commands.insert_resource(CreatureVoices(cat));
        }
        Err(e) => warn!("sound: creature voices failed to load: {e:#}"),
    }
}

/// Play the death vocal on a **live** death: a unit whose store transitions alive→dead. First
/// sight already-dead records silently (a streamed corpse doesn't cry — the same distinction the
/// animation driver makes for the settled-corpse pose).
#[allow(clippy::too_many_arguments)]
fn death_vocals(
    changed: Query<(Entity, &NetEntity, &ObjectStore, &Transform), Changed<ObjectStore>>,
    mut known_dead: Local<EntityHashMap<bool>>,
    voices: Option<Res<CreatureVoices>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    let (Some(voices), Some(mut kits), Some(assets)) = (voices, kits, assets) else {
        return;
    };
    let listener = listener.pos;
    for (entity, net, store, transform) in &changed {
        if !matches!(net.kind, EntityKind::Unit | EntityKind::Player) {
            continue;
        }
        // Reads-dead, not really-dead (decision 1022): the reference's `UNIT_DYNAMIC_FLAGS` watcher
        // fires `0x623a40(4)` — the death vocal state — on the `UNIT_DYNFLAG_DEAD` set edge
        // (`0x600543`), the very same call its real death handler makes (`0x6251b0`). So a feign
        // drops the body with its death cry, exactly like a kill.
        let dead = store.0.unit_reads_dead();
        let was = known_dead.insert(entity, dead);
        let fresh_death = was == Some(false) && dead;
        if !fresh_death {
            continue;
        }
        let kit = net
            .display_id
            .and_then(|d| voices.0.for_display(d))
            .map(|v| v.death)
            .unwrap_or(0);
        if kit == 0 {
            continue;
        }
        if let Err(e) = play_kit(
            &mut kits,
            &assets,
            &mut out,
            &config,
            listener,
            KitRef::Id(kit),
            Some(transform.translation),
            SoundCategory::Sfx,
        ) {
            warn!("death vocal (kit {kit}): {e:#}");
        }
    }
}

/// The aggro/alert flare vocals (`SMSG_AI_REACTION` → the net bridge; decision 0280): HOSTILE →
/// the aggro bark (CreatureSoundData col 10), ALERT → the alert bark (col 13) — pure audio, like
/// the client (`0x6056e0` plays no animation/UI on either leg). INTERIM, named: the client's
/// per-unit priority voice channel (a HOSTILE bark never interrupts a playing bark) and ALERT's
/// RNG sometimes-skip gate (`0x623520`) aren't modeled — vocals here play unconditionally, like
/// the exertion/injury family.
#[allow(clippy::too_many_arguments)]
fn ai_reaction_vocals(
    mut reactions: MessageReader<crate::net::AiReactionMessage>,
    units: Query<(&NetEntity, &Transform, Option<&MountChild>)>,
    mounts: Query<&NetEntity, With<MountBody>>,
    voices: Option<Res<CreatureVoices>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    if reactions.is_empty() {
        return;
    }
    let (Some(voices), Some(mut kits), Some(assets)) = (voices, kits, assets) else {
        return;
    };
    let listener = listener.pos;
    for r in reactions.read() {
        let Ok((net, transform, mount_child)) = units.get(r.unit) else {
            continue;
        };
        // The mounted redirect (byte-verified `0x60c480` — `+0xb44 ?: +0xb40`, wow-re
        // `mount-composition.md` Q4; 0441 fold-back): ALERT reads through the mount-preferred
        // getter — a mounted unit alerts with its MOUNT's voice — while HOSTILE (like DEATH)
        // reads the base row directly and stays the rider's own bark.
        let display = if r.hostile {
            net.display_id
        } else {
            mount_child
                .and_then(|mc| mounts.get(mc.0).ok())
                .and_then(|m| m.display_id)
                .or(net.display_id)
        };
        let kit = display
            .and_then(|d| voices.0.for_display(d))
            .map(|v| if r.hostile { v.aggro } else { v.alert })
            .unwrap_or(0);
        if kit == 0 {
            continue;
        }
        if let Err(e) = play_kit(
            &mut kits,
            &assets,
            &mut out,
            &config,
            listener,
            KitRef::Id(kit),
            Some(transform.translation),
            SoundCategory::Sfx,
        ) {
            warn!("aggro vocal (kit {kit}): {e:#}");
        }
    }
}

/// The ambient body loop (`loop_sound`, CreatureSoundData col 23): a creature whose body
/// inherently sounds — an elemental's rumble, a slime's gurgle, a shredder's engine — hums
/// continuously while alive. The client's `0x623800` gate, byte-verified (wow-re
/// `smsg-ai-reaction.md` §5): health > 0, `UNIT_DYNAMIC_FLAGS` DEAD (`0x20`, the feign-death
/// visual) clear, the column nonzero, and a "not already playing" latch — here the tracked
/// channel's own liveness, reconciled every frame. That reconcile shape covers the client's
/// field-delta watchers (death stops it, resurrection restarts it) *and* doubles as the restart
/// trigger the kit player's out-of-range cull documents (walk away → the loop stops; walk back →
/// it re-arms). Despawn teardown is the mixer's source-channel reaper. INTERIM, named: the loop
/// is **forced** regardless of the kit's own 0x200 flag — every col-23 kit in 5875 is authored
/// `*Loop*` yet half omit the flag, so respecting it would silence half the class (`0x461d80`'s
/// flag handling is unpinned; the record covers the reading).
#[allow(clippy::too_many_arguments)]
fn creature_body_loops(
    units: Query<(
        Entity,
        &NetEntity,
        &ObjectStore,
        &Transform,
        Option<&MountChild>,
    )>,
    mounts: Query<&NetEntity, With<MountBody>>,
    // The loop kit this system last armed per unit — a mount transition swaps the resolved
    // kit, and the superseded loop must stop (the liveness latch is per (source, kit)).
    mut armed: Local<EntityHashMap<u32>>,
    voices: Option<Res<CreatureVoices>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    let (Some(voices), Some(mut kits), Some(assets)) = (voices, kits, assets) else {
        return;
    };
    let listener = listener.pos;
    for (entity, net, store, transform, mount_child) in &units {
        if !matches!(net.kind, EntityKind::Unit | EntityKind::Player) {
            continue;
        }
        // The mounted redirect (byte-verified `0x60c480` — `+0xb44 ?: +0xb40`, wow-re
        // `mount-composition.md` Q4; 0441 fold-back): the loop column reads the mount-preferred
        // row — a mounted mechanostrider's engine hums; dismounting swaps back to the rider's.
        let display = mount_child
            .and_then(|mc| mounts.get(mc.0).ok())
            .and_then(|m| m.display_id)
            .or(net.display_id);
        let kit = display
            .and_then(|d| voices.0.for_display(d))
            .map(|v| v.loop_sound)
            .unwrap_or(0);
        // `0x623800`'s own gate verbatim (`0x623817` health, `0x62381e` the flag): RAW health ≤ 0
        // — absent = 0, deliberately not `unit_is_dead`'s max-health guard, which would leave a
        // unit whose snapshot has not landed humming — or `UNIT_DYNFLAG_DEAD` set, the feign-death
        // bit the reference re-evaluates this very gate on (`0x60053c`, decision 1022).
        let alive = store.0.unit_health().unwrap_or(0) > 0 && !store.0.unit_reads_dead();
        let desired = if alive { kit } else { 0 };
        // Stop a superseded loop first: death, or a mount transition that changed the row.
        if let Some(&prev) = armed.get(&entity) {
            if prev != desired && source_kit_playing(&out, entity, prev) {
                stop_source_kit(&mut out, entity, prev);
            }
        }
        if desired == 0 {
            armed.remove(&entity);
            continue;
        }
        armed.insert(entity, desired);
        if !source_kit_playing(&out, entity, desired) {
            // Out of range this returns without allocating a channel — the next frame retries,
            // which is exactly the re-arm-on-audible the kit player asks its triggers for.
            if let Err(e) = play_kit_ext(
                &mut kits,
                &assets,
                &mut out,
                &config,
                listener,
                KitRef::Id(desired),
                Some(transform.translation),
                SoundCategory::Sfx,
                None,
                Some(entity),
                true,
            ) {
                warn!("body loop (kit {desired}): {e:#}");
            }
        }
    }
}

/// Route the CreatureSoundData anim tags: fidgets, stand, wing flap/glide. (The `$AH0–3`
/// custom-attack columns are NOT free-standing anim vocals — they are the swing dispatch's
/// natural-weapon impact sound, record-gated and latch-exclusive with the generic weapon
/// impact; [`super::combat`] plays them off [`crate::creature_anim::SwingImpact`], decision
/// 0525.)
/// A mounted unit's fidgets are the MOUNT model's own tags, fired by the mount CHILD entity
/// carrying the mount's display — so they already resolve the mount's voice row, the split-
/// entity shape of the client's mounted redirect (`0x60c480`: `+0xb44 ?: +0xb40` — wow-re
/// `mount-composition.md` Q4, 0441 fold-back).
#[allow(clippy::too_many_arguments)]
fn creature_anim_vocals(
    mut events: MessageReader<AnimSoundEvent>,
    // GlobalTransform: the mount child's local Transform is the seat-relative ~origin — world
    // position is the only correct read for both parented and top-level sources.
    units: Query<(&NetEntity, &GlobalTransform)>,
    voices: Option<Res<CreatureVoices>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    if events.is_empty() {
        return;
    }
    let (Some(voices), Some(mut kits), Some(assets)) = (voices, kits, assets) else {
        return;
    };
    let listener = listener.pos;
    for ev in events.read() {
        let Ok((net, transform)) = units.get(ev.entity) else {
            continue;
        };
        let Some(voice) = net.display_id.and_then(|d| voices.0.for_display(d)) else {
            continue;
        };
        let kit = match &ev.ident {
            b"$FD1" => voice.fidget[0],
            b"$FD2" => voice.fidget[1],
            b"$FD3" => voice.fidget[2],
            b"$FD4" => voice.fidget[3],
            b"$FDX" => voice.stand,
            b"$WNG" => voice.wing_flap,
            b"$WGG" => voice.wing_glide,
            _ => 0,
        };
        if kit == 0 {
            continue;
        }
        if let Err(e) = play_kit(
            &mut kits,
            &assets,
            &mut out,
            &config,
            listener,
            KitRef::Id(kit),
            Some(transform.translation()),
            SoundCategory::Sfx,
        ) {
            warn!("creature vocal (kit {kit}): {e:#}");
        }
    }
}

/// Registration hook for [`super::SoundPlugin`].
/// The **fall-landing wound vocal** — the client-side hard-landing predictor's sound leg
/// (`0x602d00 → call [vtable+0x88] class 2` at `0x602d84`, byte-verified wow-re
/// `object-layer/scratch/smsg-environmentaldamage.md`; decision 0412): a landing past the HARD
/// threshold plays the unit's ordinary CreatureSoundData **wound vocal, normal row** (class 2 →
/// column `+0xc` = `injury[0]` — the same row a landed melee hit voices; the crit/crushing rows
/// are unused here). NOT wire-driven: the server's `SMSG_ENVIRONMENTALDAMAGELOG` set plays no
/// sound at all — the grunt is predicted at the landing frame off the client's own fall height.
/// The dust leg of the same predictor lives in `creature_anim::env_damage`; both gate on the
/// shared [`crate::creature_anim::HARD_LANDING_DESCENT`].
#[allow(clippy::too_many_arguments)]
fn fall_landing_vocals(
    mut landings: MessageReader<crate::creature_anim::HardLanding>,
    units: Query<(&NetEntity, &Transform)>,
    voices: Option<Res<CreatureVoices>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    if landings.is_empty() {
        return;
    }
    let (Some(voices), Some(mut kits), Some(assets)) = (voices, kits, assets) else {
        return;
    };
    let listener = listener.pos;
    for l in landings.read() {
        if l.descent <= crate::creature_anim::HARD_LANDING_DESCENT {
            continue;
        }
        let Ok((net, transform)) = units.get(l.entity) else {
            continue;
        };
        let kit = net
            .display_id
            .and_then(|d| voices.0.for_display(d))
            .map(|v| v.injury[0])
            .unwrap_or(0);
        if kit == 0 {
            continue;
        }
        if let Err(e) = play_kit(
            &mut kits,
            &assets,
            &mut out,
            &config,
            listener,
            KitRef::Id(kit),
            Some(transform.translation),
            SoundCategory::Sfx,
        ) {
            warn!("fall-landing vocal (kit {kit}): {e:#}");
        }
    }
}

pub(super) fn plugin(app: &mut App) {
    app.add_systems(Startup, load_creature_voices.after(AssetSet::Open))
        .add_systems(
            Update,
            (
                death_vocals,
                creature_anim_vocals,
                ai_reaction_vocals,
                fall_landing_vocals,
                creature_body_loops,
            )
                .in_set(WorldStage::Present),
        );
}
