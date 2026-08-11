//! List every file in the 5875 patch chain whose path contains a substring (case-insensitive):
//! `cargo run -p benilla-formats --example list_chain -- maraudon`. The "what is this thing
//! actually called" tool — a report names a *place* ("the red crystal in Maraudon"), and finding
//! the asset behind it is the first step of every scene-render diagnosis.
//!
//! Output is Blizzard data — pipe it to the scratchpad, never into the repo.

fn main() -> anyhow::Result<()> {
    let pat = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: list_chain <substring> [ext]"))?
        .to_lowercase();
    // An optional second argument filters by extension — `list_chain maraudon m2`.
    let ext = std::env::args()
        .nth(2)
        .map(|e| format!(".{}", e.to_lowercase()));
    let data = benilla_formats::wow_data().expect("no WoW install found (set $WOW_DATA)");
    let chain = benilla_formats::open_chain(&data)?;
    for e in chain.list()? {
        let lower = e.name.to_lowercase();
        if lower.contains(&pat) && ext.as_ref().is_none_or(|x| lower.ends_with(x)) {
            println!("{:>9}  {}", e.size, e.name);
        }
    }
    Ok(())
}
