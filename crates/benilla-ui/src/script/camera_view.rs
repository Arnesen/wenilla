//! The **five camera views** — `SetView` / `SaveView` / `ResetView` / `NextView` / `PrevView`,
//! plus `FlipCameraYaw`: the outbound half of the reference's `UIUtil\Camera.cpp` Lua surface.
//!
//! Six globals, no state — [`super::follow`]'s shape exactly. Nothing here reads the camera: every
//! one of these is an **engine action** over state this VM does not hold (the orbit arm, the pitch,
//! the character's facing, the zoom ceiling), so each call queues a [`CameraViewRequest`] the app
//! drains ([`super::UiScript::take_camera_view_requests`]) and applies to the rig
//! (`benilla_app::player::camera_view`). The alternative — reaching into the ECS from inside the
//! VM — is the thing decision 0068 §3 exists to forbid.
//!
//! **Where they come from.** The reference registers them in one table, `0x84f7a0` (`{const char*
//! name; handler}` pairs, dumped from `WoW.exe` 5875): rows 15…20 are `SetView 0x50b5b0`,
//! `SaveView 0x50b600`, `ResetView 0x50b640`, `NextView 0x50b680`, `PrevView 0x50b690`,
//! `FlipCameraYaw 0x50b6a0`. Sixteen 1.12 binding commands have one of these as their whole body
//! (`SETVIEW1`…`5`, `SAVEVIEW2`…`5`, `RESETVIEW2`…`5`, `NEXTVIEW`, `PREVVIEW`, `FLIPCAMERAYAW`).
//!
//! ## The argument ABI is a third shape, and it is neither of [`super::binding_abi`]'s two
//!
//! The four indexed entry points share one prologue, byte-identical across `0x50b5b0` /
//! `0x50b600` / `0x50b640`:
//!
//! ```text
//! 50b5b8  call 0x6f34d0          ; is-number(L, 1)  — a number, or a coercible string
//! 50b5bf  je   <exit>            ; NOT a number -> xor eax,eax ; ret   (silent, zero Lua values)
//! 50b5c8  call 0x6f3620          ; tonumber(L, 1)
//! 50b5cd  call 0x40a2b0          ; double -> int32, truncating toward zero
//! 50b5d4  jle  <exit>            ; n <= 0  -> silent
//! 50b5d9  jg   <exit>            ; n >  5  -> silent
//! 50b5e5  dec  eax               ; Lua 1..5  ->  internal view 0..4
//! ```
//!
//! So the failure edge **returns without raising** — it never reaches `luaL_error 0x6f4940`, which
//! is the discriminator [`super::binding_abi`]'s header sets out. A macro doing `SetView(0)`,
//! `SetView(9)`, `SetView("left")` or `SetView()` is a silent no-op in the real client, and is one
//! here. That rules out `binding_abi::number_arg` (shape A — raises) and makes
//! `binding_abi::coerced_number` (shape C — no guard, defaults to `0.0`) merely *accidentally*
//! right, so the guard is written out rather than borrowed.
//!
//! `FlipCameraYaw` takes the same is-number guard and then the raw float — `0x6f3620` straight to
//! `fstp [ebp-4]`, with no `0x40a2b0` — so its argument is **not** truncated to an integer.
//!
//! ## Two verified facts that a reader will expect to be otherwise
//!
//! - **The range is 1…5 for all three of Set/Save/Reset.** `SaveView(1)` and `ResetView(1)` are
//!   accepted by the engine and act on `FIRST_PERSON`; `0x50fa30` (SaveView's body) has no view-0
//!   gate at all. 1.12 ships no `SAVEVIEW1`/`RESETVIEW1` *binding*, which is a `Bindings.xml`
//!   decision, not an engine one — a macro reaches what the keybinding cannot.
//! - **`NextView`/`PrevView` do not wrap.** `0x50faa0` is `eax = view + 1; cmp eax,5; jge <ret>`
//!   and `0x50fac0` is `test eax,eax; jle <ret>; dec eax` — each is a hard stop at its end of the
//!   range, so holding `END` walks 1→2→3→4→5 and stays there. The app enforces it (this side only
//!   queues the verb).

use mlua::{Lua, Value};

use super::Model;

/// The number of camera views — the reference's default-string table `0x84f488` is five rows, and
/// every entry point above is bounded by it.
pub const CAMERA_VIEW_COUNT: u8 = 5;

/// One camera-view intent queued by the Lua surface, drained by the app
/// ([`super::UiScript::take_camera_view_requests`]). Plain data — [`super::follow::FollowRequest`]'s
/// twin.
///
/// The indices are the **internal** ones, `0..CAMERA_VIEW_COUNT`, already range-checked here
/// exactly where the reference range-checks them (its Lua handler, `dec eax`). The app applies
/// them; it never re-validates a number that cannot be out of range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CameraViewRequest {
    /// `SetView(n)` — make view `n-1` the live one (distance, pitch and the saved yaw offset).
    Set(u8),
    /// `SaveView(n)` — store the live pose into view `n-1`.
    Save(u8),
    /// `ResetView(n)` — put view `n-1` back to its shipped default.
    Reset(u8),
    /// `NextView()` — the view above the current one, if there is one (no wrap).
    Next,
    /// `PrevView()` — the view below the current one, if there is one (no wrap).
    Prev,
    /// `FlipCameraYaw(degrees)` — add this many **degrees** to the camera's yaw. The binding body
    /// 1.12 ships is `FlipCameraYaw(180)`.
    FlipYaw(f32),
}

impl super::UiScript {
    /// Drain the camera-view intents queued since the last call — the app applies each to the
    /// camera rig.
    pub fn take_camera_view_requests(&mut self) -> Vec<CameraViewRequest> {
        std::mem::take(&mut self.model_mut().camera_view_requests)
    }
}

/// The shared prologue of `SetView`/`SaveView`/`ResetView`: `is-number` → `tonumber` → truncate to
/// `i32` → the `1 ..= 5` window → the internal index. `None` is every silent-return edge in one.
fn view_arg(lua: &Lua, v: Value) -> Option<u8> {
    // `0x6f34d0` is-number, then `0x6f3620` tonumber, then `0x40a2b0`'s chop-toward-zero cast —
    // `lua.coerce_number` is the first two (Lua coerces a numeric string), `as i64 as i32` the
    // third (see `binding_abi::number_arg`, whose only difference from this is that it raises).
    let n = lua.coerce_number(v).ok().flatten()? as i64 as i32;
    // `test eax,eax; jle` then `cmp eax,5; jg` — both silent.
    (1..=i32::from(CAMERA_VIEW_COUNT))
        .contains(&n)
        .then(|| (n - 1) as u8)
}

/// Register the six camera-view globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // SetView(n) — `0x50b5b0`. Load view n (1..5). The reference then calls `0x512e90(view,
    // cameraViewBlendStyle)`, which also writes the `cameraView` CVar so the active view survives
    // a restart; the app does both halves.
    g.set(
        "SetView",
        lua.create_function(|lua, n: Value| {
            if let Some(view) = view_arg(lua, n) {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                model
                    .camera_view_requests
                    .push(CameraViewRequest::Set(view));
            }
            Ok(())
        })?,
    )?;

    // SaveView(n) — `0x50b600` → `0x50fa30`, which reads the camera's three TARGET fields
    // (`+0x198` distance, `+0x1e0` pitch, `+0x210` yaw) into the slot array and writes each to its
    // archived CVar. Accepts n = 1 (see the module header).
    g.set(
        "SaveView",
        lua.create_function(|lua, n: Value| {
            if let Some(view) = view_arg(lua, n) {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                model
                    .camera_view_requests
                    .push(CameraViewRequest::Save(view));
            }
            Ok(())
        })?,
    )?;

    // ResetView(n) — `0x50b640` → `0x50fae0`, which re-parses the shipped default strings at
    // `0x84f488 + 12·view` into the slot and re-applies the view if it is the live one.
    g.set(
        "ResetView",
        lua.create_function(|lua, n: Value| {
            if let Some(view) = view_arg(lua, n) {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                model
                    .camera_view_requests
                    .push(CameraViewRequest::Reset(view));
            }
            Ok(())
        })?,
    )?;

    // NextView() / PrevView() — `0x50b680` → `0x50faa0` and `0x50b690` → `0x50fac0`. No arguments
    // at all: neither handler touches the Lua stack. Neither wraps.
    g.set(
        "NextView",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.camera_view_requests.push(CameraViewRequest::Next);
            Ok(())
        })?,
    )?;
    g.set(
        "PrevView",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.camera_view_requests.push(CameraViewRequest::Prev);
            Ok(())
        })?,
    )?;

    // FlipCameraYaw(degrees) — `0x50b6a0`, the whole body being
    // `cam[+0x100] += arg × 0.01745329238474369`. Its argument keeps its fraction (no `0x40a2b0`),
    // and the is-number guard's failure edge is the same silent return the four above take.
    g.set(
        "FlipCameraYaw",
        lua.create_function(|lua, degrees: Value| {
            if let Some(d) = lua.coerce_number(degrees).ok().flatten() {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                model
                    .camera_view_requests
                    .push(CameraViewRequest::FlipYaw(d as f32));
            }
            Ok(())
        })?,
    )?;

    Ok(())
}
