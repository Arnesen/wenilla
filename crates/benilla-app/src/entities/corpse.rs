//! **The corpse OBJECT rendered as the dead body** (decision 1706) — the deferral decision 0308 §7
//! opened ("the corpse *object* rendered as the dead body — CGCorpse law") and this closes.
//!
//! A `TYPEID_CORPSE` (7) object is what you run back to: the body a released player leaves behind,
//! and the **bone pile** the server converts it into once it is reclaimed, looted, or times out.
//! Until 1706 it streamed in, spawned an entity, latched its guid for the reclaim send — and drew
//! nothing at all, because [`EntityKind`] had no variant for it and every model lane matched on
//! `Unit | Player | GameObject`.
//!
//! ## The reference's law (`CGCorpse_C` dress `0x5d6260`, model getter `0x5d6700`)
//!
//! It forks once, on `CORPSE_FIELD_FLAGS` bit 0 (`CORPSE_FLAG_BONES`), and the two halves share
//! nothing:
//!
//! - **A fresh body** (`0x5d6297`) allocates a `CCharacterComponent` at `[corpse+0x29c]` and fills
//!   it from the corpse's OWN snapshot — the seven `CORPSE_FIELD_BYTES_1/_2` bytes (`+0x69..+0x6f`)
//!   and the 19 `CORPSE_FIELD_ITEM` slots (`+0x1c + slot*4`, low 24 bits = the ItemDisplayInfo id)
//!   — through the very same compositor entry `0x478cb0` a living player is dressed by. Its model
//!   is `CORPSE_FIELD_DISPLAY_ID` down the ordinary CreatureDisplayInfo → CreatureModelData chain
//!   (`0x5d6759`), which is why the whole character pipeline (decisions 0041/0044/0045/0074) simply
//!   applies: a corpse *is* a player body wearing a wire-supplied look.
//! - **A bone pile** (`0x5d6291 jne`) builds **no component at all** — no appearance, no gear — and
//!   takes its model from race/sex instead: `0x5d670c` formats
//!   `World\Generic\PassiveDoodads\DeathSkeletons\%s%sDeathSkeleton.mdx` from `ChrRaces[race]+0x3c`
//!   (the client fileString: `"Human"`, `"Scourge"`, …) and `["Male","Female","NOSEX"][sex]`. Those
//!   16 shipped models are fully static, two-boned, one hardcoded texture each.
//!
//! Three slots are carved out of the dress loop, and none of them is a "skip if empty":
//! `0x5d6465`/`0x5d6470` skip slot 0 (head) and slot 0xe (back) when this corpse's own
//! `CORPSE_FLAG_HIDE_HELM 0x08` / `HIDE_CLOAK 0x10` are set — its own bits on its own field, and the
//! opposite instruction polarity to the player lane (wow-re `helm-cloak-hide.md` §2b) — and
//! `0x5d644e` skips slot 0x11 (ranged) unconditionally.
//!
//! ## What a corpse deliberately does NOT wear
//!
//! **No weapons.** Slots 0xf/0x10 detour at `0x5d645a`/`0x5d645f` to `0x5d649b`, which pushes the
//! raw `CORPSE_FIELD_ITEM` word as a **guid** into the object-manager lookup `0x468460` with
//! typemask 2 (`TYPEMASK_ITEM`) and skips the slot on the null return. That word is
//! `DisplayInfoID | (InventoryType << 24)` — never a guid — so the lookup cannot succeed and the
//! branch is dead. We reproduce the *outcome* (a corpse wears armour, not weapons), not the
//! mechanism: aping a dead lookup would be aping a quirk, which §3 of the contract is against.
//!
//! ## The pose
//!
//! `0x5d63de`/`0x5d6402` arm bone 0 through the shared M2 arm `0x7121a0` with **AnimationData id 6
//! (`Dead`)**, or **132 (`Drowned`)** when `0x5d6540` says so. That predicate is a **liquid** query:
//! `0x670630` over the corpse's scene node (the same call and the same `surfaceZ − subject.z` shape
//! wow-re records for the breath classifier `0x607710` and the pivot-height glide), compared against
//! the f32 at `[0x80abfc]` = **0.66666669** — i.e. a corpse submerged by more than ⅔ yd lies drowned
//! rather than merely dead. Both ids resolve through the model's own fallback table before playing:
//! `HumanMale.m2` authors neither 6 nor 132's head directly, and `AnimationData.dbc` walks
//! `Dead → Death(1)` and `Drowned → Drown(131)`.
//!
//! The clip is armed **once, seeked to its end**. A corpse object never collapses in front of you —
//! it is created after the release, already lying down — so this is the settled-pose arm the unit
//! driver already uses for a body that streamed in dead, not a replay.

use std::collections::HashMap;

use benilla_assets::{m2_url, AnimClip, ModelAnimations};
use benilla_protocol::{CorpseLook, EntityKind};
use bevy::prelude::*;

use super::display::{empty_shell, DisplayModel, ModelHandle};
use crate::creature_anim::AnimData;
use crate::net::{NetEntity, ObjectStore};

/// `AnimationData.dbc` **6 `Dead`** — the settled corpse pose (`0x5d63fe push 0x6`).
const DEAD: u16 = 6;
/// `AnimationData.dbc` **132 `Drowned`** — the submerged corpse pose (`0x5d63f7 push 0x84`).
const DROWNED: u16 = 132;
/// `[0x80abfc]`, read out of the shipped PE's `.rdata`: how far a corpse must be under a liquid
/// surface before it lies drowned rather than dead. Yards.
const DROWNED_DEPTH: f32 = 0.666_666_7;

/// The **bone-pile** body models, keyed `(race, sex)` — 16 at most, one per playable ChrRaces row ×
/// sex, and every one a static two-bone prop. Kept out of [`super::Creatures`] on purpose: that map
/// is keyed by `CreatureDisplayInfo` id, a real DBC keyspace, and a skeleton has no id in it.
#[derive(Resource, Default)]
pub(crate) struct BonesModels(pub(crate) HashMap<(u8, u8), DisplayModel>);

/// Where one corpse's body model comes from — the `0x5d6700` fork, resolved once and read by both
/// the display-build pass and the attach pass so the two can never disagree about which cache holds
/// this corpse's model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::entities) enum CorpseModel {
    /// `CORPSE_FLAG_BONES` — `<Race><Sex>DeathSkeleton`, from [`BonesModels`].
    Bones(u8, u8),
    /// A fresh body — a `CreatureDisplayInfo` id, from [`super::Creatures`] like any other body.
    Flesh(u32),
}

/// Classify a corpse entity's model source. `None` when it is not a corpse, or while its descriptor
/// has not landed (the one-frame window between the create's entity spawn and the pending-fields
/// flush), or for a bone pile whose race/sex the client data cannot name.
pub(in crate::entities) fn corpse_model(
    net: &NetEntity,
    store: Option<&ObjectStore>,
) -> Option<CorpseModel> {
    if net.kind != EntityKind::Corpse {
        return None;
    }
    let s = &store?.0;
    if s.corpse_is_bones() {
        let look = s.corpse_look()?;
        return Some(CorpseModel::Bones(look.race, look.sex.min(1)));
    }
    // The wire's display id, which for a corpse is the dead player's own body display. A corpse
    // whose create carried none has no body to build — the debug cube would be the wrong answer
    // here (nothing named a model), so it draws nothing, like the reference's null-row leg.
    net.display_id.map(CorpseModel::Flesh)
}

/// The corpse's appearance snapshot — [`ObjectFields::corpse_look`](benilla_protocol::ObjectFields::corpse_look)
/// for a corpse that is not a bone pile. `None` for anything else, which is exactly the reference's
/// gate: a bone pile builds no character component, so it has no look to read.
pub(in crate::entities) fn corpse_char_look(store: Option<&ObjectStore>) -> Option<CorpseLook> {
    let s = &store?.0;
    (!s.corpse_is_bones()).then(|| s.corpse_look())?
}

/// `World\Generic\PassiveDoodads\DeathSkeletons\<Race><Sex>DeathSkeleton.m2` — `0x5d673c`'s format
/// string `0x85fb30` with `ChrRaces[race]` column 15 and the sex table `0x856450`.
///
/// Sex is clamped to the two shipped halves: the reference's third string is `"NOSEX"`, for which no
/// skeleton file exists, and no player corpse can carry it.
fn bones_model_path(race_file: &str, sex: u8) -> String {
    let sex = if sex == 0 { "Male" } else { "Female" };
    format!("World\\Generic\\PassiveDoodads\\DeathSkeletons\\{race_file}{sex}DeathSkeleton.mdx")
}

/// Ensure a `(race, sex)` bone-pile display exists in the cache, requesting its model on first ask.
/// A race with no `ChrRaces` fileString (nothing playable — no skeleton ships) caches an empty
/// display, so the miss is asked once rather than every frame.
pub(in crate::entities) fn ensure_bones_display(
    bones: &mut BonesModels,
    races: &benilla_formats::CharCreateCatalog,
    key: (u8, u8),
    asset_server: &AssetServer,
) {
    if bones.0.contains_key(&key) {
        return;
    }
    let dm = match races.race_file(key.0) {
        Some(file) => DisplayModel {
            handle: ModelHandle::M2(asset_server.load(m2_url(&bones_model_path(file, key.1)))),
            ..empty_shell()
        },
        None => super::display::empty_display(),
    };
    bones.0.insert(key, dm);
}

/// This corpse's held pose has been armed — the arm is once-only, like the reference's, whose
/// per-frame `0x5d6850` re-arms the *same* id and so is a hold, not a replay.
#[derive(Component)]
pub(super) struct CorpsePosed;

/// Arm each freshly-attached corpse's settled pose: `Dead`, or `Drowned` when it lies more than
/// [`DROWNED_DEPTH`] under a liquid surface, seeked to the clip's end.
///
/// Runs off the attach's own output (an `AnimationPlayer` + the model's [`ModelAnimations`]) rather
/// than off a driver: a corpse has no state machine to run, and giving it an
/// [`AnimDriver`](crate::creature_anim::AnimDriver) would enrol it in the unit gait selector, which
/// has no meaning for an object with no movement, no sheath and no combat.
#[allow(clippy::type_complexity)] // one query's tuple + its Without filter
pub(super) fn pose_corpses(
    mut commands: Commands,
    mut corpses: Query<
        (
            Entity,
            &NetEntity,
            &GlobalTransform,
            &ModelAnimations,
            &mut AnimationPlayer,
            &mut bevy::animation::transition::AnimationTransitions,
        ),
        Without<CorpsePosed>,
    >,
    anim_data: Option<Res<AnimData>>,
    world: benilla_world::world_point::WorldPoint,
) {
    for (entity, net, tf, anims, mut player, mut transitions) in &mut corpses {
        if net.kind != EntityKind::Corpse {
            continue;
        }
        let wow = benilla_assets::coords::bevy_to_wow(tf.translation());
        // The reference's `0x5d6540`: ANY liquid, not water alone — it is the generic `0x670630`
        // query, the same one the breath classifier runs, so lava and slime count too.
        let submerged = world
            .liquid_at(benilla_world::world_point::Subject::Unit(entity), wow)
            .is_some_and(|hit| hit.surface_z - wow[2] > DROWNED_DEPTH);
        let want = if submerged { DROWNED } else { DEAD };
        // The model's own fallback resolution (decision 0082) — a character body authors neither
        // `Dead` nor `Drowned` directly and walks to `Death`/`Drown`.
        let catalog = anim_data.as_deref().map(|a| &a.0);
        let resolved = catalog.map_or(want, |cat| anims.resolve(want, cat).id);
        let Some(clip) = anims.find(resolved) else {
            // No clip to hold: mark it posed anyway so this doesn't re-run every frame for a body
            // whose model authors nothing (a bone pile — static, and correct as it stands).
            commands.entity(entity).insert(CorpsePosed);
            continue;
        };
        arm_settled(&mut player, &mut transitions, clip);
        debug!(
            "corpse pose: {entity} arms {} ({want} -> {resolved}) held at {:.3}s",
            if submerged { "Drowned" } else { "Dead" },
            clip.duration
        );
        commands.entity(entity).insert(CorpsePosed);
    }
}

/// Play `clip` and hold it at its end pose — the settled corpse, never the collapse.
fn arm_settled(
    player: &mut AnimationPlayer,
    transitions: &mut bevy::animation::transition::AnimationTransitions,
    clip: &AnimClip,
) {
    let active = transitions.play(player, clip.node, std::time::Duration::ZERO);
    if clip.looping {
        active.repeat();
    } else {
        active.seek_to(clip.duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 16 shipped skeleton files, spelled exactly as `0x5d673c`'s format string builds them.
    #[test]
    fn bones_paths_match_the_shipped_files() {
        assert_eq!(
            bones_model_path("Human", 0),
            "World\\Generic\\PassiveDoodads\\DeathSkeletons\\HumanMaleDeathSkeleton.mdx"
        );
        assert_eq!(
            bones_model_path("Scourge", 1),
            "World\\Generic\\PassiveDoodads\\DeathSkeletons\\ScourgeFemaleDeathSkeleton.mdx"
        );
        // `.mdx` is the authored extension the client formats; the loader swaps it for the shipped
        // `.m2` like every other model path.
        assert_eq!(
            m2_url(&bones_model_path("NightElf", 0)),
            "mpq://world/generic/passivedoodads/deathskeletons/nightelfmaledeathskeleton.m2"
        );
    }

    /// The model fork is the BONES bit, and nothing else: a bone pile carries a perfectly good
    /// display id (the server's conversion copies it verbatim) and must still resolve to a skeleton.
    #[test]
    fn bones_flag_beats_the_display_id() {
        use benilla_protocol::ObjectFields;
        let net = NetEntity {
            kind: EntityKind::Corpse,
            display_id: Some(49),
            scale: 1.0,
        };
        // race 1 (Human), sex 0, in CORPSE_FIELD_BYTES_1 bytes 1/2.
        let bytes_1 = 1u32 << 8;
        let flesh = ObjectStore(
            ObjectFields::from_pairs(&[(32, bytes_1), (33, 0)])
                .into_created(benilla_protocol::messages::ObjectType::Corpse),
        );
        assert_eq!(
            corpse_model(&net, Some(&flesh)),
            Some(CorpseModel::Flesh(49))
        );
        let bones = ObjectStore(
            ObjectFields::from_pairs(&[(32, bytes_1), (33, 0), (35, 0x01)])
                .into_created(benilla_protocol::messages::ObjectType::Corpse),
        );
        assert_eq!(
            corpse_model(&net, Some(&bones)),
            Some(CorpseModel::Bones(1, 0))
        );
        // …and a bone pile has no look to dress from, where a fresh body does.
        assert!(corpse_char_look(Some(&bones)).is_none());
        assert!(corpse_char_look(Some(&flesh)).is_some());
    }
}
