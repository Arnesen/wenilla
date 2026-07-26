//! Print a model's particle-emitter records — the authored numbers to hold a live
//! `WOW_PARTICLE_CENSUS` line against: `cargo run -p benilla-formats --example dump_emitters --
//! 'World\...\RubyCrystalLarge01.m2'`.
//!
//! The pair is the check that matters (decision 0653): the census says how many particles are
//! *live*, this says how many the file asks for (`rate × lifespan`), and the over-life ramp says
//! how big they get. A mismatch is our sim's bug; a match moves the question to the look.
//!
//! Output is Blizzard data — pipe it to the scratchpad, never into the repo.

fn main() -> anyhow::Result<()> {
    let virt = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: dump_emitters <m2 path>"))?;
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    let mut chain = benilla_formats::open_chain(&data)?;
    let bytes = chain.read_file(&virt)?;
    for (i, e) in benilla_formats::parse_m2_particle_emitters(&bytes)?
        .iter()
        .enumerate()
    {
        println!(
            "emitter[{i}] flags {:#x} bone {} shape {:?} blend {:?}",
            e.flags, e.bone, e.shape, e.blend
        );
        println!(
            "  texture {:?} tiles {}x{} head_tail {}",
            e.texture, e.tile_rows, e.tile_cols, e.head_tail
        );
        println!(
            "  speed {} var {} vrange {} hrange {} gravity {} life {} drag {}",
            e.emission_speed,
            e.speed_variation,
            e.vertical_range,
            e.horizontal_range,
            e.gravity,
            e.lifespan,
            e.drag
        );
        match e.timing.constant_rate() {
            Some(r) => println!("  rate {r}/s (constant, every sequence)"),
            None => {
                for (slot, (looping, rate, enabled)) in e.timing.slot_views().iter().enumerate() {
                    println!(
                        "  seq {slot}{}: rate {:?} enabled {:?}",
                        if *looping { "" } else { " (clamped)" },
                        rate,
                        enabled
                    );
                }
            }
        }
        println!(
            "  area {}x{} zsrc {} tail {} spin {}",
            e.area_length, e.area_width, e.z_source, e.tail_time, e.spin
        );
        println!(
            "  twinkle speed {} pct {} min {} max {}",
            e.twinkle_speed, e.twinkle_percent, e.twinkle_min, e.twinkle_max
        );
        println!("  over_life {:?}", e.over_life);
    }
    Ok(())
}
