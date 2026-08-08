//! Throwaway: the sort/admission inputs the trainer & craft windows key on. Two modes:
//!
//! * `line <skill-line-name-substring>...` — a line's abilities with their raw
//!   `SkillLineAbility.dbc` columns (`req_skill_value` col 7, `reqtrainpoints` col 14), their
//!   `Spell.dbc` name/rank, `castUI`, `attributes` and `spellLevel`.
//! * `castui <n>` — every spell whose `castUI == n`, with the skill line (if any) its first
//!   `SkillLineAbility` row names: the client's craft-admission key vs benilla's SLA join.
fn main() -> anyhow::Result<()> {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    let mut chain = benilla_formats::open_chain(&data)?;
    let skills = benilla_formats::load_skill_line_catalog(&mut chain)?;
    let spells = benilla_formats::load_spell_catalog(&mut chain)?;

    // Raw SkillLineAbility rows: (skillId, spellId, req_skill_value, reqtrainpoints).
    let sla = chain.read_file("DBFilesClient\\SkillLineAbility.dbc")?;
    let g = |o: usize| u32::from_le_bytes(sla[o..o + 4].try_into().unwrap());
    let (rows, _fields, rec) = (g(4) as usize, g(8) as usize, g(12) as usize);
    let raw: Vec<(u32, u32, u32, u32)> = (0..rows)
        .map(|r| {
            let b = 20 + r * rec;
            (g(b + 4), g(b + 8), g(b + 28), g(b + 56))
        })
        .collect();

    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_default();
    let rest: Vec<String> = args.map(|s| s.to_lowercase()).collect();

    if mode == "castui" {
        let want: u32 = rest[0].parse()?;
        let mut hits = 0;
        for spell in 1..30000u32 {
            let Some(d) = spells.get(spell) else { continue };
            if d.cast_ui != want {
                continue;
            }
            hits += 1;
            let line = skills.spell_to_line(spell).unwrap_or(0);
            let hidden = d.attributes & benilla_formats::SPELL_ATTR_IS_TRADESKILL != 0;
            println!(
                "  {spell:>6} line={line:<5} attr20={} lvl={:<3} {} ({})",
                u8::from(hidden),
                d.spell_level,
                d.name,
                d.rank.as_deref().unwrap_or("")
            );
        }
        println!("castUI == {want}: {hits} spells");
        return Ok(());
    }

    let mut lines: Vec<(u32, String)> = Vec::new();
    for id in 1..1200u32 {
        if let Some(l) = skills.line(id) {
            let lower = l.name.to_lowercase();
            if rest.iter().any(|w| lower.contains(w)) {
                lines.push((id, l.name.clone()));
            }
        }
    }
    for (id, name) in &lines {
        let mut mine: Vec<&(u32, u32, u32, u32)> = raw.iter().filter(|r| r.0 == *id).collect();
        mine.sort_by_key(|r| r.1);
        println!("== skill line {id}: {name} — {} SLA rows", mine.len());
        for &&(_, spell, req, tp) in mine.iter().take(120) {
            let d = spells.get(spell);
            let n = d.map(|d| d.name.as_str()).unwrap_or("?");
            let rank = d.and_then(|d| d.rank.as_deref()).unwrap_or("");
            println!(
                "   spell {spell:>6}  req={req:<4} tp={tp:<4} castUI={:<2} attr20={} lvl={:<3} {n} ({rank})",
                d.map(|d| d.cast_ui).unwrap_or(0),
                u8::from(d.is_some_and(|d| d.attributes
                    & benilla_formats::SPELL_ATTR_IS_TRADESKILL
                    != 0)),
                d.map(|d| d.spell_level).unwrap_or(0),
            );
        }
    }
    Ok(())
}
