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
