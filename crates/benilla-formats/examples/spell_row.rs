//! Throwaway: print the cast-arm targeting facts for named spell ids — `Targets`,
//! `EffectImplicitTargetA[0]`, the three effects, and the item-target gate columns — so a claim
//! about which targeting seam a spell arms can be checked against the real shipped Spell.dbc
//! instead of reasoned about.
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
            "{id} {:?}\n  Targets={:#06x} implicitA1={} effect_1={}\n  equippedClass={} subclassMask={:#x} invTypeMask={:#x}",
            d.name,
            d.targets,
            d.implicit_target_a1,
            d.effect_1,
            d.equipped_item_class,
            d.equipped_item_subclass_mask,
            d.equipped_item_inventory_type_mask,
        );
    }
    Ok(())
}
