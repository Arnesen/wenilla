//! Throwaway: print the interrupt-flag columns for named spell ids, so a claim about what
//! cancels a spell locally can be checked against the real shipped Spell.dbc.
fn main() -> anyhow::Result<()> {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    let mut chain = benilla_formats::open_chain(&data)?;
    let catalog = benilla_formats::load_spell_catalog(&mut chain)?;
    for arg in std::env::args().skip(1) {
        let id: u32 = arg.parse()?;
        let Some(d) = catalog.get(id) else {
            println!("{id}: no row");
            continue;
        };
        println!(
            "{id} {:?}\n  InterruptFlags={:#x} (movement bit 0x1 = {})\n  AuraInterruptFlags={:#x} ChannelInterruptFlags={:#x}\n  Attributes={:#x} AttributesEx={:#x} AttributesEx2={:#x}",
            d.name,
            d.interrupt_flags,
            d.interrupt_flags & 0x1 != 0,
            d.aura_interrupt_flags,
            d.channel_interrupt_flags,
            d.attributes,
            d.attributes_ex,
            d.attributes_ex2,
        );
    }
    Ok(())
}
