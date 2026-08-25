//! The GameTooltip widget's engine mechanics (decision 0274): the line stack + named line
//! regions, auto-size from the measure round-trip, the right-flush, SetOwner's anchor law +
//! IsOwned, SetText's implicit show, both AddLine shapes, ClearLines/Hide firing
//! `OnTooltipCleared`, SetMinimumWidth's floor, and FadeOut's ramp-then-hide.

use super::common::script;
use crate::script::*;

/// Answer every pending line-measure with deterministic per-text sizes.
fn measure_all(s: &mut UiScript, sizes: &[(&str, f32, f32)]) {
    let reqs = s.fontstrings_needing_measure();
    let answers: Vec<(u32, f32, f32, u64)> = reqs
        .iter()
        .filter_map(|r| {
            sizes
                .iter()
                .find(|(t, _, _)| *t == r.text)
                .map(|&(_, w, h)| (r.id, w, h, r.key))
        })
        .collect();
    s.set_measured_text_unwrapped(&answers);
    s.resolve();
}

/// The full stack: three lines (one double), measured, auto-sized, right-flushed, and the named
/// line globals published.
#[test]
fn line_stack_autosize_and_right_flush() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local owner = CreateFrame("Button", "Slot")
        owner:SetPoint("TOPLEFT", 100, -100); owner:SetSize(40, 40)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(owner, "ANCHOR_RIGHT")
        tt:AddLine("Tough Jerky", 1, 1, 1)
        tt:AddDoubleLine("One-Hand", "Sword", 1, 1, 1, 1, 1, 1)
        tt:AddLine("5 - 9 Damage")
        tt:Show()
        assert(tt:NumLines() == 3, "NumLines")
        assert(TTTextLeft1 and TTTextRight2, "line globals published")
        assert(TTTextLeft1:GetText() == "Tough Jerky", "line 1 text readable by name")
    "#,
    )
    .unwrap();
    s.resolve();
    measure_all(
        &mut s,
        &[
            ("Tough Jerky", 80.0, 14.0),
            ("One-Hand", 50.0, 12.0),
            ("Sword", 30.0, 12.0),
            ("5 - 9 Damage", 70.0, 12.0),
        ],
    );
    // maxw = max(80, 50+40+30, 70) = 120 ⇒ width 140; totalh = 14+2+12+2+12 = 42 ⇒ height 62.
    // ANCHOR_RIGHT: tooltip BOTTOMLEFT at owner TOPRIGHT (140, 500).
    s.run(
        r#"
        assert(TT:GetWidth() == 140, "auto width, got " .. TT:GetWidth())
        assert(TT:GetHeight() == 62, "auto height, got " .. TT:GetHeight())
    "#,
    )
    .unwrap();
    let quads = s.extract();
    let rect_of = |needle: &str| {
        quads
            .iter()
            .find_map(|q| match &q.content {
                QuadContent::Text { text: Some(t), .. } if t == needle => q.rect,
                _ => None,
            })
            .unwrap_or_else(|| panic!("no quad for {needle}"))
    };
    let l1 = rect_of("Tough Jerky");
    // Frame: left 140, bottom 500 ⇒ text inset TOPLEFT (150, 552).
    assert_eq!((l1.left, l1.top), (150.0, 552.0));
    let r2 = rect_of("Sword");
    // Right-flushed: right edge at frame.right − pad = 280 − 10 = 270.
    assert_eq!(r2.right, 270.0);
    // Seated on line 2's band (left2 top = 552 − 14 − 2 = 536).
    let l2 = rect_of("One-Hand");
    assert_eq!(l2.top, 536.0);
}

/// An EMPTY line mid-stack (the corpus' `AddLine("")`; the live shape was an empty-string
/// subtitle from the wire): a ZERO-height row that still charges its 2px slot gap — the ref's
/// empty-line shape. Pre-fix, an empty FontString never measures, so its unpinned bottom edge
/// fell back to the OWNER frame's bottom (the v1 region fallback): the line stretched to the
/// plate's bottom and the anchor chain marched every later line OUT of the plate (the live
/// NPC-tooltip spill — name inside, "Level 20"/"PvP" below the frame under the health bar).
#[test]
fn empty_line_is_a_zero_row_and_the_chain_stays_inside_the_plate() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local owner = CreateFrame("Button", "Slot2")
        owner:SetPoint("TOPLEFT", 100, -100); owner:SetSize(40, 40)
        local tt = CreateFrame("GameTooltip", "TTE")
        tt:SetOwner(owner, "ANCHOR_RIGHT")
        tt:AddLine("Marshal McBride", 0.25, 0.75, 0.25)
        tt:AddLine("")
        tt:AddLine("Level 20")
        tt:AddLine("PvP")
        tt:Show()
        assert(tt:NumLines() == 4, "NumLines counts the empty line")
    "#,
    )
    .unwrap();
    s.resolve();
    measure_all(
        &mut s,
        &[
            ("Marshal McBride", 90.0, 14.0),
            ("Level 20", 50.0, 12.0),
            ("PvP", 24.0, 12.0),
        ],
    );
    // Rows 14 + 0 + 12 + 12 with 3 slot gaps ⇒ totalh 44, height 64; maxw 90 ⇒ width 110.
    s.run(
        r#"
        assert(TTE:GetWidth() == 110, "auto width, got " .. tostring(TTE:GetWidth()))
        assert(TTE:GetHeight() == 64, "auto height, got " .. tostring(TTE:GetHeight()))
        -- The chain stays contiguous through the zero row: Level 20 sits gap+0+gap under the name.
        assert(TTETextLeft3:GetTop() == TTETextLeft1:GetBottom() - 4,
               "chain contiguous through the empty row, got " .. tostring(TTETextLeft3:GetTop())
               .. " vs " .. tostring(TTETextLeft1:GetBottom()))
        -- And the tail line lands INSIDE the plate, a full pad above its bottom edge.
        assert(TTETextLeft4:GetBottom() == TTE:GetBottom() + 10,
               "tail line inside the plate, got " .. tostring(TTETextLeft4:GetBottom())
               .. " vs plate bottom " .. tostring(TTE:GetBottom()))
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// SetOwner clears previous content (firing OnTooltipCleared), owns the frame for IsOwned, and
/// Hide drops the owner + content and fires OnTooltipCleared again.
#[test]
fn owner_clear_and_hide_lifecycle() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        cleared = 0
        local a = CreateFrame("Button", "A"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local b = CreateFrame("Button", "B"); b:SetPoint("CENTER", 50, 0); b:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT2")
        tt:SetScript("OnTooltipCleared", function() cleared = cleared + 1 end)
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:AddLine("first hover")
        tt:Show()
        assert(tt:IsOwned(a) and not tt:IsOwned(b), "owned by a")
        assert(tt:GetOwner() == a, "GetOwner returns the owner wrapper")
        tt:SetOwner(b, "ANCHOR_LEFT")
        assert(cleared >= 1, "SetOwner cleared the old content")
        assert(tt:NumLines() == 0, "content cleared on re-own")
        assert(tt:IsOwned(b) and not tt:IsOwned(a), "owner moved")
        tt:AddLine("second hover")
        tt:Hide()
        assert(not tt:IsShown(), "hidden")
        assert(not tt:IsOwned(b), "owner dropped on hide")
        assert(tt:NumLines() == 0, "content cleared on hide")
        assert(cleared >= 2, "hide fired OnTooltipCleared")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// SetText shows the tooltip implicitly (the PaperDoll empty-slot flow calls no Show); AddLine
/// does not (the corpus' `AddLine … Show()`). The colour law: positional `r, g, b` apply only
/// when the r-slot is a number — the corpus' archaic `(text, "", r, g, b)` shape has a string
/// there, so the real 1.12 binding drops the whole colour tail and renders the DEFAULT GOLD
/// (the reference's zone tooltip, director-matched; the trailing numbers never shift into place).
#[test]
fn settext_shows_and_both_addline_shapes() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local a = CreateFrame("Button", "A3"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT3")
        tt:Hide() -- the XML instance ships hidden="true"; CreateFrame defaults shown
        tt:SetOwner(a, "ANCHOR_RIGHT")
        assert(not tt:IsShown(), "SetOwner alone does not show")
        tt:AddLine("plain")
        assert(not tt:IsShown(), "AddLine does not show")
        tt:SetText("Head", 1, 1, 1)
        assert(tt:IsShown(), "SetText shows")
        assert(tt:NumLines() == 1, "SetText replaced the stack")
        -- the archaic 1.12 shape: AddLine(text, "", r, g, b) — the "" kills the colour tail
        tt:AddLine("Zone Name", "", 1.0, 0.25, 0.5)
        -- no colour at all — same default
        tt:AddLine("plain gold")
        -- the modern shape with wrap flag
        tt:AddLine("wrapped tail", 0.2, 0.4, 0.6, 1)
        -- a numeric r gates the block ON; missing g/b are UNGATED tonumber reads -> 0.0
        tt:AddLine("red only", 1)
        -- SetText requires its text: the binding raises its Usage error and adds no line
        assert(not pcall(function() tt:SetText() end), "SetText() must error")
    "#,
    )
    .unwrap();
    s.resolve();
    let quads = s.extract();
    let color_of = |needle: &str| {
        quads
            .iter()
            .find_map(|q| match &q.content {
                QuadContent::Text {
                    text: Some(t),
                    color,
                    ..
                } if t == needle => *color,
                _ => None,
            })
            .unwrap_or_else(|| panic!("no quad for {needle}"))
    };
    // The engine default gold 0xffffd200 (both no-colour shapes land on it).
    let gold = [1.0, 210.0 / 255.0, 0.0, 1.0];
    assert_eq!(color_of("Zone Name"), gold);
    assert_eq!(color_of("plain gold"), gold);
    assert_eq!(color_of("wrapped tail"), [0.2, 0.4, 0.6, 1.0]);
    // The partial tail: r gates the block on, missing g/b read as 0.0 (byte-pinned).
    assert_eq!(color_of("red only"), [1.0, 0.0, 0.0, 1.0]);
    assert!(s.take_errors().is_empty());
}

/// A tooltip whose lines haven't been measured yet holds its declared size — the gaps alone must
/// never resolve it to a gaps-only rect (the first cut summed LINE_GAP/DOUBLE_GAP for unmeasured
/// rows and collapsed a fresh 4-line tooltip to 60×26; caught during the 0274 call-site
/// migration).
#[test]
fn unmeasured_lines_hold_declared_size() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local a = CreateFrame("Button", "A6"); a:SetPoint("TOPLEFT", 100, -100); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT6")
        tt:SetSize(120, 32)
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:AddLine("Tough Jerky")
        tt:AddDoubleLine("One-Hand", "Sword")
        tt:AddLine("5 - 9 Damage")
        tt:AddLine("(3.7 damage per second)")
        tt:Show()
    "#,
    )
    .unwrap();
    // Resolve WITHOUT answering the measure round-trip: the declared 120×32 must hold.
    s.resolve();
    s.run(
        r#"
        assert(TT6:GetWidth() == 120, "declared width holds unmeasured, got " .. TT6:GetWidth())
        assert(TT6:GetHeight() == 32, "declared height holds unmeasured, got " .. TT6:GetHeight())
    "#,
    )
    .unwrap();
    // Then the measures land and the auto-size takes over.
    measure_all(
        &mut s,
        &[
            ("Tough Jerky", 80.0, 14.0),
            ("One-Hand", 50.0, 12.0),
            ("Sword", 30.0, 12.0),
            ("5 - 9 Damage", 70.0, 12.0),
            ("(3.7 damage per second)", 110.0, 12.0),
        ],
    );
    s.run(r#"assert(TT6:GetWidth() == 140, "auto width after measures, got " .. TT6:GetWidth())"#)
        .unwrap();
}

/// SetMinimumWidth floors the auto width (the money row's law), and clears with the content.
#[test]
fn minimum_width_floors_autosize() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local a = CreateFrame("Button", "A4"); a:SetPoint("TOPLEFT", 100, -100); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT4")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:AddLine("tiny")
        tt:SetMinimumWidth(90)
        tt:Show()
    "#,
    )
    .unwrap();
    s.resolve();
    measure_all(&mut s, &[("tiny", 20.0, 12.0)]);
    // floor 90 beats content 20 ⇒ width 110.
    s.run(r#"assert(TT4:GetWidth() == 110, "floored width, got " .. TT4:GetWidth())"#)
        .unwrap();
}

/// FadeOut ramps the frame alpha down and hides at the end of the ramp (owner + lines dropped,
/// OnTooltipCleared fired); fresh content mid-fade cancels the ramp at full alpha.
#[test]
fn fadeout_ramps_then_hides() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        cleared = 0
        local a = CreateFrame("Button", "A5"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT5")
        tt:SetScript("OnTooltipCleared", function() cleared = cleared + 1 end)
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetText("Fading Unit")
        tt:FadeOut()
    "#,
    )
    .unwrap();
    // Half the ramp: still shown, alpha well below 1.
    s.tick(0.25);
    s.run(r#"assert(TT5:IsShown(), "still shown mid-fade")"#)
        .unwrap();
    s.resolve();
    let mid_alpha = s
        .extract()
        .iter()
        .find_map(|q| match &q.content {
            QuadContent::Text { text: Some(t), .. } if t == "Fading Unit" => Some(q.alpha),
            _ => None,
        })
        .expect("fading line still draws");
    assert!(
        mid_alpha > 0.2 && mid_alpha < 0.8,
        "mid-fade alpha ~0.5, got {mid_alpha}"
    );
    // Re-content cancels the fade at full alpha.
    s.run(
        r#"
        TT5:SetText("Fresh Hover")
    "#,
    )
    .unwrap();
    s.tick(0.05);
    s.resolve();
    let fresh_alpha = s
        .extract()
        .iter()
        .find_map(|q| match &q.content {
            QuadContent::Text { text: Some(t), .. } if t == "Fresh Hover" => Some(q.alpha),
            _ => None,
        })
        .expect("fresh line draws");
    assert_eq!(fresh_alpha, 1.0, "fresh content restored full alpha");
    // Run a full ramp to the end: hidden + cleared.
    s.run("TT5:FadeOut()").unwrap();
    s.tick(0.6);
    s.run(
        r#"
        assert(not TT5:IsShown(), "hidden at ramp end")
        assert(TT5:NumLines() == 0, "content dropped at ramp end")
        assert(cleared >= 1, "OnTooltipCleared fired")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// SetOwner's anchor law at the screen edge CLAMPS — the client's geometry-flags-bit4 clamp
/// (`assemble 0x767a20`, wow-re layout.md), carried by every GameTooltip frame **by
/// construction** (decision 0352: no tooltip ever leaves the window). The reproduction is the
/// minimap zone-text hover (MinimapCluster.xml: `ANCHOR_LEFT` on a button at the very top of
/// the screen — plate bottom-right at the owner's top-left seats it wholly ABOVE the window):
/// the reference plate hangs DOWN from the screen top instead, size preserved, X untouched.
#[test]
fn owner_anchored_tooltip_clamps_to_screen() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local owner = CreateFrame("Button", "ZoneTextBtn")
        owner:SetPoint("TOPRIGHT", -100, 0); owner:SetSize(90, 14)
        local tt = CreateFrame("GameTooltip", "TTC")
        assert(tt:IsClampedToScreen(), "a GameTooltip clamps by construction")
        assert(not owner:IsClampedToScreen(), "a plain frame does not")
        tt:SetOwner(owner, "ANCHOR_LEFT")
        tt:AddLine("Goldshire")
        tt:AddLine("Alliance Territory")
        tt:Show()
    "#,
    )
    .unwrap();
    s.resolve();
    measure_all(
        &mut s,
        &[
            ("Goldshire", 70.0, 14.0),
            ("Alliance Territory", 120.0, 12.0),
        ],
    );
    // Auto-size: maxw 120 ⇒ width 140; totalh 14+2+12 ⇒ height 48. Unclamped, the plate's
    // BOTTOMRIGHT sits at the owner's TOPLEFT (610, 600) — top 648, wholly above the window.
    // The clamp shifts it back down: top at the screen top, size preserved, X untouched.
    s.run(
        r#"
        assert(TTC:GetTop() == 600, "top clamped to the screen top, got " .. TTC:GetTop())
        assert(TTC:GetBottom() == 552, "size preserved, got " .. TTC:GetBottom())
        assert(TTC:GetRight() == 610, "X untouched (inside), got " .. TTC:GetRight())
        TTC:SetClampedToScreen(false)
    "#,
    )
    .unwrap();
    s.resolve();
    // The flag routes: unclamped, the plate returns to the raw anchor law above the window.
    s.run(
        r#"
        assert(not TTC:IsClampedToScreen(), "flag readable")
        assert(TTC:GetBottom() == 600, "unclamped: bottom back at the owner's top")
        assert(TTC:GetTop() == 648, "unclamped: off the window again")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// `0x52fa50`'s ladder as pure logic (law §3-BUFF-TIME-FORMAT) — the arm thresholds, the
/// ceil/truncate asymmetry, and the `_P1` pick — over SYNTHETIC templates, so this asserts the
/// mechanism and carries none of the reference's own eight strings. The real ones are read off the
/// player's install and exercised in `benilla::ui_script::buff_tests`.
#[test]
fn the_duration_ladder_ceils_every_arm_but_seconds() {
    // Deliberately not the shipped wording: what is under test is which key is reached and what
    // number fills it, never what the string says.
    let table = |key: &str| -> Option<String> {
        Some(
            match key {
                "T_DAYS" => "<%d day>",
                "T_DAYS_P1" => "<%d days>",
                "T_HOURS" => "<%d hour>",
                "T_HOURS_P1" => "<%d hours>",
                "T_MIN" => "<%d min>",
                "T_MIN_P1" => "<%d mins>",
                "T_SEC" => "<%d sec>",
                "T_SEC_P1" => "<%d secs>",
                _ => return None,
            }
            .to_string(),
        )
    };
    let d = |ms: u32| crate::script::tooltip::duration_text(ms, "T", true, &table);

    // The three arm boundaries, from the compare instructions — each is exact, and the arm below
    // it counts in its own unit right up to the edge.
    assert_eq!(d(86_400_000).as_deref(), Some("<1 day>"), "the day edge");
    assert_eq!(
        d(86_399_999).as_deref(),
        Some("<24 hours>"),
        "one ms under a day is still HOURS, and ceil takes it to 24"
    );
    assert_eq!(d(3_600_000).as_deref(), Some("<1 hour>"), "the hour edge");
    assert_eq!(
        d(3_599_999).as_deref(),
        Some("<60 mins>"),
        "the carve's own example: no '1 hour' until the hour is whole"
    );
    assert_eq!(d(60_000).as_deref(), Some("<1 min>"), "the minute edge");
    assert_eq!(
        d(61_000).as_deref(),
        Some("<2 mins>"),
        "the carve's own example: 61 s ceils to 2, it does not truncate to 1"
    );

    // The asymmetry that makes this one function rather than four format calls: roundUp reaches
    // the top three arms only.
    assert_eq!(
        d(59_999).as_deref(),
        Some("<59 secs>"),
        "seconds TRUNCATE — a ceil here would read 60"
    );
    assert_eq!(d(5_400).as_deref(), Some("<5 secs>"), "5.4 s is 5, not 6");
    assert_eq!(
        d(400).as_deref(),
        Some("<0 secs>"),
        "the lapsing second: 0, and PLURAL — the reference's own last reading"
    );
    assert_eq!(d(0).as_deref(), Some("<0 secs>"), "a fully lapsed aura");
    assert_eq!(
        d(1_000).as_deref(),
        Some("<1 sec>"),
        "exactly one is singular"
    );

    // roundUp = 0 truncates the top arms too (the parameter is real, not a constant we folded).
    assert_eq!(
        crate::script::tooltip::duration_text(61_000, "T", false, &table).as_deref(),
        Some("<1 min>"),
    );

    // A key the string table does not carry yields NO LINE rather than an invented one.
    assert_eq!(
        crate::script::tooltip::duration_text(1_000, "NOPE", true, &table),
        None
    );
    // …and a family shipping only the singular falls back to it rather than vanishing.
    let singular_only = |key: &str| (key == "S_SEC").then(|| "<%d s>".to_string());
    assert_eq!(
        crate::script::tooltip::duration_text(5_000, "S", true, &singular_only).as_deref(),
        Some("<5 s>"),
    );
}

/// **B309 — a POOLED line cell that goes empty must not keep the box it measured last hover.**
///
/// `measured` is a cache the solver honours without a key check on purpose: it is the last-known
/// box, held so a line whose text just changed does not collapse for the frame its re-measure is
/// in flight. Empty text is the one case where that measure never comes — both measure asks
/// filter empty strings out — so before the fix an emptied cell held its dead box FOREVER, and
/// only the DRAWN geometry was wrong: the plate's own sum calls empty text zero
/// (`tooltip::cell`) and the Lua getters key-check (`region::measured_wh`), so both said 0 while
/// the solver stood a full row there.
///
/// Live, that is the item tooltip's SET block: `ClearLines` keeps the cache (the hover re-enter
/// loop depends on it), so the two blank gold spacers land on cells that carried real text on an
/// earlier hover. Each drew an uncounted row and the set bonuses hung below the backdrop —
/// Frostshake's Field Marshal's Raiment shot, two blanks, two rows.
#[test]
fn an_emptied_pooled_line_drops_its_stale_box_and_the_plate_still_contains_the_chain() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    // Hover one: line 2 carries real text, and gets measured.
    s.run(
        r#"
        local owner = CreateFrame("Button", "SlotR")
        owner:SetPoint("TOPLEFT", 100, -100); owner:SetSize(40, 40)
        local tt = CreateFrame("GameTooltip", "TTR")
        tt:SetOwner(owner, "ANCHOR_RIGHT")
        tt:AddLine("Marshal McBride")
        tt:AddLine("Level 20")
        tt:AddLine("PvP")
        tt:Show()
    "#,
    )
    .unwrap();
    s.resolve();
    let sizes = &[
        ("Marshal McBride", 90.0, 14.0),
        ("Level 20", 50.0, 12.0),
        ("PvP", 24.0, 12.0),
    ];
    measure_all(&mut s, sizes);
    // Hover two, SAME pooled cells: line 2 is now the blank spacer. Nothing new to measure —
    // lines 1 and 3 re-validate against their own keys, and an empty string is never asked for.
    s.run(
        r#"
        TTR:ClearLines()
        TTR:AddLine("Marshal McBride")
        -- WRAPPED, as the §22 spacer is (`render.rs`'s `addw`): the wrap pin writes a non-zero
        -- width into `size`, so only the HEIGHT falls through to the measure cache.
        TTR:AddLine("", 1, 0.82, 0, true)
        TTR:AddLine("PvP")
    "#,
    )
    .unwrap();
    s.resolve();
    measure_all(&mut s, sizes);
    s.run(
        r#"
        -- Rows 14 + 0 + 12 with two slot gaps ⇒ totalh 30, height 50: the blank costs its gap
        -- and nothing else, exactly as it does on a cell that never held text.
        assert(TTR:GetHeight() == 50, "auto height, got " .. tostring(TTR:GetHeight()))
        -- The one that was wrong: the tail line sits a full pad above the plate's bottom edge.
        -- Pre-fix it sat 12 BELOW it — line 2's dead "Level 20" box, drawn but never counted.
        assert(TTRTextLeft3:GetBottom() == TTR:GetBottom() + 10,
               "tail line inside the plate, got " .. tostring(TTRTextLeft3:GetBottom())
               .. " vs plate bottom " .. tostring(TTR:GetBottom()))
        -- And the chain is contiguous through the zero row, as on a cold cell.
        assert(TTRTextLeft3:GetTop() == TTRTextLeft1:GetBottom() - 4,
               "chain contiguous through the emptied row, got " .. tostring(TTRTextLeft3:GetTop()))
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}
