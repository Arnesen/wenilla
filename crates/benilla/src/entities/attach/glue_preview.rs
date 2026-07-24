//! The glue-preview part builder (decisions 0423 + 0465) — the entities-side half of the glue
//! booth. It lives under `attach` so it can reuse the character body pipeline directly
//! (`char_skin::build_char_skin_materials`, the geoset filter, the [`super::super::EntityPart`]
//! source, the item-display models) without widening any of it.
//!
//! When a glue screen's [`GluePreview`] look changes, this assembles the character the world's own
//! way — displayId → the shared [`Creatures`] display cache → geoset filter → the composited body
//! over a hand-built [`CharLook`] — into a flat list of [`PreviewPart`]s in [`GluePreviewBake`].
//! Both looks dress from the **same** enum-shaped `[CharEnumItem; 19]`: a **Select** look carries
//! its roster record verbatim; a **Create** look resolves the (race, class, sex) starting outfit
//! (CharStartOutfit, decision 0527) into that array — so the create preview is geared exactly like a
//! select body (the ref's create screen dresses the model, not the underwear-only body decision 0423
//! assumed). Both then get the armor-region composite + real equipment geosets, the cloak, and the
//! attach riders — helm (per-race/sex model, hide-helm honored), shoulders, and the weapons **held
//! in the hands** (the byte-verified select build — wow-re `glue-select-model.md`, folded back over
//! 0465's sheathed INTERIM). The portrait module's `sync_glue_booth` then re-lights and bakes
//! them (it owns the *booth*; this owns the *assembly*). The body displayId is added to
//! `update_display_models`'s want-list (in [`super::super`]) so its model builds with no wire
//! entity; item models register through the same [`ItemDisplays`] want the world path uses.

use benilla_formats::CharSkinSlot;
use benilla_protocol::{CharEnumItem, CHARACTER_FLAG_HIDE_CLOAK, CHARACTER_FLAG_HIDE_HELM};
use bevy::prelude::*;

use crate::assets::WorldAssets;
use crate::lighting::SharedLightBuffer;
use crate::portrait::{
    GlueLook, GluePreview, GluePreviewBake, PreviewBillboard, PreviewPart, PreviewRider,
};
use crate::terrain::WowModelMaterial;

use super::super::equipment::{attach_id, ensure_item_model, placement, ItemModelKind};
use super::super::{
    CharCreate, Characters, Creatures, EntityMaterials, EntityPart, ItemDisplays, SkinComposites,
    SkinSections,
};
use super::char_skin::{equip_geosets, BodySkin, CharLook};

/// The enum equipment slots that paint the armor-region composite, in the bodyslot order
/// (shirt · chest · belt · pants · boots · wrist · gloves · tabard → bodyslot 2–9):
/// equipment-slot indices 3, 4, 5, 6, 7, 8, 9, 18.
const ENUM_BODYSLOTS: [usize; 8] = [3, 4, 5, 6, 7, 8, 9, 18];
/// The enum's helm / shoulder / cloak slots.
const ENUM_HELM: usize = 0;
const ENUM_SHOULDER: usize = 2;
const ENUM_CLOAK: usize = 14;
/// Main hand · off hand · ranged, in [`placement`]'s held-triple slot order (0/1/2).
const ENUM_HELD: [usize; 3] = [15, 16, 17];

/// Map an item's `InventoryType` to its equipment-slot index in the enum-shaped `[_; 19]` array
/// (`EQUIPMENT_SLOT_*`), so a CharStartOutfit item (decision 0527) lands where the Select pipeline
/// reads it — the render slots the compositor paints (helm 0 · shoulder 2 · shirt 3 · chest 4 ·
/// waist 5 · legs 6 · feet 7 · wrist 8 · hands 9 · back 14 · tabard 18, wow-re `glue-select-model.md`)
/// plus the held triple (main 15 · off 16 · ranged 17). `None` for a non-worn / non-rendered type
/// (bags, ammo, quiver, relic, and NON_EQUIP consumables — the outfit's food + hearthstone).
fn equip_slot(inv_type: u8) -> Option<usize> {
    Some(match inv_type {
        1 => 0,             // HEAD
        2 => 1,             // NECK (not rendered; its slot for completeness)
        3 => 2,             // SHOULDERS
        4 => 3,             // BODY (shirt)
        5 | 20 => 4,        // CHEST / ROBE
        6 => 5,             // WAIST
        7 => 6,             // LEGS
        8 => 7,             // FEET
        9 => 8,             // WRISTS
        10 => 9,            // HANDS
        16 => 14,           // BACK (cloak)
        13 | 17 | 21 => 15, // WEAPON / TWOHAND / WEAPONMAINHAND → main hand
        14 | 22 | 23 => 16, // SHIELD / WEAPONOFFHAND / HOLDABLE → off hand
        15 | 25 | 26 => 17, // RANGED / THROWN / RANGEDRIGHT (wand, gun) → ranged
        19 => 18,           // TABARD
        _ => return None, // 0 NON_EQUIP · 11 FINGER · 12 TRINKET · 18 BAG · 24 AMMO · 28 RELIC · …
    })
}

/// Assemble the preview part (+ rider) lists when the glue-screen look changes. Runs in the
/// entity-visuals chain (after `update_display_models` has built the body model from the
/// want-list), so a fresh look's models are ready within a frame or two; until then it leaves the
/// bake untouched and retries. A bare yaw change never reaches here (the booth handles it).
#[allow(clippy::too_many_arguments)]
pub(in crate::entities) fn build_glue_preview(
    preview: Res<GluePreview>,
    mut bake: ResMut<GluePreviewBake>,
    creatures: Option<Res<Creatures>>,
    characters: Option<Res<Characters>>,
    char_create: Option<Res<CharCreate>>,
    mut displays: Option<ResMut<ItemDisplays>>,
    sections: Option<Res<SkinSections>>,
    world_assets: Option<Res<WorldAssets>>,
    shared_light: Option<Res<SharedLightBuffer>>,
    mut images: ResMut<Assets<Image>>,
    mut skin_composites: ResMut<SkinComposites>,
    asset_server: Res<AssetServer>,
    mut materials: ResMut<Assets<WowModelMaterial>>,
    mut entity_mats: ResMut<EntityMaterials>,
    // Change tracking: the look we last emitted a bake for, and whether that bake succeeded (so a
    // look whose models weren't ready yet keeps retrying until they build).
    mut state: Local<PreviewState>,
) {
    // A new look resets the retry latch.
    if state.last_look != preview.look {
        state.last_look = preview.look;
        state.built = false;
    }
    if state.built {
        return;
    }

    // No look → clear the booth once.
    let Some(look) = preview.look else {
        if bake.look.is_some() {
            *bake = GluePreviewBake {
                revision: bake.revision + 1,
                ..default()
            };
        }
        state.built = true;
        return;
    };

    // displayId → the shared display cache. The want-list in `update_display_models` has already
    // asked for this display; wait (retry, `built` stays false) until its model + parts are built.
    let (Some(creatures), Some(char_create)) = (creatures.as_deref(), char_create.as_deref())
    else {
        return;
    };
    let (race, sex) = look.body();
    let Some(display_id) = char_create.0.body_display(race, sex) else {
        state.built = true; // a non-playable race can't resolve — nothing to show, don't spin
        return;
    };
    let Some(dm) = creatures.models.get(&display_id) else {
        return; // model not built yet — retry next frame
    };
    let Some(parts) = dm.parts.as_deref() else {
        return; // asset still loading
    };

    // The body appearance is identical in shape for both looks (only its source struct differs).
    let (skin, face, hair_style, hair_color, facial_hair) = match look {
        GlueLook::Create(l) => (l.skin, l.face, l.hair_style, l.hair_color, l.facial_hair),
        GlueLook::Select(l) => (l.skin, l.face, l.hair_style, l.hair_color, l.facial_hair),
    };
    let char_look = CharLook {
        race,
        sex,
        skin,
        hair_style,
        hair_color,
        facial_hair,
        body: BodySkin::Composite { face },
    };

    // The equipment, unified as an enum-shaped `[CharEnumItem; 19]` + display flags: a Select look
    // carries its roster record verbatim; a Create look resolves the (race, class, sex) starting
    // outfit (CharStartOutfit, decision 0527) into the same slot array — each worn item at the
    // equipment slot its InventoryType maps to. Display ids are ItemDisplayInfo ids in both cases
    // (no template hop). A level-1 outfit carries no helm/cloak and there is no hide flag, so the
    // create preview's flags are 0 — the create body is dressed exactly like a geared select body.
    let (equipment, flags): ([CharEnumItem; 19], u32) = match look {
        GlueLook::Create(l) => {
            let mut equipment = [CharEnumItem::default(); 19];
            for item in char_create.0.start_outfit(l.race, l.class, l.sex) {
                if let Some(slot) = equip_slot(item.inv_type) {
                    equipment[slot] = CharEnumItem {
                        display_id: item.display_id,
                        inventory_type: item.inv_type,
                    };
                }
            }
            (equipment, 0)
        }
        GlueLook::Select(l) => (l.equipment, l.flags),
    };
    let bodyslots = ENUM_BODYSLOTS.map(|slot| equipment[slot].display_id);
    let cloak = if flags & CHARACTER_FLAG_HIDE_CLOAK != 0 {
        0
    } else {
        equipment[ENUM_CLOAK].display_id
    };
    let helm = if flags & CHARACTER_FLAG_HIDE_HELM != 0 {
        0
    } else {
        equipment[ENUM_HELM].display_id
    };
    let held = held_wants(&equipment, helm, race, sex);
    // Per-hand grip `[right, left]`: a hand whose attach point holds a weapon curls into `HandsClosed`
    // (wow-re `hand-grip-mechanism.md` — the ref's paperdoll rule `0x5059a0`: attach-point occupancy, per
    // hand). Resolved here because the flat rider list drops each held item's attach id. A shield sits on
    // the forearm (Shield attach, id 0), never a hand point, so its hand stays open.
    let grip = [
        held.iter().any(|w| w.attach == attach_id::HAND_RIGHT),
        held.iter().any(|w| w.attach == attach_id::HAND_LEFT),
    ];

    // The rider models: register each want with [`ItemDisplays`] (built by
    // `update_display_models` once the M2 loads) and gate the bake on ALL of them being ready —
    // a geared character popping in piecewise would flicker on every selection change.
    if let Some(d) = displays.as_deref_mut() {
        for w in &held {
            ensure_item_model(d, w.display, w.kind, &asset_server);
        }
        if !held.iter().all(|w| {
            d.models
                .get(&(w.display, w.kind))
                .and_then(|m| m.parts.as_ref())
                .is_some()
        }) {
            return; // an item model is still loading — retry next frame
        }
    }

    // The dressed geoset set: the worn armor's geoset groups + the cloak + the helm's hide-masks
    // (RF-0083), exactly the world path's selection (the shared helper).
    let eg = equip_geosets(displays.as_deref(), &bodyslots, cloak, helm);
    let visible = characters.as_deref().map(|c| {
        c.0.visible_geosets(race, sex, char_look.hair_style, char_look.facial_hair, &eg)
    });
    let char_mats = super::char_skin::build_char_skin_materials(
        &char_look,
        bodyslots,
        cloak,
        displays.as_deref(),
        sections.as_deref(),
        world_assets.as_deref(),
        shared_light.as_deref(),
        parts,
        &mut images,
        &mut skin_composites.0,
        &asset_server,
        &mut materials,
        &mut entity_mats.0,
    );

    // Whether this look shows the part's geoset (billboard or body alike).
    let shows = |p: &EntityPart| {
        visible
            .as_ref()
            .is_none_or(|vis| vis.contains(&p.geoset_id))
    };
    let preview_parts: Vec<PreviewPart> = parts
        .iter()
        .filter(|p| p.billboard.is_none() && shows(p))
        .map(|p| PreviewPart {
            static_mesh: p.mesh.clone(),
            skinned_mesh: p.skinned_mesh.clone(),
            material: steady_material(p, &char_mats).unwrap_or_else(|| p.material.clone()),
        })
        .collect();

    // The eye-glow (and any character billboard) — the undead/night-elf glowing eyes. The world
    // splits these into camera-facing cards; the booth is a separate camera, so it can't ride that
    // system — carry them through to the booth spawn, which seats each on its billboard bone's joint
    // and re-faces it ([`crate::portrait::booth::face_booth_billboards`]). Geoset-gated like the body
    // parts: the undead glow rides geoset 302 (the glowing "Features" variants), faithful to the DBC.
    // The batch is fullbright (M2 unlit), so it keeps its built material — no char-skin swap.
    let preview_billboards: Vec<PreviewBillboard> = parts
        .iter()
        .filter(|p| shows(p))
        .filter_map(|p| {
            let info = p.billboard.as_ref()?;
            Some(PreviewBillboard {
                mesh: p.mesh.clone(),
                material: p.material.clone(),
                bone: info.bone,
                kind: info.kind,
            })
        })
        .collect();

    // The riders: each want's item-model parts seated at the body's attach point (bone + offset),
    // the world path's `PortraitRider` shape.
    let attach_point = |id: u16| dm.attachments.iter().find(|a| a.id == id);
    let mut riders = Vec::new();
    if let Some(d) = displays.as_deref() {
        for w in &held {
            let Some(point) = attach_point(w.attach) else {
                continue; // the body has no such attach point — hold nothing (world-path rule)
            };
            let Some(item_parts) = d
                .models
                .get(&(w.display, w.kind))
                .and_then(|m| m.parts.as_deref())
            else {
                continue; // gated above; belt-and-braces
            };
            for p in item_parts.iter().filter(|p| p.billboard.is_none()) {
                riders.push(PreviewRider {
                    mesh: p.mesh.clone(),
                    material: p.material.clone(),
                    bone: point.bone,
                    offset: point.offset,
                });
            }
        }
    }

    debug!(
        "glue preview: race {race} sex {sex} display {display_id} → {} parts, {} riders, {} glow ({})",
        preview_parts.len(),
        riders.len(),
        preview_billboards.len(),
        match look {
            GlueLook::Create(_) => "create",
            GlueLook::Select(_) => "select",
        },
    );
    *bake = GluePreviewBake {
        look: Some(look),
        display_id,
        parts: preview_parts,
        riders,
        billboards: preview_billboards,
        grip,
        revision: bake.revision + 1,
    };
    state.built = true;
}

/// One wanted rider model: which display + model kind, and the body attach point it seats on.
struct HeldWant {
    display: u32,
    kind: ItemModelKind,
    attach: u16,
}

/// The select character's attach-model wants from its enum record: helm + the shoulder pair, and
/// the weapons **held in the hands** — the byte-verified select build (wow-re
/// `glue-select-model.md` TU-A, folding back decision 0465's sheathed INTERIM): the builder
/// forces SheatheType 0 (`0x472c8c`), so the sheath resolver never yields a stow point and the
/// hand code runs — mainhand to HandRight (attach 1), an off-hand weapon to HandLeft (2), a
/// shield to the Shield point (0) — and the RANGED slot is skipped outright (`0x472bfe`).
/// [`placement`]'s melee-drawn arm (`unit_sheath = 1`) is exactly that law, ranged-skip included
/// (its ranged arm only yields while ranged-drawn). A held off-hand frill (INVTYPE_HOLDABLE)
/// rides the same hand law — the verdict enumerates weapons/shields only, and the world's held
/// placement is the natural reading for the rest.
fn held_wants(equipment: &[CharEnumItem; 19], helm: u32, race: u8, sex: u8) -> Vec<HeldWant> {
    let mut wants = Vec::new();
    if helm != 0 {
        wants.push(HeldWant {
            display: helm,
            kind: ItemModelKind::Helm { race, sex },
            attach: attach_id::HELM,
        });
    }
    let shoulder = equipment[ENUM_SHOULDER].display_id;
    if shoulder != 0 {
        for (kind, attach) in [
            (ItemModelKind::ShoulderLeft, attach_id::SHOULDER_LEFT),
            (ItemModelKind::ShoulderRight, attach_id::SHOULDER_RIGHT),
        ] {
            wants.push(HeldWant {
                display: shoulder,
                kind,
                attach,
            });
        }
    }
    for (held_slot, enum_slot) in ENUM_HELD.into_iter().enumerate() {
        let item = equipment[enum_slot];
        if item.display_id == 0 {
            continue;
        }
        let Some(attach) = placement(held_slot, item.inventory_type as u32, 0, 1) else {
            continue;
        };
        let kind = if item.inventory_type == 14 {
            ItemModelKind::Shield
        } else {
            ItemModelKind::Weapon
        };
        wants.push(HeldWant {
            display: item.display_id,
            kind,
            attach,
        });
    }
    wants
}

/// The steady, world-lit material a character part wears — the body atlas (at the part's sidedness),
/// the hair texture, the cape, or the extra-skin fur — mirroring the attach swap ([`super`]) but
/// keeping only the steady variant (the booth wants no fade / interior twin). `None` for a
/// non-character part (it keeps its built material).
fn steady_material(
    part: &EntityPart,
    char_mats: &super::char_skin::CharSkinMaterials,
) -> Option<Handle<WowModelMaterial>> {
    let quint = match part.char_slot {
        Some(CharSkinSlot::Body) => {
            let (single, two) = char_mats.0.as_ref()?;
            if part.two_sided {
                two
            } else {
                single
            }
        }
        Some(CharSkinSlot::Hair) => char_mats.1.as_ref()?,
        Some(CharSkinSlot::Object) => char_mats.2.as_ref()?,
        Some(CharSkinSlot::SkinExtra) => {
            let (single, two) = &char_mats.3;
            (if part.two_sided { two } else { single }).as_ref()?
        }
        None => return None,
    };
    Some(quint.0.clone())
}

/// [`build_glue_preview`]'s per-run memory.
#[derive(Default)]
pub(in crate::entities) struct PreviewState {
    last_look: Option<GlueLook>,
    built: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The InventoryType → equipment-slot map lands each worn kind where the Select pipeline reads
    /// it, and drops the non-rendered kinds. Pinned against the real Human Warrior recruit set
    /// (byte-verified in decision 0527): shirt→body(3), pants→legs(6), boots→feet(7), the mainhand
    /// sword→15, the shield→offhand(16); the outfit's food/hearthstone (inv 0) drop out.
    #[test]
    fn equip_slot_maps_the_recruit_set_and_drops_non_worn() {
        assert_eq!(equip_slot(4), Some(ENUM_BODYSLOTS[0])); // shirt (BODY) → slot 3
        assert_eq!(equip_slot(7), Some(ENUM_BODYSLOTS[3])); // pants (LEGS) → slot 6
        assert_eq!(equip_slot(8), Some(ENUM_BODYSLOTS[4])); // boots (FEET) → slot 7
        assert_eq!(equip_slot(21), Some(ENUM_HELD[0])); // WEAPONMAINHAND → main hand (15)
        assert_eq!(equip_slot(14), Some(ENUM_HELD[1])); // SHIELD → off hand (16)
        assert_eq!(equip_slot(20), Some(ENUM_BODYSLOTS[1])); // ROBE shares the chest slot (4)
        assert_eq!(equip_slot(1), Some(ENUM_HELM)); // HEAD → helm slot
        assert_eq!(equip_slot(3), Some(ENUM_SHOULDER)); // SHOULDERS → shoulder slot
        assert_eq!(equip_slot(16), Some(ENUM_CLOAK)); // BACK → cloak slot
        assert_eq!(equip_slot(19), Some(ENUM_BODYSLOTS[7])); // TABARD → slot 18
        assert_eq!(equip_slot(15), Some(ENUM_HELD[2])); // RANGED → ranged slot (17)
                                                        // Non-worn / non-rendered kinds drop out entirely.
        for inv in [0u8, 11, 12, 18, 24, 27, 28] {
            assert_eq!(equip_slot(inv), None, "inv {inv} should not map to a slot");
        }
        // Every mapped slot is a valid index into the 19-slot enum array.
        for inv in 0u8..=30 {
            if let Some(slot) = equip_slot(inv) {
                assert!(slot < 19, "inv {inv} → slot {slot} out of range");
            }
        }
    }
}
