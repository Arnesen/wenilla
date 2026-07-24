//! Dump any file from the 5875 patch chain to stdout — the generic MPQ extractor for
//! reference reads (FrameXML sources, DBC layouts): `cargo run -p benilla-formats
//! --example dump_file -- 'Interface\FrameXML\TradeSkillFrame.lua'`. Output is Blizzard
//! data — pipe it to the scratchpad, never into the repo.

use std::io::Write;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let virt = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: dump_file <virtual\\mpq\\path>"))?;
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    let mut chain = benilla_formats::open_chain(&data)?;
    let bytes = chain.read_file(&virt)?;
    std::io::stdout().write_all(&bytes)?;
    Ok(())
}
