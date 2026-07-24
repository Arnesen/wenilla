// ─────────────────────────────────────────────────────────────────────────────────────────────
// Markup — ported verbatim from probes/text-glyph/src/markup.rs
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// WoW's inline FrameXML text markup: `|cAARRGGBB ... |r` color escapes, `\n` newlines, and `|T...|t`
/// inline textures (stripped — see the module doc's v1 simplifications).
///
/// `|c` is followed by exactly 8 hex digits in **AARRGGBB** order (alpha first — verified against
/// FrameXML usage across the client's own strings, e.g. quality-color prefixes like `|cff1eff00` for
/// uncommon-green). `|r` resets to the string's base color. Unterminated/malformed escapes are left
/// literal rather than dropped, so a typo'd markup string degrades to visible garbage instead of
/// silently eating text — the same posture real FrameXML rendering takes.
///
/// A `|H<link>|h<text>|h` hyperlink a run sits inside: the link payload (`item:2000:0:0:0`,
/// `player:Bob`) and the reconstructed link markup (`|H…|h[Name]|h` — the `OnHyperlinkClick`
/// `arg2`, what shift-click inserts into the edit box). Shared by every run the link's visible
/// text splits into (color changes, word wrap), so span collection can group by pointer identity.
#[derive(Debug, PartialEq)]
pub(crate) struct LinkInfo {
    pub(crate) link: String,
    pub(crate) markup: String,
}

/// One color run within a line: the literal text, the RGBA color (straight-alpha, 0..1) it draws
/// with — the same shape [`crate::ui_pass::UiQuad::color`] expects — and the hyperlink it belongs
/// to, if any.
#[derive(Clone)]
pub(super) struct ColorRun {
    pub(super) text: String,
    pub(super) color: [f32; 4],
    pub(super) link: Option<std::sync::Arc<LinkInfo>>,
}

/// Split `input` into lines (on `\n`) and, within each line, into [`ColorRun`]s by resolving
/// `|cAARRGGBB` / `|r` and `|H…|h…|h` hyperlinks. `|T...|t` inline-texture escapes are stripped
/// entirely (their content is never a color/text run).
pub(super) fn parse_markup(input: &str, base_color: [f32; 4]) -> Vec<Vec<ColorRun>> {
    input
        .split('\n')
        .map(|line| parse_line(line, base_color))
        .collect()
}

fn parse_line(line: &str, base_color: [f32; 4]) -> Vec<ColorRun> {
    let chars: Vec<char> = line.chars().collect();
    let mut runs = Vec::new();
    let mut color = base_color;
    let mut cur = String::new();
    // The open hyperlink, if any: (payload, visible-text accumulator, indices of runs already
    // flushed under it). The Arc is built at the closing `|h` and back-patched onto those runs.
    let mut link: Option<(String, String, Vec<usize>)> = None;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '|' && i + 1 < chars.len() {
            match chars[i + 1] {
                'c' | 'C' => {
                    if let Some(argb) = parse_color_escape(&chars, i) {
                        flush(&mut runs, &mut cur, color, &mut link);
                        color = argb;
                        i += 10; // "|c" + 8 hex digits
                        continue;
                    }
                }
                'r' | 'R' => {
                    flush(&mut runs, &mut cur, color, &mut link);
                    color = base_color;
                    i += 2;
                    continue;
                }
                'T' | 't' if link.is_none() => {
                    if let Some(end) = find_texture_close(&chars, i) {
                        i = end; // drop everything from |T through the matching |t
                        continue;
                    }
                }
                // `|H<link>|h` opens a hyperlink (no nesting — Blizzard's own strings never nest).
                'H' if link.is_none() => {
                    if let Some((payload, after)) = parse_link_open(&chars, i) {
                        flush(&mut runs, &mut cur, color, &mut link);
                        link = Some((payload, String::new(), Vec::new()));
                        i = after;
                        continue;
                    }
                }
                // The closing `|h` of an open hyperlink.
                'h' | 'H' if link.is_some() => {
                    flush(&mut runs, &mut cur, color, &mut link);
                    let (payload, visible, idxs) = link.take().expect("open link");
                    let info = std::sync::Arc::new(LinkInfo {
                        markup: format!("|H{payload}|h{visible}|h"),
                        link: payload,
                    });
                    for idx in idxs {
                        runs[idx].link = Some(info.clone());
                    }
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        if let Some((_, visible, _)) = &mut link {
            visible.push(chars[i]);
        }
        cur.push(chars[i]);
        i += 1;
    }
    // An unterminated link degrades gracefully: its runs stay plain text (no span), matching the
    // "degrade to visible garbage" posture — the |H opener was consumed, the text still shows.
    flush(&mut runs, &mut cur, color, &mut link);
    runs
}

fn flush(
    runs: &mut Vec<ColorRun>,
    cur: &mut String,
    color: [f32; 4],
    link: &mut Option<(String, String, Vec<usize>)>,
) {
    if !cur.is_empty() {
        if let Some((_, _, idxs)) = link {
            idxs.push(runs.len());
        }
        runs.push(ColorRun {
            text: std::mem::take(cur),
            color,
            link: None, // back-patched at the closing |h
        });
    }
}

/// `chars[i]` is `'|'`, `chars[i+1]` is `'H'` (opening a hyperlink). Scans for the payload
/// delimiter `|h` and returns `(payload, index-just-past-the-delimiter)`. `None` if unterminated
/// (left as literal text by the caller).
fn parse_link_open(chars: &[char], i: usize) -> Option<(String, usize)> {
    let mut j = i + 2;
    while j + 1 < chars.len() {
        if chars[j] == '|' && (chars[j + 1] == 'h' || chars[j + 1] == 'H') {
            let payload: String = chars[i + 2..j].iter().collect();
            return Some((payload, j + 2));
        }
        j += 1;
    }
    None
}

/// `chars[i]` is `'|'`, `chars[i+1]` is `'c'`/`'C'`. Parses the 8 following hex digits as
/// **AARRGGBB** and returns the normalized `[r, g, b, a]`. `None` if fewer than 8 hex digits follow
/// (malformed escape — left as literal text by the caller).
fn parse_color_escape(chars: &[char], i: usize) -> Option<[f32; 4]> {
    let start = i + 2;
    let end = start + 8;
    if end > chars.len() {
        return None;
    }
    let hex: String = chars[start..end].iter().collect();
    let argb = u32::from_str_radix(&hex, 16).ok()?;
    let a = ((argb >> 24) & 0xFF) as f32 / 255.0;
    let r = ((argb >> 16) & 0xFF) as f32 / 255.0;
    let g = ((argb >> 8) & 0xFF) as f32 / 255.0;
    let b = (argb & 0xFF) as f32 / 255.0;
    Some([r, g, b, a])
}

/// `chars[i]` is `'|'`, `chars[i+1]` is `'T'`/`'t'` (opening an inline-texture escape). Scans forward
/// for the matching `|t`/`|T` close and returns the index just past it. `None` if unterminated (left
/// as literal text by the caller, matching the "degrade to visible garbage" posture above).
fn find_texture_close(chars: &[char], i: usize) -> Option<usize> {
    let mut j = i + 2;
    while j + 1 < chars.len() {
        if chars[j] == '|' && (chars[j + 1] == 't' || chars[j + 1] == 'T') {
            return Some(j + 2);
        }
        j += 1;
    }
    None
}

#[cfg(test)]
mod markup_tests {
    use super::*;

    const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

    #[test]
    fn plain_text_is_one_run() {
        let lines = parse_markup("hello", WHITE);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), 1);
        assert_eq!(lines[0][0].text, "hello");
        assert_eq!(lines[0][0].color, WHITE);
    }

    #[test]
    fn color_escape_switches_and_resets() {
        let lines = parse_markup("a|cffff0000b|rc", WHITE);
        assert_eq!(lines[0].len(), 3);
        assert_eq!(lines[0][0].text, "a");
        assert_eq!(lines[0][0].color, WHITE);
        assert_eq!(lines[0][1].text, "b");
        assert_eq!(lines[0][1].color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(lines[0][2].text, "c");
        assert_eq!(lines[0][2].color, WHITE);
    }

    #[test]
    fn newline_splits_lines() {
        let lines = parse_markup("one\ntwo", WHITE);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0][0].text, "one");
        assert_eq!(lines[1][0].text, "two");
    }

    #[test]
    fn inline_texture_is_stripped() {
        let lines = parse_markup("a|TInterface\\Icons\\Foo:16:16|tb", WHITE);
        assert_eq!(lines[0].len(), 1);
        assert_eq!(lines[0][0].text, "ab");
    }

    #[test]
    fn hyperlink_runs_carry_the_link_and_strip_the_markers() {
        // The canonical chat item link: color outside, |H..|h[Name]|h inside.
        let lines = parse_markup("|cff1eff00|Hitem:2000:0:0:0|h[Another Helm]|h|r ok", WHITE);
        assert_eq!(lines[0].len(), 2);
        assert_eq!(lines[0][0].text, "[Another Helm]");
        assert_eq!(lines[0][0].color, [0x1e as f32 / 255.0, 1.0, 0.0, 1.0]);
        let info = lines[0][0].link.as_ref().expect("linked run");
        assert_eq!(info.link, "item:2000:0:0:0");
        assert_eq!(info.markup, "|Hitem:2000:0:0:0|h[Another Helm]|h");
        assert_eq!(lines[0][1].text, " ok");
        assert!(lines[0][1].link.is_none());
        assert_eq!(lines[0][1].color, WHITE);
    }

    #[test]
    fn color_change_inside_a_link_still_shares_one_link() {
        let lines = parse_markup("|Hplayer:Bob|h[|cffff0000Bob|r]|h", WHITE);
        // Three runs ("[", "Bob", "]"), all sharing ONE LinkInfo (pointer identity).
        assert_eq!(lines[0].len(), 3);
        let first = lines[0][0].link.as_ref().expect("linked");
        for run in &lines[0] {
            let l = run.link.as_ref().expect("all runs linked");
            assert!(std::sync::Arc::ptr_eq(first, l));
        }
        assert_eq!(first.link, "player:Bob");
        assert_eq!(first.markup, "|Hplayer:Bob|h[Bob]|h");
    }

    #[test]
    fn unterminated_link_degrades_to_plain_text() {
        // No closing |h: the opener is consumed, the text still shows, no span attaches.
        let lines = parse_markup("|Hitem:1|h[Broken", WHITE);
        assert_eq!(lines[0].len(), 1);
        assert_eq!(lines[0][0].text, "[Broken");
        assert!(lines[0][0].link.is_none());
    }
}
