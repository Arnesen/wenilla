//! Tests for the aura-state CharProc layer. The chains are the **real 5875 rows** (read off
//! `SpellVisualKit.dbc` this session with `benilla-extract charprocs`), so a schema or column-order
//! regression in the data layer shows up here as a wrong number rather than as a silent nothing —
//! which is precisely how B114 hid.

use std::collections::HashMap;

use benilla_formats::{
    char_proc_type, CharProc, SpellVisualCatalog, VisualKit, VisualStages, KIT_CHAR_PROCS,
};
use benilla_protocol::messages::ObjectFields;
use bevy::prelude::*;

use super::*;
use crate::creature_anim::SpellVisuals;

// ── The real chains, from the shipped table ───────────────────────────────────────────────────────

/// Stealth rank 1 → `SpellVisual` 184 → state kit 312: proc 7 (unmodelled) + **proc 14 @ 0.3**, no
/// effect models, no anim. The whole of B114 in one row.
const STEALTH: u32 = 1784;
const STEALTH_VISUAL: u32 = 184;
const STEALTH_KIT: u32 = 312;
/// The ghost aura → visual 886 → kit 989: proc 1 tint `9222653.0` (= `0x8CB9FD`) + proc 14 @ 0.5.
const GHOST: u32 = 8326;
const GHOST_VISUAL: u32 = 886;
const GHOST_KIT: u32 = 989;

fn proc(ty: i32, param0: f32) -> CharProc {
    CharProc {
        ty,
        params: [param0, 0.0, 0.0, 0.0],
    }
}

/// A kit carrying `procs` in slot order and nothing else — the shape of every proc-only state kit.
fn proc_kit(procs: &[CharProc]) -> VisualKit {
    let mut slots = [None; KIT_CHAR_PROCS];
    for (slot, p) in slots.iter_mut().zip(procs) {
        *slot = Some(*p);
    }
    VisualKit {
        char_proc_slots: slots,
        ..Default::default()
    }
}

// ── The dispatch ─────────────────────────────────────────────────────────────────────────────────

/// [`node_for`] is the twin of the client's proc jump table: 14 → an alpha node carrying `params[0]`,
/// 1 → a tint node with the packed colour unpacked, everything else → no node (and so no arm).
#[test]
fn the_dispatch_names_only_the_verified_procs() {
    assert_eq!(
        node_for(proc(char_proc_type::ALPHA, 0.3)),
        Some(AuraNode::Alpha(0.3)),
        "the sneak family's 0.3 rides params[0]"
    );
    assert_eq!(
        node_for(proc(char_proc_type::ALPHA, 0.0)),
        Some(AuraNode::Alpha(0.0)),
        "kit 5129's 0.0 is a real value, not an absent one"
    );
    // 9222653 = 0x8CB9FD — the ghost's pale blue-white, byte-cited in wow-re as 0xFF8CB9FD after the
    // client ORs the opaque byte on.
    assert_eq!(
        node_for(proc(char_proc_type::TINT, 9_222_653.0)),
        Some(AuraNode::Tint([0x8C, 0xB9, 0xFD])),
    );
    // 65280 = 0x00FF00 — the poison state kit's pure green.
    assert_eq!(
        node_for(proc(char_proc_type::TINT, 65_280.0)),
        Some(AuraNode::Tint([0x00, 0xFF, 0x00])),
    );
    // The five other types that ride real state kits have no verified mechanism yet: no node, so
    // nothing is invented and the kit doesn't arm on them alone.
    for ty in [2, 7, 8, 11, 13] {
        assert_eq!(node_for(proc(ty, 1.0)), None, "type {ty} is unmodelled");
    }
}

// ── The target and the ramp ──────────────────────────────────────────────────────────────────────

/// `0x60d180`: the target is `baseAlpha × the HEAD node's factor` — at most one node term, the
/// newest (nodes link at the list head as they install), never a product over the chain (wow-re
/// `base-render-alpha.md` §4, correcting the earlier `Π` reading). Stacking stealth (0.3) then the
/// ghost aura (0.5) reads 0.5 — whichever landed last — and dropping the head hands the term to
/// the next node down.
#[test]
fn the_target_is_base_times_the_newest_node() {
    let mut n = AuraNodes::new(1.0);
    assert_eq!(n.target(), 1.0, "no nodes → opaque");

    install(&mut n, STEALTH, &[AuraNode::Alpha(0.3)], 0.0);
    assert!((n.target() - 0.3).abs() < 1e-6);

    install(&mut n, GHOST, &[AuraNode::Alpha(0.5)], 0.0);
    assert!(
        (n.target() - 0.5).abs() < 1e-6,
        "the newest node is the term"
    );

    // Reaping the head hands the term back to the older node.
    n.alpha.retain(|(s, _)| *s != GHOST);
    n.retarget(0.0);
    assert!((n.target() - 0.3).abs() < 1e-6);

    // A display authored translucent (CreatureModelAlpha 128/255) multiplies the head term —
    // and stands alone when no node is live (the display-set leg, no aura anywhere).
    let mut half = AuraNodes::new(1.0);
    half.base = 128.0 / 255.0;
    install(&mut half, STEALTH, &[AuraNode::Alpha(0.3)], 0.0);
    assert!((half.target() - (128.0 / 255.0) * 0.3).abs() < 1e-6);
    half.alpha.clear();
    half.retarget(0.0);
    assert!((half.target() - 128.0 / 255.0).abs() < 1e-6);
}

/// The display-swap leg ([`refresh_base_alpha`]'s retarget): a base change rides the same 1000 ms
/// cubic ramp an aura node does — Ghost Wolf's 102/255 eases in from opaque, and swapping home
/// eases back — never snaps (`base-render-alpha.md` §5: the fade is part of the mechanism).
#[test]
fn a_base_change_rides_the_same_ramp() {
    let wolf = 102.0 / 255.0;
    let mut n = AuraNodes::new(1.0);
    n.base = wolf;
    n.retarget(0.0);
    assert!((n.tick(0.0) - 1.0).abs() < 1e-6, "starts where it was");
    assert!(
        (n.tick(AURA_ALPHA_FADE_SECS) - wolf).abs() < 1e-6,
        "arrives"
    );
    assert!(n.translucent());

    // The swap home: base back to 1.0, eased up from the live value.
    n.base = 1.0;
    n.retarget(AURA_ALPHA_FADE_SECS);
    assert!((n.from - wolf).abs() < 1e-6);
    assert!((n.tick(2.0 * AURA_ALPHA_FADE_SECS) - 1.0).abs() < 1e-6);
    assert!(!n.translucent(), "settled opaque again");
}

/// A re-applied aura installs ONE node set, not a second copy — the reference keys its nodes by spell
/// id (`node+0x18`) and its own dedup walk returns early on a spell that already has one. Without
/// this a refresh would square the factor every application.
#[test]
fn re_arming_the_same_spell_replaces_its_node() {
    let mut n = AuraNodes::new(1.0);
    install(&mut n, STEALTH, &[AuraNode::Alpha(0.3)], 0.0);
    install(&mut n, STEALTH, &[AuraNode::Alpha(0.3)], 0.0);
    assert_eq!(n.alpha.len(), 1);
    assert!((n.target() - 0.3).abs() < 1e-6);
}

/// The ramp is `StartAlphaFade(target, 1000 ms)` on the `clamp01(t)³` ease, in **both** directions:
/// arming eases down to the product, reaping eases back up — and a retarget mid-ramp starts from
/// where the ramp actually is, never from the old endpoint (a stealth dropped 300 ms into the fade
/// must not jump to 0.3 first).
#[test]
fn the_ramp_eases_over_one_second_both_ways() {
    let mut n = AuraNodes::new(1.0);
    install(&mut n, STEALTH, &[AuraNode::Alpha(0.3)], 0.0);

    assert!((n.tick(0.0) - 1.0).abs() < 1e-6, "t=0 is still opaque");
    // t³ at the halfway mark = 0.125 of the way down.
    let half = n.tick(0.5);
    assert!(
        (half - (1.0 + (0.3 - 1.0) * 0.125)).abs() < 1e-6,
        "cubic ease, not linear (got {half})"
    );
    assert!(
        (n.tick(AURA_ALPHA_FADE_SECS) - 0.3).abs() < 1e-6,
        "arrived at 1000 ms"
    );
    assert!((n.tick(5.0) - 0.3).abs() < 1e-6, "and clamps past the end");
    assert!(n.translucent());

    // The aura drops: ease back to the new product from where we are.
    n.alpha.clear();
    n.retarget(AURA_ALPHA_FADE_SECS);
    assert!((n.from - 0.3).abs() < 1e-6, "ramps from the live value");
    assert!((n.to - 1.0).abs() < 1e-6);
    assert!((n.tick(2.0 * AURA_ALPHA_FADE_SECS) - 1.0).abs() < 1e-6);
    assert!(!n.translucent(), "settled opaque again");

    // A mid-ramp retarget picks up the live value, not the endpoint.
    let mut m = AuraNodes::new(1.0);
    install(&mut m, STEALTH, &[AuraNode::Alpha(0.3)], 0.0);
    let mid = m.tick(0.7);
    m.alpha.clear();
    m.retarget(0.7);
    assert!((m.from - mid).abs() < 1e-6);
}

/// An aura refresh whose factor doesn't change must not restart the ease — otherwise a buff ticking
/// every few seconds would keep re-easing a settled body.
#[test]
fn a_retarget_to_the_same_value_does_not_restart_the_ease() {
    let mut n = AuraNodes::new(1.0);
    install(&mut n, STEALTH, &[AuraNode::Alpha(0.3)], 0.0);
    n.tick(AURA_ALPHA_FADE_SECS);
    let started = n.started;
    install(&mut n, STEALTH, &[AuraNode::Alpha(0.3)], 10.0);
    assert_eq!(n.started, started, "same product → the ramp is untouched");
    assert!((n.tick(10.0) - 0.3).abs() < 1e-6, "and stays arrived");
}

/// Proc 1's nodes link at the head like the alpha list's (`unit+0xce0`), so the tint the per-frame
/// apply reads is the NEWEST aura's — deterministic under stacking.
#[test]
fn the_tint_head_node_is_the_newest_installed() {
    let mut n = AuraNodes::new(1.0);
    assert_eq!(n.head_tint(), None);
    install(&mut n, GHOST, &[AuraNode::Tint([0x8C, 0xB9, 0xFD])], 0.0);
    install(&mut n, 12881, &[AuraNode::Tint([0x00, 0xFF, 0x00])], 0.0);
    assert_eq!(n.head_tint(), Some([0x00, 0xFF, 0x00]));
    // The newest dropping hands the head back.
    n.tint.retain(|(s, _)| *s != 12881);
    assert_eq!(n.head_tint(), Some([0x8C, 0xB9, 0xFD]));
    assert_eq!(n.target(), 1.0, "a tint node is not an alpha term");
}

// ── The watcher → drain edge, over the real Stealth chain ─────────────────────────────────────────

/// A fixture app carrying the two real chains, the aura watcher and the drain.
fn app_with_chains() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_message::<crate::creature_anim::SpellKitFx>();
    app.add_message::<AuraProc>();
    app.insert_resource(SpellVisuals(SpellVisualCatalog::from_tables(
        HashMap::from([
            (
                STEALTH_VISUAL,
                VisualStages {
                    state: STEALTH_KIT,
                    ..Default::default()
                },
            ),
            (
                GHOST_VISUAL,
                VisualStages {
                    state: GHOST_KIT,
                    ..Default::default()
                },
            ),
        ]),
        HashMap::from([
            // Kit 312's real slots: the unmodelled proc 7 FIRST, then proc 14 — so this also pins
            // that an unmodelled proc ahead of a modelled one doesn't shadow it.
            (
                STEALTH_KIT,
                proc_kit(&[proc(7, 1.0), proc(char_proc_type::ALPHA, 0.3)]),
            ),
            (
                GHOST_KIT,
                proc_kit(&[
                    proc(char_proc_type::TINT, 9_222_653.0),
                    proc(char_proc_type::ALPHA, 0.5),
                ]),
            ),
        ]),
    )));
    app.insert_resource(crate::ui_action::Spells {
        catalog: benilla_formats::SpellCatalog::from_displays(HashMap::from([
            (
                STEALTH,
                benilla_formats::SpellDisplay {
                    visual: STEALTH_VISUAL,
                    ..Default::default()
                },
            ),
            (
                GHOST,
                benilla_formats::SpellDisplay {
                    visual: GHOST_VISUAL,
                    ..Default::default()
                },
            ),
        ])),
        ..crate::ui_action::Spells::empty_for_tests()
    });
    app.add_systems(
        Update,
        (
            crate::creature_anim::arm_aura_state_fx_for_test,
            drain_aura_procs,
        )
            .chain(),
    );
    app
}

/// The arming world plus the per-instance tint channel (decision 0812): the table resource, the
/// palette allocator whose slots index it, and the publish system.
fn app_with_tint_channel() -> App {
    let mut app = app_with_chains();
    app.init_resource::<crate::instance_tint::InstanceTints>();
    app.init_resource::<crate::rig_palette::RigPalettes>();
    app.add_systems(Update, apply_aura_tint.after(drain_aura_procs));
    app
}

/// **B114, end to end at the mechanism.** Stealth's state kit carries no effect models at all, so the
/// effects-only watcher never armed it and the character showed nothing. Now the same slot edge
/// installs a proc-14 node and the unit's target alpha is 0.3 — and dropping the aura ramps it back.
#[test]
fn a_proc_only_state_kit_arms_stealth_translucency() {
    let mut app = app_with_chains();

    // The aura lands in slot 0 (UNIT_FIELD_AURA[0] = field 47) with an occupied AURAFLAGS nibble.
    let stealthed = ObjectFields::from_pairs(&[(47, STEALTH), (95, 0x0E)]);
    let unit = app
        .world_mut()
        .spawn(crate::net::ObjectStore(stealthed))
        .id();
    app.update();

    let n = app
        .world()
        .entity(unit)
        .get::<AuraNodes>()
        .expect("the sneak kit armed a node set");
    assert_eq!(n.alpha.as_slice(), [(STEALTH, 0.3)]);
    assert!((n.target() - 0.3).abs() < 1e-6);
    assert!(n.head_tint().is_none(), "kit 312 carries no tint");

    // The aura drops: the node goes and the target ramps back to opaque.
    app.world_mut()
        .entity_mut(unit)
        .insert(crate::net::ObjectStore(ObjectFields::from_pairs(&[(
            95, 0,
        )])));
    app.update();
    let n = app.world().entity(unit).get::<AuraNodes>().unwrap();
    assert!(n.alpha.is_empty(), "reaped on the aura-remove edge");
    assert!((n.target() - 1.0).abs() < 1e-6);
}

/// The ghost aura's kit carries both procs: the tint node lands beside the alpha one off a single
/// slot edge (the dispatcher walks all four slots), and the alpha is the ghost's 0.5.
#[test]
fn a_kit_with_both_procs_installs_both_nodes() {
    let mut app = app_with_chains();
    let unit = app
        .world_mut()
        .spawn(crate::net::ObjectStore(ObjectFields::from_pairs(&[
            (47, GHOST),
            (95, 0x0E),
        ])))
        .id();
    app.update();

    let n = app.world().entity(unit).get::<AuraNodes>().unwrap();
    assert_eq!(n.alpha.as_slice(), [(GHOST, 0.5)]);
    assert_eq!(n.head_tint(), Some([0x8C, 0xB9, 0xFD]));
}

/// Two auras on one unit: the newest node holds the head (the reference's link-at-head list), and
/// reaping the other leaves it untouched — the body never snaps opaque while any alpha aura lives.
#[test]
fn two_auras_stack_and_unstack_through_the_slots() {
    let mut app = app_with_chains();
    let both = ObjectFields::from_pairs(&[(47, STEALTH), (48, GHOST), (95, 0xEE)]);
    let unit = app.world_mut().spawn(crate::net::ObjectStore(both)).id();
    app.update();
    let n = app.world().entity(unit).get::<AuraNodes>().unwrap();
    assert!(
        (n.target() - 0.5).abs() < 1e-6,
        "the ghost node installed last → it is the head term"
    );

    // Stealth alone drops (slot 0 cleared, slot 1 still occupied).
    app.world_mut()
        .entity_mut(unit)
        .insert(crate::net::ObjectStore(ObjectFields::from_pairs(&[
            (48, GHOST),
            (95, 0xE0),
        ])));
    app.update();
    let n = app.world().entity(unit).get::<AuraNodes>().unwrap();
    assert_eq!(n.alpha.as_slice(), [(GHOST, 0.5)]);
    assert!((n.target() - 0.5).abs() < 1e-6);
}

/// A state kit whose every proc is unmodelled (and which carries no effect models either) must not
/// arm: no node set, so nothing claims the unit's alpha channel on a mechanism we haven't verified.
#[test]
fn an_unmodelled_proc_only_kit_arms_nothing() {
    const SAP: u32 = 6770;
    const SAP_VISUAL: u32 = 700;
    const SAP_KIT: u32 = 691;

    let mut app = app_with_chains();
    app.insert_resource(SpellVisuals(SpellVisualCatalog::from_tables(
        HashMap::from([(
            SAP_VISUAL,
            VisualStages {
                state: SAP_KIT,
                ..Default::default()
            },
        )]),
        // Kit 691's real content: proc 7 @ 2.0 and nothing else.
        HashMap::from([(SAP_KIT, proc_kit(&[proc(7, 2.0)]))]),
    )));
    app.insert_resource(crate::ui_action::Spells {
        catalog: benilla_formats::SpellCatalog::from_displays(HashMap::from([(
            SAP,
            benilla_formats::SpellDisplay {
                visual: SAP_VISUAL,
                ..Default::default()
            },
        )])),
        ..crate::ui_action::Spells::empty_for_tests()
    });

    let unit = app
        .world_mut()
        .spawn(crate::net::ObjectStore(ObjectFields::from_pairs(&[
            (47, SAP),
            (95, 0x0E),
        ])))
        .id();
    app.update();
    assert!(
        app.world().entity(unit).get::<AuraNodes>().is_none(),
        "an unmodelled proc invents no body change"
    );
}

// ── The render authoring ─────────────────────────────────────────────────────────────────────────

/// The author owns a translucent unit's parts **through the whole descendant tree** (a held weapon
/// hangs off a joint several levels down, and the reference's alpha is a per-CGUnit property its
/// attached models render off), writes the product into the tag's alpha field, and moves the part
/// onto its blend twin — without which a cutout batch ignores the alpha entirely and B114's symptom
/// survives the fix. On the frame the ramp latches opaque it releases: settled alpha, steady
/// material, and the part re-queued for the classifier.
#[test]
fn the_author_owns_the_tree_then_releases_at_opaque() {
    use crate::model_fade::FadeMaterials;
    use bevy::mesh::MeshTag;

    let cutout: Handle<crate::terrain::WowModelMaterial> =
        bevy::asset::uuid_handle!("c0000000-0000-4000-8000-00000000a114");
    let blend: Handle<crate::terrain::WowModelMaterial> =
        bevy::asset::uuid_handle!("b1000000-0000-4000-8000-00000000a114");

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.init_resource::<crate::interior::InteriorReauthor>();
    app.add_systems(Update, apply_aura_alpha);

    let fm = FadeMaterials {
        cutout: cutout.clone(),
        blend: blend.clone(),
        bake_blend: None,
        zfill: None,
    };
    // root → body part, and root → joint → held weapon (the depth the walk must reach).
    let mut nodes = AuraNodes::new(1.0);
    install(&mut nodes, STEALTH, &[AuraNode::Alpha(0.3)], 0.0);
    let root = app.world_mut().spawn(nodes).id();
    let body = app
        .world_mut()
        .spawn((
            fm.clone(),
            MeshTag(0),
            MeshMaterial3d(cutout.clone()),
            ChildOf(root),
        ))
        .id();
    let joint = app.world_mut().spawn(ChildOf(root)).id();
    let weapon = app
        .world_mut()
        .spawn((
            fm.clone(),
            MeshTag(0),
            MeshMaterial3d(cutout.clone()),
            ChildOf(joint),
        ))
        .id();

    // Run the ramp out to its end: `MinimalPlugins`' clock advances a hair per update, so drive the
    // component's own clock instead of guessing frame counts.
    app.update();
    {
        let mut root_mut = app.world_mut().entity_mut(root);
        let mut n = root_mut.get_mut::<AuraNodes>().unwrap();
        n.started = -10.0; // the ramp finished 10 s ago
    }
    app.update();

    for part in [body, weapon] {
        let tag = app.world().entity(part).get::<MeshTag>().unwrap().0;
        assert!(
            (crate::mesh_tag::alpha_of(tag) - 0.3).abs() < 1.0 / 63.0,
            "part {part} carries the aura alpha (tag {tag:#x})"
        );
        assert_eq!(
            app.world()
                .entity(part)
                .get::<MeshMaterial3d<crate::terrain::WowModelMaterial>>()
                .unwrap()
                .0,
            blend,
            "part {part} draws on the blend twin while translucent"
        );
    }

    // The aura drops and the ramp completes: the release frame restores opacity and the steady
    // material, and stops authoring.
    {
        let mut root_mut = app.world_mut().entity_mut(root);
        let mut n = root_mut.get_mut::<AuraNodes>().unwrap();
        n.alpha.clear();
        n.retarget(0.0);
        n.started = -10.0;
    }
    app.update();
    for part in [body, weapon] {
        let tag = app.world().entity(part).get::<MeshTag>().unwrap().0;
        assert!((crate::mesh_tag::alpha_of(tag) - 1.0).abs() < 1.0 / 63.0);
        assert_eq!(
            app.world()
                .entity(part)
                .get::<MeshMaterial3d<crate::terrain::WowModelMaterial>>()
                .unwrap()
                .0,
            cutout,
            "released back to the part's steady law"
        );
    }
    assert!(
        !app.world()
            .entity(root)
            .get::<AuraNodes>()
            .unwrap()
            .authoring,
        "and stops claiming the channel"
    );
}

// ── The tint's render channel (decision 0812) ─────────────────────────────────────────────────────

/// **The tint end to end**: the ghost aura's CharProc-1 head node lands in the per-instance table at
/// the unit's own rig slot, packed the way the reference packs it (`param | 0xff000000`) — and the
/// aura dropping puts the slot back to identity, so the body goes back to its own colours.
#[test]
fn the_ghost_tint_reaches_the_instance_table_and_clears_with_the_aura() {
    let mut app = app_with_tint_channel();
    let skin = crate::rig_palette::RigSkin::allocate_bones(
        app.world_mut()
            .resource_mut::<crate::rig_palette::RigPalettes>()
            .as_mut(),
        8,
        Handle::default(),
    )
    .expect("a fresh palette has room");
    let slot = skin.slot;
    assert!(slot >= 1, "slot 0 is the world's no-rig sentinel");
    let unit = app
        .world_mut()
        .spawn((
            crate::net::ObjectStore(ObjectFields::from_pairs(&[(47, GHOST), (95, 0x0E)])),
            skin,
        ))
        .id();

    app.update();
    let tints = app
        .world()
        .resource::<crate::instance_tint::InstanceTints>();
    assert_eq!(
        tints.get(slot),
        0xff8c_b9fd,
        "the ghost's pale blue-white, alpha byte and all"
    );

    // The aura leaves its slot: the reference drops the tint node (`0x5ff320`) with no ease, and the
    // channel goes back to identity — which is the shader's no-op word, not a colour.
    app.world_mut()
        .entity_mut(unit)
        .insert(crate::net::ObjectStore(ObjectFields::from_pairs(&[(
            95, 0x00,
        )])));
    app.update();
    let tints = app
        .world()
        .resource::<crate::instance_tint::InstanceTints>();
    assert_eq!(tints.get(slot), crate::instance_tint::IDENTITY);
}

/// A tinted unit going away must not leave its colour on the slot for whoever allocates it next —
/// the leak that would paint an unrelated creature ghost-blue. Covered structurally by `RigSkin`'s
/// free hook, so it holds for a despawn AND for a gear/display rebuild that replaces the component.
#[test]
fn the_rig_free_hook_clears_a_dead_units_tint() {
    let mut app = app_with_tint_channel();
    let skin = crate::rig_palette::RigSkin::allocate_bones(
        app.world_mut()
            .resource_mut::<crate::rig_palette::RigPalettes>()
            .as_mut(),
        8,
        Handle::default(),
    )
    .expect("a fresh palette has room");
    let slot = skin.slot;
    let unit = app
        .world_mut()
        .spawn((
            crate::net::ObjectStore(ObjectFields::from_pairs(&[(47, GHOST), (95, 0x0E)])),
            skin,
        ))
        .id();
    app.update();
    assert_eq!(
        app.world()
            .resource::<crate::instance_tint::InstanceTints>()
            .get(slot),
        0xff8c_b9fd,
    );

    // Despawn without any aura edge — the unit streamed out mid-buff, which is the common case.
    app.world_mut().entity_mut(unit).despawn();
    assert_eq!(
        app.world()
            .resource::<crate::instance_tint::InstanceTints>()
            .get(slot),
        crate::instance_tint::IDENTITY,
        "the slot is clean before anyone can reallocate it",
    );
}
