//! The ScrollFrame clip (decision 0112 §4/§5) — shared by [`UiScript::extract`](super::UiScript::extract)
//! and [`pointer`](super::pointer)'s hit-test. Both need the same answer to "which ScrollFrame rects
//! does this frame draw/hit within?", so the walk lives here once and mod.rs re-exports it.

use std::collections::HashMap;

use crate::layout::Rect;
use crate::widget::{FrameKind, KindState};

use super::model::Model;
use super::FrameHandle;

/// Every live ScrollFrame with a resolved rect and a live child, as `child handle → the
/// scrollframe's resolved rect`. The map [`effective_clip`] walks against; built fresh per
/// `extract`/`hit_test` call (cheap — one pass over the arena, same cost class as
/// [`crate::order::traversal`]), never cached, so a `SetScrollChild`/`SetVerticalScroll` between
/// calls is picked up with no invalidation to track.
pub(super) fn scroll_clip_sources(model: &Model) -> HashMap<FrameHandle, Rect> {
    let mut sources = HashMap::new();
    for (h, frame) in model.arena.iter_frames() {
        if frame.kind != FrameKind::ScrollFrame {
            continue;
        }
        let KindState::Scroll(state) = &frame.kind_state else {
            continue;
        };
        let Some(child) = state.child else { continue };
        if model.arena.frame(child).is_none() {
            continue; // stale child handle
        }
        if let Some(&rect) = model.resolved.get(&h) {
            sources.insert(child, rect);
        }
    }
    sources
}

/// The effective clip a frame `h` draws/hits within: intersecting the resolved rect of every
/// ScrollFrame whose registered child is `h` itself **or** an ancestor of `h` (walking `h` up
/// through its parent chain) — so both the scroll child's own quads and every descendant's quads
/// (a grandchild region, a button nested arbitrarily deep) are clipped, and nested ScrollFrames
/// (an inner ScrollFrame's child living inside an outer one's) intersect their rects rather than
/// the inner overriding the outer. `None` = unclipped (the common case — no ancestor is a
/// registered scroll child).
pub(super) fn effective_clip(
    model: &Model,
    sources: &HashMap<FrameHandle, Rect>,
    mut h: FrameHandle,
) -> Option<Rect> {
    let mut clip: Option<Rect> = None;
    loop {
        if let Some(&r) = sources.get(&h) {
            clip = Some(match clip {
                Some(c) => intersect_rect(c, r),
                None => r,
            });
        }
        match model.arena.frame(h).and_then(|f| f.parent) {
            Some(p) => h = p,
            None => break,
        }
    }
    clip
}

/// Axis-aligned rect intersection (y-up: `top`/`right` shrink to the tighter bound, `bottom`/`left`
/// grow to the tighter bound). Two ScrollFrame clips that don't overlap collapse to an inverted
/// (empty) rect — `point_in_rect` ([`pointer`](super::pointer)) then never hits it and the CPU clip
/// in `ui_pass` already treats a degenerate intersection as "nothing to draw" (see its `clip_quad`).
pub(super) fn intersect_rect(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.bottom.max(b.bottom),
        a.left.max(b.left),
        a.top.min(b.top),
        a.right.min(b.right),
    )
}
