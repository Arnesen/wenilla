//! **The render column's can-it-fail proof.**
//!
//! A probe that cannot fail is not a probe, and this arc has shipped two that could not: the UI
//! probe swallowed every raise it was built to report (`the_ui_probe_records_what_an_override_raises`
//! is that scar), and the method oracle answered "nothing missing" when it could not run at all.
//! So this file pins **both directions**, twice over.
//!
//! **Synthetically**, on three fixture addons written to be unambiguous — one that paints a
//! texture, one that hangs a FontString off a frame it did not create, and one that creates a
//! shown frame with nothing paintable on it. The third is the sharp one: a frame's own draw slot
//! emits a quad and paints **nothing** (`ui_script::extract`'s converter drops it), so an
//! implementation that counted quads instead of *painting* quads would score it as drawing and
//! every `nothing` row in the corpus would be a lie.
//!
//! **And against reality**, on the two addons the director has verified with their own eyes:
//! `!OmniCC`'s countdown numbers are on their screen, and Bagnon's bag slots were not. Two real
//! addons with known, opposite, human-checked outcomes is a better oracle than any fixture — it is
//! the check that would have caught this class months ago — so
//! [`the_directors_two_verified_addons_come_out_on_opposite_sides`] is the one to read first.

use std::path::{Path, PathBuf};

use super::{survey, Drew};

/// One throwaway AddOns root, cleaned up on drop even if a test panics.
struct Fixtures(PathBuf);

impl Fixtures {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "benilla-render-{tag}-{}-{:?}",
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

/// **The proof, in both directions.** An addon that paints must register; one that does not must
/// not — and "creates a frame" is not "paints".
#[test]
fn the_render_column_can_fail() {
    let fx = Fixtures::new("cannotfail");
    // Paints: a texture on a window of its own.
    fx.addon(
        "PaintsAWindow",
        r#"
        local f = CreateFrame("Frame", "PaintsAWindowFrame", UIParent)
        f:SetWidth(64) f:SetHeight(64)
        f:SetPoint("CENTER", UIParent, "CENTER", 0, 0)
        local t = f:CreateTexture("PaintsAWindowTexture", "ARTWORK")
        t:SetAllPoints(f)
        t:SetTexture("Interface\\Icons\\INV_Misc_Bag_08")
        f:Show()
    "#,
    );
    // Paints, but onto one of OUR frames — no window of its own anywhere.
    fx.addon(
        "PaintsOnOurs",
        r#"
        local host = ActionButton1 or UIParent
        local fs = host:CreateFontString("PaintsOnOursText", "OVERLAY", "GameFontNormal")
        fs:SetPoint("CENTER", host, "CENTER", 0, 0)
        fs:SetText("hooked")
        fs:Show()
    "#,
    );
    // Draws NOTHING — but is far from inert: it creates a frame, shows it, and that frame emits a
    // `QuadContent::Frame` entry. Nothing paints from it. This is the fixture that fails a naive
    // implementation.
    fx.addon(
        "DrawsNothing",
        r#"
        DrawsNothingRan = true
        local f = CreateFrame("Frame", "DrawsNothingFrame", UIParent)
        f:SetWidth(64) f:SetHeight(64)
        f:SetPoint("CENTER", UIParent, "CENTER", 0, 0)
        f:Show()
    "#,
    );

    let reports = survey(fx.root());
    let drew = |name: &str| {
        reports
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("{name} was not surveyed"))
    };

    // Every fixture must actually have RUN — otherwise "drew nothing" would be measuring a load
    // failure, which is the confusion this column exists to end.
    for name in ["PaintsAWindow", "PaintsOnOurs", "DrawsNothing"] {
        assert!(
            drew(name).loaded,
            "{name} must load clean or this test proves nothing: {:?}",
            drew(name).errors
        );
    }

    assert_eq!(
        drew("PaintsAWindow").render.drew(),
        Drew::Own,
        "a texture on a frame of its own is a window of its own"
    );
    assert!(
        drew("PaintsAWindow")
            .render
            .frames
            .iter()
            .any(|f| f == "PaintsAWindowFrame"),
        "…and the row names the frame it came from: {:?}",
        drew("PaintsAWindow").render.frames
    );
    assert_eq!(
        drew("PaintsOnOurs").render.drew(),
        Drew::Overlay,
        "a region hung off one of OUR frames is an overlay, not a window"
    );
    assert_eq!(
        drew("DrawsNothing").render.drew(),
        Drew::Nothing,
        "creating and showing a frame paints nothing — a frame's own draw slot is dropped by the \
         renderer, and an implementation that counted quads rather than PAINTING quads would call \
         this a pass"
    );
    assert_eq!(
        (
            drew("DrawsNothing").render.own_quads,
            drew("DrawsNothing").render.overlay_quads
        ),
        (0, 0)
    );
}

/// Where the corpus might be — the `ui_chat::ace_gate_tests` resolver, so a machine without the
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

/// **The oracle: two real addons, opposite director-verified outcomes.**
///
/// `!OmniCC` puts cooldown countdown numbers on the director's screen — it works, and it works by
/// creating an **anonymous** frame parented to one of our cooldowns, so any check built on name
/// prefixes or on "did it create a top-level window" scores it zero. Bagnon has to build its own
/// item-slot buttons, and the director saw none of them.
///
/// The two are surveyed together in one throwaway root — copies are not made, the corpus folders
/// are surveyed where they lie — and the column must place them on opposite sides:
/// `!OmniCC` an [`Drew::Overlay`], Bagnon a window of its [`Drew::Own`].
///
/// If this test ever disagrees with the director's eyes, **the test is wrong**.
#[test]
fn the_directors_two_verified_addons_come_out_on_opposite_sides() {
    let Some(corpus) = corpus() else {
        eprintln!("skipping: no vanilla addon corpus (set $BENILLA_ADDON_CORPUS)");
        return;
    };
    // A root holding just these four, SYMLINKED rather than copied. Surveying the whole corpus
    // here would put a minute onto `cargo test --workspace` for four rows — the full sweep is the
    // `addon_harness` example's job, not a unit test's.
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

    // The POSITIVE control. A render check that scores this one blank is broken, whatever else it
    // gets right — the director sees its numbers.
    assert_eq!(
        row("!OmniCC").render.drew(),
        Drew::Overlay,
        "!OmniCC's countdown text is on the director's screen; it paints via an ANONYMOUS frame \
         parented to a cooldown of ours, which is precisely what a name-based check cannot see"
    );

    // The case that started this. Bagnon builds its own window; before the two fixes it built
    // nothing at all and every other column called it fine.
    assert_eq!(
        row("Bagnon").render.drew(),
        Drew::Own,
        "Bagnon draws its own inventory window"
    );
    assert!(
        row("Bagnon")
            .render
            .frames
            .iter()
            .any(|f| f.starts_with("BagnonItem")),
        "…and the slots are what it draws — the exact thing the director could not see: {:?}",
        row("Bagnon").render.frames
    );
}
