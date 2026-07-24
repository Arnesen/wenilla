//! The wrap machinery — the pure word-boundary side of [`super`]'s text layout: [`WrapWord`]
//! tokenization, the greedy packer, and the run re-join. Pure (no atlas, no shaping) — the parent
//! binds the measure closure ([`super::wrap_line`]); the tests here run on stub measures. Split
//! out of `layout.rs` when it crossed the size budget (the fade-composite arc). The break law is
//! the byte-verified regime-2 wrap (`system/ui/scratch/fontstring-overflow.md`): break at the last
//! opportunity, force-break a no-opportunity overflow at the last fitting glyph — a rendered line
//! never exceeds the wrap width. Remaining approximation stated on [`super::wrap_line`].

use crate::ui_text::markup::ColorRun;

/// Absorbs float noise in a wrap width before the break comparisons — the width twin of
/// [`super::overflow`]'s `HEIGHT_EPS`, with the same justification: the client compares whole
/// device-pixel accumulations on both sides (dust cannot exist there), but our `max_width` is
/// anchor-graph arithmetic that may have crossed the virtual-UI seam twice (`(a/s + b/s) × s`),
/// so a box sized to *exactly* its measured content can arrive a few ulps low — and a `11.0`
/// string in a `10.999985` box must not force-break into two rows (the money purse's "34" → "…"
/// regression, decision 0605's width half). A quarter pixel is far below any real break decision
/// (a glyph step is ≥3px) and far above the seam dust (≤1e-3).
const WIDTH_EPS: f32 = 0.25;

/// One word carried through wrapping with the color it inherits from its source [`ColorRun`] and the
/// exact whitespace (`lead`) that separated it from the previous word — empty for a line's first word.
/// Preserving the verbatim separator (rather than collapsing it to one space) is what keeps Blizzard's
/// double-space-after-period intact through the wrap.
pub(super) struct WrapWord {
    text: String,
    color: [f32; 4],
    lead: String,
    /// The hyperlink the word's source run sat inside (shared Arc) — carried through the wrap so
    /// a link split across lines still spans every one of its words.
    link: Option<std::sync::Arc<crate::ui_text::markup::LinkInfo>>,
}

/// Split a markup line's color runs into [`WrapWord`]s, each carrying the verbatim whitespace that
/// separated it from the previous word (`lead`; empty for the first word). Inter-word whitespace
/// survives exactly — a double space after a period stays a double space — while a separator that
/// straddles a color boundary just rides along (whitespace draws no ink, so its color is immaterial).
/// A leading run of whitespace on the line attaches to the first word's `lead` and is dropped at emit
/// (like the pre-existing `split_whitespace`), so lines never gain a phantom indent.
pub(super) fn tokenize_words(line: &[ColorRun]) -> Vec<WrapWord> {
    let mut words: Vec<WrapWord> = Vec::new();
    let mut cur = String::new();
    let mut cur_color = [0.0f32; 4];
    let mut cur_link: Option<std::sync::Arc<crate::ui_text::markup::LinkInfo>> = None;
    let mut lead = String::new();
    for run in line {
        for ch in run.text.chars() {
            if ch.is_whitespace() {
                if !cur.is_empty() {
                    words.push(WrapWord {
                        text: std::mem::take(&mut cur),
                        color: cur_color,
                        lead: std::mem::take(&mut lead),
                        link: cur_link.take(),
                    });
                }
                lead.push(ch);
            } else {
                if cur.is_empty() {
                    cur_color = run.color;
                    cur_link = run.link.clone();
                }
                cur.push(ch);
            }
        }
    }
    if !cur.is_empty() {
        words.push(WrapWord {
            text: cur,
            color: cur_color,
            lead,
            link: cur_link,
        });
    }
    words
}
/// The pure greedy word-packer: fill each line left-to-right, breaking before the first word that
/// would push the line past `max_width` (measured by `measure`, which also measures each word's
/// verbatim inter-word separator — so a double space costs its real width). Factored out of
/// [`wrap_line`] so the break logic is unit-testable with a stub measure, independent of a baked font
/// atlas. Assumes `words` is non-empty.
///
/// A word that exceeds `max_width` alone on its line **force-breaks at the last fitting glyph** —
/// the client's no-break-opportunity path (`0x5c7780` picks the last opportunity in the line; with
/// none, `0x5c7623 fcomp / 0x5c762b je` drops the exceeding glyph and ends the line — a rendered
/// line never exceeds the wrap width; `system/ui/scratch/fontstring-overflow.md` regime 2). When
/// not even one glyph fits (a sub-glyph-width box), the builder makes no progress and bails —
/// the client drops the remainder; we mirror it per source line. Unreachable for any shipped box
/// (all are tens of px wide), kept for loop-termination correctness.
pub(super) fn greedy_pack<F: FnMut(&str) -> f32>(
    words: Vec<WrapWord>,
    max_width: f32,
    mut measure: F,
) -> Vec<Vec<ColorRun>> {
    // One adjustment at entry covers every comparison below (the pack test AND the force-break
    // walk) — see [`WIDTH_EPS`].
    let max_width = max_width + WIDTH_EPS;
    let mut out: Vec<Vec<WrapWord>> = Vec::new();
    let mut cur: Vec<WrapWord> = Vec::new();
    let mut cur_w = 0.0f32;
    for word in words {
        let ww = measure(&word.text);
        if !cur.is_empty() {
            let candidate = cur_w + measure(&word.lead) + ww;
            if candidate <= max_width {
                cur_w = candidate;
                cur.push(word);
                continue;
            }
            // Break before this word (the last break opportunity) — it starts the next line.
            out.push(std::mem::take(&mut cur));
        }
        // The word opens a fresh line. If it exceeds the width alone, force-break it: each full
        // line becomes a chunk, the remainder keeps packing.
        let mut word = word;
        let mut ww = ww;
        while ww > max_width {
            let Some((head, rest)) =
                split_at_last_fitting_glyph(&word.text, max_width, &mut measure)
            else {
                // First glyph exceeds: no progress — bail this source line (the client's builder
                // bail; the remainder is dropped).
                return finish_pack(out, cur);
            };
            out.push(vec![WrapWord {
                text: head,
                color: word.color,
                lead: std::mem::take(&mut word.lead),
                link: word.link.clone(),
            }]);
            word.text = rest;
            ww = measure(&word.text);
        }
        cur_w = ww;
        cur.push(word);
    }
    finish_pack(out, cur)
}

/// Close the packer: flush the open line and rejoin every line's words into color runs.
fn finish_pack(mut out: Vec<Vec<WrapWord>>, cur: Vec<WrapWord>) -> Vec<Vec<ColorRun>> {
    if !cur.is_empty() {
        out.push(cur);
    }
    out.iter().map(|ws| words_to_runs(ws)).collect()
}

/// The force-break point: split `text` after the longest glyph prefix that measures within
/// `max_width`. `None` when not even the first glyph fits (the no-progress case). The walk
/// re-measures the whole prefix per glyph rather than summing per-glyph steps — the step law is
/// additive, so the results agree, and force-broken words are rare and short enough that the
/// simpler exact-agreement-with-`measure` walk wins.
fn split_at_last_fitting_glyph<F: FnMut(&str) -> f32>(
    text: &str,
    max_width: f32,
    measure: &mut F,
) -> Option<(String, String)> {
    let mut fit_end = 0usize;
    for (i, c) in text.char_indices() {
        let end = i + c.len_utf8();
        if measure(&text[..end]) > max_width {
            break;
        }
        fit_end = end;
    }
    (fit_end > 0).then(|| (text[..fit_end].to_string(), text[fit_end..].to_string()))
}

/// Rejoin wrapped words into color runs: consecutive words with the same color AND the same link
/// (pointer identity) merge into one run; the verbatim whitespace between words (`lead`) attaches
/// to the preceding run (invisible, so its color is immaterial). The first word on a line drops
/// its `lead` — the trailing separator at a break.
fn words_to_runs(words: &[WrapWord]) -> Vec<ColorRun> {
    let same_link =
        |a: &Option<std::sync::Arc<crate::ui_text::markup::LinkInfo>>,
         b: &Option<std::sync::Arc<crate::ui_text::markup::LinkInfo>>| {
            match (a, b) {
                (None, None) => true,
                (Some(x), Some(y)) => std::sync::Arc::ptr_eq(x, y),
                _ => false,
            }
        };
    let mut runs: Vec<ColorRun> = Vec::new();
    for (i, word) in words.iter().enumerate() {
        if i > 0 {
            if let Some(last) = runs.last_mut() {
                last.text.push_str(&word.lead);
            }
        }
        match runs.last_mut() {
            Some(last) if last.color == word.color && same_link(&last.link, &word.link) => {
                last.text.push_str(&word.text);
            }
            _ => runs.push(ColorRun {
                text: word.text.clone(),
                color: word.color,
                link: word.link.clone(),
            }),
        }
    }
    runs
}

#[cfg(test)]
mod wrap_tests {
    use super::*;

    const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    /// A stub measure: every char (incl. the space) is one unit wide — so widths are just char
    /// counts, and the break logic is exercised without a baked font.
    fn char_measure(s: &str) -> f32 {
        s.chars().count() as f32
    }

    fn words(pairs: &[(&str, [f32; 4])]) -> Vec<WrapWord> {
        // The single-space separator these tests assume; the first word's `lead` is ignored on emit.
        pairs
            .iter()
            .map(|(t, c)| WrapWord {
                text: (*t).to_string(),
                color: *c,
                lead: " ".to_string(),
                link: None,
            })
            .collect()
    }

    /// Flatten a wrapped line's runs back to plain text (dropping color) for assertions.
    fn line_text(runs: &[ColorRun]) -> String {
        runs.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn long_line_breaks_at_word_boundaries_within_width() {
        // "Refreshing Spring Water" (a real merchant item name) at width 12 (chars): "Refreshing"
        // is 10 wide; + " Spring" (7) = 17 > 12 → break. "Spring Water" = 12 ≤ 12 → one line.
        let w = words(&[("Refreshing", WHITE), ("Spring", WHITE), ("Water", WHITE)]);
        let lines = greedy_pack(w, 12.0, char_measure);
        assert_eq!(lines.len(), 2, "wraps to two lines");
        assert_eq!(line_text(&lines[0]), "Refreshing");
        assert_eq!(line_text(&lines[1]), "Spring Water");
        // Every produced line fits the width (its char count ≤ 12).
        for l in &lines {
            assert!(line_text(l).chars().count() <= 12);
        }
    }

    #[test]
    fn short_line_stays_single() {
        let w = words(&[("Buy", WHITE), ("now", WHITE)]);
        let lines = greedy_pack(w, 100.0, char_measure);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "Buy now");
    }

    #[test]
    fn overlong_word_force_breaks_at_the_last_fitting_glyph() {
        // A single word wider than the limit force-breaks (the client's no-opportunity path,
        // fontstring-overflow.md regime 2): each produced line fits the width exactly greedily.
        let w = words(&[("Supercalifragilistic", WHITE), ("ok", WHITE)]);
        let lines = greedy_pack(w, 8.0, char_measure);
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "Supercal");
        assert_eq!(line_text(&lines[1]), "ifragili");
        // The remainder packs on with the following word like any line.
        assert_eq!(line_text(&lines[2]), "stic ok");
        for l in &lines {
            assert!(
                line_text(l).chars().count() <= 8,
                "no line exceeds the width"
            );
        }
    }

    #[test]
    fn force_break_mid_line_starts_from_the_break_opportunity() {
        // "at Supercalifragilistic": the break opportunity before the long word is taken first,
        // then the word itself force-breaks on its own lines.
        let w = words(&[("at", WHITE), ("Supercalifragilistic", WHITE)]);
        let lines = greedy_pack(w, 8.0, char_measure);
        assert_eq!(line_text(&lines[0]), "at");
        assert_eq!(line_text(&lines[1]), "Supercal");
        assert_eq!(line_text(&lines[2]), "ifragili");
        assert_eq!(line_text(&lines[3]), "stic");
    }

    /// The WIDTH_EPS law (decision 0605's width half): a box sized to exactly its measured
    /// content that arrives a few ulps LOW off the seam round-trip must not force-break — the
    /// money purse's "34" in its 10.999985-wide 11.0-content box went to "…" this way. A box a
    /// real glyph too narrow still breaks.
    #[test]
    fn content_exact_width_with_float_dust_does_not_break() {
        // One "word" of two 5.5-unit glyphs (11.0 total) in a box 15 ulps shy of 11.0.
        let measure = |s: &str| s.chars().count() as f32 * 5.5;
        let w = vec![WrapWord {
            text: "34".into(),
            color: WHITE,
            lead: "".into(),
            link: None,
        }];
        let lines = greedy_pack(w, 11.0 - 0.000015, measure);
        assert_eq!(lines.len(), 1, "float dust must not split the digits");
        assert_eq!(line_text(&lines[0]), "34");

        // A genuinely narrow box (one glyph short) still force-breaks — the epsilon is far below
        // any real break decision.
        let w2 = vec![WrapWord {
            text: "34".into(),
            color: WHITE,
            lead: "".into(),
            link: None,
        }];
        let lines2 = greedy_pack(w2, 5.5, measure);
        assert_eq!(lines2.len(), 2, "a real overflow still breaks");
    }

    #[test]
    fn sub_glyph_width_bails_without_progress() {
        // Not even one glyph fits: the builder bails (drops the line's remainder) rather than
        // spinning — the client's first-glyph-exceeds path. Unreachable for real boxes.
        let w = words(&[("ab", WHITE)]);
        let lines = greedy_pack(w, 0.5, char_measure);
        assert!(lines.is_empty());
    }

    #[test]
    fn tokenize_preserves_internal_whitespace() {
        // Blizzard's double space after a period survives as the following word's verbatim `lead`.
        let line = vec![ColorRun {
            text: "Hello.  World again".to_string(),
            color: WHITE,
            link: None,
        }];
        let ws = tokenize_words(&line);
        assert_eq!(ws.len(), 3);
        assert_eq!(ws[0].text, "Hello.");
        assert_eq!(ws[0].lead, "", "first word has no separator");
        assert_eq!(ws[1].text, "World");
        assert_eq!(ws[1].lead, "  ", "the double space is kept verbatim");
        assert_eq!(ws[2].text, "again");
        assert_eq!(ws[2].lead, " ");
    }

    #[test]
    fn wrap_preserves_double_space_between_words() {
        // On one line, the joined run text keeps the double space (the greedy join uses each word's
        // verbatim `lead`, not a collapsed single space).
        let w = vec![
            WrapWord {
                text: "Hello.".into(),
                color: WHITE,
                lead: "".into(),
                link: None,
            },
            WrapWord {
                text: "World".into(),
                color: WHITE,
                lead: "  ".into(),
                link: None,
            },
        ];
        let lines = greedy_pack(w, 100.0, char_measure);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "Hello.  World");

        // At a break the trailing separator drops (standard): a width that fits "Hello." (6) but not
        // "Hello." + "  World" splits, and neither produced line carries stray spaces.
        let w2 = vec![
            WrapWord {
                text: "Hello.".into(),
                color: WHITE,
                lead: "".into(),
                link: None,
            },
            WrapWord {
                text: "World".into(),
                color: WHITE,
                lead: "  ".into(),
                link: None,
            },
        ];
        let lines2 = greedy_pack(w2, 6.0, char_measure);
        assert_eq!(lines2.len(), 2);
        assert_eq!(line_text(&lines2[0]), "Hello.");
        assert_eq!(line_text(&lines2[1]), "World");
    }

    #[test]
    fn color_runs_survive_the_wrap() {
        // A color boundary mid-line: same-color words merge, the boundary starts a new run, and the
        // separating space rides on the preceding run (invisible).
        let w = words(&[("aa", WHITE), ("bb", WHITE), ("cc", RED)]);
        let lines = greedy_pack(w, 100.0, char_measure);
        assert_eq!(lines.len(), 1);
        let runs = &lines[0];
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "aa bb ");
        assert_eq!(runs[0].color, WHITE);
        assert_eq!(runs[1].text, "cc");
        assert_eq!(runs[1].color, RED);
    }

    #[test]
    fn each_wrapped_line_is_an_independent_run_sequence() {
        // Justify is applied per line by the emit pass (each render line measured + shifted on its
        // own — see `layout_text_quads`); wrapping's contract is that each produced line is a
        // complete, independent run sequence the justify pass can position. Verify that structure.
        let w = words(&[
            ("one", WHITE),
            ("two", RED),
            ("three", WHITE),
            ("four", RED),
        ]);
        let lines = greedy_pack(w, 10.0, char_measure); // "one two"=7 fits; +three (13) overflows
        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "one two");
        assert_eq!(line_text(&lines[1]), "three four");
        // Line 2's first run keeps its own color (the wrap didn't bleed line 1's trailing state).
        assert_eq!(lines[1][0].color, WHITE);
    }
}
