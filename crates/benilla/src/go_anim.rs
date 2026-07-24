//! GameObject open/close animation (decision 0242; chest lid folded in by 0250) — a **client-side**
//! `GAMEOBJECT_STATE` drives a skeletal M2 sequence, so a **door** swings, a **button** depresses, and a
//! **chest lid** opens/closes on its §243 state machine.
//!
//! **The model (0250, §5-VERIFIED):** the real client keeps *one* stored state per GameObject (the
//! binary's `go+0x27c`) and *one* `SetGoState` that all callers funnel through; a change of that state
//! plays the §243 transition. benilla mirrors that exactly — [`GoAnim::state`] is the single source of
//! truth, written by the **three callers** the RE census pinned:
//!
//! 1. **the wire** ([`sync_wire_go_state`]) — a `GAMEOBJECT_STATE` UpdateField change. This is the
//!    door/button driver (the server flips their state over the wire) and the first-sight rest-pose seed.
//! 2. **the open-lock spell-go** ([`open_go_lid`]) — `SMSG_SPELL_GO` for an open-lock cast targeting the
//!    GO opens it (`SetGoState(ACTIVE)`). A chest's lid pops when the *Opening* cast goes off, not on the
//!    click — the faithful timing. The server never flips a chest's wire state (its `Use(CHEST)` runs
//!    scripts only; loot is spell-driven), so this — not the wire — is what opens a chest.
//! 3. **the loot-release** ([`close_go_lid`]) — the loot window closing (`CMSG_LOOT_RELEASE`) drops the
//!    state to READY, closing the lid, with no server round-trip (the client's loot-frame close handler).
//!
//! This supersedes 0244's "chests aren't animated": 0244 was right that the *wire* state never changes on
//! loot, but wrong to conclude the chest is off the machine — it runs §243 identically, just fed from
//! loot events instead of the wire. Wiring a chest to the *wire* watch alone gave it a state that never
//! changed — the "instant open" glitch; feeding it from the loot events is the fix.
//!
//! The mechanism (wow-re `object-layer.md` §243 + `scratch/go-anim-state-machine.md`, §5-VERIFIED): the
//! `GAMEOBJECT_STATE` value maps — **with no inversion**, the same polarity the sound path uses — to a
//! held rest pose, and a *change* of state plays a one-shot transition motion that settles onto the new
//! rest pose. The client keys the played sequence by its **AnimationData.dbc id** (the door-machine's
//! internal index and the debug state-name strings at `0x860850` are both stale/off-by-one — the RE's
//! central trap; we key by id):
//!
//! | wire state | rest pose (held)      | entered by motion (one-shot) |
//! |------------|-----------------------|------------------------------|
//! | 1 READY    | `0x93` Closed         | `0x92` Close  (from open)     |
//! | 0 ACTIVE   | `0x95` Opened         | `0x94` Open   (from closed)  |
//! | 2 ALT      | `0x97` Destroyed      | `0x96` Destroy / `0x98` Rebuild |
//!
//! benilla reuses the creature skeletal path wholesale: an animated GameObject is instanced with the same
//! joints + `AnimationPlayer` + graph + [`ModelAnimations`] a creature gets ([`crate::entities::attach`]),
//! but tagged with [`GoAnim`] instead of `AnimDriver`, so this driver — not `creature_anim` — owns it.
//! Clips are keyed by AnimationData.dbc id, so a resolved id becomes a clip by a scan of
//! [`ModelAnimations::clips`], exactly as `creature_anim` does. A one-shot motion held at its end frame
//! *is* the destination rest pose, so playing the motion and holding needs no explicit settle step.
//!
//! Deferred (noted, not this slice): the §243 **fallback chain** for a model that authors only the motion
//! clips (rest pose = the neighbour motion frozen at frame 0) — folded once a real door M2 dump shows it's
//! needed; the **ANIMPROGRESS one-time seek** (resync a door streamed in mid-swing — we play from frame 0);
//! and the mid-flight **reverse blend** (interrupting a half-open door). None change the common case.

use avian3d::prelude::{Collider, ColliderDisabled};
use benilla_assets::ModelAnimations;
use bevy::animation::transition::AnimationTransitions;
use bevy::animation::RepeatAnimation;
use bevy::prelude::*;
use std::time::Duration;

use crate::net::{GuidIndex, ObjectStore};
use crate::schedule::WorldStage;

/// `GO_STATE_ACTIVE` (vmangos `GOState`) — the **open** state (door swung, chest lid up). Passable.
const GO_STATE_ACTIVE: u32 = 0;
/// `GO_STATE_READY` (vmangos `GOState`) — the **closed / solid** state. A door/button blocks movement
/// only in this state; `GO_STATE_ACTIVE` (0, open) and `GO_STATE_ACTIVE_ALTERNATIVE` (2) are passable.
const GO_STATE_READY: u32 = 1;

/// Marker + client-side state for an animated GameObject (decisions 0242/0250). Instanced by
/// [`crate::entities::attach`] on an animatable GO type whose model authors sequences; driven by
/// [`drive_go_anim`]. Distinct from creatures' `AnimDriver` so the two drivers never touch one entity.
#[derive(Component, Default)]
pub(crate) struct GoAnim {
    /// Client-authoritative `GAMEOBJECT_STATE` (the binary's stored `go+0x27c`) — the single source of
    /// truth for the §243 animation + collision. Written by the three "SetGoState callers": the wire
    /// sync, the open-lock spell-go, and the loot-release. `None` until first sight.
    state: Option<u32>,
    /// The state we last *animated* to — resolves which transition motion to play next. Distinct from
    /// `state` so a caller can change the target while [`drive_go_anim`] still knows the pose we're
    /// leaving (first sight settles the resting pose **silently** — a door that streams in already open
    /// must not replay its swing).
    shown: Option<u32>,
    /// The last `GAMEOBJECT_STATE` seen on the wire, so [`sync_wire_go_state`] writes `state` only on a
    /// *genuine* wire change — a chest's wire state is constant (its lid is driven by loot events, not the
    /// wire), so an unrelated values-update (a dyn-flag, a position) must not re-close an open lid.
    last_wire: Option<u32>,
}

/// Which GameObject types get the state-driven **animated** instance (skinned lid/door + §243 sequences):
/// the door/button state machine (**DOOR(0) / BUTTON(1)**, server-driven over the wire) plus the
/// **CHEST(3)**, whose lid runs the identical machine but fed by loot events (decision 0250). A model that
/// doesn't author a skeleton/sequences still falls back to a static mesh (the attach gate also checks the
/// joints/animations exist), so a lid-less chest simply stays static. A **GOOBER(10)** flips state too but
/// also fires custom-anim + spells, so its path is unverified and left out until checked.
pub(crate) fn go_animates(type_id: i32) -> bool {
    matches!(type_id, 0 | 1 | 3)
}

/// Which types drop their collision when open (decision 0249): **DOOR(0) / BUTTON(1)** only. A door's
/// static hull can't swing with the mesh, so an open door is made walkable by disabling the collider —
/// keyed off the server's wire state, which *is* the door's real passability. A **CHEST(3)** keeps its
/// collider in every state (you don't walk through an "open" chest), so it is deliberately excluded even
/// though it now animates.
fn collision_follows_state(type_id: i32) -> bool {
    matches!(type_id, 0 | 1)
}

/// A cast launched at a GameObject (`SMSG_SPELL_GO` carrying a `TARGET_FLAG_GAMEOBJECT`), bridged from the
/// net apply layer to this module (decision 0250). [`open_go_lid`] opens the target's lid/door iff the
/// spell carries an open-lock effect and the GO is an animated type — the client's `Spell_C` open path.
#[derive(Message, Clone, Copy)]
pub(crate) struct GoLidOpen {
    pub(crate) go_guid: u64,
    pub(crate) spell_id: u32,
}

/// What to play for the current `GAMEOBJECT_STATE`: a held **rest** pose (first sight, or a state with no
/// transition), or a one-shot transition **motion** (the swing) that lands on the new rest pose.
#[derive(Clone, Copy, Debug)]
enum Play {
    /// Snap to a held pose — no swing (first sight / stream-in).
    Rest(u16),
    /// Play a transition motion once, holding its end frame (= the destination rest pose).
    Motion(u16),
}

impl Play {
    fn anim_id(self) -> u16 {
        match self {
            Play::Rest(id) | Play::Motion(id) => id,
        }
    }
}

/// The held rest-pose animation-id for a wire `GAMEOBJECT_STATE` (§243). `None` for an unmapped state.
fn rest_anim(state: u32) -> Option<u16> {
    match state {
        0 => Some(0x95), // ACTIVE  → Opened (held open)
        1 => Some(0x93), // READY   → Closed (held closed)
        2 => Some(0x97), // ALT     → Destroyed
        _ => None,
    }
}

/// The transition-motion animation-id for a `prev → cur` state change (§243), i.e. the swing. `None` when
/// the pair has no distinct motion (falls back to snapping the rest pose).
fn motion_anim(prev: u32, cur: u32) -> Option<u16> {
    match (prev, cur) {
        (1, 0) => Some(0x94), // closed → open  : Open  motion
        (0, 1) => Some(0x92), // open   → closed: Close motion
        (_, 2) => Some(0x96), // → destroyed    : Destroy
        (2, 1) => Some(0x98), // rebuild
        _ => None,
    }
}

/// Resolve the play for a state observation: first sight (`prev` None) snaps the rest pose; a change plays
/// the transition motion if one exists, else snaps the new rest pose.
fn resolve(prev: Option<u32>, cur: u32) -> Option<Play> {
    match prev {
        None => rest_anim(cur).map(Play::Rest),
        Some(p) => motion_anim(p, cur)
            .map(Play::Motion)
            .or_else(|| rest_anim(cur).map(Play::Rest)),
    }
}

/// Caller 1 (the wire, §243): track each animated GO's `GAMEOBJECT_STATE` from the wire, acting only on a
/// *genuine* wire change. This is the door/button driver (the server flips their state over the wire) and
/// the first-sight rest-pose seed for every animated GO (a chest streams in closed). Runs on the seed
/// (`Added<GoAnim>`, when attach tags the entity) and on any later descriptor delta; the `last_wire`
/// guard makes an unrelated field change (position, dyn-flags) a no-op, so a chest whose wire state is
/// constant is never re-closed by one — its lid is owned by the loot callers below.
#[allow(clippy::type_complexity)]
fn sync_wire_go_state(
    mut gos: Query<(&ObjectStore, &mut GoAnim), Or<(Changed<ObjectStore>, Added<GoAnim>)>>,
) {
    for (store, mut anim) in &mut gos {
        let wire = store.0.gameobject_state();
        if wire == anim.last_wire {
            continue; // an unrelated field changed (position, flags, dyn-flags) — not our transition
        }
        anim.last_wire = wire;
        if let Some(s) = wire {
            anim.state = Some(s);
        }
    }
}

/// Caller 2 (the open-lock spell-go): open a chest lid / locked door when a cast with an open-lock effect
/// launches at it (`SMSG_SPELL_GO` → [`GoLidOpen`]). Gated on the spell's open-lock effect (the client's
/// `[spell+0xf4] ∈ {OPEN_LOCK, OPEN_LOCK_ITEM}` test) so a plain unit spell that merely names a GO can't
/// open it; observer-safe (another player's cast opens the chest you can see, since it resolves the GO
/// guid from the packet, not our own pending cast). Sets the client state to ACTIVE(0).
fn open_go_lid(
    mut opens: MessageReader<GoLidOpen>,
    spells: Option<Res<crate::ui_action::Spells>>,
    index: Res<GuidIndex>,
    mut gos: Query<&mut GoAnim>,
) {
    for GoLidOpen { go_guid, spell_id } in opens.read().copied() {
        let is_open_lock = spells
            .as_deref()
            .and_then(|s| s.catalog.get(spell_id))
            .is_some_and(|d| d.open_lock_type.is_some());
        if !is_open_lock {
            continue;
        }
        let Some(&e) = index.0.get(&go_guid) else {
            continue;
        };
        if let Ok(mut anim) = gos.get_mut(e) {
            anim.state = Some(GO_STATE_ACTIVE);
        }
    }
}

/// Caller 3 (the loot-release): close a chest lid when its loot window closes. The client sends
/// `CMSG_LOOT_RELEASE` and immediately drops the state to READY(1) — no server round-trip. We watch the
/// open loot source guid change (any close path: the player's close, or the server's release when the last
/// item is looted) and close the lid iff the guid that just closed is an animated GO — a looted corpse or
/// creature resolves to an entity without [`GoAnim`], so `get_mut` misses it and nothing happens.
fn close_go_lid(
    loot: Res<crate::ui_loot::LootState>,
    index: Res<GuidIndex>,
    mut gos: Query<&mut GoAnim>,
    mut last_source: Local<Option<u64>>,
) {
    let current = loot.source();
    if *last_source == current {
        return;
    }
    if let Some(closed) = *last_source {
        if let Some(&e) = index.0.get(&closed) {
            if let Ok(mut anim) = gos.get_mut(e) {
                anim.state = Some(GO_STATE_READY);
            }
        }
    }
    *last_source = current;
}

/// Play the §243 sequence for a change of the client-side [`GoAnim::state`] (written by any of the three
/// callers). Mirrors the state-transition detection of [`crate::sound::gameobject`] (first sight silent),
/// but points it at the model instead of the mixer — one system owns the visual, the other the audio.
fn drive_go_anim(
    mut gos: Query<
        (
            &mut GoAnim,
            &mut AnimationPlayer,
            &mut AnimationTransitions,
            &ModelAnimations,
        ),
        Changed<GoAnim>,
    >,
) {
    for (mut go, mut player, mut tr, anims) in &mut gos {
        let Some(state) = go.state else {
            continue;
        };
        if go.shown == Some(state) {
            continue; // a caller touched us (e.g. `last_wire` bookkeeping) but the state didn't move
        }
        let prev = go.shown;
        go.shown = Some(state);
        let Some(play) = resolve(prev, state) else {
            continue;
        };
        // Resolve the id to this model's clip (keyed by AnimationData.dbc id, as `creature_anim` does). A
        // model that doesn't author the resolved id plays nothing (the §243 freeze-fallback is a follow-on)
        // — it holds its current pose rather than snapping to bind.
        let Some(clip) = anims.clips.iter().find(|c| c.anim_id == play.anim_id()) else {
            continue;
        };
        // Snap a rest pose (blend 0 — a stream-in must not swing); ease a motion over its authored blend.
        let blend = match play {
            Play::Rest(_) => 0.0,
            Play::Motion(_) => clip.blend_time.max(0.0),
        };
        let active = tr.play(&mut player, clip.node, Duration::from_secs_f32(blend));
        active.set_repeat(if clip.looping {
            RepeatAnimation::Forever
        } else {
            RepeatAnimation::Never
        });
    }
}

/// Gate a door/button's collision on its state: solid when **closed** (`GO_STATE_READY`), passable when
/// **open** (decision 0249). The model's collision is a single static hull (not bone-bound — it *can't*
/// swing with the mesh), so an open door is made walkable by disabling the collider, not by moving it —
/// the client's only option, and the faithful one. Keyed off the **wire** state (a door's real
/// passability is the server's, and it holds for an animation-less door too — no [`GoAnim`] required), and
/// scoped to the door/button types ([`collision_follows_state`]); a chest keeps its collider whatever its
/// state. Reconciles on any `ObjectStore` change (idempotent), so a door that streams in open starts
/// passable and one that streams in closed starts solid.
#[allow(clippy::type_complexity)]
fn drive_go_collision(
    mut commands: Commands,
    gos: Query<
        (Entity, &ObjectStore, Has<ColliderDisabled>),
        (With<Collider>, Changed<ObjectStore>),
    >,
) {
    for (entity, store, disabled) in &gos {
        if !collision_follows_state(store.0.gameobject_type_id()) {
            continue;
        }
        let Some(state) = store.0.gameobject_state() else {
            continue;
        };
        let solid = state == GO_STATE_READY;
        if solid && disabled {
            commands.entity(entity).remove::<ColliderDisabled>();
        } else if !solid && !disabled {
            commands.entity(entity).insert(ColliderDisabled);
        }
    }
}

/// Registration hook, mirrored on [`crate::sound::gameobject`]'s: the three state callers write
/// [`GoAnim::state`], then the animation + collision consumers act on it, after the Net drain wrote this
/// frame's descriptor deltas + queued the open-lock [`GoLidOpen`].
pub(crate) fn plugin(app: &mut App) {
    app.add_message::<GoLidOpen>().add_systems(
        Update,
        (
            (sync_wire_go_state, open_go_lid, close_go_lid),
            (drive_go_anim, drive_go_collision),
        )
            .chain()
            .in_set(WorldStage::Present),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rest_poses_match_the_verified_table() {
        assert_eq!(rest_anim(1), Some(0x93)); // READY  → Closed
        assert_eq!(rest_anim(0), Some(0x95)); // ACTIVE → Opened (not the 0x94 Open *motion*)
        assert_eq!(rest_anim(2), Some(0x97)); // ALT    → Destroyed
        assert_eq!(rest_anim(7), None);
    }

    #[test]
    fn transitions_play_the_motion_then_settle() {
        // closed → open swings the Open motion (0x94), open → closed the Close motion (0x92).
        assert!(matches!(resolve(Some(1), 0), Some(Play::Motion(0x94))));
        assert!(matches!(resolve(Some(0), 1), Some(Play::Motion(0x92))));
        // First sight snaps the rest pose, never a motion (a door streamed in open must not swing).
        assert!(matches!(resolve(None, 0), Some(Play::Rest(0x95))));
        assert!(matches!(resolve(None, 1), Some(Play::Rest(0x93))));
        // A change with no distinct motion snaps the destination rest pose.
        assert!(matches!(resolve(Some(2), 0), Some(Play::Rest(0x95))));
    }

    #[test]
    fn chest_animates_but_keeps_its_collider() {
        // A chest (3) is on the animation machine (0250) but off the collision gate (0249): you see the
        // lid move, but you never walk through an open chest.
        assert!(go_animates(3));
        assert!(!collision_follows_state(3));
        // Doors/buttons are on both.
        assert!(go_animates(0) && collision_follows_state(0));
        assert!(go_animates(1) && collision_follows_state(1));
    }
}
