//! Equipment **resolution** (decisions 0072/0074, split out of `super`'s one file): what each unit
//! should be holding this frame, and where — the descriptor read (a creature's virtual items, a
//! player's visible-item entries through the ask-once item layer), the drawn-vs-stowed placement
//! law, the item/enchant glow id (decision 0805), and the player teardown a gear change forces.
//! The output is [`HeldItems`] + [`Equipment`] + [`Wielded`], which `super::spawn` turns into
//! children.

use benilla_protocol::EntityKind;
use bevy::prelude::*;

use crate::creature_anim::{HandGrip, NockLatch, NockedAmmo, VisualSheath, Wielded};
use crate::items::Items;
use crate::net::{NetCommands, NetEntity, ObjectStore};

use super::super::item_glow::{self, ItemGlows};
use super::super::Creatures;
use super::{
    attach_id, ensure_item_model, Equipment, HeldItems, HeldSlot, ItemDisplays, ItemModelKind,
    ATTACH_SLOTS, COMPOSITE_SLOTS, HELD_SLOTS, NO_GLOW, PLAYER_HELD_SLOTS,
};

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

/// The glow id for one held item, and the model requests it implies: the base-or-enchant fork
/// ([`item_glow::effective_visual`]) plus the cache entries for whatever it resolves to. `0` when
/// the glow chain's DBCs are absent — the item simply draws unadorned, as before decision 0805.
fn resolve_glow(
    glows: Option<&mut ItemGlows>,
    displays: &ItemDisplays,
    display: u32,
    enchants: impl IntoIterator<Item = u32>,
    asset_server: &AssetServer,
) -> i32 {
    let Some(glows) = glows else {
        return NO_GLOW;
    };
    let base = displays.catalog.get(display).map_or(0, |d| d.item_visual);
    let visual = item_glow::effective_visual(glows, base, enchants);
    item_glow::ensure_glow_models(glows, visual, asset_server);
    visual
}

/// Resolve every unit's held items from its descriptor. Creatures read display/invType/sheath straight
/// from the virtual-item fields; players go visible-item entry → [`crate::items::Items`] (ask-once query on
/// a miss). Ensures each needed display id has a [`DisplayModel`] entry in [`ItemDisplays`] (built
/// by [`super::update_display_models`] once the asset loads) and writes [`HeldItems`] on change.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(in crate::entities) fn resolve_equipment(
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
    // The item/enchant glow chain (decision 0805): resolved here beside the item itself, so a glow
    // model is requested the same frame its weapon is and an enchant change rides the item diff.
    glows: Option<ResMut<ItemGlows>>,
) {
    let Some(mut held) = held else {
        return;
    };
    let mut glows = glows;
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
        // The settled sheath state: the anim layer's **client-side committed state** (the
        // setter/reconcile cache, decision 0080 — the descriptor byte plus the policy's forces);
        // else, before the driver first runs, the raw descriptor byte.
        let committed = driver
            .and_then(|d| d.sheath_state())
            .or_else(|| s.unit_sheath_state())
            .unwrap_or(0);
        // …and the *visual* one governing a given slot's placement, which during a draw/stow
        // ceremony is **per arm** ([`VisualSheath`]): each hand's weapon moves at its own clip's
        // authored $SHL/$SHR moment, not at the byte change. A melee → ranged toggle therefore
        // has the sword already on the back while the bow is still on its way to the other hand —
        // the ceremony's two movements (`creature_anim::sheath`).
        let sheath_of = |slot: usize, inv_type: u32| {
            visual_sheath.map_or(committed, |v| v.for_slot(slot, inv_type))
        };
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
            // (display id, inventory type, item sheath type, class, subclass, material) per slot.
            let resolved: Option<(u32, u32, u8, u8, u8, u8)> = match net_entity.kind {
                EntityKind::Unit => {
                    let display = s.unit_virtual_item_display(slot as u8).filter(|d| *d != 0);
                    display.map(|d| {
                        let (class, subclass, material, inv) =
                            s.unit_virtual_item_info(slot as u8).unwrap_or((0, 0, 0, 0));
                        let sheath = s.unit_virtual_item_sheath(slot as u8).unwrap_or(0);
                        (d, inv as u32, sheath, class, subclass, material)
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
                            t.material as u8,
                        )
                    }),
                _ => None,
            };
            let Some((display, inv_type, item_sheath, class, subclass, material)) = resolved else {
                continue;
            };
            // The item's Material — the draw/stow sound's only key (decision 0882). Both wire
            // sources carry it; neither is a guess.
            wielded.materials[slot] = material;
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
                    wielded.ranged_sheath = item_sheath;
                    wielded.ranged_inv = inv_type;
                    ranged_inv_type = Some(inv_type);
                }
                _ => {}
            }
            if !char_component {
                continue; // wielded resolved; the model never attaches on a non-character body
            }
            let Some(attach) = placement(slot, inv_type, item_sheath, sheath_of(slot, inv_type))
            else {
                continue;
            };
            let kind = if inv_type == 14 {
                ItemModelKind::Shield
            } else {
                ItemModelKind::Weapon
            };
            ensure_item_model(&mut held, display, kind, &asset_server);
            // The glow (decision 0805): the display's intrinsic visual, else this weapon slot's
            // first enchant with one. The enchant half is **players only** — the enchant ids ride
            // `PLAYER_VISIBLE_ITEM`, and a creature's virtual item carries none, exactly like the
            // synthetic item the reference's `GetVirtualItem` hands its resolver.
            let enchants = (net_entity.kind == EntityKind::Player).then(|| {
                // All 7 CGItem enchant slots, the reference's scan width (`0x62ec70`); 1.12
                // broadcasts the first two (PERM, TEMP).
                (0..7u8).filter_map(|j| s.player_visible_item_enchant(PLAYER_HELD_SLOTS[slot], j))
            });
            let visual = resolve_glow(
                glows.as_deref_mut(),
                &held,
                display,
                enchants.into_iter().flatten(),
                &asset_server,
            );
            slots[slot] = Some(HeldSlot {
                display,
                kind,
                attach,
                visual,
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
            // The ammo model's own intrinsic visual — the reference attaches it at the tail of
            // the ranged/ammo builder `0x479f40` (`47a051: mov edx,[ebx+0x58]`), the same way the
            // hand attach does. No enchant lane here: this is a model, not an equipped item.
            let visual = resolve_glow(
                glows.as_deref_mut(),
                &held,
                ammo.display_id,
                [],
                &asset_server,
            );
            slots[6] = Some(HeldSlot {
                display: ammo.display_id,
                kind: ItemModelKind::Ammo,
                attach,
                visual,
            });
        }
        // The quiver on the back (wow-re `nocked-ammo-cancel.md` §H, byte-verified): while the
        // RANGED weapon is drawn — the same `0x611e10` ranged-draw transition, cleared on every
        // other ranged state — the client scans the player's OWN inventory for an ItemClass-11
        // container (Quiver/Ammo Pouch) and parents its display model at attachment 26 (no
        // transform override; no cloak conflict). Self-only by construction, exactly like the
        // client: bag slots are never replicated in 1.12, so a remote player's scan finds
        // nothing (§H1 — a two-client capture would be the clean confirmation).
        // Timed off the RANGED slot's own visual state, so the quiver arrives with the bow it
        // feeds. (Named deviation: the ref attaches it at the *start* of the ranged draw — the
        // `0x611e10(1)` call inside each drawer — where ours waits for that clip's $SHL. Same
        // clip, a few hundred ms apart, and the two are never seen separately.)
        if net_entity.kind == EntityKind::Player
            && char_component
            && sheath_of(2, ranged_inv_type.unwrap_or(0)) == 2
        {
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
                    visual: NO_GLOW,
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
                    visual: NO_GLOW,
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
                        visual: NO_GLOW,
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
