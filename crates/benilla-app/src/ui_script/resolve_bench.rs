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
//! Run with `cargo test --release -p benilla-app --lib -- --ignored --nocapture resolve_bench`;
//! `WOW_LAYOUT_PROF=1` adds the per-solve shape — and since decision 1350 its `solved=`/`swept=`
//! columns are the ones to read: they are the SCOPE of the solve, and 1350's whole claim is that
//! they stay a handful while `frames=`/`anchored=` grow without bound. A solve whose scope tracks
//! the graph again is precisely the regression this file exists to catch.

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

/// A frame that also **ticks the VM**, i.e. runs the shipped UI's own `OnUpdate` handlers, in
/// `drive_script`'s real order (tick → measure → resolve).
///
/// [`app_frame`] deliberately models only the measure/resolve half, and the tooltip benches stand
/// in for the handler by calling it themselves. That is fine when the test IS the driver — but a
/// test asking "is anything in the shipped UI writing layout on its own?" has to let the shipped
/// UI actually run, or it answers a question nobody asked (decision 1385).
fn app_frame_ticked(s: &mut UiScript, dt: f32) {
    s.tick(dt);
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

/// **The idle law** (decision 1385): the settled shipped UI, with nothing happening, must cost
/// **zero** gate walks per frame — not one cheap one, zero.
///
/// The other guards in this file pin what the ENGINE charges for a change. This one pins that
/// nothing in the shipped UI is quietly making a change every frame in the first place, which is
/// the half a law about the engine cannot see. It is the tripwire for a new always-on toucher
/// being added to `assets/ui/` — the worst shape of this bug, because it costs on *every* frame of
/// *every* session rather than only while something is animating. The sweep behind 1385 found real
/// candidates for it (`TemporaryEnchantFrame`'s OnUpdate is resident for the whole session; the
/// measure round-trip used to bump the epoch on every same-size re-measure), so the shape is not
/// hypothetical.
///
/// A failure here does not name the culprit — `WOW_LAYOUT_TOUCH_TRACE=<secs>:<n>` does, by
/// backtracing the touch sites on a live run.
#[test]
fn the_settled_shipped_ui_costs_no_gate_walk_on_a_quiet_frame() {
    let mut s = settled_default_ui();
    // Ticked frames, so the shipped UI's own OnUpdate handlers run — the whole point (see
    // `app_frame_ticked`). A couple of frames of grace first: `settled_default_ui` stops as soon
    // as the measure round-trip goes quiet, which is one edge earlier than the layout gate closing
    // behind it, and the first ticked frames arm handlers that have never run.
    for _ in 0..8 {
        app_frame_ticked(&mut s, 1.0 / 60.0);
    }
    // POSITIVE CONTROL. "Zero walks" only means anything if the shipped UI's handlers actually
    // ran — a tick that silently fired nothing would also read zero, and would make this test a
    // green light for exactly the regression it exists to catch. `BuffFrameUpdateTime` is advanced
    // by `BuffFrame_OnUpdate` on every frame (down by `elapsed`, or up by TOOLTIP_UPDATE_TIME when
    // it crosses), so one tick must move it.
    s.run("__probe_bfut = BuffFrameUpdateTime")
        .expect("read the control");
    app_frame_ticked(&mut s, 1.0 / 60.0);
    s.run(
        "if BuffFrameUpdateTime == __probe_bfut then \
         error('the VM tick ran no shipped OnUpdate — this test proves nothing') end",
    )
    .expect("the shipped UI's OnUpdate handlers must run under app_frame_ticked");

    let before = s.layout_gate_walks();
    for _ in 0..20 {
        app_frame_ticked(&mut s, 1.0 / 60.0);
    }
    assert_eq!(
        s.layout_gate_walks() - before,
        0,
        "20 idle frames of the shipped UI cost {} whole-roster gate walks — something in \
         assets/ui/ is writing a layout input every frame with nothing happening. Name it with \
         WOW_LAYOUT_TOUCH_TRACE=<secs>:<n> on a live run (decision 1385).",
        s.layout_gate_walks() - before
    );
}

/// **The castbar law** (decision 1385, ledger B283), on the full shipped UI: a region that moves
/// every frame must cost **one** whole-roster gate walk per frame.
///
/// This is the guard for the bug class 1383 named and the castbar then hit. `CastingBar.xml`'s
/// OnUpdate slides `CastingBarSpark` one `SetPoint` per frame for the length of every cast — the
/// reference's own architecture, and the classic addon idiom besides. Measured live at the
/// Stormwind gates it cost **+4.2 ms of CPU per frame** on a default UI: three walks per frame at
/// ~1.0–1.4 ms each (10,438 anchored regions re-hashed every walk), because the fingerprint was
/// hashed over the 0294 seeds as well as the inputs and so no walk could ever close tier 1.
///
/// Three things make this the falsifier rather than a smoke check:
/// * it runs on the **shipped** UI, so the whole-roster term is real — a synthetic two-frame model
///   shows the same *counts* and none of the cost;
/// * the Lua body ends in a frame getter, because that is what a live tick does. A later handler's
///   `GetWidth` forces the synchronous mid-tick solve; modelling the frame as a lone `resolve()`
///   hides two of the three walks;
/// * it asserts on `layout_gate_walks`, not `layout_solves`. A walk that concludes "nothing moved"
///   pays the identical preamble and never reaches the solve counter — one of the castbar's three
///   walks was exactly that, so the old counter under-reported the bug by a third.
#[test]
fn a_region_moving_every_frame_costs_one_gate_walk_on_the_shipped_ui() {
    let mut s = settled_default_ui();
    s.run(
        r#"
        BenchBar = CreateFrame("Frame", "BenchBar", UIParent)
        BenchBar:SetPoint("CENTER", 0, 0); BenchBar:SetWidth(195); BenchBar:SetHeight(13)
        BenchSpark = BenchBar:CreateTexture(nil, "OVERLAY")
        BenchSpark:SetWidth(32); BenchSpark:SetHeight(32)
        BenchSpark:SetPoint("CENTER", BenchBar, "LEFT", 0, 2)
        bench_spark_n = 0
        -- CastingBarFrame_OnUpdate's body, reduced to what touches layout: ride the spark along
        -- the bar's leading edge. The trailing getter stands in for any LATER handler in the same
        -- tick reading a frame rect — the thing that forces the mid-tick synchronous solve.
        function bench_spark_frame()
            bench_spark_n = bench_spark_n + 1
            BenchSpark:SetPoint("CENTER", BenchBar, "LEFT", bench_spark_n * 1.7, 2)
            local _ = UIParent:GetWidth()
        end
        "#,
    )
    .unwrap();

    // Settle the newly-created bar and spark — a birth is legitimately a wide, multi-walk solve.
    for _ in 0..6 {
        s.run("bench_spark_frame()").unwrap();
        app_frame(&mut s);
    }

    let walks_before = s.layout_gate_walks();
    let solves_before = s.layout_solves();
    for step in 0..10 {
        s.run("bench_spark_frame()").unwrap();
        app_frame(&mut s);
        let (frames, regions) = s.layout_last_scope();
        assert!(
            frames < 50 && regions < 200,
            "step {step}: moving ONE region solved {frames} frames and swept {regions} regions — \
             the scope is tracking the graph, not the change (decision 1350)"
        );
    }
    let walks = s.layout_gate_walks() - walks_before;
    let solves = s.layout_solves() - solves_before;
    assert_eq!(
        walks, 10,
        "10 frames of one moving region must cost 10 whole-roster gate walks — one each. 30 \
         means the fingerprint is hashing the 0294 seeds again, so every solve outgrows the value \
         it stores and neither it nor the settling walk behind it can close tier 1 (decision \
         1385). At the shipped roster each extra walk is ~1 ms of CPU on every frame of every cast."
    );
    assert_eq!(
        solves, 10,
        "and each of those walks must be a real solve, not a wasted one"
    );
}

/// The perf half of decision 1350, asserted as a COUNT: a tooltip content change must cost a
/// solve whose SCOPE is tooltip-sized, on a UI of 3,000 frames and 8,800 anchored regions.
///
/// Milliseconds are deliberately not the gate here. This exact cost has now been chased three
/// times — 0735 (the measure cache), 0771 (the double solve), and this — and on two of those the
/// ms column was contaminated by machine state (0713's stall class) while the work counts were
/// clean. The bound is also what makes the regression *reportable*: 1350's pin found the sweep's
/// cost tripling with no counter behind it, because `regions_swept` counted the entries the sweep
/// skipped as well as the ones it resolved.
#[test]
fn a_tooltip_content_change_solves_a_tooltip_sized_scope() {
    let mut s = settled_default_ui();
    install_changing_tooltip(&s, "ScopeSizeOwner", "scope_size_change");

    // Settle the newly-created owner and the tooltip's first content — a birth is legitimately a
    // wide solve (a new node has no cached rect to trust).
    for _ in 0..6 {
        s.run("scope_size_change()").unwrap();
        app_frame(&mut s);
    }
    for step in 0..10 {
        s.run("scope_size_change()").unwrap();
        app_frame(&mut s);
        let (frames, regions) = s.layout_last_scope();
        assert!(
            frames < 100 && regions < 500,
            "step {step}: a tooltip content change solved {frames} frames and swept {regions} \
             regions — the scope is tracking the graph, not the change (measured at the time of \
             writing: 5 frames, 67 regions, against 3,003 and 8,821 in the graph)"
        );
    }
}

/// The scoped resolve's falsifier (decision 1350): on the shipped UI, mid-sweep, a solve that
/// touched only the dirty closure must produce **exactly** the rects a from-scratch whole-graph
/// solve produces.
///
/// This is the gate for the whole change, and it is deliberately run on the frames that never
/// settle — a hover sweep changes content every frame, so `WOW_LAYOUT_VERIFY`'s settled-frame
/// comparison never fires there, and those are the only frames the scope was built for. The
/// comparison is `extract()`, not an internal rect map: a stale rect that no quad carries is not a
/// bug, and a stale rect that one does carry is the bug in the form the screen would show it.
#[test]
fn a_scoped_resolve_reproduces_the_whole_graph_solve() {
    let mut s = settled_default_ui();
    install_changing_tooltip(&s, "ScopeOwner", "scope_change");

    for step in 0..12 {
        // A content change, resolved the way the app resolves it — scoped.
        s.run("scope_change()").unwrap();
        app_frame(&mut s);
        let scoped = s.extract();

        // The same model, re-solved from nothing but its inputs.
        s.force_full_layout_resolve();
        app_frame(&mut s);
        let full = s.extract();

        assert!(
            scoped == full,
            "step {step}: the scoped resolve and the whole-graph resolve disagree — a node the \
             scope judged clean did move. {} quads vs {}",
            scoped.len(),
            full.len(),
        );
    }
}

/// The steady-state cost of the per-frame measure sweep alone: the UI settled, every FontString
/// measured, every key a cache hit — what does *asking* cost? It is paid on EVERY frame, quiet
/// ones included, which made it a suspect on 1350's successor list — and this bench closed it:
/// 0.046 ms/sweep on the full shipped UI in release (2026-08-15; the ~1.0 ms once quoted was a
/// dev-build number). Kept as the lane's regression watch: if this climbs toward a milli-
/// second, the sweep's per-row clones/hash grew or the cache stopped hitting.
#[test]
#[ignore]
fn measure_sweep_steady_state_cost() {
    let mut s = settled_default_ui();
    // One more settle so the sweep below is provably pure cache hits.
    let residual = answer_measures(&mut s);
    let t = Instant::now();
    let mut asked = 0usize;
    const N: u32 = 200;
    for _ in 0..N {
        asked += s.fontstrings_needing_measure().len();
    }
    println!(
        "measure sweep steady state: {:.4} ms/sweep ({} sweeps, {} residual requests, {} pre-settle)",
        ms(t) / f64::from(N),
        N,
        asked,
        residual,
    );
}
