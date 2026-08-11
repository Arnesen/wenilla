//! The player buff bar (`assets/ui/BuffFrame.xml`, decisions 0255/0257) against its reference
//! behaviour. The XML/Lua is the unit under test; the app-side feed (`crate::ui_aura`) is stubbed by
//! pushing an [`AuraState`] list straight through [`UiScript::set_auras`] and firing
//! `PLAYER_AURAS_CHANGED`, so these exercise the *button* handlers — the row/filter wiring, the
//! dispel-tinted border, the stack count, the countdown, the warning flash, and the right-click
//! cancel — the way the reference's own `BuffButton_*` do.
//!
//! **The window runs on the 1.12 verbs and the reference's index space** (b2ede294 landed the
//! `GetPlayerBuff` family): a button's `id` is a 0-based ordinal within its own filter,
//! `GetPlayerBuff` turns it into a CACHE POSITION, and every read below — texture, dispel class,
//! stacks, time left, cancel, tooltip — consumes that position. The tests drive it the way the
//! corpus does, including the `while GetPlayerBuff(i) >= 0 do` walk and a cancel by position.
//!
//! State is read back through the widget (`IsShown`/`GetText`/`GetAlpha`) and the paint through the
//! [`draw list`](UiScript::extract) (icon path + border tint), the same two-lens split the cast-bar
//! tests use — Lua-visible state is blind to what actually renders.

use benilla_ui::script::{AuraState, ExtractedQuad, QuadContent, UiScript};

/// Load one shipped `assets/ui/<file>`, panicking on any loader error.
fn load_xml(s: &UiScript, file: &str) {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/ui")
            .join(file),
    )
    .unwrap();
    let doc = benilla_ui::framexml::parse(&text).unwrap();
    let report = benilla_ui::loader::load(s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "{file}: loader errors: {:?}",
        report.errors
    );
}

fn harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml"); // NORMAL/HIGHLIGHT_FONT_COLOR + the FontStrings' faces
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "ActionBar.xml"); // BENILLA_FALLBACK_ICON (the unknown-icon fallback)
    load_xml(&s, "BuffFrame.xml");
    s
}

/// Build an [`AuraState`] as the feed would push it. `expiration_time` 0 = permanent (no wire
/// duration — the reference's "until cancelled"); the `GetTime()` clock starts at 0 in the harness.
#[allow(clippy::too_many_arguments)]
fn aura(
    spell_id: u32,
    name: &str,
    icon: &str,
    helpful: bool,
    count: u8,
    debuff_type: Option<&str>,
    expiration_time: f64,
    cancelable: bool,
) -> AuraState {
    AuraState {
        spell_id,
        name: Some(name.into()),
        icon: Some(icon.into()),
        count,
        debuff_type: debuff_type.map(Into::into),
        duration: expiration_time,
        expiration_time,
        helpful,
        cancelable,
        // What the feed derives for a permanent aura: no finite duration to display. Here the two
        // agree by construction, but they are different questions in the real feed — the flag is
        // DBC-derived and correct on the frame the aura appears, before any duration packet lands
        // (`benilla::ui_aura::until_cancelled`).
        until_cancelled: expiration_time == 0.0,
        channeled: false,
    }
}

/// Push the list, then fire the discrete-change event the app's feed fires — the buttons repaint.
///
/// `PLAYER_AURAS_CHANGED`, no args: the reference's own event, which `ui_aura` fires beside the
/// Era-shaped `UNIT_AURA` on the same rebuild. It is what the buttons register for now, and it is
/// what every corpus aura addon registers for.
fn push(s: &mut UiScript, auras: Vec<AuraState>) {
    s.set_auras("player", Some(auras));
    s.fire_event("PLAYER_AURAS_CHANGED", vec![]);
}

/// One tick of the app's real order (OnUpdate → resolve → draw list), returning the quads.
fn frame(s: &mut UiScript, dt: f32) -> Vec<ExtractedQuad> {
    s.tick(dt);
    s.resolve();
    s.extract()
}

fn shown(s: &UiScript, name: &str) -> bool {
    s.eval::<bool>(&format!("return {name}:IsShown()")).unwrap()
}

fn text(s: &UiScript, name: &str) -> String {
    s.eval::<String>(&format!("return tostring(({name}:GetText()) or \"\")"))
        .unwrap()
}

fn alpha(s: &UiScript, name: &str) -> f64 {
    s.eval::<f64>(&format!("return {name}:GetAlpha()")).unwrap()
}

/// The first quad drawn from `Interface\...\<leaf>` — texture regions keyed by their art.
fn tex_quad<'a>(quads: &'a [ExtractedQuad], leaf: &str) -> Option<&'a ExtractedQuad> {
    quads.iter().find(|q| match &q.content {
        QuadContent::Texture { path: Some(p), .. } => p.ends_with(leaf),
        _ => false,
    })
}

#[test]
fn buffs_and_debuffs_fill_their_own_rows_with_counts_and_the_dispel_tint() {
    let mut s = harness();
    push(
        &mut s,
        vec![
            aura(
                1126,
                "Mark of the Wild",
                "Interface\\Icons\\Spell_Nature_Regeneration",
                true,
                1,
                None,
                120.0,
                true,
            ),
            aura(
                589,
                "Shadow Word: Pain",
                "Interface\\Icons\\Spell_Shadow_ShadowWordPain",
                false,
                3,
                Some("Magic"),
                18.0,
                false,
            ),
        ],
    );
    let quads = frame(&mut s, 0.1);

    // The buff takes the first helpful button, the debuff the first harmful one; the rest hide.
    assert!(shown(&s, "BuffButton0"), "first buff -> BuffButton0");
    assert!(!shown(&s, "BuffButton1"), "no second buff");
    assert!(shown(&s, "BuffButton16"), "first debuff -> BuffButton16");
    assert!(!shown(&s, "BuffButton17"), "no second debuff");

    // Both icons drew.
    assert!(
        tex_quad(&quads, "Spell_Nature_Regeneration").is_some(),
        "buff icon drew"
    );
    assert!(
        tex_quad(&quads, "Spell_Shadow_ShadowWordPain").is_some(),
        "debuff icon drew"
    );

    // The debuff border wears the Magic tint (DebuffTypeColor["Magic"] = 0.20, 0.60, 1.00).
    let border = tex_quad(&quads, "UI-Debuff-Overlays").expect("debuff border drew");
    match &border.content {
        QuadContent::Texture {
            color: Some(c),
            tex_coords: Some(uv),
            ..
        } => {
            assert!(
                (c[0] - 0.20).abs() < 1e-3
                    && (c[1] - 0.60).abs() < 1e-3
                    && (c[2] - 1.00).abs() < 1e-3,
                "Magic tint, got {c:?}"
            );
            // The reference's overlay-quadrant crop rode through unchanged.
            let uv = uv.edges();
            assert!(
                (uv[0] - 0.296875).abs() < 1e-6 && (uv[3] - 0.515625).abs() < 1e-6,
                "debuff overlay tex-coords, got {uv:?}"
            );
        }
        other => panic!("border is not a tinted, cropped texture: {other:?}"),
    }

    // Stack count shows only above 1: the 3-stack debuff shows "3", the single buff shows nothing.
    assert_eq!(text(&s, "BuffButton16Count"), "3");
    assert_eq!(text(&s, "BuffButton0Count"), "");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

#[test]
fn a_timed_aura_counts_down_and_a_permanent_one_shows_no_timer() {
    let mut s = harness();
    push(
        &mut s,
        vec![
            // Permanent (a stance): no wire duration -> expiration 0 -> no timer.
            aura(
                2457,
                "Battle Stance",
                "Interface\\Icons\\Ability_Warrior_OffensiveStance",
                true,
                1,
                None,
                0.0,
                false,
            ),
            // Timed: 120s out. After a 0.1s tick GetTime()=0.1, so 119.9s -> SecondsToTimeAbbrev -> "2 m".
            aura(
                1126,
                "Mark of the Wild",
                "Interface\\Icons\\Spell_Nature_Regeneration",
                true,
                1,
                None,
                120.0,
                true,
            ),
        ],
    );
    frame(&mut s, 0.1);

    assert!(
        !shown(&s, "BuffButton0Duration"),
        "a permanent aura shows no timer (our 'until cancelled')"
    );
    assert!(shown(&s, "BuffButton1Duration"));
    assert_eq!(
        text(&s, "BuffButton1Duration"),
        "2 m",
        "120s remaining abbreviates to minutes"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

#[test]
fn the_warning_flash_pulses_a_low_aura_but_leaves_a_long_one_solid() {
    let mut s = harness();
    push(
        &mut s,
        vec![
            // 5 min out — never inside the 31s warning window, so it stays at full alpha.
            aura(
                1,
                "Long",
                "Interface\\Icons\\INV_Misc_QuestionMark",
                true,
                1,
                None,
                300.0,
                true,
            ),
            // 20s out — inside the window the whole run, so its button pulses.
            aura(
                2,
                "Short",
                "Interface\\Icons\\INV_Misc_QuestionMark",
                true,
                1,
                None,
                20.0,
                true,
            ),
        ],
    );

    let (mut short_min, mut long_min) = (2.0_f64, 2.0_f64);
    for _ in 0..20 {
        s.tick(0.1);
        short_min = short_min.min(alpha(&s, "BuffButton1"));
        long_min = long_min.min(alpha(&s, "BuffButton0"));
    }
    assert!(
        short_min < 0.9,
        "the 20s aura's icon pulses toward BUFF_MIN_ALPHA (min seen {short_min})"
    );
    assert!(
        (long_min - 1.0).abs() < 1e-9,
        "the 5min aura's icon stays solid (min seen {long_min})"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

#[test]
fn right_clicking_a_cancelable_buff_queues_its_spell_cancel() {
    let mut s = harness();
    push(
        &mut s,
        vec![aura(
            1126,
            "Mark of the Wild",
            "Interface\\Icons\\Spell_Nature_Regeneration",
            true,
            1,
            None,
            120.0,
            true,
        )],
    );
    frame(&mut s, 0.1);

    assert!(
        s.take_cancel_aura_requests().is_empty(),
        "nothing queued yet"
    );
    // The RightButtonUp handler -> CancelPlayerBuff(this.buffIndex) -> the app drains one
    // CMSG_CANCEL_AURA by spell id.
    s.eval::<()>("this = BuffButton0; BuffButton_OnClick()")
        .unwrap();
    assert_eq!(s.take_cancel_aura_requests(), vec![1126]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

#[test]
fn an_emptied_bar_hides_every_button() {
    let mut s = harness();
    push(
        &mut s,
        vec![aura(
            1126,
            "Mark of the Wild",
            "Interface\\Icons\\Spell_Nature_Regeneration",
            true,
            1,
            None,
            120.0,
            true,
        )],
    );
    frame(&mut s, 0.1);
    assert!(shown(&s, "BuffButton0"), "shown while the aura is up");

    // The aura drops: the feed pushes an empty list and re-fires. The button hides, and so does its
    // timer — a HIDDEN region since 1139, where it used to be blanked, so the text it last drew is
    // stale and beside the point; what the bar promises is that nothing paints.
    push(&mut s, vec![]);
    frame(&mut s, 0.1);
    assert!(!shown(&s, "BuffButton0"), "hidden once the aura is gone");
    assert!(!shown(&s, "BuffButton0Duration"));
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **Decision 0846's second defect, pinned.** The bar's countdown is a per-frame POLL, not a value
/// cached when the aura event fires — exactly as the reference does it (`BuffButton_OnUpdate` →
/// `GetPlayerBuffTimeLeft`, extracted 1.12 `BuffFrame.lua` l.128-130, which caches only the buff
/// *index* on `PLAYER_AURAS_CHANGED`).
///
/// This is what makes a *refresh* work. A recast, a reapplied DoT, cast pushback and the server's
/// replay of every duration at world entry all change an aura's expiry while changing nothing
/// discrete about it — same spell, same stacks, same dispel class — so the feed's change key does
/// not trip and **no aura event fires**. The push below therefore deliberately does NOT fire one.
/// Against the old event-cached bar the button kept counting the stale expiry down, reached zero
/// and sat at `"0 s"` for as long as the buff lived (live-measured: `app left 59.9s` beside
/// `lua left -9.9s`).
#[test]
fn a_refreshed_duration_reaches_the_bar_with_no_aura_event() {
    let mut s = harness();
    let mark = |expiry: f64| {
        aura(
            1126,
            "Mark of the Wild",
            "Interface\\Icons\\Spell_Nature_Regeneration",
            true,
            1,
            None,
            expiry,
            true,
        )
    };
    // 20s out. After the tick GetTime()=0.1 -> 19.9s -> "19 s" (SecondsToTimeAbbrev floors via %d).
    push(&mut s, vec![mark(20.0)]);
    frame(&mut s, 0.1);
    assert_eq!(text(&s, "BuffButton0Duration"), "19 s");

    // The refresh: a new expiry, everything discrete unchanged — so the feed pushes the list and
    // fires NOTHING. The button must still pick the new expiry up on its next frame.
    s.set_auras("player", Some(vec![mark(300.0)]));
    frame(&mut s, 0.1);
    assert_eq!(
        text(&s, "BuffButton0Duration"),
        "5 m",
        "the countdown re-reads the aura every frame; it does not wait for an event"
    );
    // ...and the flash follows it back out of the warning window (20s pulsed, 300s is solid).
    assert!(
        (alpha(&s, "BuffButton0") - 1.0).abs() < 1e-6,
        "no longer inside the 31s warning window, so full alpha"
    );

    // The same poll is what shows a timer that arrived AFTER the icon did — the fresh-apply case,
    // where the duration packet's stamp lands a frame later than the descriptor delta.
    //
    // **What the icon-first frame looks like MOVED when the bar came onto the 1.12 verbs.** The
    // permanence test used to be `expirationTime > 0`, which cannot tell "no duration, ever" from
    // "the duration has not landed yet" and so drew nothing for both. It is now `untilCancelled` —
    // GetPlayerBuff's second return, derived from the SPELL (`ui_aura::until_cancelled`) — so a
    // timed aura awaiting its stamp is known to be timed, and the reference counts it from `0 s`
    // and pulses it for the frame or two until the packet joins. `mark` is a timed spell, so that
    // is what it does here; the permanent case (a stance) is
    // [`a_timed_aura_counts_down_and_a_permanent_one_shows_no_timer`], and it still shows nothing.
    let mut pending = mark(0.0);
    pending.until_cancelled = false; // a timed spell, stamp not yet arrived
    s.set_auras("player", Some(vec![pending]));
    frame(&mut s, 0.1);
    assert_eq!(
        text(&s, "BuffButton0Duration"),
        "0 s",
        "the reference draws the floor of a timed aura it has no stamp for, not a blank"
    );
    s.set_auras("player", Some(vec![mark(60.0)])); // the stamp joins, still no event
    frame(&mut s, 0.1);
    assert_eq!(
        text(&s, "BuffButton0Duration"),
        "59 s",
        "a duration that lands after the icon still reaches the bar"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **`SHOW_BUFF_DURATIONS`** (decision 1139) — 0255 shipped the durations-shown geometry with no
/// switch because there was no panel to hang one on, and said the other branch was already there
/// waiting. This is it: the global (ours "1", the reference's "0") hides the timer text and closes
/// the 15px gutter each row leaves for it, down to the 5px the columns use. What it does NOT touch
/// is the warning flash — with the numbers gone, the pulse is the only thing left saying an aura is
/// about to drop, and the reference pulses from `BuffButton_OnUpdate` regardless of the setting.
#[test]
fn the_duration_switch_hides_the_timers_and_closes_their_gutter() {
    let mut s = harness();
    push(
        &mut s,
        vec![aura(
            1126,
            "Mark of the Wild",
            "Interface\\Icons\\Spell_Nature_Regeneration",
            true,
            1,
            None,
            20.0, // inside the 31s warning window, so the flash is live either way
            true,
        )],
    );
    frame(&mut s, 0.1);
    assert!(shown(&s, "BuffButton0Duration"), "timers on by default");
    assert_eq!(text(&s, "BuffButton0Duration"), "19 s");
    s.resolve();
    let shown_gap = s
        .eval::<f64>("return BuffButton0:GetBottom() - BuffButton8:GetTop()")
        .unwrap();
    assert!((shown_gap - 15.0).abs() < 1e-3, "gutter: {shown_gap}");

    // The switch, applied the way the options row applies it.
    s.run("SHOW_BUFF_DURATIONS = \"0\"; BuffButtons_UpdatePositions()")
        .unwrap();
    frame(&mut s, 0.1);
    assert!(!shown(&s, "BuffButton0Duration"), "no timer drawn");
    assert!(shown(&s, "BuffButton0"), "the icon stays");
    s.resolve();
    let hidden_gap = s
        .eval::<f64>("return BuffButton0:GetBottom() - BuffButton8:GetTop()")
        .unwrap();
    assert!((hidden_gap - 5.0).abs() < 1e-3, "gutter: {hidden_gap}");
    assert!(
        alpha(&s, "BuffButton0") < 1.0,
        "the last-31s pulse is not gated on the timers"
    );

    // And back: the text is re-written from the poll, not from anything it kept.
    s.run("SHOW_BUFF_DURATIONS = \"1\"; BuffButtons_UpdatePositions()")
        .unwrap();
    frame(&mut s, 0.1);
    assert!(shown(&s, "BuffButton0Duration"));
    assert_eq!(text(&s, "BuffButton0Duration"), "19 s");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A mixed bar: three buffs and two debuffs in one insertion-ordered cache.
fn mixed_bar() -> Vec<AuraState> {
    vec![
        // pos 0 — buff, cancelable, timed
        aura(
            1126,
            "Mark of the Wild",
            "Interface\\Icons\\A",
            true,
            1,
            None,
            120.0,
            true,
        ),
        // pos 1 — debuff, Magic, 3 stacks
        aura(
            589,
            "Shadow Word: Pain",
            "Interface\\Icons\\B",
            false,
            3,
            Some("Magic"),
            18.0,
            false,
        ),
        // pos 2 — buff, cancelable
        aura(
            1459,
            "Arcane Intellect",
            "Interface\\Icons\\C",
            true,
            1,
            None,
            600.0,
            true,
        ),
        // pos 3 — debuff, Poison
        aura(
            2818,
            "Deadly Poison",
            "Interface\\Icons\\D",
            false,
            5,
            Some("Poison"),
            12.0,
            false,
        ),
        // pos 4 — buff, NOT cancelable (a stance): right-click must be refused
        aura(
            2457,
            "Battle Stance",
            "Interface\\Icons\\E",
            true,
            1,
            None,
            0.0,
            false,
        ),
    ]
}

/// **The two index spaces, end to end.** The button `id` is a 0-based ordinal within its own
/// filter; `GetPlayerBuff` turns it into a CACHE POSITION that is absolute across filters. The bar
/// is only correct if the two agree: button N of a row must draw the aura the corpus's own walk
/// finds at that ordinal, and the position it cached must be the one an unfiltered walk reports.
///
/// The walk is spelled the way the corpus spells it — `while GetPlayerBuff(i) >= 0 do`
/// (`_LazyPig/LazyPig.lua:1174`, `Zorlen/Zorlen.lua:2797`) — because `-1`, not nil, is the
/// terminator, and because the default filter is `HELPFUL|HARMFUL`: the walk must see the debuffs
/// too or the whole thing silently halves.
#[test]
fn the_buttons_and_the_corpus_walk_agree_on_the_cache_positions() {
    let mut s = harness();
    push(&mut s, mixed_bar());
    frame(&mut s, 0.1);

    // The corpus walk: five auras, buffs AND debuffs, positions identical to the ordinals under
    // the default filter — and it terminates.
    let visited = s
        .eval::<i64>(
            r#"local i = 0
               while GetPlayerBuff(i) >= 0 do
                   assert((GetPlayerBuff(i)) == i, "unfiltered walk is position-identical")
                   i = i + 1
                   assert(i < 100, "GetPlayerBuff walk did not terminate")
               end
               return i"#,
        )
        .unwrap();
    assert_eq!(visited, 5, "three buffs and two debuffs, one cache");

    // Each row filled from its own filter, in cache order — and each button's CACHED position is
    // the absolute one, not its ordinal.
    for (button, position) in [
        ("BuffButton0", 0),  // helpful ordinal 0 -> position 0
        ("BuffButton1", 2),  // helpful ordinal 1 -> position 2 (the debuff at 1 is skipped)
        ("BuffButton2", 4),  // helpful ordinal 2 -> position 4
        ("BuffButton16", 1), // harmful ordinal 0 -> position 1
        ("BuffButton17", 3), // harmful ordinal 1 -> position 3
    ] {
        assert!(shown(&s, button), "{button} draws its aura");
        assert_eq!(
            s.eval::<i64>(&format!("return {button}.buffIndex"))
                .unwrap(),
            position,
            "{button} caches the CACHE POSITION, not its per-filter ordinal"
        );
    }
    // The rows stop where the filter runs out; nothing bleeds across.
    assert!(!shown(&s, "BuffButton3"), "only three buffs");
    assert!(!shown(&s, "BuffButton18"), "only two debuffs");
    assert_eq!(
        s.eval::<i64>("return BuffButton3.buffIndex").unwrap(),
        -1,
        "a miss is -1, the sentinel the corpus terminates on — never nil"
    );

    // The reads that hang off the position land on the right aura: the 3-stack Magic debuff and
    // the 5-stack Poison one are in the right buttons, tinted from their own dispel classes.
    assert_eq!(text(&s, "BuffButton16Count"), "3");
    assert_eq!(text(&s, "BuffButton17Count"), "5");
    assert_eq!(
        text(&s, "BuffButton0Count"),
        "",
        "a single stack shows nothing"
    );
    let quads = frame(&mut s, 0.1);
    let poison = quads
        .iter()
        .filter_map(|q| match &q.content {
            QuadContent::Texture {
                path: Some(p),
                color: Some(c),
                ..
            } if p.ends_with("UI-Debuff-Overlays") => Some(*c),
            _ => None,
        })
        // DebuffTypeColor["Poison"] = 0.00, 0.60, 0.00
        .any(|c| c[0].abs() < 1e-3 && (c[1] - 0.60).abs() < 1e-3 && c[2].abs() < 1e-3);
    assert!(poison, "the second debuff button wears the Poison tint");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **Right-click cancels by cache position.** `BuffButton_OnClick` is
/// `CancelPlayerBuff(this.buffIndex)` — the reference's own line — so the button that gets
/// cancelled is the one under the cursor even when its ordinal and its position differ, which is
/// the case for every buff after the first debuff.
///
/// The gate is the app's, unchanged: a cancelable buff queues its SPELL id, a stance and a debuff
/// are silent no-ops. That last part is what a per-filter id could not guarantee on its own — here
/// it is structural, because a harmful button's handle simply names a harmful record.
#[test]
fn right_clicking_cancels_the_aura_under_the_cursor_by_its_cache_position() {
    let mut s = harness();
    push(&mut s, mixed_bar());
    frame(&mut s, 0.1);
    assert!(s.take_cancel_aura_requests().is_empty());

    // The SECOND buff button: helpful ordinal 1, cache position 2, spell 1459. A bar that cancelled
    // by ordinal would send 589 — the debuff sitting at position 1.
    s.eval::<()>("this = BuffButton1; BuffButton_OnClick()")
        .unwrap();
    assert_eq!(
        s.take_cancel_aura_requests(),
        vec![1459],
        "the aura under the cursor, not the record at the ordinal"
    );

    // A stance (helpful, not cancelable), both debuff buttons, and an empty slot: all silent.
    for button in ["BuffButton2", "BuffButton16", "BuffButton17", "BuffButton7"] {
        s.eval::<()>(&format!("this = {button}; BuffButton_OnClick()"))
            .unwrap();
    }
    assert!(
        s.take_cancel_aura_requests().is_empty(),
        "a stance, a debuff and an empty button are all no-ops"
    );

    // The first buff still cancels normally.
    s.eval::<()>("this = BuffButton0; BuffButton_OnClick()")
        .unwrap();
    assert_eq!(s.take_cancel_aura_requests(), vec![1126]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **The temporary-enchant row** (ref `BuffFrame_Enchant_OnUpdate`, l.162-233). With nothing
/// enchanted it hides both slots and parks the bar on its resting point; with a weapon enchanted it
/// shows the icon, counts the expiry down from MILLISECONDS, and slides the top buff row left to
/// clear it — while rows 2 and 3, which hang off TempEnchant1 and TemporaryEnchantFrame, stay put.
#[test]
fn the_temporary_enchant_row_shows_a_weapon_enchant_and_moves_the_top_row_aside() {
    let mut s = harness();
    // A LOADED bar, deliberately: the enchant slots must be driven by GetWeaponEnchantInfo and by
    // nothing else. The reference builds them from `BuffButtonTemplate` and blanks the four aura
    // handlers with empty `<OnLoad/>`/`<OnEvent/>`/`<OnClick/>` elements; our loader treats an
    // empty handler body as "none given" and so KEEPS the inherited one (`compile_handler` returns
    // None and `apply_scripts` never calls SetScript). Under that idiom TempEnchant1 would take
    // `BuffButton_OnLoad`, register PLAYER_AURAS_CHANGED, and paint the first buff in the corner —
    // and a right-click on it would cancel that aura. Hence the standalone template, and hence a
    // populated bar here: an empty aura list cannot tell the two apart.
    push(&mut s, mixed_bar());
    frame(&mut s, 0.1);

    // Nothing enchanted: both slots hidden, and the bar sits where it has always sat.
    assert!(
        !shown(&s, "TempEnchant1"),
        "the enchant slots are not buff buttons: no aura may reach them"
    );
    assert!(!shown(&s, "TempEnchant2"));
    // The direct probe: none of the three aura handlers is even REGISTERED on an enchant slot.
    // This is what the reference's empty-element idiom is supposed to achieve and what our loader
    // does not do, so it is asserted rather than assumed.
    for handler in ["OnLoad", "OnEvent", "OnClick"] {
        assert!(
            s.eval::<bool>(&format!(
                "return TempEnchant1:GetScript(\"{handler}\") == nil"
            ))
            .unwrap(),
            "an enchant slot must carry no {handler} — it is not a buff button"
        );
    }
    s.resolve();
    let resting = s.eval::<f64>("return BuffFrame:GetRight()").unwrap();
    let row2 = s.eval::<f64>("return BuffButton16:GetRight()").unwrap();
    assert!(
        (resting - (1024.0 - 175.0)).abs() < 1e-3,
        "UIParent TOPRIGHT -175: {resting}"
    );

    // A main-hand enchant, 8 minutes out. The app feeds it the way `ui_char` does.
    s.set_weapon_enchants(
        Some(benilla_ui::script::WeaponEnchant {
            remaining_ms: Some(480_000),
            charges: 0,
        }),
        None,
    );
    frame(&mut s, 0.1);
    assert!(
        shown(&s, "TempEnchant1"),
        "the enchanted weapon takes slot 1"
    );
    assert!(!shown(&s, "TempEnchant2"));
    assert_eq!(
        s.eval::<i64>("return TempEnchant1:GetID()").unwrap(),
        16,
        "the live-API MainHandSlot id, which is what its item tooltip hover needs"
    );
    assert_eq!(
        text(&s, "TempEnchant1Duration"),
        "8 m",
        "480000 ms / 1000 -> 480 s -> SecondsToTimeAbbrev"
    );

    // The top buff row slid left by one icon plus the 5px gutter; the debuff row did not move.
    s.resolve();
    let shifted = s.eval::<f64>("return BuffFrame:GetRight()").unwrap();
    assert!(
        (resting - shifted - 37.0).abs() < 1e-3,
        "one 32px enchant column + the 5px gutter: {resting} -> {shifted}"
    );
    assert!(
        (s.eval::<f64>("return BuffButton16:GetRight()").unwrap() - row2).abs() < 1e-3,
        "the debuff row hangs off TemporaryEnchantFrame and stays put"
    );

    // And back: the enchant drops, the row hides and the bar returns to its resting point.
    s.set_weapon_enchants(None, None);
    frame(&mut s, 0.1);
    assert!(!shown(&s, "TempEnchant1"));
    s.resolve();
    assert!((s.eval::<f64>("return BuffFrame:GetRight()").unwrap() - resting).abs() < 1e-3);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
