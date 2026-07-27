//! Equipment visuals (decisions 0072/0074): held items (weapons, shields, ranged), worn-armor
//! resolution, and helm/shoulder attach models on units' bodies.
//!
//! Two halves, both per-frame systems chained after [`super::attach_entity_visuals`]:
//!
//! - **Resolution** ([`resolve_equipment`]) — what should each unit hold, and where? A **creature**
//!   carries its item **display ids directly** in `UNIT_VIRTUAL_ITEM_SLOT_DISPLAY` (+ class/invType and
//!   a per-item sheath type in `UNIT_VIRTUAL_ITEM_INFO`) — no lookup. A **player** exposes only item
//!   *entries* (`PLAYER_VISIBLE_ITEM_*`); the display id / inventory type / sheath type come from the
//!   item template, resolved through the ask-once item layer ([`crate::items::Items`], `CMSG_ITEM_QUERY_SINGLE` on
//!   miss — the real client's ItemCache does exactly this; other players' inventory GUIDs are
//!   server-private, so the visible-item entry is the *only* path). Drawn-vs-stowed placement follows
//!   the unit's sheath state (`UNIT_FIELD_BYTES_2` byte 0) + the item's sheath type.
//! - **Attach** ([`attach_held_items`]) — spawn each resolved item's model as a child of the body's
//!   **attach-point joint entity** (via [`BoneAttach`], inserted by the visual attach), so it rides the
//!   hand/hip/back bone through every animation — the modern analogue of the client's attach-transform
//!   install (`0x47a380`, wow-re charactermodel). The item model itself is a static mesh: its origin
//!   *is* the grip, aligned by the attach bone's animated frame.
//!
//! Item models cache per **item display id** in [`ItemDisplays`] (a [`DisplayModel`] like creatures/
//! GameObjects use, resolved from `ItemDisplayInfo.dbc` into `Item\ObjectComponents\{Weapon,Shield}\`),
//! with the display's model texture bound to the model's runtime type-2 batches
//! ([`CharSkinSlot::Object`]).

use std::collections::HashMap;

use benilla_formats::ItemDisplayCatalog;
use benilla_protocol::EntityKind;
use bevy::mesh::MeshTag;
use bevy::prelude::*;

use crate::billboard::BillboardCard;
use crate::creature_anim::{HandGrip, NockLatch, NockedAmmo, VisualSheath, Wielded};
use crate::debug_panel::{ModelKind, ModelPart};
use crate::interior::InteriorLit;
use crate::items::Items;
use crate::model_fade::{
    fade_alpha, join_unit_appear_fade, FadeMaterials, JoinedFade, PendingAppearFade, RenderFade,
    UnitAppearFade, APPEAR_FADE_SECS,
};
use crate::net::{NetCommands, NetEntity, ObjectStore};
use crate::particles::WowParticleMaterial;

use super::{Creatures, DisplayModel, ModelHandle};
use crate::model_render::m2_url;

/// The three held-item descriptor slots (vmangos `WeaponAttackType`): 0 mainhand · 1 offhand · 2 ranged.
const HELD_SLOTS: usize = 3;

/// A player's visible-item equipment slots for the held items (vmangos `EQUIPMENT_SLOT_MAINHAND/
/// OFFHAND/RANGED` = 15/16/17 — the `PLAYER_VISIBLE_ITEM_*` blocks are indexed by equipment slot).
const PLAYER_HELD_SLOTS: [u8; HELD_SLOTS] = [15, 16, 17];

/// M2 attachment-point ids (empirically pinned on `HumanMale.m2` — decision 0072): the drawn-hand
/// points, the shield forearm, and the sheathed family the client's `0x47a070` jump table selects
/// from (`K − (mainhand)` over `K ∈ {27, 31, 33}`, const 28 for the shield).
pub(in crate::entities) mod attach_id {
    /// Left forearm — a *drawn* shield.
    pub(in crate::entities) const SHIELD: u16 = 0;
    /// Right/left shoulder (pivots at ∓0.21 Y, shoulder height) — the pauldron pair.
    pub(in crate::entities) const SHOULDER_RIGHT: u16 = 5;
    pub(in crate::entities) const SHOULDER_LEFT: u16 = 6;
    /// Head — the helm.
    pub(in crate::entities) const HELM: u16 = 11;
    /// Right hand — the drawn mainhand (and a drawn ranged weapon).
    pub(in crate::entities) const HAND_RIGHT: u16 = 1;
    /// Left hand — a drawn non-shield offhand.
    pub(in crate::entities) const HAND_LEFT: u16 = 2;
    /// Right/left shoulder-blade — the stowed two-hander family (and a stowed ranged weapon).
    pub(in crate::entities) const BACK_RIGHT: u16 = 26;
    pub(in crate::entities) const BACK_LEFT: u16 = 27;
    /// Centre back — the stowed shield.
    pub(in crate::entities) const SHIELD_BACK: u16 = 28;
    /// Lower-back pair — the stowed staff family.
    pub(in crate::entities) const BACK_LOWER_MAIN: u16 = 30;
    pub(in crate::entities) const BACK_LOWER_OFF: u16 = 31;
    /// Hip pair — the stowed one-hander family (mainhand on the *left* hip, drawn across the body).
    pub(in crate::entities) const HIP_MAIN: u16 = 32;
    pub(in crate::entities) const HIP_OFF: u16 = 33;
    /// HandArrow (35, bone 126 — flag-0x04 ignore-parent-rotation) — the in-hand nocked arrow's
    /// ONE body-bone attach (wow-re `nocked-ammo-cancel.md` §E2: `0x712f70(body, 0x23)` from
    /// `0x60ba30`/the `$BWP` BowPull handler; bow/wand only). The old Special2/Special3 (0x18/
    /// 0x19) reading is REFUTED — those are the `0x479f40` model-DIRECTORY selectors
    /// (`Item\ObjectComponents\Ammo\` vs `…\Weapon\`), never attach ids (§E1).
    pub(in crate::entities) const HAND_ARROW: u16 = 0x23;
    /// The quiver-on-back attach (wow-re §H2, byte-verified 3×): the worn ammo container's
    /// model parents at M2 attachment id 26 — the same point the stowed two-hander family uses.
    pub(in crate::entities) const QUIVER: u16 = 26;
}

/// Item-display rendering: the `ItemDisplayInfo.dbc` catalog (held models + armor region textures,
/// decisions 0072/0074) + a per-display [`DisplayModel`] cache for held items. Optional resource —
/// if the DBC fails to load, units hold nothing and armor stays unpainted.
#[derive(Resource)]
pub(crate) struct ItemDisplays {
    /// `pub(crate)`: the container feed ([`crate::ui_items`]) reads the icon column off the same
    /// parse — one catalog resource serves both the world and the bags.
    pub(crate) catalog: ItemDisplayCatalog,
    /// Keyed by `(display id, model kind)` — a helm display resolves to a different file per
    /// race/sex, a shoulder display to a left/right pair (0074 slice 3c).
    pub(super) models: HashMap<(u32, ItemModelKind), DisplayModel>,
}

#[cfg(test)]
impl ItemDisplays {
    /// The icon-only test seam: a synthetic catalog with an empty model cache — for the UI feeds
    /// that read nothing off this resource but the icon column (the action bar, the bags). They
    /// live outside this module, so `models` is not theirs to build.
    pub(crate) fn icons_for_tests(catalog: ItemDisplayCatalog) -> Self {
        ItemDisplays {
            catalog,
            models: HashMap::new(),
        }
    }
}

/// A player's worn-equipment display ids by **bodyslot − 2** (shirt, chest, belt, pants, boots,
/// wrist, gloves, tabard — the armor-composite slots, decision 0074), `0` = empty. `settled` means
/// every non-empty visible-item entry has resolved through the template cache (hit or recorded
/// miss) — the attach path waits for it so a player composites dressed, not naked-then-flicker.
/// Players only; a character-model NPC's armor ships pre-baked (decision 0060).
#[derive(Component, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Equipment {
    pub(crate) bodyslots: [u32; 8],
    /// The back slot's display id (0 = no cloak) — a geoset + runtime cape texture, no body region.
    pub(crate) cloak: u32,
    /// The head slot's display id (0 = no helm) — an attach sub-model, plus the HelmetGeosetVisData
    /// hide-masks that tuck hair/facial/ears under it (wow-re RF-0083).
    pub(crate) helm: u32,
    pub(crate) settled: bool,
}

/// The [`Equipment`] a player's attached visual was built with (its composite key's equip half).
/// [`refresh_player_looks`] diffs it against the live resolution and re-attaches on change.
#[derive(Component)]
pub(super) struct AppliedEquipment(pub(super) Equipment);

/// Marks a player whose visual was torn down by [`refresh_player_looks`] (a gear change): the
/// re-attach skips the appear-fade — changing a shirt isn't a spawn.
#[derive(Component)]
pub(super) struct Reattached;

/// Rebuild a player's visual when their worn equipment changes (decision 0074): the composite atlas
/// (and, later, the equipment geosets) are baked into the attached children, so a gear change tears
/// the visual down — every child (parts, joints, held-item roots) plus the per-instance visual
/// components — and lets [`super::attach_entity_visuals`] re-run next frame with the new
/// [`Equipment`], fade-skipped via [`Reattached`]. Players only: a creature's look never changes
/// this way (weapon swaps ride [`HeldItems`]' own diff, no teardown).
pub(super) fn refresh_player_looks(
    mut commands: Commands,
    players: Query<
        (Entity, &NetEntity, &Equipment, &AppliedEquipment),
        With<super::VisualAttached>,
    >,
) {
    for (entity, net, live, applied) in &players {
        if net.kind != EntityKind::Player || !live.settled || *live == applied.0 {
            continue;
        }
        commands
            .entity(entity)
            .despawn_related::<Children>()
            .remove::<(
                super::VisualAttached,
                AppliedEquipment,
                AnimationPlayer,
                bevy::animation::transition::AnimationTransitions,
                AnimationGraphHandle,
                benilla_assets::ModelAnimations,
                crate::creature_anim::AnimDriver,
                BoneAttach,
                HeldAttached,
            )>()
            .insert(Reattached);
    }
}

/// Player equipment slots feeding the armor composite → their bodyslot−2 index (decision 0074):
/// shirt(3) chest(4) waist(5) legs(6) feet(7) wrists(8) hands(9) tabard(18). The visible-item block
/// is indexed by equipment slot, so no invType mapping is needed on this path.
const COMPOSITE_SLOTS: [(u8, usize); 8] = [
    (3, 0),
    (4, 1),
    (5, 2),
    (6, 3),
    (7, 4),
    (8, 5),
    (9, 6),
    (18, 7),
];

/// The attach sub-model slots a unit shows this frame — the three held items (mainhand/offhand/
/// ranged) plus the helm, the shoulder pair (0074 slice 3c), and the nocked ammo, each an item
/// display + model variant + the body attachment point it hangs from. Recomputed by
/// [`resolve_equipment`] and diffed by [`attach_held_items`], so equipment/sheath changes
/// re-spawn only on an actual change.
#[derive(Component, Default, Clone, PartialEq, Eq)]
pub(super) struct HeldItems {
    slots: [Option<HeldSlot>; ATTACH_SLOTS],
}

/// Total attach sub-model slots: the 3 held + helm + shoulder L/R + the nocked ammo + the
/// worn quiver (self-only, ranged-drawn — wow-re `nocked-ammo-cancel.md` §H).
const ATTACH_SLOTS: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
struct HeldSlot {
    display: u32,
    kind: ItemModelKind,
    attach: u16,
}

/// Which of an item display's models a slot shows, and where its file lives (decision 0074 slice 3c
/// — all pinned empirically against the real MPQ listing):
/// `Item\ObjectComponents\{Weapon,Shield}\model[0]` for held items; `Shoulder\model[0]`/`model[1]`
/// for the left/right pauldron (each with its own `model_texture` column); `Head\<stem>_<Ra><S>.m2`
/// for helms — per-race/sex files, prefix by race id (Hu Or Dw Ni Sc Ta Gn Tr), M/F by sex.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum ItemModelKind {
    Weapon,
    Shield,
    ShoulderLeft,
    ShoulderRight,
    Helm {
        race: u8,
        sex: u8,
    },
    /// A nocked-ammo model ([`crate::creature_anim::NockedAmmo`]) — the missile module's shape
    /// rule (its module docs): a display with `model[0]` is a thrown weapon in `Weapon\`; one
    /// with only `model[1]` is an arrow/bullet in `Ammo\`.
    Ammo,
    /// The worn ammo container on the back (wow-re §H2): `Quiver\model[0]` + its own
    /// `model_texture[0]` skin, parented at attachment 26 while the ranged weapon is drawn.
    Quiver,
}

/// The per-instance bone-riding surface, inserted at visual attach (decision 0072): the joint
/// entities in bone order + the model's attachment points (id → bone + Bevy-space local offset).
/// Held items spawn under `joints[bone]` at the offset; future bone riders (mounts) use the same.
#[derive(Component)]
pub(crate) struct BoneAttach {
    pub(crate) joints: Vec<Entity>,
    /// Attachment id → `(bone index, bevy-space offset from the bone's bind pivot)`.
    pub(crate) points: HashMap<u16, (u16, Vec3)>,
    /// Animation-event marker 4CC → the same `(bone, offset)` shape — first record per ident
    /// (the client's `0x7130e0` first-match scan). The missile launch points: `$CSL`/`$CSR`/`$CST`
    /// (casting hand left/right/two-hand, `0x60c9b0`'s cascade) and `$BWR` (ranged release).
    pub(crate) markers: HashMap<[u8; 4], (u16, Vec3)>,
}

/// The held-item children currently spawned for a unit: the [`HeldItems`] they were built from (the
/// diff key) + the spawned root entities (one per slot, despawned recursively on change).
#[derive(Component, Default)]
pub(super) struct HeldAttached {
    applied: HeldItems,
    spawned: Vec<Entity>,
}

/// The drawn/stowed attachment point for one held slot, or `None` when the item shows nothing (empty
/// slot, sheath-type-less item while stowed, or an unresolved template).
///
/// Drawn: the unit's sheath state (`UNIT_FIELD_BYTES_2` byte 0: 0 stowed · 1 melee · 2 ranged) draws
/// the matching slots into the hands (shield → forearm). Stowed: the **item's** sheath type picks the
/// body point — 1 two-hander → back · 2 staff → lower back · 3 one-hander → hip · 4 shield → centre
/// back (mainhand takes the `K−1` side of each pair — byte-verified: `0x47a070`'s `dl != 0` is the
/// mainhand bodyslot `0xf`, wow-re `ranged-sheath-display.md`, decision 0370). A sheathed ranged
/// weapon renders **nothing** — the client detaches it rather than re-pointing it to a body bone
/// (byte-verified `0x7130a0`: a pure unlink/release, uniform across bow/gun/crossbow/thrown/wand;
/// wow-re `ranged-sheath-display.md`). Drawn ranged splits by inventory type: a **bow** rides the
/// left hand, gun/crossbow/wand/thrown the right (`0x611e10`'s invType test, same note).
pub(in crate::entities) fn placement(
    slot: usize,
    inv_type: u32,
    item_sheath: u8,
    unit_sheath: u8,
) -> Option<u16> {
    use attach_id::*;
    let shield = inv_type == 14; // INVTYPE_SHIELD
    match slot {
        // Ranged: in hand while ranged-drawn (bow left, everything else right), invisible otherwise.
        2 => (unit_sheath == 2).then_some(if inv_type == 15 {
            HAND_LEFT // INVTYPE_RANGED — bows
        } else {
            HAND_RIGHT
        }),
        // Melee/shield slots: drawn in melee sheath state, else stowed by the item's sheath type.
        0 | 1 if unit_sheath == 1 => Some(match (slot, shield) {
            (0, _) => HAND_RIGHT,
            (_, true) => SHIELD,
            (_, false) => HAND_LEFT,
        }),
        0 | 1 => match item_sheath {
            1 => Some(if slot == 0 { BACK_RIGHT } else { BACK_LEFT }),
            2 => Some(if slot == 0 {
                BACK_LOWER_MAIN
            } else {
                BACK_LOWER_OFF
            }),
            3 => Some(if slot == 0 { HIP_MAIN } else { HIP_OFF }),
            4 => Some(SHIELD_BACK),
            _ => None,
        },
        _ => None,
    }
}

/// The nocked ammo's attach point (wow-re `nocked-ammo-cancel.md` §E2/E5, byte-verified): the
/// ONE body-bone attach in the whole mechanism is HandArrow (35), fired for a **bow** once its
/// BowPull event latches `[+0xd58]&0x4000` — `nock_latched` is [`NockLatch`], driven by the real
/// `$BWP`/`$BWR` listener (`drive_nock_latch`, decision 0408). Everything else shows NO nocked
/// model: gun/crossbow hit the client's `gunXbow` early return, thrown resolves the `0x19`
/// *directory* (its own weapon-model copy) but fails the `==0x18` attach gate, and a wand's
/// Shoot has no ammo item.
fn ammo_attach(ranged_inv_type: Option<u32>, nock_latched: bool) -> Option<u16> {
    const INVTYPE_RANGED_BOW: u32 = 0x0f;
    (ranged_inv_type == Some(INVTYPE_RANGED_BOW) && nock_latched).then_some(attach_id::HAND_ARROW)
}

/// Resolve every unit's held items from its descriptor. Creatures read display/invType/sheath straight
/// from the virtual-item fields; players go visible-item entry → [`crate::items::Items`] (ask-once query on
/// a miss). Ensures each needed display id has a [`DisplayModel`] entry in [`ItemDisplays`] (built
/// by [`super::update_display_models`] once the asset loads) and writes [`HeldItems`] on change.
#[allow(clippy::type_complexity)]
pub(super) fn resolve_equipment(
    mut commands: Commands,
    units: Query<(
        Entity,
        &NetEntity,
        &ObjectStore,
        Option<&HeldItems>,
        Option<&Wielded>,
        Option<&Equipment>,
        Option<&VisualSheath>,
        Option<&crate::creature_anim::AnimDriver>,
        Option<&NockedAmmo>,
        Has<NockLatch>,
    )>,
    held: Option<ResMut<ItemDisplays>>,
    mut templates: ResMut<Items>,
    net: Res<NetCommands>,
    asset_server: Res<AssetServer>,
    // The creature display cache — a character-model NPC's helm/shoulder ids + race/sex live on its
    // display's `NpcAppearance` (CreatureDisplayInfoExtra), read here to resolve its attach models.
    creatures: Option<Res<Creatures>>,
) {
    let Some(mut held) = held else {
        return;
    };
    for (
        entity,
        net_entity,
        store,
        current,
        current_wielded,
        current_equipment,
        visual_sheath,
        driver,
        nocked,
        nock_latched,
    ) in &units
    {
        if !matches!(net_entity.kind, EntityKind::Unit | EntityKind::Player) {
            continue;
        }
        let s = &store.0;
        // Worn armor (players; decision 0074): resolve the composite slots' entries → display ids.
        // `settled` only once every non-empty entry has an answer, so the first attach composites the
        // dressed atlas directly (the template cache makes later logins instant).
        if net_entity.kind == EntityKind::Player {
            let mut eq = Equipment {
                settled: true,
                ..default()
            };
            for (slot, idx) in COMPOSITE_SLOTS {
                let Some(entry) = s.player_visible_item_entry(slot).filter(|e| *e != 0) else {
                    continue;
                };
                match templates.held(entry, &net) {
                    Some(t) => eq.bodyslots[idx] = t.display_info_id,
                    None => eq.settled = false, // asked; answer pending
                }
            }
            // The cloak (equipment slot 14): geoset + cape texture, resolved the same way.
            if let Some(entry) = s.player_visible_item_entry(14).filter(|e| *e != 0) {
                match templates.held(entry, &net) {
                    Some(t) => eq.cloak = t.display_info_id,
                    None => eq.settled = false,
                }
            }
            // The helm (equipment slot 0): attach model + the RF-0083 hide-masks (its geoset effect
            // — a hair/facial/ears change — rides the Equipment diff, so donning one re-attaches).
            if let Some(entry) = s.player_visible_item_entry(0).filter(|e| *e != 0) {
                match templates.held(entry, &net) {
                    Some(t) => eq.helm = t.display_info_id,
                    None => eq.settled = false,
                }
            }
            if current_equipment != Some(&eq) {
                commands.entity(entity).insert(eq);
            }
        }
        // The *visual* sheath state: held at the pre-transition value while the draw/stow overlay
        // plays (the weapon swaps hands at the animation's authored $SHL/$SHR moment, not at the
        // byte change — [`VisualSheath`]); else the anim layer's **client-side committed state**
        // (the setter/reconcile cache, decision 0080 — the descriptor byte plus the policy's
        // forces); else, before the driver first runs, the raw descriptor byte.
        let unit_sheath = visual_sheath
            .map(|v| v.0)
            .or_else(|| driver.and_then(|d| d.sheath_state()))
            .or_else(|| s.unit_sheath_state())
            .unwrap_or(0);
        // A player wearing a NON-character display (druid form, GM morph — decision 0695)
        // attaches no equipment sub-models at all: the reference's held/helm/shoulder attach
        // lives on the CCharacterComponent (`0x47a0c0`, wow-re charactermodel node), which only
        // a character body builds — a bear-form druid shows no weapon by construction, not by a
        // hide flag. [`Wielded`] (the anim-class pair) still resolves below: what's IN the hand
        // is independent of whether its model is displayed. The creature virtual-item path (a
        // naga's trident) is a different, unit-level mechanism and rides the Unit arms untouched.
        // An unresolved display cache entry (the one-frame window after a live swap) reads as a
        // character body — harmless: this diff re-runs every frame, and an attach needs the
        // rebuilt body's `BoneAttach` first anyway.
        let char_component = net_entity.kind != EntityKind::Player
            || net_entity
                .display_id
                .and_then(|d| creatures.as_deref()?.models.get(&d))
                .is_none_or(|dm| dm.is_character_body);
        let mut slots: [Option<HeldSlot>; ATTACH_SLOTS] = [None; ATTACH_SLOTS];
        let mut wielded = Wielded::default();
        let mut ranged_inv_type = None;
        for slot in 0..HELD_SLOTS {
            // (display id, inventory type, item sheath type, item class, item subclass) per slot.
            let resolved: Option<(u32, u32, u8, u8, u8)> = match net_entity.kind {
                EntityKind::Unit => {
                    let display = s.unit_virtual_item_display(slot as u8).filter(|d| *d != 0);
                    display.map(|d| {
                        let (class, subclass, _, inv) =
                            s.unit_virtual_item_info(slot as u8).unwrap_or((0, 0, 0, 0));
                        let sheath = s.unit_virtual_item_sheath(slot as u8).unwrap_or(0);
                        (d, inv as u32, sheath, class, subclass)
                    })
                }
                EntityKind::Player => s
                    .player_visible_item_entry(PLAYER_HELD_SLOTS[slot])
                    .filter(|e| *e != 0)
                    .and_then(|entry| templates.held(entry, &net))
                    .filter(|t| t.display_info_id != 0)
                    .map(|t| {
                        (
                            t.display_info_id,
                            t.inventory_type,
                            t.sheath as u8,
                            t.class as u8,
                            t.subclass as u8,
                        )
                    }),
                _ => None,
            };
            let Some((display, inv_type, item_sheath, class, subclass)) = resolved else {
                continue;
            };
            // The wielded weapon-class pair (decision 0073's swing/ready selectors) — what's *in*
            // the hand, independent of whether its model is displayed (a sheath-less item still
            // swings with its own class). The mainhand's sheath type picks the draw/stow one-shot
            // (Sheath 89 back / HipSheath 90 hip).
            match slot {
                0 => {
                    wielded.main = Some((class, subclass));
                    wielded.main_sheath = item_sheath;
                }
                1 => {
                    wielded.off = Some((class, subclass));
                    wielded.off_sheath = item_sheath;
                }
                // The ranged slot: the local auto-repeat idle's Load/Hold selector reads it
                // (`select::ranged_load_anim`, 0099 phase 5); the InventoryType picks the
                // nocked ammo's attach point below.
                2 => {
                    wielded.ranged = Some((class, subclass));
                    ranged_inv_type = Some(inv_type);
                }
                _ => {}
            }
            if !char_component {
                continue; // wielded resolved; the model never attaches on a non-character body
            }
            let Some(attach) = placement(slot, inv_type, item_sheath, unit_sheath) else {
                continue;
            };
            let kind = if inv_type == 14 {
                ItemModelKind::Shield
            } else {
                ItemModelKind::Weapon
            };
            ensure_item_model(&mut held, display, kind, &asset_server);
            slots[slot] = Some(HeldSlot {
                display,
                kind,
                attach,
            });
        }
        // The nocked ammo (byte-verified `0x60ba30` + the Q-E round, wow-re
        // `nocked-ammo-cancel.md` §E2/E5): the ONE body-bone attach in the whole mechanism is
        // HandArrow (35), **bow-only** — gun/crossbow (`gunXbow` early return) and thrown
        // (directory selector `0x19`, never an attach id) show NO nocked model. The [`NockedAmmo`]
        // display is written per shot from `SMSG_SPELL_START`, any caster; the attach follows the
        // client's `$BWP`/`$BWR` keyframes through [`NockLatch`] (`drive_nock_latch`, decision
        // 0408 — the arrow appears at the pull and leaves with the release).
        if let (true, Some(ammo), Some(attach)) = (
            char_component,
            nocked,
            ammo_attach(ranged_inv_type, nock_latched),
        ) {
            ensure_item_model(
                &mut held,
                ammo.display_id,
                ItemModelKind::Ammo,
                &asset_server,
            );
            slots[6] = Some(HeldSlot {
                display: ammo.display_id,
                kind: ItemModelKind::Ammo,
                attach,
            });
        }
        // The quiver on the back (wow-re `nocked-ammo-cancel.md` §H, byte-verified): while the
        // RANGED weapon is drawn — the same `0x611e10` ranged-draw transition, cleared on every
        // other ranged state — the client scans the player's OWN inventory for an ItemClass-11
        // container (Quiver/Ammo Pouch) and parents its display model at attachment 26 (no
        // transform override; no cloak conflict). Self-only by construction, exactly like the
        // client: bag slots are never replicated in 1.12, so a remote player's scan finds
        // nothing (§H1 — a two-client capture would be the clean confirmation).
        if net_entity.kind == EntityKind::Player && char_component && unit_sheath == 2 {
            let mut quiver_display = None;
            for bag in 19u8..23 {
                let entry = s
                    .player_inv_slot(bag)
                    .and_then(|g| templates.object(g))
                    .and_then(|o| o.object_entry());
                let Some(t) = entry.and_then(|e| templates.held(e, &net)) else {
                    continue;
                };
                if t.class == 11 && t.display_info_id != 0 {
                    quiver_display = Some(t.display_info_id);
                    break;
                }
            }
            if let Some(display) = quiver_display {
                ensure_item_model(&mut held, display, ItemModelKind::Quiver, &asset_server);
                slots[7] = Some(HeldSlot {
                    display,
                    kind: ItemModelKind::Quiver,
                    attach: attach_id::QUIVER,
                });
            }
        }
        // Helm + shoulders (0074 slice 3c / the npc-armor arc): attach sub-models like the held items —
        // the helm's file is per-race/sex, the shoulders a left/right model pair off one display row.
        // A **player** sources them from its visible-item entries (wire → item template → display id)
        // with race/sex off its descriptor; a **character-model NPC** sources them from its display's
        // CreatureDisplayInfoExtra head/shoulder columns with race/sex from the same row — those are
        // direct ItemDisplayInfo display ids, no template round-trip. A beast NPC (no appearance row)
        // resolves nothing here, exactly as before.
        let head_shoulder: Option<(u32, u32, u8, u8)> = match net_entity.kind {
            EntityKind::Player if char_component => {
                let race = s.unit_race().unwrap_or(1);
                let sex = s.unit_gender().unwrap_or(0).min(1);
                let mut resolve = |slot: u8| {
                    s.player_visible_item_entry(slot)
                        .filter(|e| *e != 0)
                        .and_then(|entry| templates.held(entry, &net))
                        .map(|t| t.display_info_id)
                        .filter(|d| *d != 0)
                        .unwrap_or(0)
                };
                let helm = resolve(0);
                let shoulder = resolve(2);
                Some((helm, shoulder, race, sex))
            }
            EntityKind::Unit => net_entity
                .display_id
                .and_then(|disp| creatures.as_deref()?.models.get(&disp))
                .and_then(|dm| dm.npc_appearance.as_ref())
                .map(|npc| (npc.equipment[0], npc.equipment[1], npc.race, npc.sex.min(1))),
            _ => None,
        };
        if let Some((helm, shoulder, race, sex)) = head_shoulder {
            if helm != 0 {
                let kind = ItemModelKind::Helm { race, sex };
                ensure_item_model(&mut held, helm, kind, &asset_server);
                slots[3] = Some(HeldSlot {
                    display: helm,
                    kind,
                    attach: attach_id::HELM,
                });
            }
            if shoulder != 0 {
                for (kind, attach, idx) in [
                    (ItemModelKind::ShoulderLeft, attach_id::SHOULDER_LEFT, 4),
                    (ItemModelKind::ShoulderRight, attach_id::SHOULDER_RIGHT, 5),
                ] {
                    ensure_item_model(&mut held, shoulder, kind, &asset_server);
                    slots[idx] = Some(HeldSlot {
                        display: shoulder,
                        kind,
                        attach,
                    });
                }
            }
        }
        let next = HeldItems { slots };
        // Per-hand grip: a weapon in a hand's attach point curls that hand's fingers (wow-re
        // `hand-grip-mechanism.md`) — mainhand → right (id 1), non-shield offhand → left (id 2); a
        // forearm shield (id 0) or an empty hand stays open. Drives [`HandGrip`]'s finger overlay.
        let grip = HandGrip {
            right: next
                .slots
                .iter()
                .flatten()
                .any(|s| s.attach == attach_id::HAND_RIGHT),
            left: next
                .slots
                .iter()
                .flatten()
                .any(|s| s.attach == attach_id::HAND_LEFT),
        };
        if current != Some(&next) {
            commands.entity(entity).insert((next, grip));
        }
        if current_wielded != Some(&wielded) {
            commands.entity(entity).insert(wielded);
        }
    }
}

/// Ensure `display` has a [`DisplayModel`] entry: resolve its ItemDisplayInfo row to the
/// `Item\ObjectComponents\{Weapon,Shield}\` model + its runtime object texture (the display's model
/// texture — an independently-named BLP in the same folder, never derived from the model name).
pub(in crate::entities) fn ensure_item_model(
    held: &mut ItemDisplays,
    display: u32,
    kind: ItemModelKind,
    asset_server: &AssetServer,
) {
    if held.models.contains_key(&(display, kind)) {
        return;
    }
    // Per-kind: the ObjectComponents directory, which model/texture column, and the helm's
    // per-race/sex filename (`<stem>_<Ra><S>.m2` — prefix by race id, empirically pinned, 0074).
    // Ammo follows the missile module's shape rule: a `model[0]` row is a thrown weapon
    // (`Weapon\`), a `model[1]`-only row an arrow/bullet (`Ammo\`).
    let (dir_name, col) = match kind {
        ItemModelKind::Weapon => ("Weapon", 0),
        ItemModelKind::Shield => ("Shield", 0),
        ItemModelKind::ShoulderLeft => ("Shoulder", 0),
        ItemModelKind::ShoulderRight => ("Shoulder", 1),
        ItemModelKind::Helm { .. } => ("Head", 0),
        ItemModelKind::Ammo => {
            if held
                .catalog
                .get(display)
                .is_some_and(|d| d.model[0].is_some())
            {
                ("Weapon", 0)
            } else {
                ("Ammo", 1)
            }
        }
        ItemModelKind::Quiver => ("Quiver", 0),
    };
    let dir = format!("Item\\ObjectComponents\\{dir_name}");
    let dm = match held.catalog.get(display) {
        Some(d) if d.model[col].is_some() => {
            let mut model = d.model[col].clone().unwrap();
            if let ItemModelKind::Helm { race, sex } = kind {
                // Race id 1–8 → Hu Or Dw Ni Sc Ta Gn Tr; M/F by sex. All 16 variants ship for
                // every helm stem (verified against the full MPQ listing).
                const RACE_PREFIX: [&str; 8] = ["Hu", "Or", "Dw", "Ni", "Sc", "Ta", "Gn", "Tr"];
                let prefix = RACE_PREFIX[(race.clamp(1, 8) - 1) as usize];
                let letter = if sex == 1 { 'F' } else { 'M' };
                let stem = model.strip_suffix(".m2").unwrap_or(&model).to_string();
                model = format!("{stem}_{prefix}{letter}.m2");
            }
            let disp_id = display;
            debug!(
                "item model display {disp_id} → {dir}\\{model} (tex {:?})",
                d.model_texture[col]
            );
            DisplayModel {
                handle: ModelHandle::M2(asset_server.load(m2_url(&format!("{dir}\\{model}")))),
                // The runtime object skin (bound to the model's type-2 batches) — its own basename in
                // the same folder, never derived from the model name (decision 0072's naming trap).
                object_texture: d.model_texture[col].clone(),
                dir,
                ..super::empty_shell()
            }
        }
        _ => super::empty_display(),
    };
    held.models.insert((display, kind), dm);
}

/// A held-item part's resolved appear-fade join state for this spawn, folding [`JoinedFade`] with
/// whether the part itself is fade-capable ([`super::EntityPart::fade_blend`]).
#[derive(Clone, Copy)]
enum PartFade {
    Steady,
    Pending(f32),
    Live(f32),
}

/// Spawn/refresh the held-item children for every unit whose [`HeldItems`] changed (or whose item
/// model finished loading): each slot's model parts spawn under the attach point's joint entity at
/// the attachment offset, so they ride the bone. Slots whose model is still loading are left pending
/// (the `applied` diff key keeps them un-applied) and picked up on a later pass. A part spawning while
/// the unit's own appear-fade is still in flight joins it ([`join_unit_appear_fade`]) instead of
/// popping in opaque (decision 0032 read as a per-unit property).
#[allow(clippy::type_complexity)]
pub(super) fn attach_held_items(
    mut commands: Commands,
    mut units: Query<(
        &HeldItems,
        &BoneAttach,
        Option<&mut HeldAttached>,
        Entity,
        Option<&UnitAppearFade>,
        Option<&crate::interior::BodyBakeCenter>,
    )>,
    held: Option<Res<ItemDisplays>>,
    time: Res<Time>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut particle_materials: ResMut<Assets<WowParticleMaterial>>,
    shared_light: Option<Res<crate::lighting::SharedLightBuffer>>,
) {
    let (Some(held), Some(light)) = (held, shared_light) else {
        return;
    };
    let now = time.elapsed_secs();
    for (items, bones, attached, entity, unit_fade, body_center) in &mut units {
        // A held item / helm / shoulder resolves and spawns asynchronously (a template round trip, a
        // model load) — often *after* the body has already armed its appear-fade (decision 0032 is a
        // per-unit property: the reference fades the whole unit, attachments included, as one). Read
        // the unit root's fade clock once per unit so every part spawned below joins the same ramp
        // instead of popping in opaque.
        let joined = join_unit_appear_fade(unit_fade.copied());
        // Diff against what's spawned; skip when unchanged. A slot whose model hasn't built parts yet
        // is masked out of `next` so it stays "not yet applied" and re-attaches once the parts exist.
        let mut next = items.clone();
        for slot in next.slots.iter_mut() {
            let ready = slot.is_some_and(|hs| {
                held.models
                    .get(&(hs.display, hs.kind))
                    .and_then(|dm| dm.parts.as_ref())
                    .is_some()
            });
            if !ready {
                *slot = None;
            }
        }
        if attached.as_ref().is_some_and(|a| a.applied == next) {
            continue;
        }
        // Despawn the previous children, then spawn the new set.
        let mut spawned = Vec::new();
        if let Some(a) = &attached {
            for e in &a.spawned {
                commands.entity(*e).despawn();
            }
        }
        for (slot_idx, hs) in next
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|hs| (i, hs)))
        {
            let Some(dm) = held.models.get(&(hs.display, hs.kind)) else {
                continue;
            };
            let Some(parts) = dm.parts.as_ref() else {
                continue;
            };
            let Some(&(bone, offset)) = bones.points.get(&hs.attach) else {
                // Body model has no such attach point (a non-character skeleton) — hold nothing.
                continue;
            };
            let Some(&joint) = bones.joints.get(bone as usize) else {
                continue;
            };
            let root = commands
                .spawn((Transform::from_translation(offset), Visibility::default()))
                .id();
            commands.entity(joint).add_child(root);
            // The engine-drawn bowstring (0408 §G2) — for the drawn BOW only: the ranged slot's
            // left-hand fork (a bow is the one ranged weapon placed in HAND_LEFT; the client
            // registers the string callback bow-only, from the ranged-draw path). The `$WTT`/
            // `$WTB` anchors alone are NOT the gate — they are generic weapon-TRAIL begin/end
            // markers (wow-re w2d2: `WTBT`/`WTTT`, the swing-trail vertex build) that melee
            // weapons author too; keying on their presence drew a phantom "bowstring" chord
            // across the Whirlwind Axe's blade tips (decision 0531).
            if slot_idx == 2 && hs.attach == attach_id::HAND_LEFT {
                if let Some([top, bottom]) = dm.string_anchors {
                    commands.entity(root).insert(crate::bowstring::Bowstring {
                        owner: entity,
                        top: top.1,
                        bottom: bottom.1,
                    });
                }
            }
            // Billboard batches (the torch's glow card) collected for the world-root card spawn
            // below — as plain children they'd render at the item root (the grip), not the
            // authored pivot (the torch head). Decision 0153.
            let mut billboard_parts = Vec::new();
            commands.entity(root).with_children(|parent| {
                for part in parts {
                    if let Some(info) = &part.billboard {
                        billboard_parts.push((info.clone(), part));
                        continue;
                    }
                    // Per-part join decision: `joined` (the unit's clock) combined with whether this
                    // part is fade-capable at all (`fade_blend` — WMO-display parts have none, though
                    // held items are always M2). Steady = spawn opaque now, exactly as before this fix.
                    let effective = match (joined, part.fade_blend.is_some()) {
                        (_, false) => PartFade::Steady,
                        (JoinedFade::Steady, true) => PartFade::Steady,
                        (JoinedFade::Pending { since }, true) => PartFade::Pending(since),
                        (JoinedFade::Live { started }, true) => PartFade::Live(started),
                    };
                    let (init_mat, tag_alpha) = match effective {
                        PartFade::Steady => (part.material.clone(), 1.0),
                        PartFade::Pending(_) => (part.fade_blend.clone().unwrap(), 0.0),
                        // Compute the *current* alpha (not 0) so a late joiner doesn't flash invisible
                        // for a frame before `apply_render_fade` catches up — it's already mid-ramp.
                        PartFade::Live(started) => (
                            part.fade_blend.clone().unwrap(),
                            fade_alpha(0.0, 1.0, (now - started) / APPEAR_FADE_SECS),
                        ),
                    };
                    let mut child = parent.spawn((
                        Mesh3d(part.mesh.clone()),
                        MeshMaterial3d(init_mat),
                        Transform::default(),
                        ModelPart {
                            kind: ModelKind::Creature,
                            blend: part.blend,
                        },
                        // The portrait booth mirrors this rider ([`crate::portrait`]): steady
                        // material (not the fade twin) + where it sits, so the bake can seat it at
                        // the bone's bind-pose global (the booth spawns no skeleton).
                        crate::portrait::PortraitRider {
                            static_mesh: part.mesh.clone(),
                            material: part.material.clone(),
                            bone,
                            offset,
                        },
                    ));
                    // Interior-light parity with the body: both material variants + a MeshTag, so the
                    // classifier relights the weapon inside a WMO room like it does its wielder. While
                    // joining the unit's appear-fade the tag carries its alpha instead — the classifier
                    // yields via its own `Without<RenderFade>`/`Without<PendingAppearFade>` filter, same
                    // as a body part.
                    if part.material_interior.is_some() {
                        // Anchored at the WEARER's root: an equipped item M2 aliases its wearer's
                        // light collector by pointer (`[item+0x3b8]=[wearer+0x3b8]`, `0x718960` —
                        // wow-re `unit-light-combine-storm.md`), so it never runs its own
                        // classify/footprint at the carried position. The animating hand joint
                        // once anchored these, and a swing alone could trip the resample gate and
                        // split the shield's light from the body's (director-caught, 2026-07-13).
                        // The fold reference is the wearer's BODY centre for the same reason.
                        let lit_kind = match &part.material_interior_bake {
                            Some(bake) => crate::interior::InteriorKind::Bake {
                                material: bake.clone(),
                                center: body_center.map_or(dm.bake_center_local, |c| c.0),
                            },
                            None => crate::interior::InteriorKind::Matte,
                        };
                        child.insert((
                            MeshTag(crate::mesh_tag::alpha_bits(tag_alpha)),
                            InteriorLit::new(entity, lit_kind, part.material.clone()),
                        ));
                    }
                    // `FadeMaterials` is persistent bookkeeping (self-avatar zoom fade, decision 0032's
                    // despawn-fade-out), not tied to whether *this* spawn happens to join an in-flight
                    // unit fade — attach it whenever the part is fade-capable at all, steady or not.
                    if let Some(blend) = &part.fade_blend {
                        child.insert(FadeMaterials {
                            cutout: part.material.clone(),
                            blend: blend.clone(),
                            bake_blend: part.material_interior_bake_blend.clone(),
                        });
                    }
                    match effective {
                        PartFade::Steady => {}
                        PartFade::Pending(since) => {
                            child.insert(PendingAppearFade {
                                cutout: part.material.clone(),
                                blend: part.fade_blend.clone().unwrap(),
                                since,
                            });
                        }
                        PartFade::Live(started) => {
                            child.insert(RenderFade {
                                started,
                                duration: APPEAR_FADE_SECS,
                                from: 0.0,
                                to: 1.0,
                                cutout: part.material.clone(),
                                blend: part.fade_blend.clone().unwrap(),
                            });
                        }
                    }
                }
            });
            // The billboard cards (decision 0153): world-root entities FOLLOWING `root` — it sits
            // at the attach offset under the hand joint, is fresh per attach, and despawns on a
            // gear change, so the card's lifecycle and frame both come for free (same owner
            // contract as the item's emitters below).
            for (info, part) in billboard_parts {
                commands.spawn((
                    Mesh3d(part.mesh.clone()),
                    MeshMaterial3d(part.material.clone()),
                    Transform::default(),
                    ModelPart {
                        kind: ModelKind::Creature,
                        blend: part.blend,
                    },
                    BillboardCard::following(&info, root),
                ));
            }
            // The item's own particle emitters — the held torch's flame (0130 phase 4: the same
            // owner-follow rider as doodad emitters). `root` sits at the attach offset under the
            // hand joint and its frame IS the item's model frame; a held item spawns no skeleton,
            // so the rest pose applies and no pivot rebase is needed — the flame burns at its
            // authored spot (the torch tip) and follows the hand through the swing. Free entities:
            // they self-despawn with `root` via the owner contract (gear change, unit despawn).
            // The spawn transform only seeds the flicker RNG (the owner overwrites it every frame)
            // — root's entity bits de-sync two torch-bearers standing side by side.
            let rng_seed = Transform::from_translation(Vec3::splat(root.to_bits() as f32));
            for em in &dm.emitters {
                crate::particles::spawn_emitter(
                    &mut commands,
                    &mut meshes,
                    &mut particle_materials,
                    &light,
                    em,
                    rng_seed,
                    Some((root, [0.0; 3])),
                    Some(root), // a held item is an attached model — the flame fans with the swing
                    Some(root), // whole-model owner: anchor == owner, the plain carry
                    // A held item spawns no rig; its emitters run the item model's own slot-0
                    // loop on the spawn clock (the torch burns always — the doodad law).
                    crate::particles::EmitClock::Pinned,
                );
            }
            // The item's own M2 point light — **the held torch's glow** (decision 0016's law on the
            // entity half of the scene; see `super::carried_light`). `Club_1H_Torch_A_01.m2` — the
            // torch every torch-bearing NPC carries — authors exactly one: a warm
            // `(0.467, 0.290, 0.133) × 3.0` point light 0.58 yd up the shaft. It rides `root` like
            // the emitters and for the same reason (the item poses at rest, so its model space IS
            // the bone-local frame), which walks it through the hand's swing; the fence rails and
            // grass around the bearer then gather it like any other scene point light.
            super::spawn_carried_lights(&mut commands, &dm.lights, root, |_| None);
            // The item's ribbon trails (weapon enchant streaks): ride the item root — a held item
            // poses at rest, so the bone-local origin is model-space (no pivot rebase). A held item
            // rests in Stand (anim 0): a thrown weapon's trail is keyed dark there, so the flight
            // ribbon never shows in the hand — it lights only on the InFlight missile.
            for rb in &dm.ribbons {
                crate::ribbons::spawn_ribbon(
                    &mut commands,
                    &mut meshes,
                    &mut particle_materials,
                    &light,
                    rb,
                    root,
                    false,
                    Some(0),
                );
            }
            debug!(
                "held attach: unit {entity} display {} → attach {} (bone {bone}, {} parts)",
                hs.display,
                hs.attach,
                parts.len()
            );
            spawned.push(root);
        }
        let applied = HeldAttached {
            applied: next,
            spawned,
        };
        match attached {
            Some(mut a) => *a = applied,
            None => {
                commands.entity(entity).insert(applied);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ammo_attach, attach_id, placement};

    /// The ranged slot's whole placement law: in hand only while ranged-drawn (state 2) — bow
    /// (INVTYPE_RANGED 15) to the left hand, gun/crossbow/wand/thrown (RANGEDRIGHT 26 / THROWN 25)
    /// to the right — and invisible in every other sheath state, regardless of the item's own
    /// sheath type.
    #[test]
    fn ranged_slot_hidden_unless_ranged_drawn() {
        for inv_type in [15, 25, 26] {
            for item_sheath in 0..=4u8 {
                assert_eq!(placement(2, inv_type, item_sheath, 0), None);
                assert_eq!(placement(2, inv_type, item_sheath, 1), None);
                let drawn = if inv_type == 15 {
                    attach_id::HAND_LEFT
                } else {
                    attach_id::HAND_RIGHT
                };
                assert_eq!(placement(2, inv_type, item_sheath, 2), Some(drawn));
            }
        }
    }

    /// Melee slots keep the sheath-type stow table (unchanged by the ranged rule).
    #[test]
    fn melee_slots_stow_by_item_sheath_type() {
        assert_eq!(placement(0, 17, 1, 0), Some(attach_id::BACK_RIGHT));
        assert_eq!(placement(0, 21, 3, 0), Some(attach_id::HIP_MAIN));
        assert_eq!(placement(1, 14, 4, 0), Some(attach_id::SHIELD_BACK));
        assert_eq!(placement(0, 21, 3, 1), Some(attach_id::HAND_RIGHT));
    }

    /// The nocked-ammo attach law (`0x60ba30`, wow-re `nocked-ammo-cancel.md` §E2, decision
    /// 0408): HandArrow (35) is the ONE attach, bow-only, gated on the `$BWP` nock latch.
    /// Gun/crossbow/thrown never attach a nocked model.
    #[test]
    fn ammo_attach_hands_the_volleying_bow_arrow_and_nothing_else() {
        assert_eq!(ammo_attach(Some(0x0f), true), Some(attach_id::HAND_ARROW)); // bow, volleying
        assert_eq!(ammo_attach(Some(0x0f), false), None); // bow, idle — pre-BowPull, no attach
        assert_eq!(ammo_attach(Some(0x19), true), None); // thrown — directory 0x19, never an id
        assert_eq!(ammo_attach(Some(0x1a), true), None); // gun/xbow/wand — the gunXbow early return
        assert_eq!(ammo_attach(None, true), None); // no ranged record
    }
}
