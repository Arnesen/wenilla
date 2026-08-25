//! The **layout cache** — the consumer `Frame::user_placed` never had.
//!
//! `SetUserPlaced` / the drag entries set the client's userPlaced bit (`frame+0xb4 & 0x1000`,
//! [`super::object::movable`]), and its whole meaning is *"the player put this window here; put it
//! back next time"*. The real client honours that by writing a per-character `layout-cache.txt` at
//! logout and seating the frames from it at load; benilla had the bit and nothing that read it, so
//! a dragged or resized window went back to its authored anchors on the next login.
//!
//! This module is the **engine half** of that: a snapshot out, a restore in, and a dirty bit for
//! the host to debounce on. Where the file lives, when it is written and which character owns it
//! are the app's ([`benilla_app::ui_layout`] — `benilla-config/layout/<realm>-<character>.txt`,
//! through `local_state`, like every other resident of that folder).
//!
//! ## Why a host seam at all, when `GetPoint`/`SetPoint` exist
//!
//! Everything *about one frame* is already Lua-reachable — `GetNumPoints`, `GetPoint`, `GetWidth`,
//! `GetHeight`, `IsUserPlaced`, `SetPoint`, `SetWidth`, `SetHeight` — and if the saver knew which
//! frames to ask about, it would need nothing here. **Enumeration is the part Lua cannot do**:
//! there is no `EnumerateFrames` in 1.12 and inventing one would be a far wider surface than the
//! three functions below. The alternative — a hand-maintained list of names in FrameXML — is a
//! list that goes stale the first time a window becomes movable and nobody remembers to add it,
//! which is the failure mode the *bit* exists to prevent.
//!
//! So the seam is deliberately three calls wide and mirrors the chat-look seam next door
//! (`chat_window_looks` / `set_chat_window_looks` / `take_chat_window_changes`, decision 1589):
//! [`super::UiScript::user_placed_layouts`], [`super::UiScript::restore_user_placed_layouts`] and
//! [`super::UiScript::take_user_placed_change`].
//!
//! ## What a saved frame carries
//!
//! Its **name**, its **width/height**, and its **whole anchor list** — point, target, relative
//! point, offsets — because that is what `GetPoint` answers and what `SetPoint` takes. A frame is
//! addressed by name in the file for the obvious reason: handles are minted per session and mean
//! nothing across a relog.
//!
//! Two anchor targets survive the round trip and everything else drops the frame's row rather than
//! guessing:
//!
//! - the **screen root** ([`super::SCREEN`]) — written as no target, which is exactly what
//!   `GetPoint` answers there (`nil`, decision 0068's stated `UIParent` divergence);
//! - a **named frame** — written by name.
//!
//! An anchor onto an unnamed frame or onto a *region* cannot be re-addressed at load, so a frame
//! carrying one is skipped **whole**. Restoring half an anchor set would seat a window somewhere
//! nobody put it, which is worse than leaving it on its authored anchors.

use crate::layout::Anchor;
use crate::widget::FrameHandle;

use super::object::{point_from_str, point_name};
use super::{Model, SCREEN};

/// One anchor of a saved frame, in the spelling `GetPoint`/`SetPoint` use.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutPoint {
    /// The point on the saved frame (`"BOTTOMLEFT"`, …).
    pub point: String,
    /// The frame this anchor is relative to, by name; `None` is the screen root, which is what
    /// `GetPoint` reports as `nil`.
    pub relative_to: Option<String>,
    /// The point on the target.
    pub relative_point: String,
    /// The anchor's x offset, in the frame's own (pre-scale) units — `CAnchor+0x4`.
    pub x: f32,
    /// The anchor's y offset — `CAnchor+0x8`.
    pub y: f32,
}

/// One user-placed frame's persisted geometry: what a layout cache has to hold to put a window
/// back exactly where the player left it.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameLayout {
    /// The frame's global name — how the row is addressed across sessions.
    pub name: String,
    /// The frame's authored width (`LayoutInput::width`), which is what a resize drag writes.
    pub width: f32,
    /// The frame's authored height.
    pub height: f32,
    /// Every anchor the frame carries, in `GetPoint` order.
    pub points: Vec<LayoutPoint>,
}

impl super::UiScript {
    /// Snapshot every frame carrying the userPlaced bit — the save path.
    ///
    /// Sorted by name so the file is stable: a rewrite that reorders its own lines makes every
    /// diff useless and every "did this change?" check a false positive. Frames whose anchors
    /// cannot be re-addressed by name are dropped (see the module doc), as are unnamed ones —
    /// there is nothing to write them under.
    pub fn user_placed_layouts(&self) -> Vec<FrameLayout> {
        let model = self.model_ref();
        let mut out: Vec<FrameLayout> = model
            .arena
            .iter_frames()
            .filter(|(_, f)| f.user_placed)
            .filter_map(|(h, f)| {
                let name = f.name.clone()?;
                let input = model.layout_inputs.get(&h)?;
                let points = input
                    .anchors
                    .iter()
                    .map(|a| saved_point(&model, a))
                    .collect::<Option<Vec<_>>>()?;
                Some(FrameLayout {
                    name,
                    width: input.width,
                    height: input.height,
                    points,
                })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Seat saved geometry back onto the frames that still exist — the load path.
    ///
    /// Each row is applied whole or not at all: a name this build has no frame for, a point name
    /// that does not parse, or a target frame that is not loaded leaves the frame on its authored
    /// anchors rather than half-seated. Restoring **sets the userPlaced bit**, which is what keeps
    /// the position: `UIParent_ManageFramePositions` skips a user-placed frame, so the managed
    /// bottom-stack pass does not re-seat the window the player moved.
    ///
    /// It marks nothing dirty — the values came *from* the file, and an echo would re-dirty what
    /// was just read (`set_chat_window_looks`' reasoning, one store over).
    pub fn restore_user_placed_layouts(&mut self, layouts: impl IntoIterator<Item = FrameLayout>) {
        let mut model = self.model_mut();
        for l in layouts {
            let Some(h) = model.arena.lookup(&l.name) else {
                continue; // a window this build does not have — the file outlives a rename
            };
            let mut anchors: Vec<Anchor> = Vec::with_capacity(l.points.len());
            let mut usable = true;
            for p in &l.points {
                // `frame_id`, not a `frame_to_id` read: ids are minted lazily, so a target frame
                // that nothing has anchored to yet has none — and refusing the row for that would
                // make the restore depend on load order.
                let rel = match &p.relative_to {
                    None => Some(SCREEN),
                    Some(n) => model.arena.lookup(n).map(|t| model.frame_id(t)),
                };
                match (
                    point_from_str(&p.point),
                    rel,
                    point_from_str(&p.relative_point),
                ) {
                    (Some(point), Some(rel), Some(rp)) => {
                        anchors.push(Anchor::new(point, rel, rp, p.x, p.y));
                    }
                    _ => {
                        usable = false;
                        break;
                    }
                }
            }
            if !usable {
                continue;
            }
            let Some(input) = model.layout_inputs.get_mut(&h) else {
                continue;
            };
            let changed = input.anchors != anchors
                || input.width.to_bits() != l.width.to_bits()
                || input.height.to_bits() != l.height.to_bits();
            input.anchors = anchors;
            input.width = l.width;
            input.height = l.height;
            if let Some(f) = model.arena.frame_mut(h) {
                f.user_placed = true;
            }
            if changed {
                // The whole graph, not the frame alone: a restore REPOINTS anchors (a saved row
                // may name a different target than the authored one), and decision 1388's
                // frame-scoped touch is only valid while every anchor keeps its target.
                model.touch_layout();
            }
        }
    }

    /// Has a user-placed frame moved or resized since the last call? The host's cue to persist —
    /// a single bit rather than a set, because the file is written whole either way.
    pub fn take_user_placed_change(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().user_placed_changed)
    }
}

/// One live anchor → its saved form, or `None` when the target cannot be named across a session
/// (an unnamed frame, or a region — see the module doc).
fn saved_point(model: &Model, a: &Anchor) -> Option<LayoutPoint> {
    let relative_to = if a.relative_to == SCREEN {
        None
    } else {
        let h: FrameHandle = model.id_to_frame.get(&a.relative_to).copied()?;
        Some(model.arena.frame(h)?.name.clone()?)
    };
    Some(LayoutPoint {
        point: point_name(a.point).to_owned(),
        relative_to,
        relative_point: point_name(a.relative_point).to_owned(),
        x: a.x_off,
        y: a.y_off,
    })
}
