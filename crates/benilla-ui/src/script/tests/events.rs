//! RegisterEvent + fire_event via BOTH conventions (RF-0025).

use super::common::script;
use crate::script::*;

#[test]
fn fire_event_both_conventions_in_one_handler() {
    let mut s = script();
    s.run(
        r#"
        local f = CreateFrame("Frame", "EF")
        f:RegisterEvent("UNIT_HEALTH")
        f:SetScript("OnEvent", function(self, event, ...)
            r_this_eq_self = (this == self)         -- legacy `this` global == modern `self`
            r_event_global = event                  -- modern `event` arg
            r_event_eq     = (event == _G.event)    -- == legacy `event` global
            r_arg1_eq      = (arg1 == select(1, ...))  -- legacy `arg1` == modern select(1,...)
            r_arg1         = arg1
            r_arg2         = select(2, ...)
        end)
    "#,
    )
    .unwrap();

    s.fire_event(
        "UNIT_HEALTH",
        vec![ScriptValue::Str("player".into()), ScriptValue::Int(42)],
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());

    assert!(s.eval::<bool>("return r_this_eq_self").unwrap());
    assert_eq!(
        s.eval::<String>("return r_event_global").unwrap(),
        "UNIT_HEALTH"
    );
    assert!(s.eval::<bool>("return r_event_eq").unwrap());
    assert!(s.eval::<bool>("return r_arg1_eq").unwrap());
    assert_eq!(s.eval::<String>("return r_arg1").unwrap(), "player");
    assert_eq!(s.eval::<i64>("return r_arg2").unwrap(), 42);
}

#[test]
fn globals_are_restored_after_firing_nesting_safe() {
    let mut s = script();
    s.run(
        r#"
        this, event, arg1 = "outer_this", "outer_event", "outer_arg1"
        local f = CreateFrame("Frame", "NF")
        f:RegisterEvent("E")
        f:SetScript("OnEvent", function() end)
    "#,
    )
    .unwrap();
    s.fire_event("E", vec![ScriptValue::Str("x".into())]);
    // After firing, the prior global values must be restored (RF-0025 set-then-restore).
    let (t, e, a): (String, String, String) = s.eval("return this, event, arg1").unwrap();
    assert_eq!(
        (t.as_str(), e.as_str(), a.as_str()),
        ("outer_this", "outer_event", "outer_arg1")
    );
}

#[test]
fn handler_errors_are_collected_not_panicked() {
    let mut s = script();
    s.run(
        r#"
        local f = CreateFrame("Frame", "BoomF")
        f:RegisterEvent("E")
        f:SetScript("OnEvent", function() error("boom") end)
    "#,
    )
    .unwrap();
    s.fire_event("E", vec![]);
    let errs = s.errors();
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].contains("boom"), "{errs:?}");
}

/// The cross-frame dispatch ORDER law (wow-re `event-dispatch-order.md`, VERIFIED): the client's
/// per-event listener list is tail-appended (`0x7052d0`) and walked head-first (`0x703e50`) —
/// **FIFO: registration order = firing order**. Duplicate registration keeps the original
/// position (`0x702264` dup ret); unregister+re-register moves to the tail. The ZoneText frames
/// depend on this: both write PVPInfoTextString on one event — the last writer decides.
#[test]
fn events_fire_in_registration_order_fifo() {
    let mut s = script();
    s.run(
        r#"
        order = ""
        local a = CreateFrame("Frame", "FA")
        local b = CreateFrame("Frame", "FB")
        local c = CreateFrame("Frame", "FC")
        a:RegisterEvent("E"); b:RegisterEvent("E"); c:RegisterEvent("E")
        a:SetScript("OnEvent", function() order = order .. "A" end)
        b:SetScript("OnEvent", function() order = order .. "B" end)
        c:SetScript("OnEvent", function() order = order .. "C" end)
    "#,
    )
    .unwrap();
    s.fire_event("E", vec![]);
    assert_eq!(s.eval::<String>("return order").unwrap(), "ABC");

    // Duplicate registration keeps A's position (the client's dup early-ret).
    s.run("FA:RegisterEvent('E'); order = ''").unwrap();
    s.fire_event("E", vec![]);
    assert_eq!(s.eval::<String>("return order").unwrap(), "ABC");

    // Unregister + re-register moves B to the TAIL (the node is freed, the re-add appends).
    s.run("FB:UnregisterEvent('E'); FB:RegisterEvent('E'); order = ''")
        .unwrap();
    s.fire_event("E", vec![]);
    assert_eq!(s.eval::<String>("return order").unwrap(), "ACB");
}

/// **`HasScript` answers "can this widget CARRY that kind", not "does it have one set".**
///
/// That distinction is the verb's whole purpose, and every corpus caller depends on it: they ask
/// before hooking, precisely when nothing is set yet.
///
/// ```lua
/// if parent:HasScript("OnMouseDown") then          -- Tablet-2.0.lua:2409
///     local script = parent:GetScript("OnMouseDown")
///     parent:SetScript("OnMouseDown", function() … end)
/// end
/// ```
///
/// It was the top session-start blocker — 32 of 39 `attempt to call method` failures were this one
/// name — and implementing it took survivors from 41 to 69.
///
/// The known over-permission is asserted too, so it is a recorded divergence rather than a
/// discovery: our table is flat where the reference's is per widget type, so a plain Frame answers
/// true for a Button-only kind. Exact for the base kinds, which is what the corpus asks about.
#[test]
fn has_script_reports_the_kind_is_supported_not_that_one_is_set() {
    let s = script();
    s.run(r#"f = CreateFrame("Frame", "HasScriptProbe")"#)
        .unwrap();

    // True with NOTHING set — the case every caller is actually in.
    assert!(
        s.eval::<bool>(r#"return f:HasScript("OnMouseDown")"#)
            .unwrap(),
        "a frame must report it can carry OnMouseDown before one is set"
    );
    assert!(
        !s.eval::<bool>(r#"return f:HasScript("OnNotARealScript")"#)
            .unwrap(),
        "an unknown kind is false, not true"
    );

    // Tablet's exact idiom, run end to end: guard, read the (absent) handler, install one, fire it.
    let fired: bool = s
        .eval(
            r#"
            RAN = false
            if f:HasScript("OnMouseDown") then
                local prev = f:GetScript("OnMouseDown")
                f:SetScript("OnMouseDown", function() RAN = true end)
            end
            f:GetScript("OnMouseDown")()
            return RAN
        "#,
        )
        .unwrap();
    assert!(fired, "the guarded hook must install and run");

    // The recorded divergence: flat table, so a Frame says true for a Button-only kind. The
    // reference says false. Pinned so making SCRIPT_KINDS per-type has to come here and decide.
    assert!(
        s.eval::<bool>(r#"return f:HasScript("OnClick")"#).unwrap(),
        "over-permissive by design today — see the comment at the binding"
    );
}

/// **The walk steps by a next saved BEFORE the handler runs** (`0x703ee8`; decision 1324): a
/// handler that unregisters ITSELF mid-dispatch cannot rob its successor. This is AceEvent-2.0's
/// fire-once idiom for `PLAYER_LOGIN`/`VARIABLES_LOADED` — its frame unregisters inside the
/// handler, and the index-walk this replaces skipped whichever addon registered right after it
/// (Bagnon_Forever's DB never initialized; the director's SaveBagData error dialogs).
#[test]
fn a_self_unregistering_handler_does_not_rob_its_successor() {
    let mut s = script();
    s.run(
        r#"
        log = {}
        for _, n in ipairs({"WalkA", "WalkB", "WalkC"}) do
            local f = CreateFrame("Frame", n)
            f:RegisterEvent("E")
            f:SetScript("OnEvent", function()
                table.insert(log, n)
                if n == "WalkB" then WalkB:UnregisterEvent("E") end
            end)
        end
    "#,
    )
    .unwrap();
    s.fire_event("E", vec![]);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
    let log: Vec<String> = s.eval("return log").unwrap();
    assert_eq!(
        log,
        vec!["WalkA", "WalkB", "WalkC"],
        "the once-idiom's self-removal must not skip the next listener"
    );
    // The removal held: a second fire reaches only A and C.
    s.fire_event("E", vec![]);
    let log: Vec<String> = s.eval("return log").unwrap();
    assert_eq!(log, vec!["WalkA", "WalkB", "WalkC", "WalkA", "WalkC"]);
}

/// A handler that unregisters the walk's SAVED next ends the dispatch there — the reference frees
/// that node and walks into zeroed links (an accident we render as a deterministic stop) — while a
/// frame registered mid-dispatch tail-appends and is still visited.
#[test]
fn mid_dispatch_removal_of_the_next_stops_and_append_is_visited() {
    let mut s = script();
    s.run(
        r#"
        log = {}
        local function reg(n, body)
            local f = CreateFrame("Frame", n)
            f:RegisterEvent("E2")
            f:SetScript("OnEvent", function() table.insert(log, n); if body then body() end end)
            return f
        end
        reg("NxA", function()
            NxB:UnregisterEvent("E2")   -- kill the walk's saved next
        end)
        reg("NxB")
        reg("NxC")
    "#,
    )
    .unwrap();
    s.fire_event("E2", vec![]);
    let log: Vec<String> = s.eval("return log").unwrap();
    assert_eq!(
        log,
        vec!["NxA"],
        "removing the saved next ends the dispatch"
    );

    s.run(
        r#"
        log = {}
        local function reg(n, body)
            local f = CreateFrame("Frame", n)
            f:RegisterEvent("E3")
            f:SetScript("OnEvent", function() table.insert(log, n); if body then body() end end)
        end
        reg("ApA", function()
            local f = CreateFrame("Frame", "ApLate")
            f:RegisterEvent("E3")
            f:SetScript("OnEvent", function() table.insert(log, "ApLate") end)
        end)
        reg("ApB")
    "#,
    )
    .unwrap();
    s.fire_event("E3", vec![]);
    let log: Vec<String> = s.eval("return log").unwrap();
    assert_eq!(
        log,
        vec!["ApA", "ApB", "ApLate"],
        "a tail-append during dispatch is still visited this dispatch"
    );
}
