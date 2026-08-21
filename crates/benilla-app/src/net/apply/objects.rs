//! Object-stream arm bodies for [`super::apply_net_updates`]'s dispatch match — the streamed
//! world's lifecycle (create / values-merge / destroy / stream-out), the relayed player movement,
//! and the creature path packet. Each `pub(super)` fn here is exactly one arm's body; the match at
//! the call site stays the dispatcher, one call per arm.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use benilla_assets::coords::bevy_to_wow;
use benilla_protocol::{guid, EntityKind, MonsterMoveFacing, MoveSpeeds, ObjectFields, SpeedKind};
use bevy::prelude::*;

use crate::go_templates::GameObjectTemplates;
use crate::items::Items;
use crate::names::NameCache;
use benilla_world::model_fade::DespawnFade;
use benilla_world::vis_chain::VisChainOnly;

use super::super::motion::{
    create_spline, gameobject_rotation, monster_move_spline, pose_transform, resolve_facing,
    trace_create_spline, trace_move_snap, wire_yaw, write_pose, SplineStopped,
};
use super::super::{
    Guid, GuidIndex, NetCommands, NetEntity, ObjectStore, RemoteMotion, SelfGuid,
    SpeedChangeMessage, Spline, UnitSpeeds,
};

/// A GameObject plays its one-shot **Custom** animation (`SMSG_GAMEOBJECT_CUSTOM_ANIM`, decision
/// 1086) — bridged to the GO animation machine ([`crate::go_anim`]), which owns the reject
/// (`anim_id >= 4`), the id mapping (153..156) and the model-ownership gate. The load-bearing
/// sender: the fishing bobber's bite splash (`anim_id 0`).
pub(super) fn gameobject_custom_anim(
    guid: u64,
    anim_id: u32,
    plays: &mut MessageWriter<crate::go_anim::GoCustomAnim>,
) {
    debug!("net: gameobject {guid:#x} custom anim {anim_id}");
    plays.write(crate::go_anim::GoCustomAnim {
        go_guid: guid,
        anim_id,
    });
}

/// An object plays its one-shot **Despawn** animation (`SMSG_GAMEOBJECT_DESPAWN_ANIM`, decision
/// 1404) — the other half of wow-re `gameobject-anim-arm.md` §2c's channel, AnimationData id 157.
///
/// This marks the entity **immediately**, with a component rather than a message, because vmangos
/// sends `SMSG_DESTROY_OBJECT` for the same object in the same server tick: the mark has to be on
/// the entity before [`object_destroyed`] runs, or the destroy frees it before anything can play.
/// Commands apply in the order they were queued, so an insert queued here is visible to the
/// closure the destroy queues below. The arm itself, and the model-ownership gate, are
/// [`crate::go_anim`]'s.
pub(super) fn gameobject_despawn_anim(guid: u64, commands: &mut Commands, index: &GuidIndex) {
    debug!("net: gameobject {guid:#x} despawn anim");
    if let Some(&e) = index.0.get(&guid) {
        commands
            .entity(e)
            .insert(crate::go_anim::DespawnAnimAnnounced);
    }
}

/// An object entered range / was created (`SMSG_UPDATE_OBJECT` create block): spawn or refresh the
/// entity, warm the ask-once caches, and seed its descriptor store via the per-drain `pending` map.
#[allow(clippy::too_many_arguments)]
pub(super) fn object_create(
    guid: u64,
    kind: EntityKind,
    display_id: Option<u32>,
    position: [f32; 3],
    orientation: f32,
    scale: f32,
    speeds: Option<MoveSpeeds>,
    transport_progress: Option<u32>,
    transport: Option<benilla_protocol::TransportPose>,
    spline: Option<benilla_protocol::CreateSpline>,
    fields: ObjectFields,
    commands: &mut Commands,
    index: &mut GuidIndex,
    transforms: &mut Query<&mut Transform>,
    stores: &mut Query<&mut ObjectStore>,
    pending: &mut HashMap<u64, ObjectFields>,
    speed_stage: &mut SpeedStage,
    names: &mut NameCache,
    go_templates: &mut GameObjectTemplates,
    net_commands: &NetCommands,
) {
    let net = NetEntity {
        kind,
        display_id,
        scale,
    };
    // A transport's cycle anchor (decision 0438): the create block's `UPDATE_FLAG_TRANSPORT`
    // u32 + the local instant it landed. Re-creates re-anchor (the server re-sends the create at
    // map transitions and mid-course update frames precisely so clients can correct drift). The
    // transport tick owns it from here (`crate::transport`). Both ticking GO types (the client's
    // RF-0051 pair): 15 (boats — vmangos sends its path-progress clock) and 11 (elevators/lifts
    // — the same flag, `GameObject.cpp:246`, a `time-since-create % period` clock).
    let go_type = (kind == EntityKind::GameObject).then(|| fields.gameobject_type_id());
    let transport_anchor = matches!(go_type, Some(11 | 15))
        .then_some(transport_progress)
        .flatten()
        .map(|progress_ms| crate::transport::TransportAnchor {
            progress_ms,
            at: std::time::Instant::now(),
        });
    // The spawn's `GAMEOBJECT_ROTATION` quaternion — read once, wanted twice: it is this object's
    // placement (below) and, on a type-11 lift, the basis its keyframe offsets rotate through.
    let go_quat = (kind == EntityKind::GameObject)
        .then(|| fields.gameobject_rotation())
        .flatten();
    // A type-11 lift's arm seed (decision 0438 phase 3's second consumer): the keyframe path is
    // keyed by the template **entry**, the offsets rotate through the spawn's `GAMEOBJECT_ROTATION`
    // quat, and the base is the stationary spot the movement block carried — all already in this
    // create block, so the lift arm needs no template round-trip. Anchor-gated: a type-11 whose
    // create carried no progress u32 has no clock to tick and stays frozen.
    let elevator_seed = (matches!(go_type, Some(11)) && transport_anchor.is_some())
        .then(|| fields.object_entry())
        .flatten()
        .map(|entry| crate::transport::ElevatorSeed {
            entry,
            base_pos: position,
            yaw: orientation,
            quat: go_quat,
        });
    // Where this object is *pointed*. A mover carries one yaw; a GameObject is placed by its
    // `GAMEOBJECT_ROTATION` quaternion, which is the reference's own placement input and is a
    // strictly wider answer than the facing (decision 1459, `motion::gameobject_rotation`).
    let placement = match kind {
        EntityKind::GameObject => gameobject_rotation(go_quat, orientation),
        _ => wire_yaw(orientation),
    };
    // A unit/player created already ON a transport (deck NPCs stream in this way): its LIVING
    // block's rider tail is its local pose — `compose_riders` re-anchors it through the boat's
    // live matrix each frame (decision 0438 phase 2). The block's world `position` is the
    // spawn-time compose, kept as the pre-arm fallback pose.
    let rider = matches!(kind, EntityKind::Unit | EntityKind::Player)
        .then_some(transport)
        .flatten()
        .map(|t| crate::transport::TransportRider {
            transport_guid: t.guid,
            local_pos: [t.pos.x, t.pos.y, t.pos.z],
            local_orientation: t.orientation,
        });
    // Warm the name cache the moment a unit streams in — the real client's cache is
    // demand-driven too, but persisted (`CreatureCache.wdb`, wow-re dbcache node), so in
    // practice it always answers instantly; asking at first *sight* rather than first
    // *target* gives our session-lifetime cache the same instant feel (the ask-once
    // discipline makes re-creates and shared templates free).
    if matches!(kind, EntityKind::Unit | EntityKind::Player) {
        let _ = names.resolve(guid, net_commands);
    }
    // Warm the lock cache the moment a GameObject streams in (decision 0239), so a
    // right-click resolves use-vs-cast instantly — the same ask-once, ask-at-sight
    // discipline as the name cache. The lockId isn't in the create packet; only the query
    // carries it.
    if matches!(kind, EntityKind::GameObject) {
        go_templates.request(guid, net_commands);
        // Where and how big, the moment it streams in — the readout that answers "is this prop in
        // the wrong place, or the wrong size, or just drawn wrong" without a guess (decision 0637:
        // the duel flag read as huge and mislocated, and nothing in the client could say which).
        // `RUST_LOG=benilla_app::net::apply::objects=debug`.
        debug!(
            "gameobject spawn: entry {:?} display {display_id:?} type {go_type:?} \
             pos [{:.2}, {:.2}, {:.2}] scale {scale}",
            fields.object_entry(),
            position[0],
            position[1],
            position[2],
        );
    }
    // The walk this unit is ALREADY on (decision 0708): its create block's live spline, joined at the
    // server's own progress along it. Traced before it is interpreted, so the `WOW_CREATE_SPLINE=off`
    // leg of the A/B still records what the wire offered.
    trace_create_spline(guid, spline.as_ref());
    let walk = spline.and_then(create_spline);
    if let Some(&e) = index.0.get(&guid) {
        // Re-create of a tracked guid: refresh identity + pose. A create is a fresh server snapshot, so
        // any in-flight extrapolation is stale too — clear it.
        commands.entity(e).insert(net).remove::<RemoteMotion>();
        // Speeds go through the drain's stage, never straight onto the entity — a
        // `SMSG_FORCE_*_SPEED_CHANGE` riding the same tick as this create has to be able to land
        // on top of it (decision 1478, B213).
        if let Some(s) = speeds {
            speed_stage.seed(guid, s);
        }
        if let Some(anchor) = transport_anchor {
            commands.entity(e).insert(anchor);
        }
        if let Some(seed) = elevator_seed {
            commands.entity(e).insert(seed);
        }
        match rider {
            Some(r) => {
                commands.entity(e).insert(r);
            }
            None => {
                commands
                    .entity(e)
                    .remove::<crate::transport::TransportRider>();
            }
        }
        // The snapshot's own path outranks whatever we were riding; a create *without* one says the
        // unit is standing still, so a stale path goes.
        match walk {
            Some(s) => {
                commands.entity(e).insert(s);
            }
            None => {
                commands.entity(e).remove::<Spline>();
            }
        }
        write_pose(commands, transforms, e, position, placement);
        // Overlay the fresh snapshot's descriptor fields onto the existing store.
        merge_fields(stores, pending, e, guid, fields);
    } else {
        // A transport spawns hidden: its create pose is the *stationary* spawn point (or worse,
        // the origin), not where the boat is in its cycle — the transport tick unhides it at the
        // first sampled pose (decision 0438).
        let visibility = if transport_anchor.is_some() {
            Visibility::Hidden
        } else {
            Visibility::default()
        };
        let mut entity = commands.spawn((
            Guid(guid),
            net,
            pose_transform(position, placement),
            visibility,
        ));
        // Chain-only visibility (benilla_world::vis_chain): the net root renders nothing —
        // its model parts and joints are the children, and the cull authorities flip this
        // root's `Visibility` through `Mut` writes, which don't re-add the sweep row.
        entity.vis_chain_only();
        if let Some(s) = speeds {
            speed_stage.seed(guid, s); // staged, like the re-create arm above (1478)
        }
        if let Some(anchor) = transport_anchor {
            entity.insert(anchor);
        }
        if let Some(seed) = elevator_seed {
            entity.insert(seed);
        }
        if let Some(r) = rider {
            entity.insert(r);
        }
        if let Some(s) = walk {
            entity.insert(s);
        }
        index.0.insert(guid, entity.id());
        // Seed the store via the pending flush — the entity isn't spawned until the sync point.
        pending.insert(guid, fields);
    }
}

/// An item or container entered our view (`SMSG_UPDATE_OBJECT` descriptor-only create) — no scene
/// entity; the item store owns it.
pub(super) fn item_create(guid: u64, container: bool, fields: ObjectFields, items: &mut Items) {
    debug!("net: item create {guid:#x} (container: {container})");
    items.insert_object(guid, fields);
}

/// An existing object moved to a new authoritative pose (an `SMSG_UPDATE_OBJECT` movement block) —
/// a one-off correction/relocation that supersedes any active path.
pub(super) fn object_move(
    guid: u64,
    position: [f32; 3],
    orientation: f32,
    commands: &mut Commands,
    index: &GuidIndex,
    transforms: &mut Query<&mut Transform>,
) {
    if let Some(&e) = index.0.get(&guid) {
        commands.entity(e).remove::<Spline>();
        // A movement block carries a yaw and nothing else — the only GameObjects that get one are
        // transports, whose pose the transport tick owns from the next frame (decision 0438).
        write_pose(commands, transforms, e, position, wire_yaw(orientation));
    }
}

/// A relayed player movement packet (`MSG_MOVE_*`): the mover's authoritative pose + live move
/// flags. The reference SCHEDULES a remote's apply (decision 0601, wow-re
/// `remote-apply-timing.md`): the mover's own replay chain gives the packet a client fire-time
/// (decision 0615, [`crate::net::motion::RelayMove`] → `RelayChain::schedule`); an already-due move
/// applies now, a future one queues on the unit and fires in `drain_pending_moves` — the dead-reckon
/// covering the mover's own timeline in between, which is what kills the arrival-jitter snap.
/// `WOW_REMOTE_SNAP=1` restores raw apply-at-arrival for an A/B.
#[allow(clippy::too_many_arguments)] // the wire fields + the apply context, one per concern
pub(super) fn unit_move(
    guid: u64,
    mv: crate::net::motion::RelayMove,
    now_ms: f64,
    commands: &mut Commands,
    index: &GuidIndex,
    self_guid: &SelfGuid,
    remote_motion: &mut Query<&mut RemoteMotion>,
    transforms: &mut Query<&mut Transform>,
    landings: &mut MessageWriter<crate::creature_anim::HardLanding>,
    self_moves: &mut MessageWriter<crate::net::SelfMoveMessage>,
) {
    use crate::net::motion::{apply_move, arrival_snap, trace_relay, PendingMove, RelayOutcome};
    // Addressed to US: the server writing our own pose, never an echo of ours (every one is
    // `SetAsServerSide`, `ctime = 0`). The reference APPLIES it — there is no mover-guid gate
    // anywhere on its inbound move path, and the local player resolves through the same object
    // lookup as anyone else (decision 0725; wow-re `self-addressed-move.md`). What is ours is only
    // *where it goes*: our avatar's motion source is the controller, not [`RemoteMotion`], so the
    // pose crosses to `player::wire_in` instead of down this lane.
    if self_guid.0 == Some(guid) {
        trace_relay(
            guid,
            &mv,
            &Default::default(),
            now_ms,
            0,
            RelayOutcome::SelfMover,
        );
        self_moves.write(crate::net::SelfMoveMessage {
            position: mv.position,
            orientation: mv.orientation,
            flags: mv.flags,
            pitch: mv.pitch,
            fall_time: mv.fall_time,
            jump: mv.jump,
        });
        return;
    }
    let Some(&e) = index.0.get(&guid) else {
        // No entity for this guid — the packet changes nothing. Traced rather than dropped in
        // silence: a mover that keeps running while these pile up is a streaming bug, not a replay
        // one, and the two look identical from the outside (decision 0619).
        trace_relay(
            guid,
            &mv,
            &Default::default(),
            now_ms,
            0,
            RelayOutcome::Unknown,
        );
        return;
    };
    {
        // The server is authoritative now, not any creature path — drop a stale spline.
        commands.entity(e).remove::<Spline>();
        if let Ok(mut rm) = remote_motion.get_mut(e) {
            // The chain reads the mover's state as it stands BEFORE this move applies — the
            // reference times the packet off the live `[esi+0x40]`/`[esi+0x150]`.
            let (flags, queue_empty) = (rm.flags, rm.pending.is_empty());
            let fire_ms = rm.relay.schedule(mv.wire_ms, now_ms, flags, queue_empty);
            let at_arrival = arrival_snap() || rm.fires_at_arrival(fire_ms, now_ms);
            trace_relay(
                guid,
                &mv,
                &rm.relay,
                now_ms,
                rm.pending.len(),
                if at_arrival {
                    RelayOutcome::Now
                } else {
                    RelayOutcome::Queued
                },
            );
            if at_arrival {
                apply_move(e, &mv, &mut rm, now_ms, commands, landings);
            } else {
                // Fire-times are monotone per unit by construction (a chained fire never lands
                // before its predecessor — decision 0615), so the queue stays ordered.
                rm.pending.push_back(PendingMove { fire_ms, mv });
            }
        } else {
            // First move for this unit: apply immediately (the chain seeds on it — its own fire
            // is arrival — and paces from the next packet on) and place it this frame; the
            // component insert is deferred, so the extrapolator won't see it until next frame.
            let mut rm = RemoteMotion {
                wow_pos: mv.position,
                orientation: mv.orientation,
                flags: 0,
                pitch: 0.0,
                speed: 0.0,
                vertical_velocity: 0.0,
                jump_xy_vel: [0.0; 2],
                fall_start_z: None,
                pending: std::collections::VecDeque::new(),
                relay: Default::default(),
                last_apply_ms: now_ms,
                last_apply_pos: mv.position,
            };
            let fire_ms = rm.relay.schedule(mv.wire_ms, now_ms, 0, true);
            debug_assert_eq!(fire_ms, now_ms, "a seeding packet fires at arrival");
            trace_relay(guid, &mv, &rm.relay, now_ms, 0, RelayOutcome::Seed);
            apply_move(e, &mv, &mut rm, now_ms, commands, landings);
            write_pose(
                commands,
                transforms,
                e,
                mv.position,
                wire_yaw(mv.orientation),
            );
            commands.entity(e).insert(rm);
        }
    }
}

/// A descriptor delta (`SMSG_UPDATE_OBJECT` values block): merge into the object's store — a scene
/// object's in place (or into the pending seed), an item's into the item store. An unknown guid —
/// a `Values` with no create seen — is dropped, as before.
pub(super) fn object_values(
    guid: u64,
    fields: ObjectFields,
    index: &GuidIndex,
    stores: &mut Query<&mut ObjectStore>,
    pending: &mut HashMap<u64, ObjectFields>,
    items: &mut Items,
) {
    if let Some(&e) = index.0.get(&guid) {
        merge_fields(stores, pending, e, guid, fields);
    } else if guid::is_item(guid) {
        items.merge_object(guid, fields);
    }
}

/// The object ceased to exist (`SMSG_DESTROY_OBJECT` — corpse decay ahead of respawn, a despawn).
pub(super) fn object_destroyed(
    guid: u64,
    commands: &mut Commands,
    index: &mut GuidIndex,
    items: &mut Items,
) {
    // The reference frees it on the spot, an **instant pop** with no fade-out
    // (byte-verified, wow-re selection-death-clear RE — the only lifecycle fade is the
    // appear-fade on create). A respawn then streams in as a fresh entity. If it was the
    // target, the ring's gone-entity branch clears the selection next frame — the
    // reference's teardown does the same (and sends the same `CMSG_SET_SELECTION 0`).
    //
    // …*unless the object is pinned* — `0x464920` on a still-pinned object only sets the
    // pending-destroy bit and returns, and the real free waits for the last pin to drop (wow-re
    // `go-display-sound-events.md` §6d). The one pin benilla takes is the despawn animation
    // announced a moment earlier by `SMSG_GAMEOBJECT_DESPAWN_ANIM`, which is the whole of how an
    // object gets to play its own despawn after the server says it is gone (decision 1404).
    // The guid leaves the index either way: to the server it no longer exists.
    if let Some(e) = index.0.remove(&guid) {
        commands.queue(move |world: &mut bevy::ecs::world::World| {
            if world
                .get::<crate::go_anim::DespawnAnimAnnounced>(e)
                .is_some()
            {
                if let Ok(mut ent) = world.get_entity_mut(e) {
                    ent.insert(crate::go_anim::PendingDestroy);
                }
            } else if let Ok(ent) = world.get_entity_mut(e) {
                ent.despawn();
            }
        });
    }
    // An item destroy (consumed, sold) never had a scene entity — clear the item store.
    items.remove_object(guid);
}

/// Stream-out (out-of-range, the update-object `OutOfRange` block): the unit still exists, we just
/// left its range.
pub(super) fn objects_removed(guids: Vec<u64>, commands: &mut Commands, index: &mut GuidIndex) {
    // Don't pop the entity, fade it out, then despawn (`apply_despawn_fade` drives the ramp; an
    // entity with no fadeable geometry pops straight out there). Director-verified look:
    // on the reference, distant mobs fade out, never blink out (destroy, above, is the
    // byte-verified instant pop; 0067's open question, settled by the director's eyes —
    // which reference mechanism produces the fade is unpinned and doesn't matter here).
    for g in guids {
        if let Some(e) = index.0.remove(&g) {
            commands.entity(e).insert(DespawnFade::default());
        }
    }
}

/// A creature path packet (`SMSG_MONSTER_MOVE`): apply the dictated facing snap, then attach or
/// clear the travel spline.
#[allow(clippy::too_many_arguments)]
pub(super) fn monster_move(
    guid: u64,
    start: [f32; 3],
    spline_id: u32,
    path: Vec<[f32; 3]>,
    facing: MonsterMoveFacing,
    stop: bool,
    duration_ms: u32,
    flying: bool,
    commands: &mut Commands,
    index: &GuidIndex,
    transforms: &mut Query<&mut Transform>,
) {
    if let Some(&e) = index.0.get(&guid) {
        // The DESYNC readout (decision 0708): how far this packet is about to teleport the unit — the
        // gap between where we have been drawing it and where the server says the path begins. A
        // correctly-followed creature reads ~0; a frozen one reads the whole walk it slept through.
        trace_move_snap(
            guid,
            transforms.get(e).ok().map(|t| bevy_to_wow(t.translation)),
            start,
            stop,
            duration_ms,
        );
        // Apply the dictated final facing (moveType 2/3/4) as a **snap** — faithful to the
        // client, which stores it straight into the unit's **raw** movement facing (`0x7c6f30`).
        // This is the *packet*-driven re-face — a scripted/emote/aggro `SetFacingTo` the
        // server actually sends. (The client-local re-faces — squaring up on a target, and
        // turning to face you while an interaction window is open — carry no packet at all and
        // are `motion::drive_display_facing`'s, decision 1467.) Because it is the raw facing
        // that moved, the display smoother's state goes with it: the unit re-seeds from this
        // pose instead of swinging back to the heading it was snapped off. When a real path
        // follows, `sample_splines` overwrites the rotation with the travel direction each
        // frame (faithful — the client's spline-follow snaps the mesh yaw to the path tangent;
        // wow-re body-facing §4). The receipt snap thus only sticks for a path-less move (a
        // `Stop`/in-place re-face); a moving unit ends on its last tangent.
        if !matches!(facing, MonsterMoveFacing::None) {
            let target_pos = |g: u64| {
                index
                    .0
                    .get(&g)
                    .and_then(|&te| transforms.get(te).ok())
                    .map(|t| bevy_to_wow(t.translation))
            };
            if let Some(orientation) = resolve_facing(facing, start, target_pos) {
                if let Ok(mut t) = transforms.get_mut(e) {
                    t.rotation = Quat::from_rotation_y(orientation);
                    commands
                        .entity(e)
                        .remove::<crate::net::motion::DisplayFacing>();
                }
            }
        }
        // The spline is authoritative now, not the relay stream — the mirror of `unit_move`'s
        // "drop a stale spline". A splined PLAYER (charge, taxi) otherwise keeps a stale
        // `RemoteMotion` whose old flags outrank the spline in the anim selector's `unify`
        // precedence (Stand frozen mid-flight); the relay re-seeds it on its next packet.
        //
        // A spline move also **un-nocks and drops the weapon-visual hold**, unconditionally:
        // `0x6018f0` (this packet's handler, via `0x603f00` registered on opcodes 0xDD/0x2AE) has
        // exactly one `ret`, and its shared tail runs `0x6020e8 call 0x60d040` (clear `0x400`) →
        // `0x6020ef call 0x60f530` (un-nock) → `RecomputeBaseAnim(-1)` on every path through it
        // (wow-re `shooter-stop-law.md` §J1.2/§J1.3, byte-verified over the whole extent). So an
        // archer NPC yanked along a path, or a player charged/knocked back, loses the arrow —
        // the ranged sheath is untouched, exactly like the locomotion un-nock.
        commands.entity(e).remove::<(
            RemoteMotion,
            crate::creature_anim::NockLatch,
            crate::creature_anim::RangedHold,
        )>();
        match monster_move_spline(path, spline_id, stop, duration_ms, flying) {
            // A moving path: sample_splines drives the transform along every waypoint.
            Some(spline) => {
                commands.entity(e).insert(spline).remove::<SplineStopped>();
            }
            // Stop/clear: freeze where the last sample left it (≈ the endpoint) — and keep the id,
            // which the server is waiting to hear back for a player-driven unit (decision 1281).
            None => {
                commands
                    .entity(e)
                    .remove::<Spline>()
                    .insert(SplineStopped(spline_id));
            }
        }
    }
}

/// The ask-once GameObject template (`SMSG_GAMEOBJECT_QUERY_RESPONSE`, decision 0239): cache it and
/// resolve the lockId from the type-specific `data[]` slot — the interact routing reads it to choose
/// use-vs-cast; the hover tooltip reads the name (decision 0276's GO law).
pub(super) fn gameobject_info(
    entry: u32,
    type_id: u32,
    display_id: u32,
    name: String,
    data: &[i32; 24],
    go_templates: &mut GameObjectTemplates,
) {
    debug!("net: gameobject template {entry} type {type_id} display {display_id} {name:?}");
    go_templates.insert(entry, type_id, name, data);
}

/// Merge a descriptor delta into an object's store: into the per-drain `pending` seed when the entity was
/// created earlier this same drain (its spawn `Command` hasn't run, so it isn't queryable yet), else in
/// place on the live component. The final `else` — in the index but neither live nor pending — should not
/// happen (a create always seeds `pending` first), but seeds defensively rather than drop the delta.
fn merge_fields(
    stores: &mut Query<&mut ObjectStore>,
    pending: &mut HashMap<u64, ObjectFields>,
    entity: Entity,
    guid: u64,
    delta: ObjectFields,
) {
    if let Some(f) = pending.get_mut(&guid) {
        f.merge(delta);
    } else if let Ok(mut s) = stores.get_mut(entity) {
        s.0.merge(delta);
    } else {
        pending.insert(guid, delta);
    }
}

/// Per-drain staging for every mover's [`UnitSpeeds`] — the drain's single speed writer
/// (decision 1478).
///
/// The two speed sources used to disagree about *when* they land. [`object_create`] inserts a whole
/// speed set through `Commands`, which is deferred to the drain's sync point; a
/// `SMSG_FORCE_*_SPEED_CHANGE` edited one slot of the live component immediately. Mixed in one
/// drain, the **later packet loses** either way: on an entity born this drain the component isn't
/// there yet so the query miss was silent, and on one that already existed the create's deferred
/// insert landed afterwards and overwrote it.
///
/// vmangos puts exactly those two packets back to back on purpose. `HandleMoveWorldportAckOpcode`
/// sends the self create block inside `Map::Add` → `SendInitSelf` (carrying the speeds the player
/// still has) and then, three statements later, strips the mount on a map that forbids one
/// (`if (!mEntry->IsMountAllowed()) RemoveSpellsCausingAura(SPELL_AURA_MOUNTED)`) — whose
/// `SMSG_FORCE_RUN_SPEED_CHANGE` therefore rides the same tick, and so the same drain. That is
/// B213: `.tele` into BWL/LBRS on a mount auto-dismounted you and left the mount's run speed on
/// foot, because the create's 11.2 yd/s landed last.
///
/// So both sources stage here instead, **in packet order**, and the drain flushes once at the end —
/// the `pending: HashMap<u64, ObjectFields>` pattern (decision 0061) applied to speeds. Nothing
/// writes the component directly any more, which is what makes the order the wire's order.
#[derive(Default)]
pub(super) struct SpeedStage(HashMap<u64, MoveSpeeds>);

impl SpeedStage {
    /// A create block's whole speed set. A create is the server's newest snapshot of the mover, so
    /// it **replaces** anything staged for that guid rather than merging into it.
    fn seed(&mut self, guid: u64, speeds: MoveSpeeds) {
        self.0.insert(guid, speeds);
    }

    /// Write one speed-kind slot, seeding the staged set from the live component on first touch.
    /// `live` is only called when nothing is staged yet; `None` from it means there is nowhere to
    /// write — an untracked guid, or a mover whose create carried no movement block — and the
    /// change is reported unapplied rather than silently dropped.
    fn set(
        &mut self,
        guid: u64,
        kind: SpeedKind,
        speed: f32,
        live: impl FnOnce() -> Option<MoveSpeeds>,
    ) -> bool {
        let s = match self.0.entry(guid) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => match live() {
                Some(s) => v.insert(s),
                None => return false,
            },
        };
        match kind {
            SpeedKind::Walk => s.walk = speed,
            SpeedKind::Run => s.run = speed,
            SpeedKind::RunBack => s.run_back = speed,
            SpeedKind::Swim => s.swim = speed,
            SpeedKind::SwimBack => s.swim_back = speed,
            SpeedKind::TurnRate => s.turn_rate = speed,
        }
        true
    }

    /// Land every staged set on its entity, now that this drain's spawn `Command`s have run.
    /// `try_insert`: the same drain may have queued a despawn for it (a stream-out, the worldport
    /// purge), and a guid the purge dropped from the index resolves to nothing at all.
    pub(super) fn flush(self, commands: &mut Commands, index: &GuidIndex) {
        for (guid, speeds) in self.0 {
            if let Some(&e) = index.0.get(&guid) {
                commands.entity(e).try_insert(UnitSpeeds(speeds));
            }
        }
    }
}

/// Read a mover's live speed set — [`SpeedStage::set`]'s seed for a mover that already exists.
fn live_speeds(index: &GuidIndex, speeds: &Query<&UnitSpeeds>, guid: u64) -> Option<MoveSpeeds> {
    index
        .0
        .get(&guid)
        .and_then(|&e| speeds.get(e).ok())
        .map(|s| s.0)
}

/// A forced speed change on a mover (aura/mount/GM `.modify speed`): stage the new value onto the
/// entity's speed set, and — when the mover is our own player — forward to the controller, which
/// answers the mandatory ack with its live pose (the TeleportMessage pattern). An unknown guid
/// still acks if it's ours-by-guid; a foreign mover (we never control others) is only applied,
/// never acked — acking a unit we don't control is the server's error path.
#[allow(clippy::too_many_arguments)]
pub(super) fn force_speed_change(
    guid: u64,
    kind: SpeedKind,
    counter: u32,
    speed: f32,
    index: &GuidIndex,
    speeds: &Query<&UnitSpeeds>,
    stage: &mut SpeedStage,
    self_guid: &SelfGuid,
    speed_changes: &mut MessageWriter<SpeedChangeMessage>,
) {
    let applied = stage.set(guid, kind, speed, || live_speeds(index, speeds, guid));
    if self_guid.0 == Some(guid) {
        info!("net: force {kind:?} speed change -> {speed} yd/s (counter {counter})");
        // Our own mover having no speed set to write is B213's failure shape, and it was silent
        // for as long as it existed. It should be unreachable now that the create stages too —
        // so if it ever fires, the next report starts from a line instead of from a guess.
        if !applied {
            warn!(
                "net: force {kind:?} speed change for OUR mover {guid:#x} landed nowhere \
                 (no speed set staged or live) — we still ack, so the server and we now disagree"
            );
        }
        speed_changes.write(SpeedChangeMessage {
            guid,
            kind,
            counter,
            speed,
        });
    } else {
        debug!("net: force speed change for foreign mover {guid:#x} — applied {applied}, no ack");
    }
}

/// An observed unit's speed changed (the SPLINE_SET / MOVE_SET families — another player
/// mounting up, a hastened creature): stage it onto that unit's speed set, nothing to ack
/// (decision 0441). The MOVE_SET flavour's pose already arrived as its own UnitMove.
pub(super) fn speed_changed(
    guid: u64,
    kind: SpeedKind,
    speed: f32,
    index: &GuidIndex,
    speeds: &Query<&UnitSpeeds>,
    stage: &mut SpeedStage,
) {
    if !stage.set(guid, kind, speed, || live_speeds(index, speeds, guid)) {
        // Ordinary: a unit's speed broadcast can outrun its create block. Nothing to ack and the
        // create that follows carries the same set, so this self-heals.
        debug!("net: {kind:?} speed change for untracked mover {guid:#x} — nothing to write");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: u64 = 0x0000_0000_0000_002A;

    /// A dismounted human's set — 1.12.1's base run 7.0 (the `--speed` probe's own golden).
    fn on_foot() -> MoveSpeeds {
        MoveSpeeds {
            walk: 2.5,
            run: 7.0,
            run_back: 4.5,
            swim: 4.722_222,
            swim_back: 2.5,
            turn_rate: std::f32::consts::PI,
        }
    }

    /// The same set with a 60% mount's run speed on it — what `SendInitSelf` puts in the create
    /// block of a worldport that is *about* to strip the mount.
    fn mounted() -> MoveSpeeds {
        MoveSpeeds {
            run: 11.2,
            ..on_foot()
        }
    }

    /// **B213, pinned.** vmangos's `HandleMoveWorldportAckOpcode` sends the self create block
    /// (`Map::Add` → `SendInitSelf`, still carrying the mount's 11.2 yd/s) and then, three
    /// statements later, strips the mount because the destination map forbids one — so
    /// `SMSG_FORCE_RUN_SPEED_CHANGE` 7.0 rides the same tick and lands in the same drain.
    /// Staged in packet order the change wins; before 1478 the create's *deferred* `UnitSpeeds`
    /// insert landed last and `.tele` into BWL left the avatar running at mount speed on foot.
    #[test]
    fn a_force_change_beats_a_create_from_the_same_drain() {
        let mut stage = SpeedStage::default();
        stage.seed(ME, mounted());
        assert!(stage.set(ME, SpeedKind::Run, 7.0, || panic!(
            "the create staged a set this drain — the live component must not be consulted"
        )));
        stage.flush_into(|guid, s| {
            assert_eq!(guid, ME);
            assert_eq!(s.run, 7.0, "the dismount is the newer packet");
            assert_eq!(
                s.walk, 2.5,
                "the other slots still come from the create block"
            );
        });
    }

    /// The reverse order is just as much the wire's order: a create block is the server's newest
    /// snapshot of the mover, so one arriving *after* a change replaces it whole rather than
    /// merging under it.
    #[test]
    fn a_create_after_a_change_replaces_it() {
        let mut stage = SpeedStage::default();
        assert!(stage.set(ME, SpeedKind::Run, 7.0, || Some(mounted())));
        stage.seed(ME, mounted());
        stage.flush_into(|_, s| assert_eq!(s.run, 11.2));
    }

    /// Nothing staged yet (an entity that has existed since an earlier drain): the live component
    /// seeds the cell, so a one-slot change keeps every other slot it isn't addressing.
    #[test]
    fn a_change_on_a_live_mover_seeds_from_the_component() {
        let mut stage = SpeedStage::default();
        assert!(stage.set(ME, SpeedKind::Run, 11.2, || Some(on_foot())));
        stage.flush_into(|_, s| {
            assert_eq!(s.run, 11.2);
            assert_eq!(s.run_back, on_foot().run_back);
            assert_eq!(s.turn_rate, on_foot().turn_rate);
        });
    }

    /// A change with nowhere to land — an untracked guid, or a mover whose create carried no
    /// movement block — reports itself unapplied rather than vanishing. It stages nothing, so a
    /// later create can't inherit a slot invented for a set that never existed.
    #[test]
    fn a_change_with_no_speed_set_is_reported_not_swallowed() {
        let mut stage = SpeedStage::default();
        assert!(!stage.set(ME, SpeedKind::Run, 7.0, || None));
        let mut flushed = 0;
        stage.flush_into(|_, _| flushed += 1);
        assert_eq!(flushed, 0);
    }

    /// The trap itself, machine-checked rather than asserted in prose: a component inserted
    /// through this drain's `Commands` is **not** visible to a query in the same drain — the spawn
    /// only runs at the sync point. That is why staging is the fix and why a live-component write
    /// could never have been the fix: the force-change arm had nothing to write onto.
    #[test]
    fn a_component_this_drain_queued_is_invisible_until_the_sync_point() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        let missed = world
            .run_system_once(|mut commands: Commands, live: Query<&UnitSpeeds>| {
                let e = commands.spawn(UnitSpeeds(mounted())).id();
                live.get(e).is_err()
            })
            .expect("the probe system runs");
        assert!(
            missed,
            "a create's UnitSpeeds must be unreadable later in the same drain — if this ever \
             passes, `Commands` stopped being deferred and 1478's staging can be reconsidered"
        );
    }

    impl SpeedStage {
        /// [`SpeedStage::flush`] without the ECS half — the staged sets in the shape the flush
        /// would insert them, so the ordering rules above are testable as the pure thing they are.
        fn flush_into(self, mut f: impl FnMut(u64, MoveSpeeds)) {
            for (guid, speeds) in self.0 {
                f(guid, speeds);
            }
        }
    }
}
