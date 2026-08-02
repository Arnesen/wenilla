//! Spell **missiles** (decision 0099 phase 4): the travelling projectile a Speed>0 cast launches
//! at each target — Fireball's fireball, an NPC frostbolt — and the impact hand-off when it
//! arrives.
//!
//! The router (`crate::creature_anim::spell_visual::route_cast_visuals`) resolves the GO's DBC
//! chain and emits one [`MissileSpawn`]; this module owns launch and flight.
//!
//! **Launch is release-keyed, not GO-keyed** (the client's `unit+0xac` pending queue): a GO whose
//! cast kit plays a body animation parks its projectiles on the caster ([`PendingMissiles`]) and
//! launches them when that animation's **release event keyframe** fires — `$CSL`/`$CSR`/`$CST`
//! (casting hand left/right/two-hand) or `$BWR` (ranged release), the anim-event dispatcher's
//! drain arms (`0x5ffbd0` → `0x60c940`) — from the fired marker's live bone-ridden position: the
//! fireball leaves the raised hand at the top of the throw, not the hold pose at GO. A GO with no
//! cast-kit animation launches immediately (`0x6e7a70`'s flush), and a queued cast whose keyframe
//! never fires launches when the one-shot ends (the client's on-anim-finish `0x5fc920` →
//! `0x60c9b0`), both through the same `$CSL` → `$CSR` → `$CST` → unit-base cascade (`0x60c9b0`).
//!
//! The **arrival deadline is fixed at GO** (the client's `0x61ceb0`: remaining travel time =
//! `distance / Spell.dbc Speed` **minus the time spent queued** — riding the hand eats flight
//! time, the same clock the server delays the damage by). A release already past its deadline
//! arrives on the spot — at melee range there is no visible flight or trail at all, just the
//! impact on the target, exactly the reference's close-range look.
//!
//! In flight, the client's **arrive-on-time** mover (`0x61ceb0`/`0x61e2a0`, wow-re `w2f1.md`):
//! one entity per target, model parts + particle emitters from the shared [`SpellFx`] path-keyed
//! cache (the same held-items pattern as the attach-point effects); each frame the missile covers
//! `remaining distance / remaining time · dt` toward the target's **destination attach point** —
//! so a moving target bends the path (homing) and arrival lands exactly on schedule. Arrival on a
//! landed target writes [`CastEventKind::Impact`] back to the router (the client's unit-impact
//! hand-off `0x61dc50`); arrival on a missed target plays the victim's dodge/block defense clip
//! instead ([`miss_defense_state`] — the `0x61ceb0` dispatch, never Parry).
//!
//! A missile with no visual-chain model flies the **wire ammo model** instead (decision 0099
//! phase 5 — every basic shot spell: Auto Shot, the Shoot family, Throw, all `SpellVisual = 0`):
//! the GO's ammo block names an `ItemDisplayInfo` row, resolved by its **shape** — flight model
//! in the right slot → `Item\ObjectComponents\Ammo\` (arrows/bullets), left slot → `…\Weapon\`
//! (thrown: the weapon itself flies) — each with its own object skin, exactly the held-items
//! texture mechanism. The client resolves the same row through `0x479f40`, forking on the wire's
//! ammo InventoryType (`0x19` = thrown); the shape rule reproduces it on every real row
//! (`benilla-formats` `items.rs` tests). No model at all (no chain, no ammo) → the missile flies
//! invisible and still impacts on schedule.
//!
//! Approximations, named: `$BWR` launches from its own event marker rather than the ranged
//! weapon model's HandArrow/Bullet attachment (`0x23`/`0x24` — the client's first choices before
//! it, too, falls back to the event position); the anim-end flush is a **polled** edge (plus a
//! [`RELEASE_WAIT_MAX`] never-played timeout) where the client gets a per-anim finish callback;
//! homing aims at the dest attach point rather than solving the client's
//! ray-vs-bounding-sphere intercept (`0x61d230` — same body point in practice); a missed
//! target's arrival plays its dodge/block clip but the projectile itself still ends there
//! rather than deflecting (`0x61dd50`'s bounce); and the ground/dest-location arrival path
//! (`0x61d870` — needs the wire's dest position and positional effect spawning) stays deferred.

use bevy::animation::transition::AnimationTransitions;
use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;

use crate::creature_anim::{
    oneshot_is_live, AnimData, AnimDriver, AnimSoundEvent, CastEvent, CastEventKind, DefenseAnim,
    MissileSpawn,
};
use crate::model_render::m2_url;

use super::equipment::ItemDisplays;
use super::spell_fx::{attach_effect_visuals, EffectHost, SpellFx};
use super::{BoneAttach, DisplayModel, ModelHandle};

/// The anim-event idents that release queued missiles — the dispatcher's drain arms (`0x5ffbd0`:
/// `$CSL`/`$CSR`/`$CST` casting hand, `$BWR` ranged release; all four call the queue drain
/// `0x60c940`).
const RELEASE_IDENTS: [[u8; 4]; 4] = [*b"$CSL", *b"$CSR", *b"$CST", *b"$BWR"];

/// The flush cascade when no release event names a marker (immediate launch at GO, the anim-end
/// flush): the client's `0x60c9b0` — `$CSL`, then `$CSR`, then `$CST`, else the unit's base.
const MARKER_CASCADE: [[u8; 4]; 3] = [*b"$CSL", *b"$CSR", *b"$CST"];

/// How long a queued missile waits for a one-shot that never starts before flushing anyway
/// (seconds). The release animation arms within a frame or two of the GO, after which the
/// anim-end edge owns the flush and this window no longer applies; a caster that never shows one
/// (the kit anim didn't resolve on this model) would otherwise hang its projectiles forever —
/// the client has no such window because its anim-finish callback always fires. Comfortably
/// above any arm latency (a frame or two, even hitchy).
const RELEASE_WAIT_MAX: f32 = 0.25;

/// The dest-attach fallback tail (after the spawn's own tag): the client's cascade again.
const DEST_FALLBACKS: [u16; 2] = [0xf, 0x13];

/// `AnimationData.dbc` **InFlight** — the sequence a projectile MODEL plays while it travels. A
/// thrown weapon (`Item\ObjectComponents\Weapon\Thrown_1H_*.m2`) authors it as the end-over-end
/// tumble (e.g. the dagger's 19-key bone rotation) and keys its trail ribbon's per-sequence
/// visibility ON only here — Stand (worn in hand) and Impact (landed) keep it off. A projectile
/// model without an InFlight sequence (arrows/bullets fly straight; the fireball tumbles via a
/// global sequence) is unaffected — the rig falls back to its file-order-first clip.
const INFLIGHT_ANIM: u16 = 144;

/// The missile's orientation for flight direction `f` (Bevy space) — the client's exact frame
/// build (`0x61e2a0`, wow-re `w2f1.md`): model **+X = the flight direction**, side = up × dir,
/// up re-orthogonalized as dir × side — i.e. NO roll, model-up stays world-up-ish however the
/// flight pitches. (A shortest-arc rotation rolls on pitched flight, which would tilt the trail
/// ribbons' authored + cross.) In Bevy terms: local −Z (the wow_to_bevy image of wow +X) → `f`,
/// local +Y (image of wow +Z) → the re-orthogonalized up.
fn missile_facing(f: Vec3) -> Quat {
    let side = Vec3::Y.cross(f);
    if side.length_squared() < 1e-6 {
        // Straight up/down — the degenerate vertical case; any roll is fine here.
        return Quat::from_rotation_arc(-Vec3::Z, f);
    }
    let up = f.cross(side.normalize()).normalize();
    // Columns are the images of local X/Y/Z: X → f × up (= −side, the handedness-consistent
    // image of wow +Y), Y → up, Z → −f.
    Quat::from_mat3(&Mat3::from_cols(f.cross(up), up, -f))
}

/// The projectile's in-flight LOOP sound (`SpellVisual` field 10, wow-re `w2f1.md`'s
/// `CMissile+0x44` loop handle): a channel tracked to the missile entity — it follows the
/// projectile as it flies and is reaped when the projectile arrives or streams out. Written by
/// [`spawn_missiles`]/[`move_missiles`]; consumed by `crate::sound::missile`.
#[derive(Message, Clone, Copy)]
pub(crate) enum MissileSound {
    /// Begin `kit_sound` as a loop tracked to `entity` (the launched projectile). `pos` is the
    /// launch point — the channel is born positional there (the just-spawned entity's `Transform`
    /// may not have flushed yet), then rides the entity as it flies.
    Start {
        entity: Entity,
        kit_sound: u32,
        pos: Vec3,
    },
    /// The projectile is gone (arrived / streamed out) — stop its flight loop.
    Stop { entity: Entity },
}

/// One projectile in flight.
#[derive(Component)]
pub(super) struct Missile {
    /// The spell that fired it — the impact event's key.
    spell_id: u32,
    /// The unit it flies at (despawn quietly if it streams out mid-flight).
    target: Entity,
    /// The M2 attach tag it homes to on the target (`SpellVisual` field 9), `None` = base.
    dest_tag: Option<u16>,
    /// `None` — the spell landed: arrival plays the impact hand-off. `Some(code)` — the wire's
    /// `SpellMissInfo` miss: arrival instead plays the victim's defense clip for DODGE(3)/
    /// BLOCK(5) (the client's `Missile_C::Update 0x61ceb0` dispatch — Dodge 30 / ShieldBlock 24,
    /// never Parry; wow-re `smsg-attackerstate-consequences.md` §Q4) and no impact kit.
    miss: Option<u8>,
    /// The model-cache key ([`SpellFx`]); parts attach once the M2 loads. `None` = nothing to
    /// show (no visual-chain model, no resolvable ammo) — an invisible flight that still impacts.
    path: Option<String>,
    /// Remaining flight time, seconds — the client's arrive-on-time integrator state.
    arrive_in: f32,
    /// Model parts/emitters spawned (they wait on the async M2 load, like every display model).
    parts_spawned: bool,
    /// The caster's ranged-weapon fallback `SpellVisual` id (resolved at GO time) — handed back
    /// in the arrival's [`CastEventKind::Impact`] so a basic shot's impact kit resolves through
    /// it (the caster may despawn mid-flight; decision 0370).
    weapon_visual: Option<u32>,
}

/// A unit's attach-point world position through the given tag cascade, else its base translation.
/// `None` only when the unit itself is gone.
fn attach_world_pos(
    unit: Entity,
    tags: impl IntoIterator<Item = u16>,
    units: &Query<(&GlobalTransform, Option<&BoneAttach>)>,
    joints: &Query<&GlobalTransform>,
) -> Option<Vec3> {
    let (base, bones) = units.get(unit).ok()?;
    let point = bones.and_then(|b| {
        tags.into_iter()
            .find_map(|tag| b.points.get(&tag).copied())
            .and_then(|(bone, offset)| b.anchor(bone).map(|j| (j, offset)))
    });
    Some(
        match point.and_then(|(j, off)| joints.get(j).ok().map(|g| g.transform_point(off))) {
            Some(p) => p,
            None => base.translation(),
        },
    )
}

/// The caster's **launch position**: the fired release event's own marker when there is one (the
/// dispatcher passes the event's live position, `0x5ffbd0` → `0x60c940(param_4)`), else the
/// `$CSL` → `$CSR` → `$CST` marker cascade at the current pose, else the unit's base — the
/// client's `0x60c9b0` exactly. Same bone-riding transform as [`attach_world_pos`], reading the
/// event-marker table instead of the attachment table. `None` only when the caster is gone.
fn launch_world_pos(
    caster: Entity,
    fired: Option<[u8; 4]>,
    units: &Query<(&GlobalTransform, Option<&BoneAttach>)>,
    joints: &Query<&GlobalTransform>,
) -> Option<Vec3> {
    let (base, bones) = units.get(caster).ok()?;
    let point = bones.and_then(|b| {
        fired
            .into_iter()
            .chain(MARKER_CASCADE)
            .find_map(|ident| b.markers.get(&ident).copied())
            .and_then(|(bone, offset)| b.anchor(bone).map(|j| (j, offset)))
    });
    Some(
        match point.and_then(|(j, off)| joints.get(j).ok().map(|g| g.transform_point(off))) {
            Some(p) => p,
            None => base.translation(),
        },
    )
}

/// One GO's projectiles parked on their caster, waiting for the release keyframe — the client's
/// `unit+0xac` pending list. The model-cache key is resolved at queue time (the client creates
/// the missile's M2 at queue too, so the async load warms while the release animation plays).
struct QueuedGo {
    spawn: MissileSpawn,
    /// The [`SpellFx`] cache key ([`Missile::path`]'s value-to-be) — `None` = an invisible flight.
    key: Option<String>,
    /// Seconds spent queued — subtracted from the flight time at launch (`0x61ceb0`'s
    /// `elapsed` term: the arrival deadline was fixed at GO).
    queued: f32,
    /// Whether a one-shot has been seen live on the caster since queuing — arms the anim-end
    /// flush edge (never flush on "no one-shot" before the release animation even started).
    saw_oneshot: bool,
}

/// Every caster's pending queue (the client's per-unit `+0xac` list heads). A caster that
/// streams out drops its queue with it.
#[derive(Resource, Default)]
pub(super) struct PendingMissiles(EntityHashMap<Vec<QueuedGo>>);

/// The ammo display's flight model as a [`SpellFx`] cache entry: the **shape rule** (module
/// docs — right slot = `Ammo\`, left slot = `Weapon\`, thrown), with the row's own object skin.
/// The key is `ammo:<display id>` — opaque to the cache, unique per display, so two displays
/// sharing a model but not a skin never collide. `None` = unknown display / model-less row.
fn ensure_ammo_model(
    fx: &mut SpellFx,
    displays: &ItemDisplays,
    display_id: u32,
    asset_server: &AssetServer,
) -> Option<String> {
    let key = format!("ammo:{display_id}");
    if !fx.models.contains_key(&key) {
        let row = displays.catalog.get(display_id)?;
        let (dir_name, col) = if row.model[0].is_some() {
            ("Weapon", 0)
        } else {
            ("Ammo", 1)
        };
        let model = row.model[col].as_ref()?;
        let dir = format!("Item\\ObjectComponents\\{dir_name}");
        let dm = DisplayModel {
            handle: ModelHandle::M2(asset_server.load(m2_url(&format!("{dir}\\{model}")))),
            object_texture: row.model_texture[col].clone(),
            dir,
            ..super::empty_shell()
        };
        fx.models.insert(key.clone(), dm);
    }
    Some(key)
}

/// The wire `SpellMissInfo` codes whose missile arrival plays a victim defense clip, mapped to
/// the melee `$CPP` dispatch's victimState keys (the [`DefenseAnim`] consumer's
/// `select::defense_anim`): DODGE(3) → the dodge state (Dodge 30), BLOCK(5) → the block state
/// (ShieldBlock 24). Every other code — miss/resist/evade/immune/deflect — plays nothing, and
/// PARRY(4) maps to nothing by construction (the recorded negative: a ranged arrival never
/// parries; wow-re `smsg-attackerstate-consequences.md` §Q4).
fn miss_defense_state(code: u8) -> Option<u32> {
    match code {
        3 => Some(2), // SPELL_MISS_DODGE → the dispatch's DODGES state
        5 => Some(5), // SPELL_MISS_BLOCK → BLOCKS
        _ => None,
    }
}

/// The arrival hand-off — the client's `Missile_C::Update` (`0x61ceb0`) outcome dispatch: a
/// landed spell routes back to the router as [`CastEventKind::Impact`] (the impact/state kits +
/// wound flinch); a missed one instead plays the victim's defense clip per
/// [`miss_defense_state`], and no impact kit plays.
fn arrival_handoff(
    target: Entity,
    spell_id: u32,
    weapon_visual: Option<u32>,
    miss: Option<u8>,
    impacts: &mut MessageWriter<CastEvent>,
    defenses: &mut MessageWriter<DefenseAnim>,
    play_seq: &mut crate::creature_anim::PlaySeq,
) {
    match miss {
        None => {
            impacts.write(CastEvent {
                entity: target,
                spell_id,
                kind: CastEventKind::Impact { weapon_visual },
                // Scene-time stamp: the client plays the impact at arrival, after the frame's
                // packet handlers — a fresh tick sorts it the same way.
                seq: play_seq.next(),
            });
        }
        Some(code) => {
            if let Some(victim_state) = miss_defense_state(code) {
                defenses.write(DefenseAnim {
                    victim: target,
                    victim_state,
                });
            }
        }
    }
}

/// Launch one queued GO's projectiles from `launch`: one missile entity per target, its
/// **remaining** travel time `distance / speed − time queued` (the client's `0x61ceb0` — the
/// arrival deadline was fixed at GO). A release already past its deadline arrives on the spot:
/// the arrival hand-off plays now, no flight entity, no flight loop — the melee-range cast
/// shows only the hit, the reference's close-range look.
#[allow(clippy::too_many_arguments)]
fn launch_go(
    go: &QueuedGo,
    launch: Vec3,
    commands: &mut Commands,
    units: &Query<(&GlobalTransform, Option<&BoneAttach>)>,
    joints: &Query<&GlobalTransform>,
    sounds: &mut MessageWriter<MissileSound>,
    impacts: &mut MessageWriter<CastEvent>,
    defenses: &mut MessageWriter<DefenseAnim>,
    play_seq: &mut crate::creature_anim::PlaySeq,
) {
    for &(target, miss) in &go.spawn.targets {
        let dest = go.spawn.dest_tag.into_iter().chain(DEST_FALLBACKS);
        let Some(aim) = attach_world_pos(target, dest, units, joints) else {
            continue; // target already gone — its impact would be invisible anyway
        };
        let arrive_in = launch.distance(aim) / go.spawn.speed.max(f32::EPSILON) - go.queued;
        if arrive_in <= 0.0 {
            arrival_handoff(
                target,
                go.spawn.spell_id,
                go.spawn.weapon_visual,
                miss,
                impacts,
                defenses,
                play_seq,
            );
            continue;
        }
        let dir = (aim - launch).normalize_or(-Vec3::Z);
        let entity = commands
            .spawn((
                Missile {
                    spell_id: go.spawn.spell_id,
                    target,
                    dest_tag: go.spawn.dest_tag,
                    miss,
                    path: go.key.clone(),
                    arrive_in,
                    // Nothing to show → nothing to wait on (the invisible flight).
                    parts_spawned: go.key.is_none(),
                    weapon_visual: go.spawn.weapon_visual,
                },
                Transform::from_translation(launch).with_rotation(missile_facing(dir)),
                Visibility::default(),
            ))
            .id();
        // The flight loop starts at launch and rides the projectile (the sound is independent
        // of the model — an invisible missile still whooshes) until `move_missiles` reaps it.
        if let Some(kit_sound) = go.spawn.missile_sound {
            sounds.write(MissileSound::Start {
                entity,
                kit_sound,
                pos: launch,
            });
        }
    }
}

/// Consume the router's [`MissileSpawn`] and own the **pending queue** (module docs): a GO whose
/// cast kit animates parks on the caster; the release keyframe (`$CSL`/`$CSR`/`$CST`/`$BWR` off
/// the caster's playing clip) drains it from the fired marker's live position; a GO without one
/// launches now through the `0x60c9b0` cascade. Backstops: the one-shot ending without its
/// keyframe (the client's on-anim-finish flush), a one-shot that never starts
/// ([`RELEASE_WAIT_MAX`]), a caster that streams out (queue dies with it). The model-cache entry
/// is created at queue time (shared with the attach-point effects — the same `.mdx` is one load
/// however many things use it); a chain-less spawn resolves the wire ammo display instead
/// ([`ensure_ammo_model`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_missiles(
    mut commands: Commands,
    time: Res<Time>,
    mut spawns: MessageReader<MissileSpawn>,
    mut anim_events: MessageReader<AnimSoundEvent>,
    mut pending: ResMut<PendingMissiles>,
    fx: Option<ResMut<SpellFx>>,
    displays: Option<Res<ItemDisplays>>,
    asset_server: Res<AssetServer>,
    units: Query<(&GlobalTransform, Option<&BoneAttach>)>,
    joints: Query<&GlobalTransform>,
    casters: Query<(
        &AnimDriver,
        &AnimationPlayer,
        &AnimationTransitions,
        &benilla_assets::ModelAnimations,
    )>,
    anim_data: Option<Res<AnimData>>,
    mut sounds: MessageWriter<MissileSound>,
    mut impacts: MessageWriter<CastEvent>,
    mut defenses: MessageWriter<DefenseAnim>,
    mut play_seq: ResMut<crate::creature_anim::PlaySeq>,
) {
    let Some(mut fx) = fx else { return };
    // Intake: resolve the model key now (warm the load while the release animation plays), then
    // park or launch on the GO's release gate.
    for spawn in spawns.read() {
        let key = match (&spawn.path, spawn.ammo_display_id) {
            (Some(path), _) => {
                fx.models
                    .entry(path.clone())
                    .or_insert_with(|| DisplayModel {
                        handle: ModelHandle::M2(asset_server.load(m2_url(path))),
                        ..super::empty_shell()
                    });
                Some(path.clone())
            }
            (None, Some(display_id)) => displays
                .as_deref()
                .and_then(|d| ensure_ammo_model(&mut fx, d, display_id, &asset_server)),
            (None, None) => None,
        };
        let go = QueuedGo {
            spawn: spawn.clone(),
            key,
            queued: 0.0,
            saw_oneshot: false,
        };
        if spawn.awaits_release {
            pending.0.entry(spawn.caster).or_default().push(go);
        } else if let Some(launch) = launch_world_pos(spawn.caster, None, &units, &joints) {
            launch_go(
                &go,
                launch,
                &mut commands,
                &units,
                &joints,
                &mut sounds,
                &mut impacts,
                &mut defenses,
                &mut play_seq,
            );
        } // else: caster gone before its projectile left — nothing to show
    }
    // The release keyframes: a drain ident fired on a caster with a queue launches everything
    // queued there, from the fired marker's position (the client passes the event's own position
    // into the drain).
    for ev in anim_events.read() {
        if !RELEASE_IDENTS.contains(&ev.ident) {
            continue;
        }
        let Some(gos) = pending.0.remove(&ev.entity) else {
            continue;
        };
        let Some(launch) = launch_world_pos(ev.entity, Some(ev.ident), &units, &joints) else {
            continue;
        };
        for go in &gos {
            launch_go(
                go,
                launch,
                &mut commands,
                &units,
                &joints,
                &mut sounds,
                &mut impacts,
                &mut defenses,
                &mut play_seq,
            );
        }
    }
    // The backstop poll: age every queue; flush when the caster's one-shot ended without firing
    // a release keyframe (the client's on-anim-finish `0x5fc920` → `0x60c9b0`), or never started
    // ([`RELEASE_WAIT_MAX`]); drop a queue whose caster is gone.
    let dt = time.delta_secs();
    let catalog = anim_data.as_deref().map(|d| &d.0);
    let mut flushes: Vec<(Entity, Vec<QueuedGo>)> = Vec::new();
    pending.0.retain(|&caster, gos| {
        if !units.contains(caster) {
            return false; // caster streamed out — its pending projectiles die with it
        }
        let live = casters
            .get(caster)
            .is_ok_and(|(drv, player, tr, anims)| oneshot_is_live(drv, player, tr, anims, catalog));
        let mut flush = false;
        for go in gos.iter_mut() {
            go.queued += dt;
            go.saw_oneshot |= live;
            flush |= (go.saw_oneshot && !live) || (!go.saw_oneshot && go.queued > RELEASE_WAIT_MAX);
        }
        if flush {
            // Launched below — the launch's borrows (commands, the queries) are disjoint from
            // this retain, but not expressible inside its closure.
            flushes.push((caster, std::mem::take(gos)));
            return false;
        }
        true
    });
    for (caster, gos) in flushes {
        let Some(launch) = launch_world_pos(caster, None, &units, &joints) else {
            continue;
        };
        for go in &gos {
            launch_go(
                go,
                launch,
                &mut commands,
                &units,
                &joints,
                &mut sounds,
                &mut impacts,
                &mut defenses,
                &mut play_seq,
            );
        }
    }
}

/// Spawn a missile's model parts + particle emitters once its M2 finishes building (the shared
/// cache's `parts` fill in `super::update_display_models`) — children of the missile entity, so
/// they ride the mover below. An unloadable model just flies invisible and still impacts on time.
#[allow(clippy::too_many_arguments)]
pub(super) fn attach_missile_models(
    mut commands: Commands,
    mut missiles: Query<(Entity, &mut Missile)>,
    fx: Option<Res<SpellFx>>,
    time: Res<Time>,
    mut wow_materials: ResMut<Assets<crate::terrain::WowModelMaterial>>,
    mut tint_reg: ResMut<super::spell_fx::FxTintAnims>,
    ibps: Res<Assets<bevy::mesh::skinning::SkinnedMeshInverseBindposes>>,
    mut palettes: ResMut<crate::rig_palette::RigPalettes>,
) {
    let Some(fx) = fx else {
        return;
    };
    for (entity, mut missile) in &mut missiles {
        if missile.parts_spawned {
            continue;
        }
        let Some(key) = &missile.path else {
            continue; // unreachable (spawn sets parts_spawned) — but never look up a None key
        };
        let Some(dm) = fx.models.get(key) else {
            continue; // unreachable — the spawn created the cache entry
        };
        // The one shared effect-visuals body (`spell_fx::attach_effect_visuals`): the rig (the
        // fireball's constant bone keys rotate its authored frame into flight-forward, its
        // global sequence tumbles the molten core, bones 7/8 set the ribbon + cross), skinned
        // parts, joint-riding cards/emitters/ribbons. `false` = still loading — attach on a
        // later pass, mid-flight is fine.
        //
        // The missile runs its model's InFlight sequence when it authors one: the thrown weapon
        // (Thrown_1H_*.m2) tumbles end-over-end through its InFlight bone rotation AND keys its
        // trail ribbon's per-sequence visibility ON — the worn item sits in Stand with neither.
        // A projectile with no InFlight sequence (an arrow, a fireball whose tumble is a global
        // sequence) falls to the model's own `Stand` inside the rig, unchanged.
        if !attach_effect_visuals(
            &mut commands,
            entity,
            dm,
            time.elapsed_secs(),
            false, // a missile flies — its flat quads are geometry, never ground decals
            // A missile is a FREE world model: its trail stays world-frozen, and its pool
            // finishes in place when it impacts (0202's drain).
            EffectHost::default(),
            // A projectile is the separate `CMissile` TU, not a `CEffect`: it has no kit stage and
            // no Birth/Hold/Decay lifecycle — it flies its one sequence and dies on arrival.
            None,
            &mut wow_materials,
            &mut tint_reg,
            &ibps,
            &mut palettes,
            Some(INFLIGHT_ANIM),
        ) {
            continue;
        }
        missile.parts_spawned = true;
    }
}

/// The per-frame mover — the client's arrive-on-time step (`0x61e2a0` over `0x61ceb0`'s clock):
/// each frame the missile covers `dt / remaining-time` of the gap to the target's live dest
/// attach point, so speed re-derives as `distance / remaining time` and a moving target bends
/// the path. On schedule-end it snaps to the point, runs the [`arrival_handoff`] (a landed
/// target's impact, a missed one's dodge/block), and despawns (children with it; emitters
/// self-release via the owner contract).
#[allow(clippy::too_many_arguments)]
pub(super) fn move_missiles(
    mut commands: Commands,
    time: Res<Time>,
    mut missiles: Query<(Entity, &mut Missile, &mut Transform)>,
    units: Query<(&GlobalTransform, Option<&BoneAttach>)>,
    joints: Query<&GlobalTransform>,
    mut impacts: MessageWriter<CastEvent>,
    mut defenses: MessageWriter<DefenseAnim>,
    mut sounds: MessageWriter<MissileSound>,
    mut play_seq: ResMut<crate::creature_anim::PlaySeq>,
) {
    let dt = time.delta_secs();
    for (entity, mut missile, mut transform) in &mut missiles {
        let dest = missile.dest_tag.into_iter().chain(DEST_FALLBACKS);
        let Some(aim) = attach_world_pos(missile.target, dest, &units, &joints) else {
            // The target streamed out mid-flight (the client's dead-handle ground path — ours
            // just ends the flight).
            sounds.write(MissileSound::Stop { entity });
            commands.entity(entity).despawn();
            continue;
        };
        let to_target = aim - transform.translation;
        if missile.arrive_in <= dt {
            arrival_handoff(
                missile.target,
                missile.spell_id,
                missile.weapon_visual,
                missile.miss,
                &mut impacts,
                &mut defenses,
                &mut play_seq,
            );
            sounds.write(MissileSound::Stop { entity });
            commands.entity(entity).despawn();
            continue;
        }
        transform.translation += to_target * (dt / missile.arrive_in);
        missile.arrive_in -= dt;
        if let Some(dir) = to_target.try_normalize() {
            transform.rotation = missile_facing(dir);
        }
    }
}

/// Headless tests for the **pending queue** (module docs): park on a deferred GO, drain on the
/// release keyframe from the fired marker, subtract the queued time from the flight, impact on
/// the spot past the deadline, flush a never-played one-shot. The anim-end flush edge
/// (`saw_oneshot` set, then the one-shot finishing) needs a live driver rig — that path rides the
/// director's live NPC-caster runs instead.
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use bevy::asset::AssetPlugin;

    use super::*;
    use crate::creature_anim::PlaySeq;

    /// `Spell.dbc` Speed such that the test target's 24-unit range is a 1.000 s flight.
    const SPEED: f32 = 24.0;

    fn app() -> App {
        let mut app = App::new();
        // No TimePlugin: the tests advance `Time` by hand so every queued-duration is exact.
        app.add_plugins((TaskPoolPlugin::default(), AssetPlugin::default()));
        app.init_resource::<Time>();
        app.init_resource::<super::super::spell_fx::SpellFx>();
        app.init_resource::<PendingMissiles>();
        app.init_resource::<PlaySeq>();
        app.add_message::<MissileSpawn>()
            .add_message::<AnimSoundEvent>()
            .add_message::<MissileSound>()
            .add_message::<CastEvent>()
            .add_message::<DefenseAnim>();
        app.add_systems(Update, spawn_missiles);
        app
    }

    fn step(app: &mut App, dt: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(dt));
        app.update();
    }

    /// A caster whose `$CSL` marker joint sits at `hand` (the release launch point).
    fn caster(app: &mut App, hand: Vec3) -> Entity {
        let joint = app
            .world_mut()
            .spawn(GlobalTransform::from_translation(hand))
            .id();
        app.world_mut()
            .spawn((
                GlobalTransform::default(),
                BoneAttach {
                    anchors: HashMap::from([(0u16, joint)]),
                    points: HashMap::new(),
                    markers: HashMap::from([(*b"$CSL", (0u16, Vec3::ZERO))]),
                },
            ))
            .id()
    }

    fn go(caster: Entity, target: Entity, awaits_release: bool) -> MissileSpawn {
        MissileSpawn {
            caster,
            spell_id: 133,
            path: None, // an invisible flight — the queue mechanics under test don't need a model
            ammo_display_id: None,
            dest_tag: None,
            speed: SPEED,
            targets: vec![(target, None)],
            weapon_visual: None,
            missile_sound: None,
            awaits_release,
        }
    }

    fn missiles(app: &mut App) -> Vec<(f32, Vec3)> {
        app.world_mut()
            .query::<(&Missile, &Transform)>()
            .iter(app.world())
            .map(|(m, t)| (m.arrive_in, t.translation))
            .collect()
    }

    /// A deferred GO parks; the `$CSL` keyframe drains it from the marker's live position, with
    /// the time spent queued already subtracted from the flight (the client's `0x61ceb0`).
    #[test]
    fn release_keyframe_launches_from_the_marker_minus_queued_time() {
        let mut app = app();
        let hand = Vec3::new(0.0, 1.5, 0.0);
        let caster = caster(&mut app, hand);
        let target = app
            .world_mut()
            .spawn(GlobalTransform::from_xyz(24.0, 1.5, 0.0))
            .id();
        app.world_mut().write_message(go(caster, target, true));
        step(&mut app, 0.05);
        step(&mut app, 0.05);
        assert!(missiles(&mut app).is_empty(), "parked until the keyframe");
        app.world_mut().write_message(AnimSoundEvent {
            entity: caster,
            ident: *b"$CSL",
            data: 0,
        });
        step(&mut app, 0.05);
        let launched = missiles(&mut app);
        assert_eq!(launched.len(), 1);
        let (arrive_in, at) = launched[0];
        assert_eq!(at, hand, "launched from the fired marker's position");
        // 24 units at speed 24 = 1.0 s, minus the 0.10 s spent queued (the two parked frames;
        // the release frame's drain runs before its backstop aging).
        assert!(
            (arrive_in - 0.90).abs() < 1e-3,
            "flight time {arrive_in} ≠ 1.0 − 0.10 queued"
        );
    }

    /// A release already past its arrival deadline (melee range) spawns no flight at all — the
    /// impact plays on the spot, the reference's close-range look.
    #[test]
    fn release_past_deadline_impacts_on_the_spot() {
        let mut app = app();
        let caster = caster(&mut app, Vec3::ZERO);
        // 2.4 units at speed 24 = a 0.1 s flight; the queue waits 0.2 s before the keyframe.
        let target = app
            .world_mut()
            .spawn(GlobalTransform::from_xyz(2.4, 0.0, 0.0))
            .id();
        app.world_mut().write_message(go(caster, target, true));
        step(&mut app, 0.10);
        step(&mut app, 0.10);
        app.world_mut().write_message(AnimSoundEvent {
            entity: caster,
            ident: *b"$CSL",
            data: 0,
        });
        step(&mut app, 0.016);
        assert!(missiles(&mut app).is_empty(), "no flight entity");
        let impacts: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<CastEvent>>()
            .drain()
            .filter(|e| matches!(e.kind, CastEventKind::Impact { .. }))
            .collect();
        assert_eq!(impacts.len(), 1, "the impact plays at release");
        assert_eq!(impacts[0].entity, target);
        assert_eq!(impacts[0].spell_id, 133);
    }

    /// A one-shot that never starts (the kit anim didn't resolve on this model) flushes after
    /// [`RELEASE_WAIT_MAX`] through the marker cascade instead of hanging forever.
    #[test]
    fn never_played_oneshot_flushes_after_the_wait_window() {
        let mut app = app();
        let hand = Vec3::new(0.0, 1.5, 0.0);
        let caster = caster(&mut app, hand);
        let target = app
            .world_mut()
            .spawn(GlobalTransform::from_xyz(24.0, 1.5, 0.0))
            .id();
        app.world_mut().write_message(go(caster, target, true));
        step(&mut app, 0.10);
        step(&mut app, 0.10);
        assert!(missiles(&mut app).is_empty(), "still inside the window");
        step(&mut app, 0.10); // cumulative 0.30 > RELEASE_WAIT_MAX
        let launched = missiles(&mut app);
        assert_eq!(launched.len(), 1, "flushed by the backstop");
        assert_eq!(launched[0].1, hand, "through the $CSL cascade");
        assert!(
            (launched[0].0 - 0.70).abs() < 1e-3,
            "0.30 s queued subtracted"
        );
    }

    /// A missed target's arrival plays the victim's defense clip, not the impact (the
    /// `Missile_C::Update` dispatch, wow-re `smsg-attackerstate-consequences.md` §Q4):
    /// DODGE(3) → the dodge state, BLOCK(5) → the block state, a plain MISS(1) → nothing —
    /// and PARRY(4) → nothing (the recorded "never Parry" negative).
    #[test]
    fn missed_arrival_plays_dodge_or_block_never_impact_or_parry() {
        for (code, expect_state) in [(3u8, Some(2u32)), (5, Some(5)), (1, None), (4, None)] {
            let mut app = app();
            let caster = caster(&mut app, Vec3::ZERO);
            // 2.4 units at speed 24 = a 0.1 s flight; parked 0.2 s → arrival on the spot.
            let target = app
                .world_mut()
                .spawn(GlobalTransform::from_xyz(2.4, 0.0, 0.0))
                .id();
            let mut spawn = go(caster, target, true);
            spawn.targets = vec![(target, Some(code))];
            app.world_mut().write_message(spawn);
            step(&mut app, 0.10);
            step(&mut app, 0.10);
            app.world_mut().write_message(AnimSoundEvent {
                entity: caster,
                ident: *b"$CSL",
                data: 0,
            });
            step(&mut app, 0.016);
            assert!(
                missiles(&mut app).is_empty(),
                "no flight entity (code {code})"
            );
            let impacts = app
                .world_mut()
                .resource_mut::<Messages<CastEvent>>()
                .drain()
                .filter(|e| matches!(e.kind, CastEventKind::Impact { .. }))
                .count();
            assert_eq!(impacts, 0, "a miss never plays the impact (code {code})");
            let defenses: Vec<_> = app
                .world_mut()
                .resource_mut::<Messages<DefenseAnim>>()
                .drain()
                .collect();
            match expect_state {
                Some(state) => {
                    assert_eq!(defenses.len(), 1, "one defense clip (code {code})");
                    assert_eq!(defenses[0].victim, target);
                    assert_eq!(defenses[0].victim_state, state);
                }
                None => assert!(defenses.is_empty(), "code {code} plays nothing"),
            }
        }
    }

    /// A GO whose cast kit plays no body animation launches the frame it arrives (the client's
    /// `0x6e7a70` flush), from the marker cascade, full flight time.
    #[test]
    fn immediate_go_launches_at_once() {
        let mut app = app();
        let hand = Vec3::new(0.0, 1.5, 0.0);
        let caster = caster(&mut app, hand);
        let target = app
            .world_mut()
            .spawn(GlobalTransform::from_xyz(24.0, 1.5, 0.0))
            .id();
        app.world_mut().write_message(go(caster, target, false));
        step(&mut app, 0.016);
        let launched = missiles(&mut app);
        assert_eq!(launched.len(), 1);
        assert_eq!(launched[0].1, hand);
        assert!((launched[0].0 - 1.0).abs() < 1e-3, "full flight time");
    }
}
