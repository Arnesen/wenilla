//! **The use column — "does it survive being USED?"**
//!
//! ## Why there is a fifth column, and why it is the fourth of its kind
//!
//! This survey has now been wrong at four successive depths, and a human found every one of them by
//! **playing**, never a number here:
//!
//! 1. `loaded` measured *"the addon's files ran"*. Bagnon scored perfect and did nothing.
//! 2. [`super::drive_session_start`] measured *"the handlers ran"* (1213). Same result.
//! 3. [`super::drive_ui_probe`] measured *"the override body ran"*. Same result.
//! 4. [`super::render`] measured *"something was drawn"* (1230). Bagnon now draws sixteen bag
//!    slots — and the director hovered them and got a wall of
//!    `attempt to call global 'ContainerFrameItemButton_OnEnter' (a nil value)`.
//!
//! Every one of those columns asks whether something **ran or appeared**. None of them **touches**
//! anything. So an addon whose UI is fully drawn and completely inert scores full marks on all
//! four, which is exactly what Bagnon did on the morning this was written.
//!
//! This column drives real pointer input — hover, left click, right click, drag — at the frames the
//! addon **itself painted**, through the same [`UiScript::mouse_move`]/[`UiScript::mouse_button`]
//! entry points the app feeds from the host window, and reports what raised.
//!
//! ## "Nothing raised" and "nothing was touched" are different answers
//!
//! [`Used::Untouched`] exists because the failure this instrument is most likely to repeat is its
//! own history: the UI probe once shipped with a `pcall` that swallowed every raise and reported a
//! spotless corpus forever. A probe that drove **zero** targets and printed "clean" would be that
//! bug with a different mechanism, so a row that touched nothing says so in the verdict itself and
//! is counted separately in the report. [`UseReport::driven`] is printed beside every verdict for
//! the same reason: a clean row with `driven = 0` is not evidence of anything.
//!
//! `!OmniCC` — director-verified working — is precisely that shape, and it is the reason the
//! distinction is not academic. Its entire visible output is a `FontString` on an **anonymous,
//! mouse-disabled** `Frame` parented to a cooldown; there is nothing on it a pointer can reach, and
//! there is nothing wrong with it either. `untouched`, not `raised`.
//!
//! ## What gets driven: the addon's own frames, and only those
//!
//! The target list is [`super::render`]'s own attribution — the quads that addon painted — turned
//! into points and hit-tested. **A candidate is kept only when the frame that captures the point is
//! one the addon created**; when one of ours is on top, or the painted region belongs to a
//! mouse-disabled frame and our window underneath eats the click, the candidate is dropped rather
//! than driven.
//!
//! That rule is the whole attribution argument. Driving one of *our* frames and charging what it
//! raises to whichever addon happens to be in the VM is the mis-attribution decision 1209 was
//! written about, and it would smear a single FrameXML bug across two hundred rows.
//!
//! The cost of the rule, stated: **an addon that only hooks our frames is invisible here.** A
//! `PlayerFrame:SetScript("OnEnter", …)` replacement is a real corpus shape, and this column scores
//! it `untouched`. Reaching it needs a script-table diff rather than a handle diff — a different
//! instrument, named here so nobody reads an `untouched` row as proof of anything.
//!
//! ## What a raise here means, and what it does not
//!
//! It means **a player doing that would see that error**. It does *not* say whose fault it is: the
//! wall the director hit is our missing `ContainerFrameItemButton_OnEnter`, reached through
//! Bagnon's slot button. That is the same open question 1213 §4 and 1204 §4 left about every other
//! column, and it is deliberately not answered here — "hovering this addon's UI raises" is the
//! finding, whoever owes the fix.

use std::collections::HashSet;

use benilla_ui::script::UiScript;
use benilla_ui::widget::FrameHandle;

use super::render::{Painted, RenderBaseline};

/// How many distinct frames one addon gets driven. **The bound, and what it drops.**
///
/// 218 VMs times this times the seven pointer events below is the whole cost of the column, and
/// each event pays a full [`UiScript::hit_test`] (a traversal + sort of the arena). Eight is what a
/// window's worth of interactive surface looks like — Bagnon's sixteen slots are sixteen instances
/// of one button template, so the ninth tells you nothing the first did not — and it holds the
/// column's share of the survey to seconds rather than minutes.
///
/// What it drops: the 9th..Nth distinct target, in painter order (so the earliest-drawn survive).
/// An addon whose only broken widget is the twelfth thing it painted reads clean here. Stated
/// rather than hidden, and the number is a constant precisely so raising it is one edit.
pub const MAX_USE_TARGETS: usize = 8;

/// How far the drag gesture travels. Must exceed `cursor::DRAG_START_THRESHOLD` (4 px, strict `>`)
/// or `OnDragStart` never fires and the "drag" is silently two clicks; small enough that it stays
/// inside a 16-px item slot, or the release lands on a different frame and `OnReceiveDrag` goes
/// somewhere else.
const DRAG_PX: f32 = 5.0;

/// Where the cursor is parked at the end, to fire the last `OnLeave`.
///
/// Off-screen by a mile: [`UiScript::hit_test`] answers `None` there whatever the addon built, so
/// the final hover boundary is guaranteed to be crossed. A hover half-driven — `OnEnter` fired and
/// `OnLeave` never — is the shape that leaves a tooltip up forever in play, and it is a real corpus
/// bug class, so the probe always closes the pair.
const PARKED: (f32, f32) = (-10_000.0, -10_000.0);

/// What happened when the addon's own UI was used.
///
/// Deliberately **not** ordered: there is no honest ranking between "it raised" and "there was
/// nothing to touch", and deriving one would invite a `>` comparison that quietly asserts it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Used {
    /// **Nothing was touched** — the addon painted nothing a pointer can reach, so this row is not
    /// a pass and not a failure. The verdict exists so a probe that drove zero targets can never
    /// be read as one that drove some and found nothing (see the module doc).
    #[default]
    Untouched,
    /// Driven, and it raised. The director's Bagnon: sixteen slots on screen, dead to the touch.
    Raised,
    /// Driven, and nothing raised.
    Survived,
}

impl Used {
    /// The one-word form for a report row.
    pub fn word(self) -> &'static str {
        match self {
            Used::Untouched => "untouched",
            Used::Raised => "raised",
            Used::Survived => "ok",
        }
    }
}

/// What one addon's UI did when it was used.
#[derive(Debug, Clone, Default)]
pub struct UseReport {
    /// Frames actually driven — **the number that makes a clean row mean anything**. Zero here and
    /// an empty [`Self::errors`] is [`Used::Untouched`], not a pass.
    pub driven: usize,
    /// Distinct frames of the addon's own that answered a hit-test, **before** [`MAX_USE_TARGETS`].
    /// Greater than [`Self::driven`] means the cap dropped some; printed so the bound is visible in
    /// the data rather than only in this comment.
    pub touchable: usize,
    /// The names of the frames driven (anonymous ones omitted), so a row can be read back to a
    /// place in the addon's own code. Capped by [`MAX_USE_TARGETS`] already.
    pub frames: Vec<String>,
    /// Errors raised by the input, verbatim, in the order they happened.
    pub errors: Vec<String>,
}

impl UseReport {
    pub fn verdict(&self) -> Used {
        match (self.driven, self.errors.is_empty()) {
            (0, _) => Used::Untouched,
            (_, false) => Used::Raised,
            (_, true) => Used::Survived,
        }
    }
}

/// Drive hover, both clicks and a drag at everything the addon drew, and report what raised.
///
/// **Runs after every other column has been read and before the method oracle**, for the two
/// reasons 1225 and 1230 already established: after, so no number beside it can be perturbed by
/// input this probe invented; before, because the oracle stands up one widget of every kind and
/// those sixteen frames of the harness's own must never become an addon's input targets.
///
/// `painted` is [`super::render`]'s attribution, not a fresh walk — see [`Painted`].
pub(super) fn measure_use(
    script: &mut UiScript,
    baseline: &RenderBaseline,
    painted: &[Painted],
) -> UseReport {
    let mut report = UseReport::default();
    // PUT THE TOOLTIP AWAY FIRST, before a single hit-test.
    //
    // `measure_render` deliberately leaves `GameTooltip` **shown** — it is measuring what is on
    // screen at the end, and a tooltip is on screen. But it is one of OURS, it lives in the
    // TOOLTIP strata (above everything), and it is therefore topmost over whatever it covers: any
    // addon frame underneath would hit-test to the tooltip, be dropped by the attribution rule
    // below as "one of ours", and leave the addon reading `untouched` for a reason that has
    // nothing to do with the addon. That is this column's own founding failure — a probe that
    // silently drove nothing and reported clean — so it is closed here rather than left to luck.
    //
    // Hiding it is also the faithful state: in play the tooltip trails the cursor, it is never the
    // thing under it. Done BEFORE the error baseline below, so an `OnHide` hook's raise is not
    // charged to input this probe has not driven yet.
    let _ = script.run(
        r#"
        if GameTooltip and type(GameTooltip.Hide) == "function" then pcall(function() GameTooltip:Hide() end) end
    "#,
    );
    // The addon's painted quads, resolved to the frames a cursor over them would actually hit.
    //
    // Two dedupes, and they answer different questions. `owners` bounds the WORK: an icon, its
    // border, its count text and its highlight are four quads on one button, and hit-testing each
    // is four traversals for one answer. `hits` bounds the TARGETS: two different buttons can
    // stack, and driving the same captured frame twice would double-count a raise.
    script.resolve();
    let mut owners: HashSet<FrameHandle> = HashSet::new();
    let mut hits: HashSet<FrameHandle> = HashSet::new();
    let mut targets: Vec<(FrameHandle, (f32, f32))> = Vec::new();
    for p in painted {
        if !owners.insert(p.owner) {
            continue;
        }
        let Some(hit) = script.hit_test_frame(p.point.0, p.point.1) else {
            continue; // painted, but nothing there takes the mouse
        };
        // THE ATTRIBUTION RULE. One of ours captured the point — our window on top, or the
        // addon's own region sitting on a mouse-disabled frame with ours underneath. Driving it
        // would charge our handler's raise to this addon, in all 218 VMs (1209).
        if baseline.is_pre_existing(hit) {
            continue;
        }
        if !hits.insert(hit) {
            continue;
        }
        targets.push((hit, p.point));
    }
    report.touchable = targets.len();
    targets.truncate(MAX_USE_TARGETS);
    report.driven = targets.len();
    report.frames = targets
        .iter()
        .filter_map(|(fh, _)| script.frame_name(*fh))
        .collect();

    // The engine collects handler raises itself (`UiScript::push_error`), so there is nothing to
    // swallow here and nothing to remember to re-read: a `split_off` of the tail is the whole
    // capture. This is deliberately NOT the `pcall`-and-collect shape `drive_ui_probe` needs —
    // that one calls Lua globals directly, this one goes through the real input path.
    let before = script.errors().len();
    for (_, (x, y)) in &targets {
        drive_one(script, *x, *y);
    }
    // Close the hover pair on the last target (see [`PARKED`]).
    script.mouse_move(PARKED.0, PARKED.1);
    report.errors = script.errors().split_off(before);
    report
}

/// One target, four gestures: **hover · left click · right click · drag**.
///
/// Every one of them is what it says. `mouse_move` fires the real `OnEnter`/`OnLeave` pair on a
/// hover-boundary crossing; `mouse_button` runs the real press/release path — `OnMouseDown`,
/// `OnMouseUp`, the `RegisterForClicks` adjudication that decides whether `OnClick` fires at all
/// and with which button, the double-click detector, and the drag trio. None of it is simulated:
/// this is the same code the host window's events reach.
///
/// A [`UiScript::resolve`] between gestures because a handler is free to move, show or build
/// things, and the next hit-test must see what it did. It is tier-1 gated (nothing touched the
/// layout ⇒ immediate return), so the quiet case is free.
///
/// **The drag is honest, with one stated degradation.** On a frame that called `RegisterForDrag`
/// the press → move → release below is the real gesture: `OnDragStart` past the 4-px threshold,
/// then `OnDragStop` on the source and `OnReceiveDrag` on whatever the release hit. On a frame
/// that did not, the same three events are simply a second click — and because the probe does not
/// tick, that second click lands inside the 300-ms window and fires `OnDoubleClick` where the
/// widget has one. Both are inputs a player produces; neither is invented.
///
/// **Two of these four lines overlap, and the falsification run found it rather than the design —
/// so it is written down here before somebody "simplifies" one of them away.** Deleting the hover
/// move alone does *not* stop `OnEnter` firing (the drag's move crosses the same boundary), and
/// deleting the left click alone does *not* stop `OnClick` firing (the degenerate drag above *is*
/// a left click). Only removing **every** cursor move, or **every** left-button transition, turns
/// the matching fixture red — which is what `use_probe_tests` actually pins. The overlap is not
/// waste: the ORDER is the player's (you hover a thing before you click it), and a handler that
/// only breaks on the second click is a real bug class.
fn drive_one(script: &mut UiScript, x: f32, y: f32) {
    // HOVER. The single most common thing a player does to a bag slot, and the gesture the
    // director's error wall came from.
    script.mouse_move(x, y);
    script.resolve();
    // LEFT CLICK — press and release on the same frame, which is what `wants_click` requires
    // before `OnClick` fires at all.
    script.mouse_button(x, y, "LeftButton", true);
    script.mouse_button(x, y, "LeftButton", false);
    script.resolve();
    // RIGHT CLICK. Not a duplicate of the left one: the default registration is
    // `{"LeftButtonUp"}`, so a right click reaches `OnClick` only on a widget that asked for it —
    // which every container slot in the game does, because right-click is how you use an item.
    script.mouse_button(x, y, "RightButton", true);
    script.mouse_button(x, y, "RightButton", false);
    script.resolve();
    // DRAG.
    script.mouse_button(x, y, "LeftButton", true);
    script.mouse_move(x + DRAG_PX, y);
    script.mouse_button(x + DRAG_PX, y, "LeftButton", false);
    script.resolve();
}
