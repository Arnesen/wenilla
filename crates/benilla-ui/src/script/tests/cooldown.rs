//! The Cooldown widget kind (decision 0137 phase 4): the `SetCooldown` API, the reference
//! machine's derived phases through extraction, and the tick auto-hide (`Cooldown.lua`'s
//! `OnAnimFinished` → `Hide` edge), plus the per-action dynamic-state read API.

use super::common::script;
use crate::script::*;

fn cooldown_content(s: &UiScript) -> Option<(f32, Option<f32>)> {
    s.extract().into_iter().find_map(|q| match q.content {
        QuadContent::Cooldown { fraction, flash } => Some((fraction, flash)),
        _ => None,
    })
}

/// The machine, end to end on the engine clock: hidden → sweep (fraction scrubs with GetTime) →
/// the 1 s finish flash → auto-hidden by tick.
#[test]
fn cooldown_widget_sweeps_flashes_and_hides() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        cd = CreateFrame("Cooldown", "TestCooldown")
        cd:SetWidth(36); cd:SetHeight(36); cd:SetPoint("CENTER")
        cd:Hide()
    "#,
    )
    .unwrap();
    s.resolve();
    assert_eq!(cooldown_content(&s), None, "hidden — no slot emitted");

    // Arm a 10 s cooldown that started 2.5 s ago (the reference's start-in-the-past shape from
    // GetActionCooldown) and show — the sweep reads 25 %.
    s.run("cd:SetCooldown(GetTime() - 2.5, 10); cd:Show()")
        .unwrap();
    s.resolve();
    let (fraction, flash) = cooldown_content(&s).expect("shown — the slot is emitted");
    assert!(
        (fraction - 0.25).abs() < 1e-3,
        "sweep at 25%, got {fraction}"
    );
    assert_eq!(flash, None, "no flash while the sweep runs");

    // Advance past the sweep end into the flash's middle: fraction ≥ 1, flash ≈ 0.5.
    s.tick(8.0); // now 0.5 s into the flash window
    s.resolve();
    let (fraction, flash) = cooldown_content(&s).expect("flash phase still emits");
    assert!(fraction >= 1.0);
    let f = flash.expect("the finish flash runs for 1 s after the sweep");
    assert!((f - 0.5).abs() < 0.05, "flash midway, got {f}");

    // Past the flash: tick hides the widget (the OnAnimFinished → Hide edge).
    s.tick(1.0);
    s.resolve();
    assert_eq!(cooldown_content(&s), None, "finished — tick hid the widget");
    assert!(
        !s.eval::<bool>("return cd:IsVisible() == 1").unwrap(),
        "the frame itself is hidden, not just skipped"
    );
}

/// The per-action dynamic-state API: the 1/nil conventions, IsActionInRange's tri-state, and
/// GetActionCooldown's GetTime-space triple that goes cold at expiry.
#[test]
fn action_state_bindings_answer_the_reference_conventions() {
    let mut s = script();

    // No state pushed: everything nil / cold.
    assert!(s.eval::<bool>("return IsUsableAction(3) == nil").unwrap());
    assert!(s.eval::<bool>("return IsActionInRange(3) == nil").unwrap());
    assert!(s
        .eval::<bool>("local st, d, e = GetActionCooldown(3); return st == 0 and d == 0 and e == 1")
        .unwrap());

    s.tick(100.0); // an arbitrary clock epoch
    s.set_action_state(
        3,
        Some(ActionState {
            usable: false,
            not_enough_mana: true,
            in_range: Some(false),
            has_range: true,
            current: true,
            auto_repeat: false,
            is_attack: false,
            consumable: true,
            equipped: false,
            // 4 s remaining of a 10 s cooldown, running: started at GetTime 94.
            cooldown: Some((94_000, 10_000, true)),
        }),
    );

    // The 1/nil pairs, exactly as the transcribed `if` reads them.
    assert!(s
        .eval::<bool>("local u, oom = IsUsableAction(3); return u == nil and oom == 1")
        .unwrap());
    assert!(s.eval::<bool>("return IsActionInRange(3) == 0").unwrap());
    assert!(s.eval::<bool>("return ActionHasRange(3) == 1").unwrap());
    assert!(s.eval::<bool>("return IsCurrentAction(3) == 1").unwrap());
    assert!(s
        .eval::<bool>("return IsAutoRepeatAction(3) == nil")
        .unwrap());
    assert!(s.eval::<bool>("return IsConsumableAction(3) == 1").unwrap());

    // GetActionCooldown: the pushed absolute start, verbatim in seconds.
    assert!(s
        .eval::<bool>(
            "local st, d, e = GetActionCooldown(3); \
             return math.abs(st - 94) < 0.001 and d == 10 and e == 1"
        )
        .unwrap());

    // 5 s later the same stored pair still answers (4 s window has 0 left → but 100+? no: it
    // expires at start+10 = now+4) …after the remaining 4 s pass, the read goes cold — the
    // stale-refeed guard (a re-fed finished pair must not replay the sweep/flash).
    s.tick(3.9);
    assert!(s
        .eval::<bool>("local st, d = GetActionCooldown(3); return d == 10")
        .unwrap());
    s.tick(0.2);
    assert!(s
        .eval::<bool>("local st, d, e = GetActionCooldown(3); return st == 0 and d == 0 and e == 1")
        .unwrap());

    // An on-hold (enable == 0) cooldown never goes cold on its own — parked until the event.
    s.set_action_state(
        3,
        Some(ActionState {
            cooldown: Some((100_000, 30_000, false)),
            ..Default::default()
        }),
    );
    s.tick(120.0);
    assert!(s
        .eval::<bool>("local st, d, e = GetActionCooldown(3); return d == 30 and e == 0")
        .unwrap());

    // Clearing the state clears the reads.
    s.set_action_state(3, None);
    assert!(s.eval::<bool>("return IsCurrentAction(3) == nil").unwrap());
}

/// The absolute-start triple is the anchor — both director regressions pin here. Reset-on-kill
/// ("the cooldown indicator resets when you kill a mob"): an unrelated field flip re-pushes the
/// same running cooldown, whose triple carries the SAME start, so the sweep holds. The vanished
/// GCD pie ("spamming Rend during Charge never shows the pie"): a fail-clear + re-arm between
/// feeds pushes a triple with a NEW start — under the old `(remaining, duration)` shape the two
/// arms read byte-identical and the seam kept the first, long-elapsed anchor.
#[test]
fn the_absolute_start_triple_holds_the_anchor_and_a_rearm_moves_it() {
    let mut s = script();
    s.tick(100.0);
    let cooling = |usable: bool| ActionState {
        usable,
        cooldown: Some((100_000, 15_000, true)),
        ..Default::default()
    };
    s.set_action_state(3, Some(cooling(false)));
    assert!(s
        .eval::<bool>("local st = GetActionCooldown(3); return math.abs(st - 100) < 1e-3")
        .unwrap());

    // 8 s later the kill drops combat and `usable` flips; the re-push carries the same start.
    // The anchor holds at 100 — the sweep never restarts (the reset-on-kill invariant).
    s.tick(8.0);
    s.set_action_state(3, Some(cooling(true)));
    assert!(s
        .eval::<bool>(
            "local st, d = GetActionCooldown(3); \
             return math.abs(st - 100) < 1e-3 and d == 15"
        )
        .unwrap());

    // A RE-ARM carries a fresh start and moves the anchor — even with the same duration (the
    // vanished-pie shape: same spell, same 15 s, armed again at t=107).
    s.set_action_state(
        3,
        Some(ActionState {
            cooldown: Some((107_000, 15_000, true)),
            ..Default::default()
        }),
    );
    assert!(s
        .eval::<bool>("local st = GetActionCooldown(3); return math.abs(st - 107) < 1e-3")
        .unwrap());
}

/// The same absolute-start law on the other two converters: the stance bar's form cooldown and
/// the bag slot's item cooldown hold their anchors across unchanged re-pushes.
#[test]
fn shapeshift_and_container_cooldowns_keep_their_anchors_too() {
    let mut s = script();
    s.tick(50.0);
    let form = || ShapeshiftFormView {
        spell_id: 2457,
        name: "Battle Stance".into(),
        cooldown: Some((50_000, 10_000, true)),
        ..Default::default()
    };
    s.set_shapeshift_forms(vec![form()]);
    let slot = || ContainerSlot {
        item_id: 118,
        count: 1,
        cooldown: Some((50_000, 30_000, true)),
        ..Default::default()
    };
    let bag = || ContainerState {
        name: Some("Backpack".into()),
        num_slots: 16,
        slots: std::collections::HashMap::from([(1, slot())]),
    };
    s.set_container(0, Some(bag()));
    assert!(s
        .eval::<bool>("local st = GetShapeshiftFormCooldown(1); return math.abs(st - 50) < 1e-3")
        .unwrap());
    assert!(s
        .eval::<bool>("local st = C_Container.GetContainerItemCooldown(0, 1); return math.abs(st - 50) < 1e-3")
        .unwrap());

    // Re-push both with the SAME raw triples 5 s later: the anchors hold at 50.
    s.tick(5.0);
    s.set_shapeshift_forms(vec![form()]);
    s.set_container(0, Some(bag()));
    assert!(s
        .eval::<bool>("local st = GetShapeshiftFormCooldown(1); return math.abs(st - 50) < 1e-3")
        .unwrap());
    assert!(s
        .eval::<bool>("local st = C_Container.GetContainerItemCooldown(0, 1); return math.abs(st - 50) < 1e-3")
        .unwrap());
}
