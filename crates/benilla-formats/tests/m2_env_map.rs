//! Difftest the M2 **generated-texcoord** (environment-map) detection against real art — the
//! `texCoordSet(+0x12) → texture_unit_lookup(0x9c)` two-hop the reference gates at `0x70b8bd`
//! (`<= 2` = a vertex UV channel, higher = a generated env coordinate).
//!
//! Both halves matter, and only real art proves them. The Deeprun Tram's glass tube is the
//! **positive**: it authors `texture_unit_lookup = [-1]` and, precisely because the runtime is
//! meant to supply the coordinates, leaves **every one of its 330 vertices at exactly (0,0)** — so
//! a renderer that misses the flag paints the whole tube in one corner texel of a reflection sheet
//! (`AKGNOMEREFLECT.BLP` texel 0,0 = 225,221,142, doubled by the batch's Mod2x blend: the flat
//! yellow). The weapon rack is the **discriminator**: the same model carries both kinds, so a
//! parse that simply answered "env" everywhere would pass the first assert and fail here.
//!
//! Skips (passes) when the client isn't present at `<repo>/WoW/Data`.

use std::path::PathBuf;

use benilla_formats::{load_m2_mesh, open_chain};

fn vanilla_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
}

#[test]
fn tram_glass_batches_generate_their_texcoords() {
    let data = vanilla_data_dir();
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let mut chain = open_chain(&data).expect("open vanilla patch chain");
    let subs = load_m2_mesh(
        &mut chain,
        "World\\Generic\\Gnome\\Passive Doodads\\GnomeMachine\\GnomeSubwayGlass.m2",
    )
    .expect("load GnomeSubwayGlass");

    assert!(!subs.is_empty(), "the glass tube has render batches");
    assert!(
        subs.iter().all(|s| s.env_map),
        "every GnomeSubwayGlass batch authors texture_unit_lookup = -1 (generated texcoords)"
    );
    // The half that makes the flag load-bearing rather than cosmetic: there is no fallback here,
    // because the authored UVs carry no information at all.
    for s in &subs {
        assert!(
            s.uvs.iter().all(|uv| uv[0] == 0.0 && uv[1] == 0.0),
            "an env-mapped batch's authored UVs are degenerate — the runtime supplies them"
        );
    }
}

#[test]
fn weapon_rack_splits_env_from_uv_batches() {
    let data = vanilla_data_dir();
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let mut chain = open_chain(&data).expect("open vanilla patch chain");
    let subs = load_m2_mesh(
        &mut chain,
        "World\\Generic\\Human\\Passive Doodads\\WeaponRacks\\GeneralWeaponrack01.m2",
    )
    .expect("load GeneralWeaponrack01");

    // `texture_unit_lookup = [0, -1]`: the rack/blade batches name UV channel 0, the two
    // ARMORREFLECT sheen layers (`texCoordSet 1`) generate theirs.
    let env: Vec<&str> = subs
        .iter()
        .filter(|s| s.env_map)
        .filter_map(|s| s.texture.as_deref())
        .collect();
    assert!(
        !env.is_empty()
            && env
                .iter()
                .all(|t| t.to_ascii_lowercase().contains("armorreflect")),
        "only the ARMORREFLECT sheen layers env-map on the rack, got {env:?}"
    );
    assert!(
        subs.iter().any(|s| !s.env_map),
        "the rack's body and blade batches read their authored UVs"
    );
}
