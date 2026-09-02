//! The shipped **Reputation tab** (`assets/ui/ReputationFrame.xml`) and the **reputation watch
//! bar** (`assets/ui/ActionBar.xml`) driven end-to-end, engine-only (no Bevy) — the per-window test
//! module the skills/spellbook/bank files already establish.
//!
//! What it pins is the PAINT law, which is the half `benilla-ui`'s own
//! `script::reputation::tests` structurally cannot reach: that module drives the twelve globals and
//! asserts the tuples, and stops at the seam. Everything below is about what the tuples become on
//! screen — which of the 15 fixed row slots a visible index lands in, whether a header slot draws
//! the plus or the minus art, the `FACTION_BAR_COLORS[standingID]` fill, the scroll offset
//! re-binding the slots, the popup's three boxes, and the watch bar's own text and posture.
//!
//! The fixture deliberately mirrors the engine module's own (Alliance over Ironforge + Stormwind,
//! Steamwheedle over Booty Bay, the parentless bucket last) so a failure here reads against a shape
//! that is already pinned one layer down, and the two can be compared line for line.
//!
//! **The window around the page is the reference's own since 1751** — `CharacterFrame.xml` and
//! `PaperDollFrame.xml` off the player's chain — so every test here opens with
//! `wow_data_or_skip!()` and loads [`super::test_ui::CHARACTER_UI`]. The page itself
//! (`ReputationFrame.xml`) and the watch bar (`ActionBar.xml`) are still ours.

use benilla_ui::script::{FactionEntry, ReputationState, UiScript, UnitState};

/// An ordinary bar row: visible, not a header, `standing_id` 5 ("Friendly") sitting 1000 into a
/// 6000-wide rank window. The same numbers `benilla-ui`'s own fixture uses.
fn entry(faction_id: u32, rep_list_id: u32, parent_id: u32, name: &str) -> FactionEntry {
    FactionEntry {
        faction_id,
        rep_list_id,
        parent_id,
        name: name.into(),
        description: format!("About the {name}."),
        standing: 4000,
        standing_id: 5,
        bar_min: 3000,
        bar_max: 9000,
        visible: true,
        is_header: false,
        at_war: false,
        can_toggle_at_war: true,
        inactive: false,
    }
}

/// A header row as the wire really delivers one: flag `0x08` set and NOT visible (the Steamwheedle
/// Cartel's actual byte). Its own visibility never gates its group.
fn header(faction_id: u32, rep_list_id: u32, name: &str) -> FactionEntry {
    FactionEntry {
        visible: false,
        is_header: true,
        ..entry(faction_id, rep_list_id, 0, name)
    }
}

/// The engine module's own fixture, pushed out of order so the engine's sort is what shows. The
/// visible rows it produces, in order:
///
/// | slot | row |
/// |------|-----|
/// | 1 | header `Alliance` |
/// | 2 | `Ironforge` |
/// | 3 | `Stormwind` |
/// | 4 | header `Steamwheedle Cartel` |
/// | 5 | `Booty Bay` |
/// | 6 | header `Other` |
/// | 7 | `Argent Dawn` |
/// | 8 | `Bloodsail Buccaneers` |
fn state() -> ReputationState {
    ReputationState {
        entries: vec![
            entry(72, 19, 469, "Stormwind"),
            entry(529, 13, 0, "Argent Dawn"),
            header(469, 11, "Alliance"),
            entry(21, 1, 169, "Booty Bay"),
            entry(47, 20, 469, "Ironforge"),
            header(169, 10, "Steamwheedle Cartel"),
            entry(87, 0, 0, "Bloodsail Buccaneers"),
        ],
        watched: None,
    }
}

/// The character window, whose third tab this page is — [`super::test_ui::CHARACTER_UI`], which
/// already carries `ReputationFrame.xml` (stock `CharacterFrame_ShowSubFrame` hides all five pages
/// by name, unguarded) and `ActionBar.xml` (the watch bar, and the show/hide pair's
/// `ShowWatchedReputationBarText`). This page adds nothing of its own.
///
/// It was a hand-copied manifest prefix until the window became the reference's (decision 1751):
/// stock `CharacterFrame_OnLoad` reaches into four other files before it does anything else, so the
/// list stopped being short enough for a per-module copy to stay honest.
fn load_page(s: &UiScript) {
    for file in super::test_ui::CHARACTER_UI {
        super::test_ui::load_ui_strict(s, file);
    }
}

/// A level-`level` player behind the window — enough of one that opening it is quiet.
///
/// **Race and class carry both halves of their pairs**, which is not decoration since 1751:
/// `UnitRace`/`UnitClass` answer `(localized, file)` or `nil, nil` — the binding `zip`s the two —
/// and stock `PaperDollFrame_SetLevel` formats level, race and class into `CharacterLevelText`
/// unguarded (`PaperDollFrame.lua:100-104`), on every show of the window this page is a tab of.
/// A `UnitState::default()` player therefore raises the moment `ToggleCharacter` runs.
fn player(level: u32) -> UnitState {
    UnitState {
        exists: true,
        level,
        race: Some("Human".into()),
        race_file: Some("Human".into()),
        class: Some("Warrior".into()),
        class_file: Some("WARRIOR".into()),
        ..UnitState::default()
    }
}

/// The Reputation page, open on its tab, with [`state`] pushed and a level-40 player behind it.
fn shown_reputation_page() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_page(&s);
    s.set_unit("player", Some(player(40)));
    s.set_reputation(state());
    s.run(r#"ToggleCharacter("ReputationFrame")"#).unwrap();
    s.resolve();
    s
}

/// Click the centre of a named frame through the real pointer pipeline, the way the app does — so
/// a frame silently eating the row's clicks fails here rather than on the director's screen.
fn click_center(s: &mut UiScript, name: &str) {
    s.resolve();
    let (x, y) = s
        .eval::<(f32, f32)>(&format!(
            "return ({name}:GetLeft() + {name}:GetRight()) / 2, \
                    ({name}:GetTop() + {name}:GetBottom()) / 2"
        ))
        .unwrap();
    s.mouse_move(x, y);
    s.mouse_button(x, y, "LeftButton", true);
    s.mouse_button(x, y, "LeftButton", false);
}

fn text_of(s: &mut UiScript, expr: &str) -> String {
    s.eval::<String>(&format!("return {expr}:GetText() or \"\""))
        .unwrap()
}

fn shown(s: &mut UiScript, name: &str) -> bool {
    s.eval::<bool>(&format!("return {name}:IsShown() and true or false"))
        .unwrap()
}

/// **The page is the character window's tab 3, and it comes up on it.** The whole renumber rides on
/// this: `ToggleCharacter` selects the row from the page's own `id=`
/// (ref `CharacterFrame.lua:11`), so a page seated at the wrong id shows the right pane under the
/// wrong tab. Skills moved to 4 in the same breath and is asserted here for the same reason.
#[test]
fn the_reputation_page_opens_on_the_windows_third_tab() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = shown_reputation_page();
    assert!(
        s.eval::<bool>("return ReputationFrame:IsVisible()")
            .unwrap(),
        "the Reputation page is up"
    );
    assert_eq!(
        s.eval::<i64>("return PanelTemplates_GetSelectedTab(CharacterFrame)")
            .unwrap(),
        3,
        "and the tab row selects 3 — the reference's own slot for Reputation"
    );
    // **The id IS the tab slot**, and that is the whole coupling: `ToggleCharacter` selects the tab
    // with `PanelTemplates_SetTab(CharacterFrame, subFrame:GetID())`
    // (`CharacterFrame.lua:10`), while `CharacterFrameTab_OnClick` hardcodes which page each tab
    // opens (`:41-43` — tab 3 → Reputation, tab 4 → Skills). A page whose `id=` disagreed would
    // open on click and light the wrong tab.
    assert_eq!(
        s.eval::<i64>("return ReputationFrame:GetID()").unwrap(),
        3,
        "the id IS the tab slot the reference's tab 3 opens"
    );
    assert_eq!(
        s.eval::<i64>("return SkillFrame:GetID()").unwrap(),
        4,
        "and Skills is the reference's 4 beside it"
    );
    // **`CHARACTERFRAME_SUBFRAMES` is the hide-all list, not the tab map** — re-pointed here at
    // 1751, because our deleted `CharacterFrame.xml` wrote the two as one thing and the
    // reference's does not. Stock line 1 is
    // `{ "PaperDollFrame", "PetPaperDollFrame", "SkillFrame", "ReputationFrame", "HonorFrame" }` —
    // Skills THIRD and Reputation FOURTH, the opposite of their tab slots — and it is only ever
    // walked by `CharacterFrame_ShowSubFrame`, which shows the one it was named and hides the rest
    // (`CharacterFrame.lua:25-33`). So what this page needs from the list is membership: a page
    // missing from it is a page nothing ever hides, which is the "one page at a time" assertion at
    // the end of this test.
    assert!(
        s.eval::<bool>(
            "for _, v in CHARACTERFRAME_SUBFRAMES do if v == \"ReputationFrame\" then return true end end return false"
        )
        .unwrap(),
        "the hide-all list names this page"
    );

    // Eight rows in fifteen slots: the tail goes dark on BOTH twins, and a list that fits raises
    // neither the scroll bar nor its trough (the kit shows and hides the pair together).
    assert_eq!(s.eval::<i64>("return GetNumFactions()").unwrap(), 8);
    assert!(
        !shown(&mut s, "ReputationBar9"),
        "slot 9 has nothing to hold"
    );
    assert!(!shown(&mut s, "ReputationHeader9"), "neither twin shows");
    assert!(!shown(&mut s, "ReputationListScrollFrameScrollBar"));
    assert!(!shown(&mut s, "ReputationListScrollFrameScrollBarTrough"));

    // Switching away hides it again through the same one-page-at-a-time switch.
    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    assert!(!shown(&mut s, "ReputationFrame"), "one page at a time");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **A header slot draws the fold icon its state calls for, and a click folds the group.**
///
/// Two laws in one: the minus/plus art is chosen from `isCollapsed` (ref `ReputationFrame.lua`
/// l.54-58), and the click has to REPAINT — `CollapseFactionHeader` is a pure model write in this
/// engine where the client's C binding raises `UPDATE_FACTION`, so a handler that only calls the
/// global leaves the folded rows on screen (ReputationFrame.xml's deviation 3).
#[test]
fn a_header_row_paints_its_fold_icon_and_folds_on_click() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = shown_reputation_page();

    assert_eq!(text_of(&mut s, "ReputationHeader1"), "Alliance");
    assert!(shown(&mut s, "ReputationHeader1"), "slot 1 is a header");
    assert!(!shown(&mut s, "ReputationBar1"), "so its bar is down");
    let icon = |s: &mut UiScript| {
        s.eval::<String>("return ReputationHeader1:GetNormalTexture():GetTexture() or \"\"")
            .unwrap()
    };
    assert!(
        icon(&mut s).contains("UI-MinusButton-Up"),
        "an expanded header wears the MINUS: {}",
        icon(&mut s)
    );
    assert_eq!(
        text_of(&mut s, "ReputationBar2FactionName"),
        "Ironforge",
        "its two children follow it"
    );

    click_center(&mut s, "ReputationHeader1");

    assert!(
        icon(&mut s).contains("UI-PlusButton-Up"),
        "a collapsed header wears the PLUS: {}",
        icon(&mut s)
    );
    assert_eq!(
        text_of(&mut s, "ReputationHeader2"),
        "Steamwheedle Cartel",
        "and slot 2 has re-bound to the next group — the repaint the click owes"
    );
    assert!(
        !shown(&mut s, "ReputationBar2"),
        "Ironforge is not on screen at all any more"
    );
    assert_eq!(
        s.eval::<i64>("return GetNumFactions()").unwrap(),
        6,
        "the two folded children leave the visible list"
    );

    // …and unfolding puts them straight back, through the same handler.
    click_center(&mut s, "ReputationHeader1");
    assert_eq!(text_of(&mut s, "ReputationBar2FactionName"), "Ironforge");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **A bar row paints its name, its standing word and its `FACTION_BAR_COLORS[standingID]` fill**,
/// over a bar whose range is the rank window NORMALIZED — `barMax - barMin` and `barValue - barMin`
/// (ref `ReputationFrame.lua` l.79-82), which is the entire reason the engine reports those three
/// absolute.
#[test]
fn a_bar_row_paints_name_standing_and_the_faction_bar_colour() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = shown_reputation_page();

    assert_eq!(text_of(&mut s, "ReputationBar2FactionName"), "Ironforge");
    assert_eq!(
        text_of(&mut s, "ReputationBar2FactionStanding"),
        "Friendly",
        "standingID 5 is FACTION_STANDING_LABEL5"
    );
    assert_eq!(
        s.eval::<(f64, f64)>("return ReputationBar2:GetMinMaxValues()")
            .unwrap(),
        (0.0, 6000.0),
        "the 3000..9000 rank window, normalized to 0..6000"
    );
    assert_eq!(
        s.eval::<f64>("return ReputationBar2:GetValue()").unwrap(),
        1000.0,
        "and 4000 standing is 1000 into it"
    );
    let (r, g, b) = s
        .eval::<(f32, f32, f32)>("return ReputationBar2:GetStatusBarColor()")
        .unwrap();
    assert_eq!(
        (r, g, b),
        (0.0, 0.6, 0.1),
        "FACTION_BAR_COLORS[5] — the green every friendly-and-better rank shares"
    );

    // The hover swaps the standing word for the raw progress and lights the glow pair; leaving puts
    // the word back (ref ReputationFrame.xml l.189-203).
    assert!(
        !shown(&mut s, "ReputationBar2Highlight1"),
        "glow starts down"
    );
    let (x, y) = s
        .eval::<(f32, f32)>(
            "return (ReputationBar2:GetLeft() + ReputationBar2:GetRight()) / 2, \
                    (ReputationBar2:GetTop() + ReputationBar2:GetBottom()) / 2",
        )
        .unwrap();
    s.mouse_move(x, y);
    assert_eq!(
        text_of(&mut s, "ReputationBar2FactionStanding"),
        "|cffffffff 1000 / 6000|r",
        "hovering shows the numbers, in the reference's own colour-coded form"
    );
    assert!(shown(&mut s, "ReputationBar2Highlight1"));
    assert!(shown(&mut s, "ReputationBar2Highlight2"));
    s.mouse_move(0.0, 0.0);
    assert_eq!(
        text_of(&mut s, "ReputationBar2FactionStanding"),
        "Friendly",
        "and leaving restores the word"
    );
    assert!(
        !shown(&mut s, "ReputationBar2Highlight1"),
        "the glow goes with it — this row is not the selected one"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **The 15 row slots are a window onto the list, and the scroll offset is what moves it.** The
/// faux kit re-binds fixed slots to a moving data offset rather than scrolling anything (decision
/// 0251), so this is the assertion that the slots really are re-bound and not just clipped.
#[test]
fn the_scroll_offset_rebinds_the_fixed_row_slots() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_page(&s);
    s.set_unit("player", Some(player(40)));

    // One header over twenty children: 21 visible rows against 15 slots.
    let mut entries = vec![header(469, 11, "Alliance")];
    for i in 1..=20u32 {
        entries.push(entry(1000 + i, i, 469, &format!("Faction {i:02}")));
    }
    s.set_reputation(ReputationState {
        entries,
        watched: None,
    });
    s.run(r#"ToggleCharacter("ReputationFrame")"#).unwrap();
    s.resolve();

    assert_eq!(s.eval::<i64>("return GetNumFactions()").unwrap(), 21);
    assert_eq!(text_of(&mut s, "ReputationHeader1"), "Alliance");
    assert_eq!(text_of(&mut s, "ReputationBar1FactionName"), "");
    assert_eq!(text_of(&mut s, "ReputationBar15FactionName"), "Faction 14");
    assert!(
        shown(&mut s, "ReputationListScrollFrameScrollBar"),
        "21 rows in 15 slots raises the scroll bar"
    );
    assert!(
        shown(&mut s, "ReputationListScrollFrameScrollBarTrough"),
        "and the trough it rides in comes up with it (the kit shows the pair together)"
    );

    // **The trough fits this window to the pixel, and that is not a coincidence.**
    // `BenillaScrollTrough_Seat`'s 21-above / 20-below / 8-left hang was DERIVED from
    // ReputationFrame's own reference anchors (ScrollTemplates.xml cites ref l.573-599 against
    // ref-UIPanelTemplates.xml l.166-181), so the seated trough has to land back on the reference's
    // own numbers here: TOPRIGHT + (-2, +5), BOTTOMRIGHT + (+29, -4), 31 wide. Miss it by 4 and the
    // arrow buttons ride out of their sockets onto the caps, which is what B224 reported.
    let d = |s: &mut UiScript, expr: &str| s.eval::<f64>(&format!("return {expr}")).unwrap();
    let sf_right = d(&mut s, "ReputationListScrollFrame:GetRight()");
    let sf_top = d(&mut s, "ReputationListScrollFrame:GetTop()");
    let sf_bottom = d(&mut s, "ReputationListScrollFrame:GetBottom()");
    let t = "ReputationListScrollFrameScrollBarTrough";
    assert_eq!(d(&mut s, &format!("{t}:GetLeft()")), sf_right - 2.0);
    assert_eq!(d(&mut s, &format!("{t}:GetRight()")), sf_right + 29.0);
    assert_eq!(d(&mut s, &format!("{t}:GetTop()")), sf_top + 5.0);
    assert_eq!(d(&mut s, &format!("{t}:GetBottom()")), sf_bottom - 4.0);
    assert_eq!(d(&mut s, &format!("{t}:GetWidth()")), 31.0);

    // Scroll three rows down: every slot re-binds, and slot 1 stops being a header.
    s.run("FauxScrollFrame_SetOffset(ReputationListScrollFrame, 3) ReputationFrame_Update()")
        .unwrap();
    assert!(
        !shown(&mut s, "ReputationHeader1"),
        "slot 1 now holds a bar, so its header twin is down"
    );
    assert_eq!(text_of(&mut s, "ReputationBar1FactionName"), "Faction 03");
    assert_eq!(text_of(&mut s, "ReputationBar15FactionName"), "Faction 17");

    // Past the end the kit CLAMPS rather than running off it: 21 rows in 15 slots bottom out at
    // offset 6, so asking for 9 lands on that same last page with the final row in slot 15.
    s.run("FauxScrollFrame_SetOffset(ReputationListScrollFrame, 9) ReputationFrame_Update()")
        .unwrap();
    assert_eq!(
        s.eval::<i64>("return FauxScrollFrame_GetOffset(ReputationListScrollFrame)")
            .unwrap(),
        6,
        "clamped to numItems - numToDisplay, so no slot can bind past the end"
    );
    assert_eq!(text_of(&mut s, "ReputationBar1FactionName"), "Faction 06");
    assert_eq!(text_of(&mut s, "ReputationBar15FactionName"), "Faction 20");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **Clicking a bar opens the detail popup on that faction**, with its name, its description and
/// its three switches in the states the row's own flags call for — including the disabled, greyed
/// At War box a peace-forced faction gets (ref `ReputationFrame.lua` l.115-121). Clicking the same
/// bar again closes it, which is the reference's own toggle (l.152-153).
#[test]
fn clicking_a_bar_opens_the_detail_popup_on_that_faction() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_page(&s);
    s.set_unit("player", Some(player(40)));
    // Ironforge at war and peace-forced — the two flags the popup reads back off the row.
    let mut st = state();
    for e in &mut st.entries {
        if e.name == "Ironforge" {
            e.at_war = true;
            e.can_toggle_at_war = false;
        }
    }
    s.set_reputation(st);
    s.run(r#"ToggleCharacter("ReputationFrame")"#).unwrap();
    s.resolve();

    assert!(!shown(&mut s, "ReputationDetailFrame"), "closed to start");
    assert!(
        shown(&mut s, "ReputationBar2AtWarCheck"),
        "the at-war pennant flies on the row itself"
    );

    click_center(&mut s, "ReputationBar2");

    assert!(shown(&mut s, "ReputationDetailFrame"), "the popup opened");
    assert_eq!(
        s.eval::<i64>("return GetSelectedFaction()").unwrap(),
        2,
        "on Ironforge's visible row"
    );
    assert_eq!(text_of(&mut s, "ReputationDetailFactionName"), "Ironforge");
    assert_eq!(
        text_of(&mut s, "ReputationDetailFactionDescription"),
        "About the Ironforge."
    );
    assert!(
        shown(&mut s, "ReputationBar2Highlight1"),
        "and the selected row keeps its glow with the mouse away"
    );

    let checked = |s: &mut UiScript, box_name: &str| {
        s.eval::<bool>(&format!("return {box_name}:GetChecked() and true or false"))
            .unwrap()
    };
    assert!(
        checked(&mut s, "ReputationDetailAtWarCheckBox"),
        "at war, so the box is ticked"
    );
    assert!(
        !s.eval::<bool>("return ReputationDetailAtWarCheckBox:IsEnabled() ~= 0")
            .unwrap(),
        "peace-forced, so the box is dead"
    );
    let (r, g, b) = s
        .eval::<(f32, f32, f32)>("return ReputationDetailAtWarCheckBoxText:GetTextColor()")
        .unwrap();
    assert_eq!(
        (r, g, b),
        (0.5, 0.5, 0.5),
        "and its label greys with it (GRAY_FONT_COLOR)"
    );
    assert!(!checked(&mut s, "ReputationDetailInactiveCheckBox"));
    assert!(!checked(&mut s, "ReputationDetailMainScreenCheckBox"));

    // The reference's toggle: the same bar again closes the popup rather than re-opening it.
    click_center(&mut s, "ReputationBar2");
    assert!(!shown(&mut s, "ReputationDetailFrame"), "clicked shut");

    // **The whole ROW is the click target, not just the 137px bar.** The template's
    // `HitRectInsets left="-126"` (ref `ReputationFrame.xml` l.61-63) is a NEGATIVE inset, which
    // WIDENS the mouse rect back over the faction name — so clicking "Ironforge" selects it exactly
    // as clicking its bar does. Driven from the name's own centre, which is outside the bar's rect.
    let name_x = s
        .eval::<f32>(
            "return (ReputationBar2FactionName:GetLeft() + ReputationBar2FactionName:GetRight()) / 2",
        )
        .unwrap();
    let bar_left = s.eval::<f32>("return ReputationBar2:GetLeft()").unwrap();
    assert!(
        name_x < bar_left,
        "the name really does sit left of the bar ({name_x} < {bar_left})"
    );
    let y = s
        .eval::<f32>("return (ReputationBar2:GetTop() + ReputationBar2:GetBottom()) / 2")
        .unwrap();
    assert_eq!(
        s.hit_test_name(name_x, y).as_deref(),
        Some("ReputationBar2"),
        "the widened hit rect reaches the name"
    );
    s.mouse_move(name_x, y);
    s.mouse_button(name_x, y, "LeftButton", true);
    s.mouse_button(name_x, y, "LeftButton", false);
    assert!(
        shown(&mut s, "ReputationDetailFrame"),
        "and a click there opens the popup, the same as a click on the bar"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **The watch bar shows what is being watched, and swaps posture at max level.**
///
/// Below max level it STACKS above the XP bar, 8px tall, wearing the `UI-ReputationWatchBar` end
/// art; at max level it REPLACES the XP bar, 13px tall on MainMenuBar's own top, wearing the dwarf
/// art instead so the swap reads as the same bar (ref `ReputationFrame.lua` l.184-230). The rested
/// tick goes with the XP bar it rode, which is one of the two guards that were cut from
/// `ExhaustionTick_Update` as unreachable while this bar could never show.
#[test]
fn the_watch_bar_shows_the_watched_factions_progress_and_swaps_at_max_level() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_page(&s);
    s.set_unit("player", Some(player(40)));
    assert!(
        !shown(&mut s, "ReputationWatchBar"),
        "nothing watched: the bar stays down, as it has since it was a stub"
    );

    // Ironforge is rep slot 20; watching it is a server field, so it rides the push.
    let mut st = state();
    st.watched = Some(20);
    s.set_reputation(st);
    s.fire_event("UPDATE_FACTION", vec![]);
    s.resolve();

    assert!(shown(&mut s, "ReputationWatchBar"), "the bar came up");
    assert_eq!(
        text_of(&mut s, "ReputationWatchStatusBarText"),
        "Ironforge 1000 / 6000",
        "name plus the NORMALIZED progress, the reference's own string"
    );
    let (r, g, b) = s
        .eval::<(f32, f32, f32)>("return ReputationWatchStatusBar:GetStatusBarColor()")
        .unwrap();
    assert_eq!((r, g, b), (0.0, 0.6, 0.1), "FACTION_BAR_COLORS[5] again");
    assert_eq!(
        s.eval::<(f64, f64)>("return ReputationWatchStatusBar:GetMinMaxValues()")
            .unwrap(),
        (0.0, 6000.0)
    );
    assert!(
        shown(&mut s, "MainMenuExpBar"),
        "below 60 the strip STACKS on the XP bar rather than replacing it"
    );
    assert!(
        shown(&mut s, "ReputationWatchBarTexture0"),
        "rep end art up"
    );
    assert!(!shown(&mut s, "ReputationXPBarTexture0"), "dwarf art down");
    assert_eq!(
        s.eval::<f64>("return ReputationWatchStatusBar:GetHeight()")
            .unwrap(),
        8.0
    );

    // **`UIParent_ManageFramePositions`'s `reputation` branch fires for the first time.** It has
    // been live in UIParent.xml since decision 0272 and gated on
    // `ReputationWatchBar:IsShown() and MainMenuExpBar:IsShown()`, which that file's own header
    // notes "still cannot fire" — it can now, and this is the check that it does. `PETACTIONBAR_YPOS`
    // is the cheapest witness: an `isVar` row the pass writes as a plain global (baseY 97, plus the
    // row's own `reputation = 9`), so it needs no frame loaded to read back.
    assert_eq!(
        s.eval::<f64>("return PETACTIONBAR_YPOS").unwrap(),
        106.0,
        "the bottom stack lifts by the row's 9 while the stacked watch bar is up"
    );
    s.run("ReputationWatchBar:Hide() UIParent_ManageFramePositions()")
        .unwrap();
    assert_eq!(
        s.eval::<f64>("return PETACTIONBAR_YPOS").unwrap(),
        97.0,
        "and drops back to the base when it goes"
    );
    s.run("ReputationWatchBar:Show() UIParent_ManageFramePositions()")
        .unwrap();

    // Ding to 60: the reputation strip takes the XP bar's place, art and all.
    s.set_unit("player", Some(player(60)));
    s.fire_event(
        "PLAYER_LEVEL_UP",
        vec![benilla_ui::script::ScriptValue::Int(60)],
    );
    s.resolve();

    assert!(shown(&mut s, "ReputationWatchBar"), "still watching");
    assert!(
        !shown(&mut s, "MainMenuExpBar"),
        "at 60 the strip REPLACES the XP bar"
    );
    assert!(
        !shown(&mut s, "MainMenuBarMaxLevelBar"),
        "and the brass rail stays down — the watched bar is what fills that space"
    );
    assert!(
        !shown(&mut s, "ExhaustionTick"),
        "no XP strip, no rested tick"
    );
    assert!(shown(&mut s, "ReputationXPBarTexture0"), "dwarf art up");
    assert!(!shown(&mut s, "ReputationWatchBarTexture0"), "rep art down");
    assert_eq!(
        s.eval::<f64>("return ReputationWatchStatusBar:GetHeight()")
            .unwrap(),
        13.0
    );

    // Stop watching: at 60 with nothing watched the brass rail is what takes the XP bar's place.
    let mut st = state();
    st.watched = None;
    s.set_reputation(st);
    s.fire_event("UPDATE_FACTION", vec![]);
    s.resolve();
    assert!(!shown(&mut s, "ReputationWatchBar"));
    assert!(shown(&mut s, "MainMenuBarMaxLevelBar"), "the rail is back");
    assert!(!shown(&mut s, "MainMenuExpBar"));
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
