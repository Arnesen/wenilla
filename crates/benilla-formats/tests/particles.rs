//! Difftest the vanilla M2 particle-emitter parser against the real `ElwynnCampfire.m2` (the
//! Goldshire-area campfire). Skips (passes) when the client isn't present at `<repo>/WoW/Data`.

use std::path::PathBuf;

use benilla_formats::{open_chain, parse_m2_particle_emitters, ParticleBlend, ParticleShape};

fn vanilla_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
}

#[test]
fn campfire_emitters_match_real_bytes() {
    let data = vanilla_data_dir();
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let mut chain = open_chain(&data).expect("open vanilla patch chain");
    let bytes = chain
        .read_file("World\\Azeroth\\Elwynn\\PassiveDoodads\\Campfire\\ElwynnCampfire.m2")
        .expect("read ElwynnCampfire.m2");

    let emitters = parse_m2_particle_emitters(&bytes).expect("parse emitters");

    // The campfire has exactly two additive plane emitters: a wide slow glow/smoke plume and a fast
    // narrow flame with a 4×4 cell flicker. These values are read straight off the real file.
    assert_eq!(emitters.len(), 2, "campfire has two emitters");

    for e in &emitters {
        assert_eq!(e.shape, ParticleShape::Plane);
        assert_eq!(e.blend, ParticleBlend::Add);
        assert!(e.lifespan > 0.0 && e.lifespan.is_finite());
        assert!(e.emission_rate.first() > 0.0 && e.emission_rate.first().is_finite());
        assert_eq!(
            e.emission_rate.keys.len(),
            1,
            "ambient prop rates are constant tracks"
        );
        assert!(e.horizontal_range > 6.0, "campfire emits in a full ring");
        // Texture resolves to a real .blp via the M2 textures table.
        assert!(
            e.texture.as_deref().is_some_and(|t| !t.is_empty()),
            "emitter texture resolves, got {:?}",
            e.texture
        );
    }

    // Glow/smoke plume: wide 20° cone, long life, low rate, single cell.
    let glow = &emitters[0];
    assert!(
        (glow.lifespan - 4.0).abs() < 1e-3,
        "glow lifespan ~4.0, got {}",
        glow.lifespan
    );
    assert!(
        (glow.emission_rate.first() - 6.0).abs() < 1e-3,
        "glow rate ~6, got {}",
        glow.emission_rate.first()
    );
    assert_eq!((glow.tile_rows, glow.tile_cols), (1, 1));
    assert!(glow.vertical_range > 0.3, "glow has a wide cone");

    // Flame: short life, high rate, narrow cone, 4×4 cell animation.
    let flame = &emitters[1];
    assert!(
        (flame.lifespan - 1.5).abs() < 1e-3,
        "flame lifespan ~1.5, got {}",
        flame.lifespan
    );
    assert!(
        (flame.emission_rate.first() - 20.0).abs() < 1e-3,
        "flame rate ~20, got {}",
        flame.emission_rate.first()
    );
    assert_eq!(
        (flame.tile_rows, flame.tile_cols),
        (4, 4),
        "flame has a 4×4 flicker atlas"
    );
    assert!(flame.vertical_range < 0.2, "flame is a tight upward jet");

    // Drag (file +0x194): the velocity-decay term the verified integrator applies as
    // `vel −= min(dt·drag, 1)·vel`. The campfire's smoke/glow plume carries a gentle 0.5 (contained
    // column); its short-lived flame carries 0.0 (a free upward jet). Read straight off the real
    // bytes — the candelabra props instead author a strong 10.0 and rely on it to stay a flicker
    // (decision 0027).
    assert!(
        (glow.drag - 0.5).abs() < 1e-3,
        "campfire glow drag ~0.5, got {}",
        glow.drag
    );
    assert!(
        flame.drag == 0.0,
        "campfire flame drag 0, got {}",
        flame.drag
    );

    // Over-life ramps (verified tail). Dump the sampled color/size/cell across life, and assert the
    // believability invariants: a fading additive weight (A) and a sensible size in yards.
    eprintln!("campfire emitters OK:");
    for (i, e) in emitters.iter().enumerate() {
        let ol = &e.over_life;
        eprintln!(
            "  [{i}] {:?} {:?} tex={:?} life={} rate={:?} cone={:.3} tiles={}x{}",
            e.shape,
            e.blend,
            e.texture,
            e.lifespan,
            e.emission_rate.keys,
            e.vertical_range,
            e.tile_rows,
            e.tile_cols
        );
        eprintln!(
            "      mid={:.2} color={:?} scale={:?} cells A{:?} B{:?}",
            ol.mid, ol.color, ol.scale, ol.cell_a, ol.cell_b
        );
        for u in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
            let (c, s, cell) = ol.sample(u);
            eprintln!("      u={u:.2}: rgba={c:?} size={s:.3} cell={cell}");
        }

        // Sizes are finite, non-negative, and small (campfire props are ~sub-yard to a couple yards).
        for s in ol.scale {
            assert!(
                s.is_finite() && (0.0..8.0).contains(&s),
                "emitter {i} scale {s} sane"
            );
        }
        // Color/alpha keys are valid 0..1.
        for k in ol.color {
            for ch in k {
                assert!(
                    (0.0..=1.0).contains(&ch),
                    "emitter {i} color channel {ch} in 0..1"
                );
            }
        }
        assert!(
            (0.0..=1.0).contains(&ol.mid),
            "emitter {i} midPoint in 0..1"
        );
    }

    // The glow/smoke plume fades out: its additive weight (alpha) at end-of-life is below its peak.
    let glow_ol = &emitters[0].over_life;
    let a_start = glow_ol.sample(0.0).0[3];
    let a_end = glow_ol.sample(1.0).0[3];
    assert!(
        a_end <= a_start,
        "glow alpha should not rise over life ({a_start} -> {a_end})"
    );

    // The flame's cell index advances across its 4×4 atlas over life (the flicker animation).
    let flame_ol = &emitters[1].over_life;
    let cell_start = flame_ol.sample(0.0).2;
    let cell_end = flame_ol.sample(1.0).2;
    assert!(
        cell_end >= cell_start,
        "flame cell advances ({cell_start} -> {cell_end})"
    );
}

/// The record-tail **twinkle** fields (wow-re `part-simspace-fields.md`, their `ac915a7d`):
/// file +0x188/+0x18c are twinkleScale **{min, max}** — a GATED per-frame size flicker, skipped
/// when the range is degenerate — NOT a spawn-time size multiplier. The discriminating real-data
/// case is the kobold candle: it authors `{0, 0}` and burns in the reference client, which the old
/// `base + rand·variation` reading collapsed to size zero (the director's "candles not burning").
#[test]
fn twinkle_fields_gate_not_scale() {
    let data = vanilla_data_dir();
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let mut chain = open_chain(&data).expect("open vanilla patch chain");

    // Kobold candle: twinkle {0,0} — degenerate range, the multiplier must be identity.
    let kobold = parse_m2_particle_emitters(
        &chain
            .read_file("Creature\\Kobold\\Kobold.m2")
            .expect("read Kobold.m2"),
    )
    .expect("parse kobold emitters");
    assert_eq!(kobold.len(), 1, "kobold has one candle emitter");
    let candle = &kobold[0];
    assert_eq!((candle.twinkle_min, candle.twinkle_max), (0.0, 0.0));
    assert_eq!(
        candle.twinkle(0.7),
        1.0,
        "degenerate {{0,0}} twinkle is identity — the candle burns at ramp size"
    );
    // Its base size is the over-life ramp alone — nonzero at mid-life.
    assert!(
        candle.over_life.sample(0.5).1 > 0.0,
        "the candle flame's over-life size ramp is nonzero"
    );

    // Campfire glow plume: twinkle {0, 1} — an active flicker range; samples lerp min..max.
    let campfire = parse_m2_particle_emitters(
        &chain
            .read_file("World\\Azeroth\\Elwynn\\PassiveDoodads\\Campfire\\ElwynnCampfire.m2")
            .expect("read ElwynnCampfire.m2"),
    )
    .expect("parse campfire emitters");
    let glow = &campfire[0];
    assert_eq!((glow.twinkle_min, glow.twinkle_max), (0.0, 1.0));
    assert_eq!(glow.twinkle(0.25), 0.25, "active range lerps min..max");
    // A degenerate NON-ZERO range is also identity ({1,1} torches burn steady, not 1–2× inflated).
    assert!(glow.twinkle_percent.is_finite() && glow.twinkle_speed.is_finite());
}

/// The file→runtime flag remap (wow-re `part-simspace-fields.md` corrections `1f40db0b`, loader
/// block `0x70faf8–0x70fc44`): the space switch is FILE bit 0x10 (→ rt 0x100), the size-by-scale
/// enable FILE 0x20 (→ rt 0x200) — pinned on real content whose behavior the reference shows:
/// the kobold candle (0x01) is carried with no trail and un-flagged for both; the swinging
/// chandelier's candle flames (0x11/0x15) are model-space (they rigidly ride the swing); the
/// campfire (0x21/0x29) scales its flame size with the placement.
#[test]
fn flag_remap_reads_the_file_bits() {
    let data = vanilla_data_dir();
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let mut chain = open_chain(&data).expect("open vanilla patch chain");

    let kobold =
        parse_m2_particle_emitters(&chain.read_file("Creature\\Kobold\\Kobold.m2").unwrap())
            .unwrap();
    assert_eq!(kobold[0].flags, 0x01);
    assert!(!kobold[0].model_space() && !kobold[0].scale_size_by_instance());

    let chandelier = parse_m2_particle_emitters(
        &chain
            .read_file("World\\Dungeon\\GoldshireInn\\InnChandelier\\InnChandelier.m2")
            .unwrap(),
    )
    .unwrap();
    assert!(
        chandelier.iter().take(6).all(|e| e.model_space()),
        "the swinging candle flames are model-space (file bit 0x10)"
    );

    let campfire = parse_m2_particle_emitters(
        &chain
            .read_file("World\\Azeroth\\Elwynn\\PassiveDoodads\\Campfire\\ElwynnCampfire.m2")
            .unwrap(),
    )
    .unwrap();
    assert!(
        campfire
            .iter()
            .all(|e| e.scale_size_by_instance() && !e.model_space()),
        "campfire: size-by-scale (0x20) set, model-space (0x10) clear"
    );
}
