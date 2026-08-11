//! The `toplevel` flag and the raise law — `SetToplevel`/`IsToplevel`, `Raise`/`Lower`, and the
//! raise itself with its real trigger (Show) and its real gate (occlusion). The mechanism, the byte
//! addresses and the clause-by-clause mapping onto our order model are in `script::object::toplevel`.
//!
//! Every test drives the PRODUCTION path — the Lua bindings, the loader, and the real
//! `mouse_button`/`mouse_move` entry points — and asserts on the two things that are actually
//! observable: `GetFrameLevel()` (the arithmetic) and the extracted painter order (the pixels).

use super::common::script;
use crate::script::{QuadContent, UiScript};

/// The order textures actually paint in, by their texture path — the render list `extract` returns
/// IS the draw order (`order::traversal`'s ZKey sort).
fn painted(s: &mut UiScript) -> Vec<String> {
    s.resolve();
    s.extract()
        .iter()
        .filter_map(|q| match &q.content {
            QuadContent::Texture { path: Some(p), .. } => Some(p.clone()),
            _ => None,
        })
        .collect()
}

fn level(s: &mut UiScript, frame: &str) -> i64 {
    s.eval::<i64>(&format!("return {frame}:GetFrameLevel()"))
        .unwrap()
}

/// Two overlapping MEDIUM frames: `Board` — already on screen and sitting at frame level **5** —
/// and `Dialog`, born at level 0 and hidden.
///
/// The level split is the point. A frame's link stamp already lifts it within its bucket when it is
/// shown (`resequence_to_tail`, decision 0557), so a same-level sibling would prove nothing about
/// the raise; **level outranks the link stamp** in the draw key, so nothing but a real level bump
/// can get `Dialog` over `Board`. This is the shape of the complaint the law answers: a dialog
/// opening behind a window that happens to sit higher in its stratum.
fn board_and_dialog() -> UiScript {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        Board = CreateFrame("Frame", "Board")
        Board:SetPoint("BOTTOMLEFT", 100, 100)
        Board:SetSize(300, 300)
        Board:SetFrameLevel(5)
        Board:CreateTexture(nil, "ARTWORK"):SetTexture("Board.blp")

        Dialog = CreateFrame("Frame", "Dialog")
        Dialog:SetPoint("BOTTOMLEFT", 200, 200)   -- overlaps Board's top-right quadrant
        Dialog:SetSize(300, 300)
        Dialog:CreateTexture(nil, "ARTWORK"):SetTexture("Dialog.blp")
        Dialog:Hide()
        "#,
    )
    .unwrap();
    s.resolve();
    s
}

// ── The flag ────────────────────────────────────────────────────────────────────────────────────

/// `SetToplevel`/`IsToplevel` round-trip, including the reference's **default-true** optional
/// argument (`0x775440` marshals an optional boolean defaulting to true, unlike `SetMovable`).
/// Nothing is born toplevel.
#[test]
fn the_flag_round_trips_and_defaults_off() {
    let s = script();
    s.run(r#"F = CreateFrame("Frame", "F")"#).unwrap();
    assert!(
        !s.eval::<bool>("return F:IsToplevel()").unwrap(),
        "no frame is born toplevel"
    );
    s.run("F:SetToplevel(true)").unwrap();
    assert!(s.eval::<bool>("return F:IsToplevel()").unwrap());
    s.run("F:SetToplevel(false)").unwrap();
    assert!(!s.eval::<bool>("return F:IsToplevel()").unwrap());
    // The omitted argument is TRUE, not false — the reference's `SetToplevel()` turns the bit ON.
    s.run("F:SetToplevel()").unwrap();
    assert!(
        s.eval::<bool>("return F:IsToplevel()").unwrap(),
        "SetToplevel() with no argument sets the bit"
    );
    // Lua truthiness, like every other flag setter here: the corpus writes SetToplevel(1).
    s.run("F:SetToplevel(nil); F:SetToplevel(1)").unwrap();
    assert!(s.eval::<bool>("return F:IsToplevel()").unwrap());
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// `toplevel="true"` in XML lands on the real method — it was a warn-once gap in the loader until
/// this law existed, which left every window authored toplevel (thirteen of our own frames, 82
/// corpus addons) unable to come to the front while `SetToplevel` did not exist at all.
///
/// `enableKeyboard` is deliberately still warned: implementing one clause is not a licence to go
/// quiet about the other.
#[test]
fn the_xml_toplevel_attribute_reaches_the_method_and_enable_keyboard_still_warns() {
    let s = script();
    let doc = crate::framexml::parse(
        r#"<Ui>
             <Frame name="XmlTop" toplevel="true" enableKeyboard="true">
               <Size><AbsDimension x="100" y="50"/></Size>
               <Anchors>
                 <Anchor point="BOTTOMLEFT"><Offset><AbsDimension x="10" y="10"/></Offset></Anchor>
               </Anchors>
             </Frame>
           </Ui>"#,
    )
    .expect("valid FrameXML");
    let report = crate::loader::load(&s, &doc, &|_| None);
    assert!(
        !report.warnings.iter().any(|w| w.contains("SetToplevel")),
        "no gap warning any more: {:?}",
        report.warnings
    );
    assert!(
        report.warnings.iter().any(|w| w.contains("EnableKeyboard")),
        "the clause we did NOT build stays loud: {:?}",
        report.warnings
    );
    assert!(s.eval::<bool>("return XmlTop:IsToplevel()").unwrap());
}

// ── The raise on Show ───────────────────────────────────────────────────────────────────────────

/// The control, and the behaviour before this landed: a **non-toplevel** frame is shown and moves
/// nowhere. `Dialog` paints under `Board` because its level is lower, exactly as the draw key says.
#[test]
fn a_non_toplevel_frame_does_not_move_when_shown() {
    let mut s = board_and_dialog();
    s.run("Dialog:Show()").unwrap();
    assert_eq!(level(&mut s, "Dialog"), 0, "no raise, no level change");
    assert_eq!(level(&mut s, "Board"), 5, "and no compaction either");
    assert_eq!(
        painted(&mut s),
        vec!["Dialog.blp".to_string(), "Board.blp".to_string()],
        "the dialog opens BEHIND the window it overlaps"
    );
}

/// The headline: the same frame, marked `toplevel`, comes to the front on **Show** — the trigger
/// the law names (`effective_visible_show 0x76ae10` @`0x76aee0`), not on a click.
///
/// The arithmetic is pinned here because it is the whole mapping onto our order model. Visible
/// MEDIUM levels are `{0 (Dialog), 5 (Board)}`; `level_compact 0x764eb0` renumbers those occupied
/// levels contiguously into `[0, 2)` — Board 5 → 1 — and the raise then writes
/// `level := bucket->count` = **2**. Not a live max-scan (`5 + 1`), not a re-stamp: one above the
/// top *occupied* level, counted after the compaction.
#[test]
fn a_raise_is_top_occupied_level_plus_one_after_compaction() {
    let mut s = board_and_dialog();
    s.run("Dialog:SetToplevel(true)").unwrap();
    s.run("Dialog:Show()").unwrap();

    assert_eq!(
        level(&mut s, "Board"),
        1,
        "compaction renumbered the stratum's occupied levels into [0, count)"
    );
    assert_eq!(
        level(&mut s, "Dialog"),
        2,
        "the raise is bucket->count, read AFTER the compaction"
    );
    assert_eq!(
        painted(&mut s),
        vec!["Board.blp".to_string(), "Dialog.blp".to_string()],
        "the dialog is now in front"
    );
}

/// The trigger is the **`effectiveVisible` false→true transition**, not the `Show()` call: a frame
/// whose own `shown` bit is already set raises when an ancestor's Show finally makes it visible, and
/// a `Show()` that changes no effective visibility raises nothing.
///
/// The setup order is load-bearing and is worth reading as the law's own proof. `Holder` is hidden
/// *before* `Dialog:Show()`, so that Show is not a transition — leave it visible and `Dialog` raises
/// there instead, ends up above everything, and the transition under test then correctly declines
/// because nothing is left to raise over. (That mis-ordering is what the first draft of this test
/// did; the gate was right and the fixture was wrong.)
#[test]
fn the_trigger_is_the_effective_visibility_transition_not_the_show_call() {
    let mut s = board_and_dialog();
    s.run(
        r#"
        Holder = CreateFrame("Frame", "Holder")
        Holder:SetPoint("BOTTOMLEFT", 200, 200)
        Holder:SetSize(300, 300)
        Holder:Hide()
        Dialog:SetParent(Holder)
        Dialog:SetToplevel(true)
        Dialog:Show()          -- own bit set, parent hidden: NOT effective-visible, no transition
        "#,
    )
    .unwrap();
    assert_eq!(
        level(&mut s, "Dialog"),
        0,
        "a Show that moves no effective visibility raises nothing"
    );
    assert_eq!(level(&mut s, "Board"), 5, "and compacts nothing");

    s.run("Holder:Show()").unwrap();
    assert_eq!(
        level(&mut s, "Dialog"),
        2,
        "Dialog became effective-visible through its parent's Show and raised itself"
    );
    assert_eq!(level(&mut s, "Board"), 1, "the compaction ran with it");
}

// ── The gate ────────────────────────────────────────────────────────────────────────────────────

/// **Occlusion-gated**: a toplevel frame that overlaps nothing changes nothing at all. The gate sits
/// *before* the compaction in `0x7650f0`, so a declined raise leaves the whole stratum's levels
/// untouched too — `Board` keeps its authored 5.
#[test]
fn a_raise_on_a_frame_that_overlaps_nothing_is_a_total_no_op() {
    let mut s = board_and_dialog();
    s.run(
        r#"
        Dialog:ClearAllPoints()
        Dialog:SetPoint("BOTTOMLEFT", 600, 450)   -- clear of Board's (100,100)-(400,400)
        Dialog:SetSize(100, 100)
        Dialog:SetToplevel(true)
        Dialog:Show()
        "#,
    )
    .unwrap();
    assert_eq!(level(&mut s, "Dialog"), 0, "nothing to raise over");
    assert_eq!(
        level(&mut s, "Board"),
        5,
        "and the gate declined before compaction"
    );
}

/// The scan runs from the frame's **own level upward** (`for lvl = T->+0xc4; lvl < bucket->count`).
/// A toplevel frame whose only overlap is with something *below* it is already in front of it, so
/// there is nothing to raise over.
#[test]
fn the_scan_ignores_frames_below_the_raised_frames_own_level() {
    let mut s = board_and_dialog();
    s.run(
        r#"
        Board:SetFrameLevel(3)
        Dialog:SetFrameLevel(5)
        Dialog:SetToplevel(true)
        Dialog:Show()
        "#,
    )
    .unwrap();
    assert_eq!(
        level(&mut s, "Dialog"),
        5,
        "the overlap is below it; no raise"
    );
    assert_eq!(level(&mut s, "Board"), 3);
}

/// A window overlaps its own children by construction, so the scan excludes the raised frame's
/// subtree (`is_descendant 0x767010`). Without that clause every toplevel frame with content in it
/// would raise on every trigger.
#[test]
fn the_scan_excludes_the_raised_frames_own_subtree() {
    let mut s = board_and_dialog();
    s.run(
        r#"
        Board:Hide()                              -- leave the dialog alone on screen
        Dialog:SetToplevel(true)
        Inner = CreateFrame("Frame", "Inner", Dialog)
        Inner:SetAllPoints()                      -- exactly covers its parent
        Dialog:Show()
        "#,
    )
    .unwrap();
    assert_eq!(
        level(&mut s, "Dialog"),
        0,
        "its own child is not an occlusion"
    );
}

/// A raise **never changes the stratum** — `0x7650f0` never writes `+0xc0`. A LOW toplevel frame
/// raises over its LOW neighbours and stays under every MEDIUM frame, however many times it is
/// raised. (This is the mechanism that could not have fixed the party-frame-through-loot-window
/// bug: wow-re's CLAIM 2, refuted.)
#[test]
fn a_raise_can_never_lift_a_frame_out_of_its_stratum() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        Med = CreateFrame("Frame", "Med")
        Med:SetPoint("BOTTOMLEFT", 100, 100)
        Med:SetSize(300, 300)
        Med:CreateTexture(nil, "ARTWORK"):SetTexture("Med.blp")

        LowOther = CreateFrame("Frame", "LowOther")
        LowOther:SetFrameStrata("LOW")
        LowOther:SetPoint("BOTTOMLEFT", 100, 100)
        LowOther:SetSize(300, 300)
        LowOther:SetFrameLevel(4)
        LowOther:CreateTexture(nil, "ARTWORK"):SetTexture("LowOther.blp")

        LowTop = CreateFrame("Frame", "LowTop")
        LowTop:SetFrameStrata("LOW")
        LowTop:SetPoint("BOTTOMLEFT", 150, 150)
        LowTop:SetSize(300, 300)
        LowTop:SetToplevel(true)
        LowTop:CreateTexture(nil, "ARTWORK"):SetTexture("LowTop.blp")
        LowTop:Hide()
        "#,
    )
    .unwrap();
    s.resolve();
    s.run("LowTop:Show()").unwrap();

    assert_eq!(
        s.eval::<String>("return LowTop:GetFrameStrata()").unwrap(),
        "LOW",
        "the raise never writes the stratum"
    );
    assert!(
        level(&mut s, "LowTop") > level(&mut s, "LowOther"),
        "it did raise, within LOW"
    );
    assert_eq!(
        painted(&mut s),
        vec![
            "LowOther.blp".to_string(),
            "LowTop.blp".to_string(),
            "Med.blp".to_string(),
        ],
        "still under every MEDIUM frame — a stratum is not something a raise can cross"
    );
}

// ── Propagation and compaction ──────────────────────────────────────────────────────────────────

/// `propagate = 1` shifts **same-strata** children by the same delta (`0x76a4f0` @`0x76a58a`), so
/// the raised subtree keeps its internal order; cross-strata children are untouched
/// (`0x76a582: cmp …; jne`).
///
/// The arithmetic in full. Visible MEDIUM levels before the raise are `{0 Dialog, 1 Kid, 2 Grandkid,
/// 5 Board}`; compaction maps them to `{0, 1, 2, 3}` (Board 5 → 3) and reports `count = 4`. The
/// raise writes `Dialog := 4`, i.e. `delta = +4`, and pushes that same +4 into Kid (1 → 5) and
/// Grandkid (2 → 6) — gaps and order preserved. `Cross`, moved to the DIALOG stratum, keeps its 1.
#[test]
fn the_raised_subtree_shifts_by_one_delta_and_keeps_its_internal_order() {
    let mut s = board_and_dialog();
    s.run(
        r#"
        Kid = CreateFrame("Frame", "Kid", Dialog)          -- level 1 (parent + 1)
        Grandkid = CreateFrame("Frame", "Grandkid", Kid)   -- level 2
        Cross = CreateFrame("Frame", "Cross", Dialog)      -- level 1, then a different stratum
        Cross:SetFrameStrata("DIALOG")
        Dialog:SetToplevel(true)
        Dialog:Show()
        "#,
    )
    .unwrap();

    assert_eq!(level(&mut s, "Board"), 3, "compacted 5 -> 3");
    assert_eq!(level(&mut s, "Dialog"), 4, "raised to bucket->count");
    assert_eq!(level(&mut s, "Kid"), 5, "same delta (+4) as its parent");
    assert_eq!(level(&mut s, "Grandkid"), 6, "and so does the grandchild");
    assert_eq!(
        level(&mut s, "Cross"),
        1,
        "a cross-strata child is skipped by the propagate"
    );
    assert_eq!(
        s.eval::<String>("return Cross:GetFrameStrata()").unwrap(),
        "DIALOG"
    );
}

/// Compaction is what keeps `level := top + 1` from ratcheting. Two overlapping toplevel windows
/// traded back and forth twenty times settle inside a two-level band instead of climbing one step
/// per show — and the frame shown last is on top every single time.
///
/// Without `level_compact 0x764eb0` this test ends at level 20, and a long session ends at
/// `u16::MAX`.
#[test]
fn compaction_bounds_the_raise_across_repeated_shows() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        A = CreateFrame("Frame", "A")
        A:SetPoint("BOTTOMLEFT", 100, 100)
        A:SetSize(300, 300)
        A:SetToplevel(true)
        B = CreateFrame("Frame", "B")
        B:SetPoint("BOTTOMLEFT", 200, 200)
        B:SetSize(300, 300)
        B:SetToplevel(true)
        "#,
    )
    .unwrap();
    s.resolve();

    for _ in 0..10 {
        s.run("A:Hide(); A:Show()").unwrap();
        assert!(
            level(&mut s, "A") > level(&mut s, "B"),
            "the window just shown is in front"
        );
        s.run("B:Hide(); B:Show()").unwrap();
        assert!(level(&mut s, "B") > level(&mut s, "A"));
    }
    assert!(
        level(&mut s, "A") <= 2 && level(&mut s, "B") <= 2,
        "twenty raises stay inside the band compaction defines, not 20 levels up: A={} B={}",
        level(&mut s, "A"),
        level(&mut s, "B")
    );
}

// ── The Lua verbs ───────────────────────────────────────────────────────────────────────────────

/// `Frame:Raise()` — the explicit script call (`0x775a50` → `0x76a5b0` → `0x7650f0(force = 1)`).
/// Called on a **non-toplevel** frame it acts on the nearest toplevel ancestor, and on nothing at
/// all when there is none: the gate lives in the worker, so no call site has to check.
#[test]
fn lua_raise_acts_on_the_nearest_toplevel_ancestor_and_is_silent_without_one() {
    let mut s = board_and_dialog();
    s.run(
        r#"
        Dialog:SetToplevel(true)
        Dialog:Show()
        Kid = CreateFrame("Frame", "Kid", Dialog)
        Loose = CreateFrame("Frame", "Loose")
        Loose:SetPoint("BOTTOMLEFT", 200, 200)
        Loose:SetSize(300, 300)
        "#,
    )
    .unwrap();
    s.resolve();
    // Re-lower the dialog under Board so there is something to raise over again.
    s.run("Board:SetFrameLevel(9); Dialog:SetFrameLevel(0)")
        .unwrap();
    s.run("Kid:Raise()").unwrap();
    assert!(
        level(&mut s, "Dialog") > level(&mut s, "Board"),
        "raising a child raised the toplevel window it lives in"
    );

    // A frame with no toplevel in its chain: a total no-op, and not an error.
    let board_before = level(&mut s, "Board");
    let loose_before = level(&mut s, "Loose");
    s.run("Loose:Raise()").unwrap();
    assert_eq!(level(&mut s, "Loose"), loose_before);
    assert_eq!(level(&mut s, "Board"), board_before);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// `Frame:Lower()` is a **verified no-op stub** in build 5875 (`0x7652a0` = `xor eax,eax; ret 4`,
/// its frame argument never read). It exists so a caller gets the reference's silence rather than a
/// nil-call error — and it must not become "the opposite of Raise".
#[test]
fn lua_lower_exists_and_does_nothing() {
    let mut s = board_and_dialog();
    s.run("Dialog:SetToplevel(true); Dialog:Show()").unwrap();
    let (d, b) = (level(&mut s, "Dialog"), level(&mut s, "Board"));
    s.run("Dialog:Lower(); Board:Lower()").unwrap();
    assert_eq!((level(&mut s, "Dialog"), level(&mut s, "Board")), (d, b));
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// The drag-start trigger (`0x7652b0` @`0x7652d7`): grabbing a movable toplevel window brings it
/// forward before the drag begins. This was the deferral `object::movable`'s doc carried ("benilla
/// has no raise law yet") and it is closed.
#[test]
fn starting_a_move_raises_the_dragged_window() {
    let mut s = board_and_dialog();
    s.run(
        r#"
        Dialog:SetToplevel(true)
        Dialog:SetMovable(true)
        Dialog:EnableMouse(true)
        Dialog:Show()
        Board:SetFrameLevel(9)          -- put the dialog back underneath
        Dialog:SetFrameLevel(0)
        "#,
    )
    .unwrap();
    s.resolve();
    assert!(level(&mut s, "Dialog") < level(&mut s, "Board"));

    s.run("Dialog:StartMoving()").unwrap();
    assert!(
        level(&mut s, "Dialog") > level(&mut s, "Board"),
        "the grab brought it to the front"
    );
    s.run("Dialog:StopMovingOrSizing()").unwrap();
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}
