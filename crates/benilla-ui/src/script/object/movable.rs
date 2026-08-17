//! Frame method-table cluster: the movable/resizable window family — `SetMovable`/`IsMovable`,
//! `StartMoving`/`StopMovingOrSizing`, `SetUserPlaced`/`IsUserPlaced`, `SetResizable`/
//! `IsResizable`. Split out of [`super`] purely for size — see its module doc for the shared
//! id/handle plumbing and method-table wiring.
//!
//! This is "drag this bar somewhere", and after events it is the most-used addon frame feature.
//! The canonical idiom is four lines, of which only the two engine verbs were missing here:
//!
//! ```lua
//! f:SetMovable(true)
//! f:RegisterForDrag("LeftButton")
//! f:SetScript("OnDragStart", function() this:StartMoving() end)
//! f:SetScript("OnDragStop",  function() this:StopMovingOrSizing() end)
//! ```
//!
//! and that layering is the reference's own: `RegisterForDrag 0x776d60` + the threshold-gated
//! `OnDragStart`/`OnDragStop` pair (wow-re `cursor-dragdrop-payload.md` §9, our decision 0216 §3 —
//! [`crate::script::cursor`], dispatched from [`crate::script::pointer`]) are a **separate**
//! system from the engine move below. The gesture decides *when* the handlers run; this module
//! owns only what happens between `StartMoving` and `StopMovingOrSizing`.
//!
//! ## The mechanism (wow-re `system/ui`, VERIFIED unless said otherwise)
//!
//! The three flags are bits of the frame's flag word `[frame+0xb4]`, all written by the one
//! generic setter `0x76a3c0` (`if (v) flags |= mask; else flags &= ~mask;` — no calls, no side
//! effects): **movable `0x100`** (`SetMovable 0x776420` / `IsMovable 0x7764d0`), **resizable
//! `0x200`** (`SetResizable 0x776590` / `IsResizable 0x776640`), **userPlaced `0x1000`**
//! (`SetUserPlaced 0x776a50` / `IsUserPlaced 0x776b40`). The XML attributes land on the same
//! setter (`movable="true"` → `0x76a3c0(0x100)`, `resizable="true"` → `0x200` — wow-re
//! `rf24-framexml-loader.md`), which is why the loader now *calls* these instead of warning.
//!
//! `StartMoving 0x776700` tests the movable bit and raises when it is clear, then enters the drag
//! at `0x7652b0`, which `Raise()`s the frame, **sets the userPlaced bit itself**, and records the
//! whole drag in ONE root-side slot: `root+0xcfc` = the dragged frame, `+0xd00` = the drag type
//! (Lua `StartMoving` passes **3**), `+0xd08/+0xd0c` = the cursor at the last sample. One slot
//! means one drag: there is no second frame to move, which is why `StartMoving` carries a
//! `root+0xcfc != 0` guard at `7767e8` and why `StopMovingOrSizing 0x776990` compares
//! `[root+0xcfc] == self` before clearing.
//!
//! Movement is a **pump**, run from the mouse-move handler: `0x7655b0` (a diffed-bit-exact
//! PRIMITIVE) takes `dx = x − root+0xd08`, `dy = y − root+0xd0c`, and — only if either is
//! non-zero — applies them and re-centers the sample. The application (`0x76a660` → the 9-case
//! `geo_768710`, also a diffed-bit-exact PRIMITIVE) selects on the 3×3 region the drag grabbed:
//! the eight edge/corner cases resize, and **case 4, the plain move, accumulates the scaled delta
//! straight into the anchor's offsets in place** — `xOffset += dx/scale`, `yOffset += dy/scale`
//! (`CAnchor+0x4`/`+0x8`, wow-re `frame-model.md`) — then invalidates the layout (`0x7680e0`).
//!
//! Two consequences worth stating, because both are easy to get wrong:
//!
//! - **Nothing is written at stop.** `0x765640` is a fifteen-byte state clear (`root+0xcfc = 0;
//!   root+0xd00 = 0`) — there is no `SetPoint`, no `ClearAllPoints`, no rewrite of the anchor set
//!   to a single point. The frame keeps where it was dragged to because the *anchors themselves*
//!   were being moved the whole time, so `GetPoint()` afterwards reports the frame's own point
//!   with the dragged offsets — exactly what an addon then saves.
//! - **The mouse button does not end a Lua move.** The mouse-up handler `0x766420` auto-stops
//!   only drag types 1 and 2 (the built-in title-bar/edge drags); type 3 — the one `StartMoving`
//!   starts — must be stopped by an explicit `StopMovingOrSizing`. That is why the idiom above
//!   wires `OnDragStop` at all.
//!
//! ## What benilla does NOT take from that, and why
//!
//! - **All anchors translate, not just one.** wow-re's note reads the case-4 accumulate as hitting
//!   a single anchor record; it is their composition of verified pieces, not a recorded finding,
//!   and a one-anchor translation would *deform* a frame stretched between two anchors instead of
//!   moving it. Every movable frame in practice carries exactly one anchor, where the two readings
//!   are identical, so translating the whole set is the same behaviour everywhere it is observable
//!   and the sane behaviour where it is not.
//! - **No clamp rebate.** `geo_768710` rebates the residual delta when the clamp pushes back
//!   (`SetClampedToScreen`); we do not, so dragging a clamped frame into the screen edge and back
//!   out can lag the cursor. Named, not hidden — the rebate is one of the two halves of that
//!   primitive we have not transcribed (the other is the eight resize cases).
//! - **The `Raise()` is wired** (it used to be listed here as a deferral): `0x7652b0` raises the
//!   dragged frame before it takes the drag slot, and [`start_moving`] does the same through
//!   [`super::toplevel::raise`]. Like every raise it is occlusion-gated and confined to the frame's
//!   own stratum, so grabbing a movable window that overlaps nothing still changes no draw order.
//! - **No resize drag.** `StartSizing 0x776830` and the eight resize cases are not built.
//!   `SetResizable`/`IsResizable` are the flag only, and `StopMovingOrSizing` — which is one verb
//!   for both halves in the reference too — already covers the stop side of a resize that does
//!   not exist yet.

use mlua::{Lua, Table};

use crate::script::Model;
use crate::widget::FrameHandle;

use super::frame_handle_of;
use super::layout_methods::eff_scale;

/// The one in-flight `StartMoving` — the client's `root+0xcfc`/`+0xd08`/`+0xd0c` drag slot, held
/// in [`Model::moving`] beside the drag gesture that normally drives it.
///
/// `sample` is the cursor at the last pump, not at the grab: [`advance_move`] applies the
/// *difference* and re-centers, which is `0x7655b0`'s own shape. Nothing else is needed, because
/// the position being edited lives in the frame's anchors rather than here.
///
/// Pruning on frame destroy: the same boat as [`Model::drag_registered`] — nothing in this engine
/// destroys a frame yet; [`advance_move`] ends a move whose frame went away regardless.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FrameMove {
    /// The frame being moved (`root+0xcfc`).
    frame: FrameHandle,
    /// The cursor position at the last pump (`root+0xd08`/`+0xd0c`), UI px, y-up.
    sample: (f32, f32),
    /// Whether releasing the mouse ends this move on its own.
    ///
    /// **The reference's drag MODE, reduced to the one bit that is observable from Lua.** The
    /// mouse-up handler `0x766420` auto-cancels modes 1 (modifier-drag) and 2 (title region) and
    /// leaves mode 3 (`StartMoving`) running until `StopMovingOrSizing`
    /// (wow-re `widget-api-batch-benilla.md` Q6). So a title drag ends on release while a scripted
    /// one outlives the button — which is exactly the distinction this module's own doc already
    /// records for the drag/move split.
    pub(crate) auto_stop: bool,
}

/// An in-flight `StartSizing` drag: which frame, which grip, and the cursor at the last pump.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FrameSizing {
    frame: FrameHandle,
    /// The grip the caller named — the EDGES this drag moves. `LEFT` moves the left edge and
    /// leaves the right where it is, `BOTTOMRIGHT` moves both of those, and so on.
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
    sample: (f32, f32),
}

/// Populate `m`'s movable/resizable methods (see the module doc).
pub(super) fn install(lua: &Lua, m: &Table) -> mlua::Result<()> {
    // SetMovable(flag) / IsMovable() — flag word bit 0x100 (`0x776420`/`0x7764d0` through the
    // generic setter `0x76a3c0`). A pure flag write: it moves nothing, and clearing it mid-drag
    // stops nothing either (the setter has no side effects — wow-re ledger `0x76a3c0`); it is the
    // guard `StartMoving` tests, and the XML `movable="true"` attribute writes the same bit.
    // mlua's bool conversion is Lua truthiness, so the corpus's `SetMovable(1)` reads as true —
    // matching the reference binding's own `toboolean` marshal.
    m.set(
        "SetMovable",
        lua.create_function(|lua, (this, flag): (Table, bool)| {
            set_flag(lua, &this, Flag::Movable, flag)
        })?,
    )?;
    m.set(
        "IsMovable",
        lua.create_function(|lua, this: Table| get_flag(lua, &this, Flag::Movable))?,
    )?;
    // SetResizable(flag) / IsResizable() — bit 0x200 (`0x776590`/`0x776640`), `SetMovable`'s exact
    // shape. Stored and reported; no resize DRAG is built (see the module doc).
    m.set(
        "SetResizable",
        lua.create_function(|lua, (this, flag): (Table, bool)| {
            set_flag(lua, &this, Flag::Resizable, flag)
        })?,
    )?;
    m.set(
        "IsResizable",
        lua.create_function(|lua, this: Table| get_flag(lua, &this, Flag::Resizable))?,
    )?;
    // SetUserPlaced(flag) / IsUserPlaced() — bit 0x1000 (`0x776a50`/`0x776b40`), the client's "the
    // user placed this frame; persist its position across sessions" bit, and the one setter of the
    // three that is GUARDED: `776adb: test ah,0x3` refuses unless the frame is movable OR
    // resizable, raising the third of this family's error strings.
    //
    // benilla stores and reports the flag and **nothing consumes it yet**: persisting a frame's
    // position belongs with the layout cache, not with the drag that moved it, and building a
    // private position store here would put a second one beside the one that should own it. The
    // overwhelmingly common addon shape — set the bit, then save your own coordinates — is
    // unaffected either way.
    m.set(
        "SetUserPlaced",
        lua.create_function(|lua, (this, flag): (Table, bool)| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let ok = model
                .arena
                .frame(h)
                .is_some_and(|f| f.movable || f.resizable);
            if !ok {
                return Err(not_flagged(&model, h, "movable or resizable"));
            }
            if let Some(f) = model.arena.frame_mut(h) {
                f.user_placed = flag;
            }
            Ok(())
        })?,
    )?;
    m.set(
        "IsUserPlaced",
        lua.create_function(|lua, this: Table| get_flag(lua, &this, Flag::UserPlaced))?,
    )?;
    // StartMoving() — enter the drag (`0x776700` → `0x7652b0`). Raises on a frame that is not
    // movable, sets the userPlaced bit like the reference's drag-start does, and takes the one
    // drag slot; a second call while a move is in flight is refused, since there is only ever one
    // slot to take.
    m.set(
        "StartMoving",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            start_moving(&mut model, h)
        })?,
    )?;
    // StartSizing(point) — begin a resize drag from a named grip (`0x776830`, verified in wow-re's
    // ledger; the reference's own caller is `FloatingChatFrame.lua:600`,
    // `this:GetParent():StartSizing(anchorPoint)`).
    //
    // **What is verified and what is read, kept apart on purpose.** Verified: the verb exists, it
    // takes the grip name, it returns nothing, and `StopMovingOrSizing` ends it (the same slot
    // clear as a move). NOT recorded anywhere in wow-re — the ledger has it as ORCHESTRATION with
    // no inline math — is WHICH EDGES a given grip moves. Taken here as the plain meaning of an
    // anchor point, which is how the reference's own caller uses it (its resize grips pass the
    // corner they sit in): the named edges follow the cursor and the opposite ones stay put. If an
    // RE pass ever contradicts that, this comment is where to correct it.
    //
    // Four corpus addons reach it through ONE line — `FuBar_Panel.lua:980`, replicated into
    // FuBar_CorkFu, FuBar_FuXPFu, FuBar_SpellStatusFu and oRA2 — which is 1207's rule and why the
    // count is not four independent votes.
    m.set(
        "StartSizing",
        lua.create_function(|lua, (this, point): (Table, Option<String>)| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            if !model.arena.frame(h).is_some_and(|f| f.resizable) {
                return Err(not_flagged(&model, h, "resizable"));
            }
            // Same "do not start a second one" guard `start_moving` carries.
            if model.sizing.is_some() || model.moving.is_some() {
                return Ok(());
            }
            let p = point.unwrap_or_default().to_ascii_uppercase();
            let (left, right) = (p.contains("LEFT"), p.contains("RIGHT"));
            let (top, bottom) = (p.contains("TOP"), p.contains("BOTTOM"));
            // A grip naming no edge would resize nothing; the reference has no such call and we
            // refuse rather than invent one.
            if !(left || right || top || bottom) {
                return Ok(());
            }
            super::toplevel::raise(&mut model, h);
            if let Some(f) = model.arena.frame_mut(h) {
                f.user_placed = true;
            }
            let sample = model.cursor_pos;
            model.sizing = Some(FrameSizing {
                frame: h,
                left,
                right,
                top,
                bottom,
                sample,
            });
            Ok(())
        })?,
    )?;
    // StopMovingOrSizing() — leave the drag (`0x776990` → `0x765640`): a state clear, and only
    // when `self` IS the frame in the slot. Nothing is written back — the pump has been moving the
    // anchors all along, so the frame simply keeps the last position it was dragged to. Harmless
    // when nothing is moving, or when something else is (a double call, an addon that also wires
    // it to OnMouseUp, an OnDragStop that never had a StartMoving).
    m.set(
        "StopMovingOrSizing",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            if model.sizing.is_some_and(|sz| sz.frame == h) {
                model.sizing = None;
            }
            if model.moving.is_some_and(|mv| mv.frame == h) {
                model.moving = None;
            }
            Ok(())
        })?,
    )?;
    Ok(())
}

/// Which bit of the frame's flag word a `Set*`/`Is*` pair addresses (`[frame+0xb4]`, written by
/// `0x76a3c0`) — the three benilla models, as arena fields rather than a packed word.
#[derive(Clone, Copy)]
enum Flag {
    /// `0x100`.
    Movable,
    /// `0x200`.
    Resizable,
    /// `0x1000`.
    UserPlaced,
}

fn set_flag(lua: &Lua, this: &Table, which: Flag, value: bool) -> mlua::Result<()> {
    let h = frame_handle_of(lua, this)?;
    let mut model = lua.app_data_mut::<Model>().expect("model");
    if let Some(f) = model.arena.frame_mut(h) {
        match which {
            Flag::Movable => f.movable = value,
            Flag::Resizable => f.resizable = value,
            Flag::UserPlaced => f.user_placed = value,
        }
    }
    Ok(())
}

fn get_flag(lua: &Lua, this: &Table, which: Flag) -> mlua::Result<bool> {
    let h = frame_handle_of(lua, this)?;
    let model = lua.app_data_ref::<Model>().expect("model");
    Ok(model.arena.frame(h).is_some_and(|f| match which {
        Flag::Movable => f.movable,
        Flag::Resizable => f.resizable,
        Flag::UserPlaced => f.user_placed,
    }))
}

/// The family's refusal, named frame and all — the reference's own shape (it formats the frame's
/// `GetName()`, substituting a literal when the frame is anonymous).
///
/// **The text is OURS.** wow-re records the guards and the three `.rdata` addresses
/// (`0x879810` StartMoving, `0x879828` StartSizing, `0x879844` SetUserPlaced) but not the strings
/// at them — only the truncated IDA symbol stubs `aFrameSIsNotMov` / `aFrameSIsNotRes` /
/// `aFrameSIsNotM_0`, which is what this wording is shaped after. Nothing should match on it.
fn not_flagged(model: &Model, h: FrameHandle, want: &str) -> mlua::Error {
    let who = model
        .arena
        .frame(h)
        .and_then(|f| f.name.clone())
        .unwrap_or_else(|| "<anonymous>".into());
    mlua::Error::runtime(format!("Frame {who} is not {want}"))
}

/// `StartMoving()`'s body (`0x776700` → `0x7652b0`): refuse a frame that is not movable, refuse a
/// second move while one is in flight (there is one drag slot), **raise**, then take the slot —
/// setting the userPlaced bit, which the reference's drag-start does itself rather than leaving to
/// `SetUserPlaced`.
fn start_moving(model: &mut Model, h: FrameHandle) -> mlua::Result<()> {
    if !model.arena.frame(h).is_some_and(|f| f.movable) {
        return Err(not_flagged(model, h, "movable"));
    }
    // `7767e8`'s `root+0xcfc != 0` guard. What the reference does past *not starting a second
    // move* is not recorded; refusing silently is the reading that cannot invent behaviour.
    if model.moving.is_some() {
        return Ok(());
    }
    // The drag-start raise — `0x7652b0` @`0x7652d7`, before it sets the userPlaced bit and records
    // the drag slot, which is the order kept here. Grabbing a window brings it forward; the worker
    // supplies the toplevel gate (a non-toplevel movable frame raises nothing).
    super::toplevel::raise(model, h);
    if let Some(f) = model.arena.frame_mut(h) {
        f.user_placed = true;
    }
    model.moving = Some(FrameMove {
        frame: h,
        sample: model.cursor_pos,
        // Lua `StartMoving` is mode 3: it survives the button and ends only at
        // `StopMovingOrSizing`.
        auto_stop: false,
    });
    Ok(())
}

/// Begin a **title-region** move — mode 2, the drag a mouse-down inside `frame:GetTitleRegion()`
/// starts (wow-re Q6, `0x7662c0` → `0x765320(frame, mode=2, …)` → `0x7652b0`).
///
/// [`start_moving`]'s body **minus the movable gate**, and that omission is the carved part rather
/// than a shortcut: the movable bit is `frame+0xb4 & 0x100`, tested by `StartMoving` (`0x77678b`,
/// else `"Frame %s is not movable"`) and by the modifier-drag path — and **not read anywhere** on
/// `0x7662c0`→`0x765320`→`0x7652b0`→`0x768430`. Q6 marks the no-gate reading VERIFIED and the
/// observable claim INFERRED, because both FrameXML users happen to be `movable="true"`; a title
/// region on a non-movable frame is the case that would tell them apart, and it drags here.
///
/// Returns whether a move actually started — `false` when one was already in flight, which is
/// `0x7767e8`'s `root+0xcfc != 0` guard and the same refusal [`start_moving`] makes.
pub(crate) fn start_title_move(model: &mut Model, h: FrameHandle) -> bool {
    if model.moving.is_some() {
        return false;
    }
    super::toplevel::raise(model, h);
    if let Some(f) = model.arena.frame_mut(h) {
        f.user_placed = true;
    }
    model.moving = Some(FrameMove {
        frame: h,
        sample: model.cursor_pos,
        auto_stop: true,
    });
    true
}

/// Pump an in-flight move to the cursor at `pos` — the engine half of `0x7655b0`, run from
/// [`crate::script::UiScript::mouse_move`] beside [`crate::script::cursor::maybe_start_drag`] and,
/// like it, BEFORE the hover-boundary early return (a frame dragged around underneath the cursor
/// crosses no boundary at all, so a move parked behind that return would only advance when the
/// cursor happened to leave the frame).
///
/// Applies `(pos − sample)` scaled into the frame's anchor offsets and re-centers the sample; a
/// zero delta does nothing at all, and a frame that died mid-move ends the move rather than
/// writing to a dead handle.
/// Pump an in-flight `StartSizing`: move the gripped edges to the cursor, leave the others.
///
/// The mirror of [`advance_move`] — same sample-and-recentre, same dead-frame bail, same
/// local-unit scaling (offsets are pre-scale, so a cursor delta divides by the frame's own scale on
/// the way in). Where a move translates every anchor, a resize moves the gripped edges only: the
/// width/height change, and the anchor offsets follow only for the edges that actually moved, which
/// is what keeps the OPPOSITE edge planted.
pub(crate) fn advance_size(model: &mut Model, pos: (f32, f32)) {
    let Some(sz) = model.sizing else { return };
    if model.arena.frame(sz.frame).is_none() {
        model.sizing = None;
        return;
    }
    let (dx, dy) = (pos.0 - sz.sample.0, pos.1 - sz.sample.1);
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    let inv = 1.0 / eff_scale(model, sz.frame);
    let (dx, dy) = (dx * inv, dy * inv);
    let Some(input) = model.layout_inputs.get_mut(&sz.frame) else {
        return;
    };
    // Width grows when the RIGHT grip goes right, or the LEFT grip goes left. Height likewise with
    // y-up: the TOP grip going up grows it, the BOTTOM grip going down grows it.
    let dw = if sz.right {
        dx
    } else if sz.left {
        -dx
    } else {
        0.0
    };
    let dh = if sz.top {
        dy
    } else if sz.bottom {
        -dy
    } else {
        0.0
    };
    let (w0, h0) = (input.width, input.height);
    input.width = (input.width + dw).max(1.0);
    input.height = (input.height + dh).max(1.0);
    let mut moved = input.width.to_bits() != w0.to_bits() || input.height.to_bits() != h0.to_bits();
    // The planted edge: a frame anchored by its LEFT that is gripped on the LEFT has to move too,
    // or the resize would push the right edge instead. Only the gripped axis shifts.
    if !input.anchors.is_empty() && (sz.left || sz.bottom) {
        for a in &mut input.anchors {
            if sz.left {
                a.x_off += dx;
            }
            if sz.bottom {
                a.y_off += dy;
            }
        }
        moved |= (sz.left && dx != 0.0) || (sz.bottom && dy != 0.0);
    }
    // The zero-delta return above is on the RAW cursor delta, which is not the same question: a
    // single-axis grip dragged purely across its axis gives `dw == dh == 0`, and so does a grip
    // held past `.max(1.0)` saturation. Both used to bump the epoch and then hash all ~10k
    // anchored regions to conclude nothing had moved — the castbar's bug class in miniature, for
    // every frame of such a drag (decision 1385).
    if moved {
        // Offsets only — a resize grip never repoints an anchor, so the frame names itself and the
        // cached graph survives the drag (decision 1388). A drag is per-frame by nature: this is
        // the difference between a smooth window resize and one that re-derives 13,656 nodes on
        // every mouse-move.
        model.touch_layout_frame(sz.frame);
    }
    model.sizing = Some(FrameSizing { sample: pos, ..sz });
}

pub(crate) fn advance_move(model: &mut Model, pos: (f32, f32)) {
    let Some(mv) = model.moving else { return };
    if model.arena.frame(mv.frame).is_none() {
        model.moving = None;
        return;
    }
    let (dx, dy) = (pos.0 - mv.sample.0, pos.1 - mv.sample.1);
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    // Offsets are LOCAL units — the resolver multiplies them by the frame's own layoutScale
    // (`anchor_resolve_x`), so the cursor's screen delta divides by it on the way in, exactly the
    // `dx/scale` of `geo_768710`'s case 4.
    let inv = 1.0 / eff_scale(model, mv.frame);
    let (dx, dy) = (dx * inv, dy * inv);
    if let Some(input) = model.layout_inputs.get_mut(&mv.frame) {
        // Translating the whole set, not slot 0 — see the module doc. An anchorless frame is not
        // resolvable and so is not on screen to drag; it simply does not move.
        if !input.anchors.is_empty() {
            for a in &mut input.anchors {
                a.x_off += dx;
                a.y_off += dy;
            }
            // The one mutation path every layout setter shares — the tier-1 epoch, never the
            // solver's arrays (see [`super::layout_methods`]'s mutate-only-on-change law; a zero
            // delta returned above, so this write always moved something). Translating every
            // anchor moves no TARGET, so the frame names itself (decision 1388).
            model.touch_layout_frame(mv.frame);
        }
    }
    model.moving = Some(FrameMove { sample: pos, ..mv });
}
