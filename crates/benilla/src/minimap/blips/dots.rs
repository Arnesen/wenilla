//! The `ObjectIcons.blp` **dot layer** — the per-object cell lists of the classifier
//! `0x4eaa90` (quest gold cell 3, tracking gold/red cells 0/1, party blue cell 4). Split from
//! the blip layer's landmark/arrow half; the shared frame geometry ([`BlipCtx`]), hover slot,
//! and size basis live in the parent module (the byte law and its provenance are in the
//! parent's module doc).

use std::collections::HashMap;

use bevy::math::Rect;
use bevy::prelude::*;

use benilla_assets::coords::bevy_to_wow;
use benilla_formats::{LockCatalog, ShapeshiftForm, LOCK_KEY_SKILL};
use benilla_protocol::EntityKind;

use crate::go_templates::GameObjectTemplates;
use crate::names::NameCache;
use crate::net::{GuidIndex, NetEntity};
use crate::ui_pass::{UiQuad, UiQuads, UvRect};

use super::{party_member_pos, BlipCtx, MinimapBlipHover, TrackedCandidates, BLIP_BASIS_PX};

/// The quest dot's quad: 8 × 8 px (`bc82a8 = base·0.00625` × 1280, ctor-frozen; the per-cell
/// scale table `{1,1,1,1,1.3}` leaves quest cell 3 at 1.0 — only the party cell 4 is 1.3×).
const QUEST_DOT_PX: f32 = 8.0;
/// The party dot's quad: the same 8-px base at the cell table's 1.3× party scale.
const PARTY_DOT_PX: f32 = 8.0 * 1.3;
/// `ObjectIcons.blp` cell 4 — the blue party-member dot (col 0, row 1 of the 4×4 grid).
const PARTY_DOT_CELL: [f32; 4] = [0.0, 0.25, 0.25, 0.5];
/// `ObjectIcons.blp` cell 0 — the gold tracked-RESOURCE dot (col 0, row 0): a GameObject
/// passing the resource-tracking predicate (Find Herbs/Minerals; wow-re §B2).
const TRACKED_GO_CELL: [f32; 4] = [0.0, 0.25, 0.0, 0.25];
/// `ObjectIcons.blp` cell 1 — the red tracked-UNIT dot (col 1, row 0): a unit passing the
/// creature-tracking predicate (Track Beasts/Humanoids/…; wow-re §B2).
const TRACKED_UNIT_CELL: [f32; 4] = [0.25, 0.5, 0.0, 0.25];
/// `UNIT_DYNAMIC_FLAGS` bit 0x2 — the per-viewer "always show on minimap" flag (vmangos
/// `UNIT_DYNFLAG_TRACK_UNIT`; the server sets it on a Hunter's Mark victim for the caster).
/// Byte-verified as `0x5ed210`'s first clause (`+0x224 & 0x2`, the 0564 fold-back).
const UNIT_DYNFLAG_TRACK_UNIT: u32 = 0x2;
/// Creature type 7 — Humanoid: the resolver's player/race fallback. Byte-verified via the
/// shipped `ChrRaces.dbc` (col 9 = 7 for all nine playable races; wow-re
/// `track-predicates.md`, the 0564 fold-back) — also the `<= 0` fallback of the shapeshift
/// override.
const CREATURE_TYPE_HUMANOID: u32 = 7;

/// `ObjectIcons.blp` cell for a DIALOG_STATUS — **status 7 only** (`cmp [obj+0xcb8],7` at
/// `0x4eac31`, VERIFIED): the gold cell 3. Every other status draws no quest dot.
fn quest_dot_cell(status: u32) -> Option<[f32; 4]> {
    (status == 7).then_some([0.75, 1.0, 0.0, 0.25])
}

/// Draw a gold dot per quest-giver at status 7, at the unit's live position, hard-culled at
/// the view radius in 3-D world distance (`range² < dx²+dy²+dz²` skips — no rim ride);
/// records a hover hit with the guid. Called AFTER the player arrow: dots draw last, on top.
#[allow(clippy::too_many_arguments)]
pub(in crate::minimap) fn emit_quest_dots(
    ctx: &BlipCtx,
    statuses: &HashMap<u64, u32>,
    guids: &GuidIndex,
    unit_pos: &Query<&GlobalTransform, With<NetEntity>>,
    icons: &Handle<Image>,
    player_indoors: bool,
    unit_indoors: impl Fn(Vec3) -> bool,
    quads: &mut UiQuads,
    hover: &mut MinimapBlipHover,
) {
    for (&npc, &status) in statuses {
        let Some(cell) = quest_dot_cell(status) else {
            continue;
        };
        let Some(&entity) = guids.0.get(&npc) else {
            continue; // despawned / out of range — no live position to mark
        };
        let Ok(tf) = unit_pos.get(entity) else {
            continue;
        };
        let w = bevy_to_wow(tf.translation());
        let d3 =
            ((w[0] - ctx.wx).powi(2) + (w[1] - ctx.wy).powi(2) + (w[2] - ctx.wz).powi(2)).sqrt();
        if d3 > ctx.radius_yd {
            continue;
        }
        // The cross-interior GREY (`0xffb0b0b0`, byte-pinned render value): the classifier
        // greys entries whose colorFlag is set — the "indoor/subzone distinction" via the
        // containment query `0x670540`. Implemented as the indoor-containment MISMATCH (the
        // same down-ray the entity light classifier uses); the exact compare is INTERIM
        // pending its scoped pin.
        let grey = unit_indoors(tf.translation()) != player_indoors;
        let tint = if grey { 0xb0 as f32 / 255.0 } else { 1.0 };
        let rect = Rect::from_center_size(
            ctx.center + ctx.offset(w),
            Vec2::splat(ctx.side * (QUEST_DOT_PX / BLIP_BASIS_PX)),
        );
        quads.overlays.push(UiQuad {
            rect,
            z_key: ctx.z,
            texture: Some(icons.clone()),
            uv: UvRect::from_tex_coords(cell),
            color: [tint, tint, tint, ctx.alpha],
            ..default()
        });
        if let (Some(c), Some(ui)) = (ctx.cursor, ctx.cursor_ui) {
            if rect.contains(c) {
                *hover = MinimapBlipHover::Npc(npc, ui, grey);
            }
        }
    }
}

/// The resource-tracking predicate (the classifier's `0x5ed2b0` leg): does this GameObject's
/// lock match the active `PLAYER_TRACK_RESOURCES` mask? The GO's template lockId resolves
/// through `Lock.dbc`; ANY skill-keyed slot whose `LockType.dbc` id `n` has mask bit
/// `1 << (n − 1)` set matches (the server sets exactly that bit from the tracking aura's
/// MiscValue — vmangos `HandleAuraTrackResources`). Lock-less GOs (lockId 0) never track.
fn tracked_resource(mask: u32, lock_id: u32, locks: &LockCatalog) -> bool {
    if mask == 0 || lock_id == 0 {
        return false;
    }
    locks.slots(lock_id).is_some_and(|slots| {
        slots.iter().any(|s| {
            s.key_type == LOCK_KEY_SKILL
                && (1..=32).contains(&s.index)
                && mask & (1u32 << (s.index - 1)) != 0
        })
    })
}

/// Our own tracking state, read once off the SelfPlayer descriptor: the two masks + the
/// track-stealthed bit (`PLAYER_FIELD_BYTES & 0x2`).
#[derive(Clone, Copy, Default)]
pub(in crate::minimap) struct SelfTracking {
    pub(in crate::minimap) creatures: u32,
    pub(in crate::minimap) resources: u32,
    pub(in crate::minimap) stealthed: bool,
}

/// The creature-tracking predicate — byte-carved `0x5ed210` (wow-re `track-predicates.md`,
/// the 0564 fold-back): two always-show clauses first — `UNIT_DYNFLAG_TRACK_UNIT` (Hunter's
/// Mark) and *our* track-stealthed bit against the target's CREEP vis-flag (the
/// TRACK_STEALTHED(151) consumer) — then the unit's creature type against
/// `PLAYER_TRACK_CREATURES` (bit `creatureType − 1`). No alive/dead or faction gate
/// (byte-verified: neither predicate tests either).
fn tracked_creature(
    tracking: SelfTracking,
    creature_type: Option<u32>,
    dynamic_flags: u32,
    unit_creeping: bool,
) -> bool {
    if dynamic_flags & UNIT_DYNFLAG_TRACK_UNIT != 0 {
        return true;
    }
    if tracking.stealthed && unit_creeping {
        return true;
    }
    tracking.creatures != 0
        && creature_type
            .is_some_and(|t| (1..=32).contains(&t) && tracking.creatures & (1u32 << (t - 1)) != 0)
}

/// The client's creature-type resolver, transcribed — `0x605570` (byte-carved 3-way, the 0564
/// fold-back): a nonzero shapeshift form reads `SpellShapeshiftForm.dbc`'s creatureType FIRST
/// (`<= 0` → Humanoid — a cat-form druid is a Beast); else an NPC reads its cached creature
/// template, a player its race → Humanoid (`ChrRaces.dbc` col 9 = 7 for all nine playable
/// races, dumped from the shipped file).
fn creature_type_of(
    kind: EntityKind,
    shapeshift_form: u8,
    entry: Option<u32>,
    names: &NameCache,
    forms: Option<&HashMap<u32, ShapeshiftForm>>,
) -> Option<u32> {
    if shapeshift_form != 0 {
        if let Some(t) =
            forms.and_then(|f| f.get(&u32::from(shapeshift_form)).map(|r| r.creature_type))
        {
            return Some(if t >= 1 {
                t as u32
            } else {
                CREATURE_TYPE_HUMANOID
            });
        }
    }
    match kind {
        EntityKind::Unit => entry.and_then(|e| names.creature_type(e)),
        EntityKind::Player => Some(CREATURE_TYPE_HUMANOID),
        _ => None,
    }
}

/// Draw the tracking dots (decision 0560): the gold cell-0 dot per tracked GameObject, then
/// the red cell-1 dot per tracked unit — the classifier's fall-through for objects NOT at
/// quest status 7 (those draw the quest dot instead; the `==7` branch is tested first,
/// byte-verified `0x4eac31`). Same hard 3-D radius cull, cross-interior grey, and hover law
/// as the quest dots; drawn just before them (the draw walks the cell lists in order, so
/// cells 0/1 sit under a same-spot quest or party dot).
#[allow(clippy::too_many_arguments)]
pub(in crate::minimap) fn emit_tracking_dots(
    ctx: &BlipCtx,
    tracking: SelfTracking,
    candidates: &TrackedCandidates,
    statuses: &HashMap<u64, u32>,
    names: &NameCache,
    templates: &GameObjectTemplates,
    locks: Option<&LockCatalog>,
    forms: Option<&HashMap<u32, ShapeshiftForm>>,
    icons: &Handle<Image>,
    player_indoors: bool,
    unit_indoors: impl Fn(Vec3) -> bool,
    quads: &mut UiQuads,
    hover: &mut MinimapBlipHover,
) {
    // NB the unit pass runs even with an empty creature mask — the two always-show clauses
    // need no tracking aura bit in it (Hunter's Mark marks the victim, not the caster;
    // track-stealthed rides PLAYER_FIELD_BYTES, not the mask).
    // A candidate's dot, shared by both passes: range-cull, grey, push, hover.
    let mut dot = |guid: u64, tf: &GlobalTransform, cell: [f32; 4], name: DotName| {
        let w = bevy_to_wow(tf.translation());
        let d3 =
            ((w[0] - ctx.wx).powi(2) + (w[1] - ctx.wy).powi(2) + (w[2] - ctx.wz).powi(2)).sqrt();
        if d3 > ctx.radius_yd {
            return;
        }
        let grey = unit_indoors(tf.translation()) != player_indoors;
        let tint = if grey { 0xb0 as f32 / 255.0 } else { 1.0 };
        let rect = Rect::from_center_size(
            ctx.center + ctx.offset(w),
            Vec2::splat(ctx.side * (QUEST_DOT_PX / BLIP_BASIS_PX)),
        );
        quads.overlays.push(UiQuad {
            rect,
            z_key: ctx.z,
            texture: Some(icons.clone()),
            uv: UvRect::from_tex_coords(cell),
            color: [tint, tint, tint, ctx.alpha],
            ..default()
        });
        if let (Some(c), Some(ui)) = (ctx.cursor, ctx.cursor_ui) {
            if rect.contains(c) {
                *hover = match name {
                    DotName::Guid => MinimapBlipHover::Npc(guid, ui, grey),
                    DotName::Known(n) => MinimapBlipHover::TrackedGo(n, ui, grey),
                };
            }
        }
    };
    // Cell 0 — tracked GameObjects (gold): template lockId through Lock.dbc.
    if tracking.resources != 0 {
        if let Some(locks) = locks {
            for (guid, net, tf, _) in candidates.iter() {
                if net.kind != EntityKind::GameObject || statuses.get(&guid.0).copied() == Some(7) {
                    continue;
                }
                let Some(t) = templates.get(guid.0) else {
                    continue; // template not answered yet — no lock to test
                };
                if tracked_resource(tracking.resources, t.lock_id, locks) {
                    dot(guid.0, tf, TRACKED_GO_CELL, DotName::Known(t.name.clone()));
                }
            }
        }
    }
    // Cell 1 — tracked units (red): the 3-way creature-type resolver + the always-show pair.
    for (guid, net, tf, store) in candidates.iter() {
        if !matches!(net.kind, EntityKind::Unit | EntityKind::Player) {
            continue;
        }
        if statuses.get(&guid.0).copied() == Some(7) {
            continue; // the ==7 branch already drew the quest dot
        }
        let (dyn_flags, form, creeping) = store
            .map(|s| {
                (
                    s.0.unit_dynamic_flags(),
                    s.0.unit_shapeshift_form(),
                    s.0.unit_is_stealthed(),
                )
            })
            .unwrap_or((0, 0, false));
        let creature_type = creature_type_of(
            net.kind,
            form,
            benilla_protocol::guid::entry(guid.0),
            names,
            forms,
        );
        if tracked_creature(tracking, creature_type, dyn_flags, creeping) {
            dot(guid.0, tf, TRACKED_UNIT_CELL, DotName::Guid);
        }
    }
}

/// How a tracking dot's hover names itself: a unit resolves by guid through the name cache;
/// a GameObject's template name is already in hand.
enum DotName {
    Guid,
    Known(String),
}

/// The party **dots** — the in-range half of `place_party_raid_blips`: the blue `ObjectIcons`
/// cell 4 at the member's true position, at the cell table's 1.3× scale (10.4 px on the frozen
/// basis). Drawn last with the object dots (`0x4ed7b7`'s order: above the arrows and the player
/// arrow).
pub(in crate::minimap) fn emit_party_dots(
    ctx: &BlipCtx,
    group: &crate::ui_party::GroupState,
    guids: &GuidIndex,
    unit_pos: &Query<&GlobalTransform, With<NetEntity>>,
    icons: &Handle<Image>,
    quads: &mut UiQuads,
) {
    for m in group.party_slots() {
        let Some((x, y)) = party_member_pos(m, group, guids, unit_pos) else {
            continue;
        };
        let d = ((x - ctx.wx).powi(2) + (y - ctx.wy).powi(2)).sqrt();
        if d / ctx.radius_yd > super::BLIP_EDGE_RATIO {
            continue; // out of range — the arrow pass drew it
        }
        quads.overlays.push(UiQuad {
            rect: Rect::from_center_size(
                ctx.center + ctx.offset([x, y, 0.0]),
                Vec2::splat(ctx.side * (PARTY_DOT_PX / BLIP_BASIS_PX)),
            ),
            z_key: ctx.z,
            texture: Some(icons.clone()),
            uv: UvRect::from_tex_coords(PARTY_DOT_CELL),
            color: [1.0, 1.0, 1.0, ctx.alpha],
            ..default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only status 7 dots (the gold cell 3). Status 6 — despite the vmangos "red dot"
    /// comment — draws nothing on the 1.12 client (byte-verified `==7` at 0x4eac31).
    #[test]
    fn quest_dot_is_status_seven_only_gold_cell_three() {
        assert_eq!(quest_dot_cell(7), Some([0.75, 1.0, 0.0, 0.25]));
        for s in [0, 1, 2, 3, 4, 5, 6, 8] {
            assert_eq!(quest_dot_cell(s), None, "status {s} must not dot");
        }
    }

    /// The tracking predicates' mask-bit law (decision 0560): bit `1 << (n − 1)` where `n` is
    /// the GO lock's skill-slot `LockType` id (resources) or the unit's creature type
    /// (creatures) — the exact bit the server sets from the tracking aura's MiscValue — plus
    /// the always-show dyn-flag clause that needs no mask at all.
    #[test]
    fn tracking_predicates_follow_the_mask_bit_law() {
        use benilla_formats::{LockSlot, LOCK_KEY_ITEM, MAX_LOCK_SLOTS};
        // A mining-vein-shaped lock (one SKILL slot, Mining LockType 3) and a key-ITEM lock
        // whose index happens to collide numerically.
        let mut vein = [LockSlot::default(); MAX_LOCK_SLOTS];
        vein[0] = LockSlot {
            key_type: LOCK_KEY_SKILL,
            index: 3,
            skill: 0,
        };
        let mut keyed = [LockSlot::default(); MAX_LOCK_SLOTS];
        keyed[0] = LockSlot {
            key_type: LOCK_KEY_ITEM,
            index: 3,
            skill: 0,
        };
        let locks = LockCatalog::from_rows([(38, vein), (40, keyed)]);
        assert!(tracked_resource(1 << 2, 38, &locks), "mining bit lights it");
        assert!(
            !tracked_resource(1 << 1, 38, &locks),
            "herbalism bit doesn't"
        );
        assert!(!tracked_resource(0, 38, &locks), "no mask, no dot");
        assert!(
            !tracked_resource(1 << 2, 0, &locks),
            "lockId 0 never tracks"
        );
        assert!(
            !tracked_resource(1 << 2, 40, &locks),
            "a key-ITEM slot's index is an item entry, not a LockType"
        );
        assert!(!tracked_resource(1 << 2, 99, &locks), "unknown lock id");

        // Track Beasts sets bit 0 (Beast is creature type 1).
        let beasts = SelfTracking {
            creatures: 1,
            ..default()
        };
        assert!(tracked_creature(beasts, Some(1), 0, false));
        assert!(!tracked_creature(
            beasts,
            Some(CREATURE_TYPE_HUMANOID),
            0,
            false
        ));
        assert!(!tracked_creature(
            SelfTracking::default(),
            Some(1),
            0,
            false
        ));
        assert!(
            !tracked_creature(beasts, None, 0, false),
            "type not cached yet — no dot"
        );
        // The always-show pair (the `0x5ed210` carve): Hunter's Mark forces the dot with no
        // tracking aura on us; track-stealthed lights only a CREEP-flagged unit — and only
        // the conjunction of the two bits does.
        assert!(tracked_creature(
            SelfTracking::default(),
            None,
            UNIT_DYNFLAG_TRACK_UNIT,
            false
        ));
        let stealth = SelfTracking {
            stealthed: true,
            ..default()
        };
        assert!(tracked_creature(stealth, None, 0, true));
        assert!(!tracked_creature(stealth, None, 0, false), "not sneaking");
        assert!(
            !tracked_creature(SelfTracking::default(), None, 0, true),
            "we don't track stealthed"
        );
    }

    /// The creature-type resolver's 3-way (`0x605570`, the 0564 fold-back): shapeshift
    /// override first (`<= 0` → Humanoid), then the cached template for NPCs, the Humanoid
    /// fallback for players.
    #[test]
    fn creature_type_resolver_prefers_the_shapeshift_override() {
        use benilla_formats::ShapeshiftForm;
        let names = NameCache::default();
        let forms: HashMap<u32, ShapeshiftForm> = [
            (
                1,
                ShapeshiftForm {
                    creature_type: 1, // Cat → Beast
                    ..Default::default()
                },
            ),
            (
                16,
                ShapeshiftForm {
                    creature_type: 0, // a <=0 row reads Humanoid (the resolver's fallback)
                    ..Default::default()
                },
            ),
        ]
        .into();
        // A cat-form PLAYER is a Beast; unshifted, a player is a Humanoid.
        assert_eq!(
            creature_type_of(EntityKind::Player, 1, None, &names, Some(&forms)),
            Some(1)
        );
        assert_eq!(
            creature_type_of(EntityKind::Player, 0, None, &names, Some(&forms)),
            Some(CREATURE_TYPE_HUMANOID)
        );
        // A <=0 creatureType row resolves Humanoid, not the race/template path.
        assert_eq!(
            creature_type_of(EntityKind::Player, 16, None, &names, Some(&forms)),
            Some(CREATURE_TYPE_HUMANOID)
        );
        // An unshifted NPC with no cached template yet resolves nothing (no dot until the
        // ask-once query answers); a GameObject never resolves a creature type.
        assert_eq!(
            creature_type_of(EntityKind::Unit, 0, Some(69), &names, Some(&forms)),
            None
        );
        assert_eq!(
            creature_type_of(EntityKind::GameObject, 0, None, &names, Some(&forms)),
            None
        );
    }

    /// The whole client-side chain against the REAL 5875 data (skips without it): the
    /// tracking spell's `EffectMiscValue` → the server's mask bit → the gathering node's
    /// `Lock.dbc` skill slot. Find Minerals lights a Copper Vein, Find Herbs a Peacebloom —
    /// and neither lights the other's node.
    #[test]
    fn real_find_minerals_lights_a_copper_vein_not_an_herb() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let spells = benilla_formats::load_spell_catalog(&mut chain).expect("Spell.dbc");
        let locks = benilla_formats::load_lock_catalog(&mut chain).expect("Lock.dbc");
        let forms =
            benilla_formats::load_shapeshift_forms(&mut chain).expect("SpellShapeshiftForm.dbc");

        // The server's mask law, applied to the real spell row: bit `1 << (MiscValue − 1)` of
        // the aura-`kind` effect (44 TRACK_CREATURES / 45 TRACK_RESOURCES).
        let mask_of = |spell_id: u32, kind: u32| -> u32 {
            let s = spells.get(spell_id).expect("spell row");
            (0..3)
                .find_map(|i| {
                    (s.effect_apply_aura[i] == kind).then(|| {
                        let m = s.effect_misc_value[i];
                        assert!((1..=32).contains(&m), "MiscValue {m} out of mask range");
                        1u32 << (m - 1)
                    })
                })
                .expect("tracking effect present")
        };

        // Find Minerals 2580 ↔ Copper Vein (gameobject_template 1731, chest lockId 38 —
        // vmangos world DB — whose Lock.dbc row is the Mining LockType 3 skill slot).
        let minerals = mask_of(2580, 45);
        assert_eq!(minerals, 1 << 2, "Find Minerals' MiscValue is Mining (3)");
        assert!(tracked_resource(minerals, 38, &locks));
        // Find Herbs 2383 ↔ Peacebloom/Silverleaf (lockId 29, Herbalism LockType 2).
        let herbs = mask_of(2383, 45);
        assert!(tracked_resource(herbs, 29, &locks));
        assert!(!tracked_resource(minerals, 29, &locks), "cross-profession");
        assert!(!tracked_resource(herbs, 38, &locks), "cross-profession");
        // Track Beasts 1494: TRACK_CREATURES MiscValue 1 = Beast — a wolf dots red, a
        // humanoid doesn't. And through the shapeshift override on the REAL
        // SpellShapeshiftForm.dbc, a cat-form (1) druid IS a Beast to it.
        let beasts = SelfTracking {
            creatures: mask_of(1494, 44),
            ..Default::default()
        };
        assert!(tracked_creature(beasts, Some(1), 0, false));
        assert!(!tracked_creature(
            beasts,
            Some(CREATURE_TYPE_HUMANOID),
            0,
            false
        ));
        let cat = creature_type_of(
            EntityKind::Player,
            1,
            None,
            &NameCache::default(),
            Some(&forms),
        );
        assert_eq!(cat, Some(1), "cat form resolves Beast (DBC col 12)");
        assert!(tracked_creature(beasts, cat, 0, false));
    }
}
