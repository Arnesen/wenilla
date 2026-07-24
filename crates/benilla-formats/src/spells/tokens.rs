//! The spell-description **$-token engine** (decision 0274 P2) — the substitution the real
//! client runs over `Spell.dbc` Description/AuraDescription text (and item trigger lines), with
//! the value formulas byte-verified by the 0276 fold-back (wow-re `tooltip-content-law.md`,
//! `0x5075f0 → 0x507710`, the effect-value core `0x6e3800`):
//!
//! - `$s` (and `$m`/`$M`): `MIN = BasePoints + BaseDice`, `MAX = BasePoints + DieSides·BaseDice`
//!   — the general n-dice rule (the common `BaseDice = 1` case reduces to `base+1 … base+dieSides`).
//!   `$s` prints one value when `MIN == MAX`, else `"MIN to MAX"`. The byte law also carries a
//!   per-level term (`DicePerLevel·max(0, casterLevel − spellLevel)` inside both bounds, plus an
//!   uncaptured `RealPointsPerLevel` float term); those columns aren't parsed yet — INTERIM the
//!   flat value, which is exact for the overwhelming majority of 1.12 rows (per-level dice are
//!   rare). Values print sign-absolute: the client shows "causes 12 damage", not "-12".
//! - `$o`: the over-time total `perTick · duration / period` (period = `EffectAmplitude`,
//!   defaulting 5000 ms when 0 — the byte default).
//! - `$d`: the duration via `SpellDuration.dbc` — "until cancelled" when permanent; whole
//!   seconds/minutes/hours text (INTERIM shape pending the `0x52fa50` formatter's pin).
//! - `$t` period seconds · `$a` radius yards (`SpellRadius.dbc`) · `$h` proc chance · `$x` chain
//!   targets · `$e` the multiple-value float · `$r` range yards · `$u` stack/charge count
//!   (unparsed — leaves the token in place, a visible fold-back flag).
//! - Cross-spell refs `$<id><token><idx>` (e.g. `$1234s1`) resolve through the caller's lookup.
//! - `$/N;`/`$*N;` divide/multiply the following token's value by N.
//! - `$l<singular>:<plural>;` picks by the last substituted numeric value; `$g<m>:<f>;` renders
//!   the first (male) form until a caster-gender input exists.
//!
//! Unknown tokens pass through untouched (visible, greppable) rather than vanishing.

use super::{SpellDisplay, SpellDurationCatalog, SpellRadiusCatalog};

/// The inputs one substitution runs over. `lookup` resolves cross-spell references (`$1234s1`).
pub struct TokenContext<'a> {
    pub durations: &'a SpellDurationCatalog,
    pub radii: &'a SpellRadiusCatalog,
    pub lookup: &'a dyn Fn(u32) -> Option<&'a SpellDisplay>,
    /// The player's home-bind AREA name ("Goldshire") — the `$z` token (the hearthstone text
    /// "Returns you to $z."). Fed from `SMSG_BINDPOINTUPDATE`'s areaId through AreaTable.dbc;
    /// `None` (no bind seen yet) leaves the token raw, like any unresolved token.
    pub home_area: Option<&'a str>,
}

/// The byte-verified effect bounds (`0x6e3800`, flat term): `(min, max)`.
fn effect_bounds(d: &SpellDisplay, slot: usize) -> (i64, i64) {
    let base = i64::from(*d.effect_base_points.get(slot).unwrap_or(&0));
    let dice = i64::from(*d.effect_base_dice.get(slot).unwrap_or(&0));
    let sides = i64::from(*d.effect_die_sides.get(slot).unwrap_or(&0));
    (base + dice, base + sides * dice)
}

/// A spell's duration in ms (flat term; -1 = permanent, None = no duration row).
fn duration_ms(d: &SpellDisplay, ctx: &TokenContext) -> Option<i64> {
    let row = ctx.durations.get(d.duration_index)?;
    Some(i64::from(row.base_ms))
}

/// Whole-unit duration text — INTERIM shape (the `0x52fa50` formatter's exact rendering is
/// unpinned): "N sec" / "N min" / "N hours", "until cancelled" for the permanent sentinel.
fn duration_text(ms: i64) -> String {
    if ms < 0 {
        return "until cancelled".into();
    }
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs} sec")
    } else if secs < 3600 {
        format!("{} min", secs / 60)
    } else {
        format!("{} hours", secs / 3600)
    }
}

/// Trim a float to the client's terse style (no trailing zeros: 2.5 → "2.5", 3.0 → "3").
fn trim_float(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

/// One token's substituted text + the numeric value the `$l` plural picker keys on.
fn token_value(
    letter: char,
    slot: usize,
    d: &SpellDisplay,
    ctx: &TokenContext,
    scale: f64,
) -> Option<(String, f64)> {
    let scaled = |v: i64| -> i64 {
        if scale == 1.0 {
            v
        } else {
            (v as f64 * scale).round() as i64
        }
    };
    match letter.to_ascii_lowercase() {
        's' => {
            let (min, max) = effect_bounds(d, slot);
            let (min, max) = (scaled(min.abs()), scaled(max.abs()));
            Some(if min == max {
                (min.to_string(), min as f64)
            } else {
                (format!("{min} to {max}"), max as f64)
            })
        }
        'm' if letter == 'm' => {
            let (min, _) = effect_bounds(d, slot);
            let v = scaled(min.abs());
            Some((v.to_string(), v as f64))
        }
        'm' => {
            // 'M'
            let (_, max) = effect_bounds(d, slot);
            let v = scaled(max.abs());
            Some((v.to_string(), v as f64))
        }
        'o' => {
            let (min, max) = effect_bounds(d, slot);
            let period = i64::from(*d.effect_amplitude.get(slot).unwrap_or(&0)).max(0);
            let period = if period == 0 { 5000 } else { period };
            let dur = duration_ms(d, ctx).unwrap_or(0).max(0);
            let total = |v: i64| scaled((v.abs() * dur / period).max(0));
            let (tmin, tmax) = (total(min), total(max));
            Some(if tmin == tmax {
                (tmin.to_string(), tmin as f64)
            } else {
                (format!("{tmin} to {tmax}"), tmax as f64)
            })
        }
        'd' => {
            let ms = duration_ms(d, ctx)?;
            let v = if ms < 0 { 0.0 } else { ms as f64 / 1000.0 };
            Some((duration_text(ms), v))
        }
        't' => {
            let period = i64::from(*d.effect_amplitude.get(slot).unwrap_or(&0));
            let period = if period == 0 { 5000 } else { period };
            let v = period as f64 / 1000.0;
            Some((trim_float(v), v))
        }
        'a' => {
            let idx = *d.effect_radius_index.get(slot).unwrap_or(&0);
            let r = ctx.radii.get(idx)?;
            Some((trim_float(f64::from(r.radius)), f64::from(r.radius)))
        }
        'h' => Some((d.proc_chance.to_string(), f64::from(d.proc_chance))),
        'x' => {
            let v = *d.effect_chain_targets.get(slot).unwrap_or(&0);
            Some((v.to_string(), f64::from(v)))
        }
        'e' => {
            let v = f64::from(*d.effect_multiple_value.get(slot).unwrap_or(&0.0));
            Some((trim_float(v), v))
        }
        'z' => {
            // Player state, not spell data: the home-bind area name (see TokenContext).
            let name = ctx.home_area?;
            Some((name.to_string(), 0.0))
        }
        'r' => {
            // The range's max yards — resolved by the caller's range catalog at view-build time
            // would be cleaner, but the token is rare in descriptions; the display carries the
            // index only, so leave unresolved here (pass through).
            None
        }
        _ => None,
    }
}

/// Substitute every `$`-token in `text` against `spell` (byte-verified formulas, module doc).
pub fn substitute(text: &str, spell: &SpellDisplay, ctx: &TokenContext) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut last_value: f64 = 0.0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&text[i..i + ch_len]);
            i += ch_len;
            continue;
        }
        let start = i;
        i += 1;
        // $/N; or $*N; — scale the next token.
        let mut scale = 1.0f64;
        if i < bytes.len() && (bytes[i] == b'/' || bytes[i] == b'*') {
            let op = bytes[i];
            let mut j = i + 1;
            let num_start = j;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b'.') {
                j += 1;
            }
            if let Ok(n) = text[num_start..j].parse::<f64>() {
                if j < bytes.len() && bytes[j] == b';' {
                    j += 1;
                }
                scale = if op == b'/' { 1.0 / n } else { n };
                i = j;
            }
        }
        // Optional cross-spell id digits.
        let id_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let ref_spell: Option<u32> = if i > id_start {
            text[id_start..i].parse().ok()
        } else {
            None
        };
        // The plural / gender selectors.
        if i < bytes.len()
            && (bytes[i] == b'l' || bytes[i] == b'L' || bytes[i] == b'g' || bytes[i] == b'G')
        {
            let selector = bytes[i].to_ascii_lowercase();
            if let Some(end) = text[i + 1..].find(';') {
                let body = &text[i + 1..i + 1 + end];
                if let Some((a, b)) = body.split_once(':') {
                    let pick = match selector {
                        b'l' => {
                            if (last_value - 1.0).abs() < 1e-9 {
                                a
                            } else {
                                b
                            }
                        }
                        _ => a, // $g: the male form until a caster-gender input exists
                    };
                    out.push_str(pick);
                    i = i + 1 + end + 1;
                    continue;
                }
            }
        }
        // The token letter + its optional 1-based slot digit.
        let Some(&letter_b) = bytes.get(i) else {
            out.push('$');
            continue;
        };
        let letter = letter_b as char;
        if !letter.is_ascii_alphabetic() {
            out.push_str(&text[start..i + 1]);
            i += 1;
            continue;
        }
        i += 1;
        let slot = if i < bytes.len() && bytes[i].is_ascii_digit() {
            let s = (bytes[i] - b'1') as usize;
            i += 1;
            s.min(2)
        } else {
            0
        };
        let target: &SpellDisplay = match ref_spell {
            Some(id) => match (ctx.lookup)(id) {
                Some(s) => s,
                None => {
                    out.push_str(&text[start..i]);
                    continue;
                }
            },
            None => spell,
        };
        match token_value(letter, slot, target, ctx, scale) {
            Some((sub, val)) => {
                last_value = val;
                out.push_str(&sub);
            }
            None => out.push_str(&text[start..i]), // unknown token: keep raw (fold-back flag)
        }
    }
    out
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spells::SpellDisplay;

    fn ctx<'a>(
        durations: &'a SpellDurationCatalog,
        radii: &'a SpellRadiusCatalog,
        lookup: &'a dyn Fn(u32) -> Option<&'a SpellDisplay>,
    ) -> TokenContext<'a> {
        TokenContext {
            home_area: None,
            durations,
            radii,
            lookup,
        }
    }

    fn none_lookup<'a>(_: u32) -> Option<&'a SpellDisplay> {
        None
    }

    /// The byte formula: base 13, dice 1, sides 9 → "14 to 22" (Fireball rank 1's shape); a
    /// diceless effect prints one value.
    #[test]
    fn s_token_bounds() {
        let durations = SpellDurationCatalog::default();
        let radii = SpellRadiusCatalog::default();
        let mut d = SpellDisplay {
            effect_base_points: [13, 24, 0],
            effect_base_dice: [1, 0, 0],
            effect_die_sides: [9, 0, 0],
            ..Default::default()
        };
        let c = ctx(&durations, &radii, &none_lookup);
        assert_eq!(
            substitute("causes $s1 Fire damage and $s2 more", &d, &c),
            "causes 14 to 22 Fire damage and 24 more"
        );
        // Negative base points print absolute (the client's "reduces by N" phrasing).
        d.effect_base_points = [-31, 0, 0];
        d.effect_base_dice = [1, 0, 0];
        d.effect_die_sides = [1, 0, 0];
        assert_eq!(
            substitute("reduces armor by $s1", &d, &c),
            "reduces armor by 30"
        );
    }

    /// $o totals per-tick over the duration; $d prints the duration; $t the period; the $/N
    /// scale divides; $l picks plural by the last value.
    #[test]
    fn overtime_duration_period_scale_plural() {
        let mut durations = SpellDurationCatalog::default();
        durations.insert_for_tests(1, 18_000);
        let radii = SpellRadiusCatalog::default();
        let d = SpellDisplay {
            duration_index: 1,
            effect_base_points: [2, 0, 0],
            effect_base_dice: [1, 0, 0],
            effect_die_sides: [1, 0, 0],
            effect_amplitude: [3000, 0, 0],
            ..Default::default()
        };
        let c = ctx(&durations, &radii, &none_lookup);
        assert_eq!(
            substitute("Deals $o1 damage over $d, every $t1 sec.", &d, &c),
            "Deals 18 damage over 18 sec, every 3 sec."
        );
        assert_eq!(
            substitute("Restores $/2;s1 health: $l point:points;.", &d, &c),
            // 3/2 rounds to 2 (min==max==3 scaled by 0.5 → 2, rounded); plural picks "points"
            "Restores 2 health: points.".to_string()
        );
    }

    /// Cross-spell refs resolve through the lookup; unknown tokens pass through visibly.
    #[test]
    fn cross_spell_and_unknown_tokens() {
        let durations = SpellDurationCatalog::default();
        let radii = SpellRadiusCatalog::default();
        let other = SpellDisplay {
            effect_base_points: [99, 0, 0],
            effect_base_dice: [1, 0, 0],
            effect_die_sides: [1, 0, 0],
            ..Default::default()
        };
        let lookup = |id: u32| -> Option<&SpellDisplay> { (id == 1234).then_some(&other) };
        let d = SpellDisplay::default();
        let c = TokenContext {
            home_area: Some("Goldshire"),
            durations: &durations,
            radii: &radii,
            lookup: &lookup,
        };
        assert_eq!(
            substitute("as strong as $1234s1 hits", &d, &c),
            "as strong as 100 hits"
        );
        assert_eq!(substitute("stacks $u times", &d, &c), "stacks $u times");
        // $z — the home-bind area (the hearthstone's "Returns you to $z."); raw when unfed.
        assert_eq!(
            substitute("Returns you to $z.", &d, &c),
            "Returns you to Goldshire."
        );
        let unbound = TokenContext {
            home_area: None,
            durations: &durations,
            radii: &radii,
            lookup: &lookup,
        };
        assert_eq!(
            substitute("Returns you to $z.", &d, &unbound),
            "Returns you to $z."
        );
    }
}
