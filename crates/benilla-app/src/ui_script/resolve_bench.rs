//! What a UI change costs on the **full shipped UI** — the headless twin of the live hover
//! recorder (`hover_log`), and the loop that made the tooltip-hover cost reproducible without a
//! play session.
//!
//! The live runs said a hover frame that ran the layout fixpoint cost ~10 ms against ~0.2 ms for
//! one that did not, but neither the synthetic tooltip probes nor the shipped `BagFrame.xml` hover
//! loop reproduced it: with content that does not MOVE, the change gate absorbs everything. The
//! missing ingredient is that a real hover sweep shows a *different item every frame* — line widths
//! that actually change. That is what [`tooltip_change_costs_a_whole_ui_solve`] drives, and with it
//! the cost reproduces headlessly and can be measured against a fix.
//!
//! Run with `cargo test --release -p benilla -- --ignored --nocapture resolve_bench`;
//! `WOW_LAYOUT_PROF=1` adds the per-solve shape (rounds, graph size, frame-vs-region split).

use std::time::Instant;

use benilla_ui::script::UiScript;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// The deterministic stand-in for the app's font engine: every string has one size, so a measure
/// answer is stable across frames and only real text changes move the layout.
fn answer_measures(s: &mut UiScript) -> usize {
    let reqs = s.fontstrings_needing_measure();
    let n = reqs.len();
    if n > 0 {
        let answers: Vec<(u32, f32, f32, u64)> = reqs
            .iter()
            .map(|r| {
                #[allow(clippy::cast_precision_loss)]
                let w = r.text.chars().count() as f32 * 6.0;
                match r.wrap_width {
                    Some(ww) if w > ww => (r.id, ww, 12.0 * (w / ww).ceil(), r.key),
                    _ => (r.id, w, 12.0, r.key),
                }
            })
            .collect();
        s.set_measured_text_unwrapped(&answers);
    }
    n
}

/// The full shipped UI, loaded and settled — the graph a real hover actually pays for.
fn settled_default_ui() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1600.0, 900.0);
    super::load_default_ui(&s);
    s.set_screen_size(1600.0, 900.0);
    for _ in 0..12 {
        s.resolve();
        if answer_measures(&mut s) == 0 {
            break;
        }
    }
    s
}

/// One frame in the app's own order (`extract::drive_script`): measure FIRST, then resolve.
fn app_frame(s: &mut UiScript) {
    answer_measures(s);
    s.resolve();
}

/// A tooltip whose lines change width every call — the hover sweep across a bag grid, where each
/// slot holds a different item.
fn install_changing_tooltip(s: &UiScript, owner: &str, func: &str) {
    s.run(&format!(
        r#"
        local a = CreateFrame("Button", "{owner}"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        {func}_n = 0
        function {func}()
            {func}_n = {func}_n + 1
            GameTooltip:SetOwner({owner}, "ANCHOR_RIGHT")
            local pad = string.rep("x", math.mod({func}_n, 17) + 1)
            GameTooltip:AddLine("Item " .. pad, 1, 1, 1)
            GameTooltip:AddDoubleLine("Shield", "Off Hand", 1, 1, 1, 1, 1, 1)
            GameTooltip:AddLine("Armor " .. pad)
            GameTooltip:AddLine("Use: restores " .. pad .. " health over 21 sec.", 0, 1, 0, 1)
            GameTooltip:Show()
        end
        "#
    ))
    .unwrap();
}

#[test]
#[ignore = "bench, run explicitly"]
fn tooltip_change_costs_a_whole_ui_solve() {
    let mut s = settled_default_ui();

    let (r0, s0) = (s.layout_rounds(), s.layout_solves());
    let t = Instant::now();
    for _ in 0..20 {
        s.resolve();
    }
    println!(
        "QUIET    {:.3} ms/resolve  solves={} rounds={}",
        ms(t) / 20.0,
        s.layout_solves() - s0,
        s.layout_rounds() - r0
    );

    install_changing_tooltip(&s, "BenchOwner", "change");
    let (r1, s1) = (s.layout_rounds(), s.layout_solves());
    let n = 50u32;
    let t = Instant::now();
    for _ in 0..n {
        s.run("change()").unwrap();
        app_frame(&mut s);
    }
    println!(
        "CHANGED  {:.3} ms/frame  solves/frame={:.2} rounds/frame={:.2}",
        ms(t) / f64::from(n),
        (s.layout_solves() - s1) as f64 / f64::from(n),
        (s.layout_rounds() - r1) as f64 / f64::from(n),
    );
}

/// The gate the measure-first order exists to hold: a tooltip content change costs the frame
/// **one** layout solve, not two.
///
/// The old order resolved, then measured, then had to resolve AGAIN because the answers moved the
/// anchor solve's read set — and on the shipped UI each of those walks 2,164 frames and sweeps
/// 6,297 regions per round, for a change that touches ten FontStrings inside one frame. Nothing
/// about the second solve is tooltip-sized, and nothing about it is cheap.
#[test]
fn a_tooltip_content_change_costs_exactly_one_layout_solve() {
    let mut s = settled_default_ui();
    install_changing_tooltip(&s, "GateOwner", "gate_change");

    // Settle the newly-created owner + the tooltip's first content.
    for _ in 0..4 {
        s.run("gate_change()").unwrap();
        app_frame(&mut s);
    }
    let before = s.layout_solves();
    for _ in 0..10 {
        s.run("gate_change()").unwrap();
        app_frame(&mut s);
    }
    let solves = s.layout_solves() - before;
    assert_eq!(
        solves, 10,
        "10 content changes must cost 10 solves — one each. Two per change means the measure \
         round-trip is running AFTER the resolve again (extract::drive_script's order)."
    );
    // …and the measure answers must have landed in that one solve: a frame that resolved before
    // measuring leaves the fresh text unmeasured until the next frame, which is the visible half
    // of the same bug (a tooltip plate one frame behind its content).
    assert_eq!(
        answer_measures(&mut s),
        0,
        "after a settled frame nothing may still be waiting to be measured"
    );
}
