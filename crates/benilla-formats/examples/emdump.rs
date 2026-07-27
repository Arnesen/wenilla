//! `emdump <internal\path.m2>` — every particle emitter one model authors, one line each.
//!
//! The per-emitter counterpart to `benilla-extract partcensus`, which answers "which models in the
//! corpus carry emitters matching this flag mask?" but never what an individual emitter *is*. B16
//! needed the other direction — the voidwalker's eye glow is emitters 1 and 2 of four, bone 60/61,
//! flags `0x0001` (plain anchored), `Add`/`GLOWBALL.BLP` — and reading that off a hex dump by hand
//! is exactly the kind of asset fact that should be one command.
//!
//! The header line carries the model's **reach** (how far its own transparent-pass batches sort
//! from its origin — `m2_owner_reach`) and the `Transparent3d` draw-order rung it produces
//! (`owner_last_rung`): the rung is what keeps a model's effects drawing after its own transparent
//! batches, and it is sized from that number, so the two belong on screen together.

fn main() -> anyhow::Result<()> {
    let virt = std::env::args().nth(1).unwrap();
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    let mut chain = benilla_formats::open_chain(&data)?;
    let bytes = chain.read_file(&virt)?;
    let dir = virt.rsplit_once('\\').map_or("", |(d, _)| d);
    let subs = benilla_formats::parse_m2_render_submeshes(&bytes, dir, &[]).unwrap_or_default();
    // The renderer's own bound and rung, not a re-derivation of them: a survey tool has to print
    // what the renderer will actually do.
    let reach = benilla_formats::m2_owner_reach(&subs);
    let rung = benilla_formats::owner_last_rung(reach);
    println!("{virt}: reach {reach:.3} yd -> draw-order rung {rung:.0}");
    for (i, e) in benilla_formats::parse_m2_particle_emitters(&bytes)?
        .iter()
        .enumerate()
    {
        println!(
            "emitter {i}: bone {:>3} flags {:#06x} model_space={} pos ({:.4},{:.4},{:.4}) blend {:?} \
             shape {:?} speed {:.4}±{:.4} grav {:.4} life {:.4} tex {:?}",
            e.bone,
            e.flags,
            e.model_space(),
            e.position[0],
            e.position[1],
            e.position[2],
            e.blend,
            e.shape,
            e.emission_speed,
            e.speed_variation,
            e.gravity,
            e.lifespan,
            e.texture,
        );
    }
    Ok(())
}
