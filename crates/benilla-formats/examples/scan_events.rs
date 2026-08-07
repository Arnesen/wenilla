//! Which models author a given M2 animation event? `cargo run -p benilla-formats --example
//! scan_events -- '$CCH' weapon` — scans every `.m2` in the 5875 chain whose path contains the
//! (optional, case-insensitive) substring and prints each model authoring the 4CC, with the
//! event's bone and raw WoW position. The sibling of `dump_attach`: that answers "what does THIS
//! model author", this answers "who else authors THIS event" — the question every event-keyed
//! consumer (bowstring `$WTT`, fishing line `$CCH`, GO sound slots `$GC0`) eventually asks when
//! deciding whether the ident alone is a safe gate.
//!
//! Output is Blizzard data — pipe it to the scratchpad, never into the repo.

fn le_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn le_f32(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn main() -> anyhow::Result<()> {
    let ident = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: scan_events <4cc, e.g. $CCH> [path-substring]"))?;
    anyhow::ensure!(ident.len() == 4, "the event ident is exactly 4 bytes");
    let pat = std::env::args().nth(2).unwrap_or_default().to_lowercase();
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    let chain = benilla_formats::open_chain(&data)?;
    let (mut scanned, mut hits) = (0usize, 0usize);
    for e in chain.list()? {
        let lower = e.name.to_lowercase();
        if !lower.ends_with(".m2") || !lower.contains(&pat) {
            continue;
        }
        let Ok(b) = chain.read(&e.name) else { continue };
        if b.len() < 0x11c || &b[0..4] != b"MD20" {
            continue;
        }
        scanned += 1;
        let (count, ofs) = (le_u32(&b, 0x114) as usize, le_u32(&b, 0x118) as usize);
        for i in 0..count {
            let rec = ofs + i * 44;
            if rec + 44 > b.len() {
                break;
            }
            if &b[rec..rec + 4] == ident.as_bytes() {
                hits += 1;
                println!(
                    "{}  bone {} pos ({:+.3},{:+.3},{:+.3})",
                    e.name,
                    le_u32(&b, rec + 8),
                    le_f32(&b, rec + 12),
                    le_f32(&b, rec + 16),
                    le_f32(&b, rec + 20),
                );
            }
        }
    }
    println!("-- {scanned} models scanned, {hits} authoring {ident}");
    Ok(())
}
