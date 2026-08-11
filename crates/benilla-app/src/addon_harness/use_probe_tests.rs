//! **The use column's can-it-fail proof, in both directions and per gesture.**
//!
//! This arc has shipped two instruments that could not fail — the UI probe swallowed every raise it
//! was built to report, and the method oracle answered "nothing missing" when it could not run at
//! all — so a new column arrives with its falsifier or it does not arrive.
//!
//! Two directions, and both are load-bearing:
//!
//! - **It must report.** Four fixtures, one per gesture, each drawing one button and raising from
//!   exactly one handler: `OnEnter`, `OnClick` on the left, `OnClick` on the right (registered for
//!   `RightButtonUp` only, so a left click cannot reach it), and `OnDragStart`. A column that
//!   drove only clicks passes three of these and fails one, which is the point of splitting them.
//! - **It must stay quiet.** A fixture that draws the *same* button with **no handlers at all**
//!   must come out [`Used::Survived`] with `driven ≥ 1` and zero errors — the shape that proves the
//!   errors above came from the addons and not from the probe.
//!
//! ...and one more, which is the distinction the whole column is built around: a fixture that
//! **paints but takes no mouse** must come out [`Used::Untouched`] with `driven == 0`, never
//! `Survived`. "Nothing raised" and "nothing was touched" are different answers, and an instrument
//! that cannot tell them apart is the one that reported a spotless corpus for a month.
//!
//! Then the real oracle: the director's two verified addons. `!OmniCC` works and must not be
//! reported broken; Bagnon's slots are the reason this file exists and must at minimum be
//! **reachable** — whether they still raise is the parallel handler fix's to change, so the
//! durable assertion is that the probe can get its hands on them at all.
//!
//! ## What the falsification run actually established (2026-08-11)
//!
//! Each was applied to `use_probe`, run, and reverted — a can-it-fail claim is worth what it was
//! measured at, not what it was asserted at:
//!
//! | change to the probe | this file |
//! |---|---|
//! | right-click press/release removed | RED — `RaisesOnRightClick` reads `Survived`, `driven=1` |
//! | drag presses and releases in place (no 4-px move) | RED — `RaisesOnDrag` |
//! | every `mouse_move` removed | RED — `RaisesOnEnter` |
//! | every left-button transition removed | RED — `RaisesOnLeftClick` |
//! | `Untouched` folded into `Survived` | RED — `PaintsButTakesNoMouse` |
//!
//! And the two **negative** results, which are the useful ones: removing only the hover move
//! leaves this file GREEN (the drag's own move crosses the same hover boundary), and removing only
//! the explicit left click leaves it GREEN (the drag press/release *is* a left click on a frame
//! that never registered for drag). So what these fixtures pin is the gesture, not the line —
//! stated in `drive_one` too, where somebody would otherwise delete an "obviously redundant" call.

use std::path::{Path, PathBuf};

use super::{survey, Used};

/// One throwaway AddOns root, cleaned up on drop even if a test panics. (The twin of
/// `render_tests::Fixtures`; kept local so the two files' fixtures cannot collide in `temp_dir`.)
struct Fixtures(PathBuf);

impl Fixtures {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "benilla-use-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    /// Write `<root>/<name>/{<name>.toc, body.lua}`.
    fn addon(&self, name: &str, body: &str) -> &Self {
        let dir = self.0.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}.toc")),
            "## Interface: 11200\nbody.lua\n",
        )
        .unwrap();
        std::fs::write(dir.join("body.lua"), body).unwrap();
        self
    }

    fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for Fixtures {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A 64×64 painted Button at the centre of the screen, plus whatever `extra` wires onto it.
///
/// Painted, because an unpainted frame never reaches the target list at all — the use column takes
/// its aim from the render column's attribution, so every fixture here has to draw first. Centred,
/// because the corners of the default UI are full of our own windows and a fixture underneath one
/// of them would be dropped by the attribution rule for a reason the test is not about.
fn painted_button(extra: &str) -> String {
    format!(
        r#"
        local f = CreateFrame("Button", "FixtureButton", UIParent)
        f:SetWidth(64) f:SetHeight(64)
        f:SetPoint("CENTER", UIParent, "CENTER", 0, 0)
        local t = f:CreateTexture("FixtureButtonIcon", "ARTWORK")
        t:SetAllPoints(f)
        t:SetTexture("Interface\\Icons\\INV_Misc_Bag_08")
        f:Show()
        {extra}
    "#
    )
}

/// **The proof: every gesture is really driven, and silence is really silence.**
#[test]
fn the_use_column_can_fail() {
    let fx = Fixtures::new("cannotfail");
    // One fixture per gesture. Each raises from exactly ONE handler, so a probe that skips that
    // gesture reports this addon clean and the assertion below catches it by name.
    fx.addon(
        "RaisesOnEnter",
        &painted_button(r#"f:SetScript("OnEnter", function() error("USEFIXTURE_ENTER") end)"#),
    );
    fx.addon(
        "RaisesOnLeftClick",
        &painted_button(r#"f:SetScript("OnClick", function() error("USEFIXTURE_LEFTCLICK") end)"#),
    );
    // Registered for the RIGHT button ONLY — the default set is `{"LeftButtonUp"}`, so this
    // handler is unreachable by any amount of left-clicking. It is the fixture that separates
    // "the probe clicks" from "the probe clicks with both buttons", which matters because
    // right-click is how every container slot in the game is used.
    fx.addon(
        "RaisesOnRightClick",
        &painted_button(
            r#"
            f:RegisterForClicks("RightButtonUp")
            f:SetScript("OnClick", function() error("USEFIXTURE_RIGHTCLICK") end)
        "#,
        ),
    );
    // `OnDragStart` fires only past the 4-px threshold, so this one also proves the probe's drag
    // actually MOVES rather than pressing and releasing in place.
    fx.addon(
        "RaisesOnDrag",
        &painted_button(
            r#"
            f:RegisterForDrag("LeftButton")
            f:SetScript("OnDragStart", function() error("USEFIXTURE_DRAG") end)
        "#,
        ),
    );
    // THE OTHER DIRECTION. The same painted, mouse-taking button with nothing wired to it at all:
    // driven, and silent. Without this row the four above prove only that the probe can produce
    // errors, not that the errors came from the addons.
    fx.addon("SilentButTouchable", &painted_button(""));
    // ...and the distinction the column exists for. It paints exactly as much as the others and
    // takes no mouse, so nothing of the addon's answers a hit-test: `untouched`, never `ok`.
    fx.addon(
        "PaintsButTakesNoMouse",
        r#"
        local f = CreateFrame("Frame", "UntouchableFrame", UIParent)
        f:SetWidth(64) f:SetHeight(64)
        f:SetPoint("CENTER", UIParent, "CENTER", 0, 0)
        f:EnableMouse(false)
        local t = f:CreateTexture("UntouchableIcon", "ARTWORK")
        t:SetAllPoints(f)
        t:SetTexture("Interface\\Icons\\INV_Misc_Bag_08")
        f:Show()
    "#,
    );

    let reports = survey(fx.root());
    let row = |name: &str| {
        reports
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("{name} was not surveyed"))
    };

    // Every fixture must have LOADED and DRAWN, or a `driven = 0` below would be measuring a load
    // failure or a blank window rather than the thing under test.
    for name in [
        "RaisesOnEnter",
        "RaisesOnLeftClick",
        "RaisesOnRightClick",
        "RaisesOnDrag",
        "SilentButTouchable",
        "PaintsButTakesNoMouse",
    ] {
        let r = row(name);
        assert!(
            r.loaded,
            "{name} must load clean or this test proves nothing: {:?}",
            r.errors
        );
        assert_ne!(
            r.render.drew(),
            super::Drew::Nothing,
            "{name} must paint, or the use probe has nothing to aim at"
        );
    }

    // ── It reports, per gesture ────────────────────────────────────────────────────────────────
    for (name, marker) in [
        ("RaisesOnEnter", "USEFIXTURE_ENTER"),
        ("RaisesOnLeftClick", "USEFIXTURE_LEFTCLICK"),
        ("RaisesOnRightClick", "USEFIXTURE_RIGHTCLICK"),
        ("RaisesOnDrag", "USEFIXTURE_DRAG"),
    ] {
        let r = row(name);
        assert_eq!(
            r.used.verdict(),
            Used::Raised,
            "{name} raises from the one handler its gesture reaches; a column that does not drive \
             that gesture reports it clean. driven={} errors={:?}",
            r.used.driven,
            r.used.errors
        );
        assert!(
            r.used.errors.iter().any(|e| e.contains(marker)),
            "{name}'s error must be ITS error, not something the probe stirred up elsewhere: {:?}",
            r.used.errors
        );
    }

    // ── ...and it stays quiet when there is nothing to report ──────────────────────────────────
    let quiet = row("SilentButTouchable");
    assert_eq!(
        quiet.used.verdict(),
        Used::Survived,
        "a painted, mouse-taking button with no handlers must survive being used: {:?}",
        quiet.used.errors
    );
    assert!(
        quiet.used.driven >= 1,
        "…and it must actually have been DRIVEN — a `Survived` with driven=0 is the failure this \
         whole column is about"
    );

    // ── ...and it never calls "nothing was touched" a pass ─────────────────────────────────────
    let untouched = row("PaintsButTakesNoMouse");
    assert_eq!(
        untouched.used.verdict(),
        Used::Untouched,
        "a frame that paints and takes no mouse offers the probe nothing; that is not a pass"
    );
    assert_eq!(
        (untouched.used.driven, untouched.used.touchable),
        (0, 0),
        "…and it says so in the numbers, not only in the verdict"
    );
}

/// Where the corpus might be — the same resolver `render_tests` uses, so a machine without the
/// third-party corpus skips rather than reddens.
fn corpus() -> Option<PathBuf> {
    if let Some(over) = std::env::var_os("BENILLA_ADDON_CORPUS") {
        let p = PathBuf::from(over);
        if p.is_dir() {
            return Some(p);
        }
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    (2usize..=4)
        .filter_map(|up| manifest.ancestors().nth(up))
        .map(|root| root.join("wow-addons-vanilla"))
        .find(|c| c.is_dir())
}

/// **The real oracle: the director's two verified addons.**
///
/// - `!OmniCC` **works** — its countdown numbers are on their screen. It must not be reported as
///   broken. It is also the reference [`Used::Untouched`]: its whole visible output is a
///   `FontString` on an anonymous, mouse-disabled `Frame`, so there is nothing on it to click and
///   nothing wrong with it either. If this column ever calls it `raised`, the column is wrong.
/// - **Bagnon** is why this file exists: sixteen bag slots drawn and dead to the touch. The
///   assertion here is the durable half — the probe can **reach** them (`driven ≥ 1`, on a
///   `BagnonItem*` frame). Whether they still raise is the parallel handler fix's to change, so
///   asserting `Raised` would turn somebody else's fix into this file's failure; what must never
///   regress is the probe's ability to get its hands on the slots at all, because a column that
///   silently stops touching them is exactly how this instrument has been wrong four times.
#[test]
fn the_directors_two_verified_addons_are_reachable_and_omnicc_is_not_broken() {
    let Some(corpus) = corpus() else {
        eprintln!("skipping: no vanilla addon corpus (set $BENILLA_ADDON_CORPUS)");
        return;
    };
    let fx = Fixtures::new("oracle");
    for name in ["!OmniCC", "Bagnon", "Bagnon_Core", "Bagnon_Forever"] {
        std::os::unix::fs::symlink(corpus.join(name), fx.root().join(name)).unwrap();
    }
    let reports = survey(fx.root());
    let row = |name: &str| {
        reports
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("{name} is not in the corpus"))
    };

    let omni = row("!OmniCC");
    assert_ne!(
        omni.used.verdict(),
        Used::Raised,
        "!OmniCC is on the director's screen and works; a column that calls it broken is broken \
         itself: {:?}",
        omni.used.errors
    );

    let bagnon = row("Bagnon");
    assert!(
        bagnon.used.driven >= 1,
        "Bagnon draws bag slots the director can put a cursor on; the probe must be able to reach \
         at least one (drew={:?} touchable={})",
        bagnon.render.frames,
        bagnon.used.touchable
    );
    assert!(
        bagnon
            .used
            .frames
            .iter()
            .any(|f| f.starts_with("BagnonItem")),
        "…and the thing it touched must be a SLOT — the exact widget the director hovered: {:?}",
        bagnon.used.frames
    );
}
