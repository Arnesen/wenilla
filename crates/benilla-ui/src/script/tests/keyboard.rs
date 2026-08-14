//! The frame keyboard delivery law (decision 1319; wow-re
//! `system/ui/scratch/frame-key-script-delivery.md`, §5 trio — VERIFIED).
//!
//! Each test pins one clause that a plausible-but-wrong implementation gets backwards.

use super::common::script;

/// The bucket gate (§3.2): membership is the **keyboard-enabled flag**, not the presence of a
/// script. A Lua-created frame auto-enables nothing (the reference's `SetScript` doesn't either),
/// so its handler is unreachable until `EnableKeyboard(true)` puts it in the walk.
#[test]
fn a_key_script_alone_does_not_put_a_frame_in_the_walk() {
    let mut s = script();
    s.run(
        r#"
        got = nil
        f = CreateFrame("Frame", "KbUnenabled")
        f:SetScript("OnChar", function() got = arg1 end)
    "#,
    )
    .unwrap();
    assert!(
        !s.char_input("7"),
        "an unenabled frame is not in the bucket"
    );
    assert_eq!(s.eval::<Option<String>>("return got").unwrap(), None);

    s.run("f:EnableKeyboard(true)").unwrap();
    assert!(
        s.char_input("7"),
        "enabled: now in the walk, and it consumes"
    );
    assert_eq!(
        s.eval::<Option<String>>("return got").unwrap().as_deref(),
        Some("7")
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// The existence gate's asymmetry (§3): a frame carrying **only** an `OnKeyUp` consumes every
/// key-down and runs nothing. This is the clause an implementation "tidies away" — and doing so
/// silently un-suppresses that frame's keybindings.
#[test]
fn only_an_onkeyup_still_swallows_the_key_down() {
    let mut s = script();
    s.run(
        r#"
        downs = 0
        f = CreateFrame("Frame", "KbUpOnly")
        f:EnableKeyboard(true)
        f:SetScript("OnKeyUp", function() downs = downs + 1 end)
    "#,
    )
    .unwrap();
    assert!(s.key_input("ESCAPE"), "the OnKeyUp slot alone consumes");
    assert_eq!(
        s.eval::<i64>("return downs").unwrap(),
        0,
        "…and fires nothing"
    );

    // Neither slot: declines outright.
    s.run("f:SetScript(\"OnKeyUp\", nil)").unwrap();
    assert!(!s.key_input("ESCAPE"), "no key slot at all: declines");
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// The walk order (§2): **strata descending first**, and only then level. A LOW frame can never
/// take a key from a HIGH one, however high its level — the same "a raise never changes stratum"
/// shape the toplevel law has.
#[test]
fn the_walk_is_strata_then_level_then_registration() {
    let mut s = script();
    s.run(
        r#"
        winner = nil
        function mk(name, strata, level)
            local f = CreateFrame("Frame", name)
            f:SetFrameStrata(strata)
            f:SetFrameLevel(level)
            f:EnableKeyboard(true)
            f:SetScript("OnKeyDown", function() winner = name end)
            return f
        end
        -- registered first, but the LOWEST stratum: must lose despite its huge level
        mk("KbLow", "LOW", 99)
        mk("KbHigh", "HIGH", 1)
    "#,
    )
    .unwrap();
    assert!(s.key_input("ESCAPE"));
    assert_eq!(
        s.eval::<String>("return winner").unwrap(),
        "KbHigh",
        "strata beats level"
    );

    // Within one stratum: higher level wins, and equal levels keep registration order.
    s.run(
        r#"
        winner = nil
        mk("KbHighTop", "HIGH", 5)
        mk("KbHighTie", "HIGH", 5)
    "#,
    )
    .unwrap();
    assert!(s.key_input("ESCAPE"));
    assert_eq!(
        s.eval::<String>("return winner").unwrap(),
        "KbHighTop",
        "level 5 beats level 1; the earlier of the two 5s wins the tie"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// **At most one frame consumes** (§2, "Delivery"): the walk stops at the first consumer, so a
/// second keyboard frame below it never sees the key.
#[test]
fn the_first_consumer_ends_the_walk() {
    let mut s = script();
    s.run(
        r#"
        seen = {}
        function mk(name, strata)
            local f = CreateFrame("Frame", name)
            f:SetFrameStrata(strata)
            f:EnableKeyboard(true)
            f:SetScript("OnChar", function() table.insert(seen, name) end)
        end
        mk("KbTip", "TOOLTIP")
        mk("KbDlg", "DIALOG")
    "#,
    )
    .unwrap();
    assert!(s.char_input("x"));
    assert_eq!(s.eval::<i64>("return table.getn(seen)").unwrap(), 1);
    assert_eq!(s.eval::<String>("return seen[1]").unwrap(), "KbTip");
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// A hidden frame is in no bucket (the link gate the walk shares with the draw order), so it
/// neither fires nor consumes — and the frame below it gets the key instead.
#[test]
fn a_hidden_frame_is_not_in_the_walk() {
    let mut s = script();
    s.run(
        r#"
        winner = nil
        function mk(name, strata)
            local f = CreateFrame("Frame", name)
            f:SetFrameStrata(strata)
            f:EnableKeyboard(true)
            f:SetScript("OnChar", function() winner = name end)
            return f
        end
        top = mk("KbHidden", "TOOLTIP")
        mk("KbBelow", "DIALOG")
        top:Hide()
    "#,
    )
    .unwrap();
    assert!(s.char_input("q"));
    assert_eq!(s.eval::<String>("return winner").unwrap(), "KbBelow");
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// A raising handler must not eat the key or abort the walk's caller: the error is recorded, the
/// frame still consumed (consumption is the C++ gate's, never the handler's — §3.1).
#[test]
fn a_raising_handler_still_consumes_and_is_recorded() {
    let mut s = script();
    s.run(
        r#"
        f = CreateFrame("Frame", "KbBoom")
        f:EnableKeyboard(true)
        f:SetScript("OnChar", function() error("boom") end)
    "#,
    )
    .unwrap();
    assert!(s.char_input("z"), "the gate consumed regardless");
    assert!(
        s.errors().iter().any(|e| e.contains("boom")),
        "the raise is recorded: {:?}",
        s.errors()
    );
}
