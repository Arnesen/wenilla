//! `gseqdump <internal\path.m2>` — every global-sequence bone channel with its RAW KEYS.
//!
//! `benilla-extract m2anim` names which bones carry global-sequence channels and their periods,
//! but never the key VALUES — and for a twinkle (a scale loop on a star card) the values ARE the
//! effect: a 0→1 flicker and a 0→20 flare are the same one-line summary. Output is Blizzard
//! data — pipe it to the scratchpad, never into the repo.

fn main() -> anyhow::Result<()> {
    let virt = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: gseqdump <m2 path>"))?;
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    let mut chain = benilla_formats::open_chain(&data)?;
    let bytes = chain.read_file(&virt)?;
    for g in benilla_formats::parse_m2_global_sequence_bones(&bytes) {
        println!("bone {}", g.bone);
        if let Some(t) = &g.translation {
            println!("  T period {}ms keys {:?}", t.period_ms, t.keys);
        }
        if let Some(r) = &g.rotation {
            println!("  R period {}ms keys {:?}", r.period_ms, r.keys);
        }
        if let Some(s) = &g.scale {
            println!("  S period {}ms keys {:?}", s.period_ms, s.keys);
        }
    }
    Ok(())
}
