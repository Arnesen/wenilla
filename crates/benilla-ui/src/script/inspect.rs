//! The **inspect** surface (decision 0631) — the intent queues behind the ref's
//! `NotifyInspect`/`ClearInspectPlayer`, and the foreign-unit equipment view the unit-keyed
//! `GetInventoryItem*` family reads through.
//!
//! ## Why there is so little here
//!
//! Inspect is the thinnest of the item windows because **the data is already local**. A player's
//! worn gear rides `PLAYER_VISIBLE_ITEM_<n>_0`, which is `UF_FLAG_PUBLIC` — the server streams it
//! to every observer, and benilla already decodes it to render other players' equipment
//! (`benilla::entities::equipment`). So the inspect window paints from a descriptor we hold, not
//! from a reply we wait for; `SMSG_INSPECT` echoes the guid and nothing else, and no reference
//! handler registers an inspect event. `InspectFrame_Show` calls `NotifyInspect(unit)` and
//! `ShowUIPanel` in the same breath, and `InspectPaperDollFrame_OnShow` reads all 19 slots
//! immediately (`Blizzard_InspectUI.lua:6-13`, `InspectPaperDollFrame.lua:57-79`).
//!
//! ## The seam
//!
//! - **Intents:** `NotifyInspect(unit)` queues the token (the app resolves it → player guid →
//!   `CMSG_INSPECT`, which server-side also sets our selection); `ClearInspectPlayer()` flags the
//!   drop, the ref's own `InspectFrame_OnHide` call.
//! - **Push:** the app pushes an [`InspectView`] each frame the inspected unit's resolved slot
//!   views change ([`UiScript::set_inspect`]), or `None` when nothing is being inspected.
//! - **Read:** *no new item getters.* The reference's own `GetInventoryItemTexture(unit, slot)`
//!   family is already unit-keyed; `super::char_stats::player_inv_slot` routes `"player"` to the
//!   self feed and the inspected token here.
//! - **Range:** the two verified d² predicates, `CanInspect` and `CheckInteractDistance`, over one
//!   app-fed per-token distance map ([`UiScript::set_inspect_reach`]) — the VM holds no positions.
//!
//! The view is keyed by **unit token**, not guid — the ref stores `InspectFrame.unit` as a token
//! and re-reads it on `PLAYER_TARGET_CHANGED`, so an inspect window follows a re-target exactly as
//! the real one does. The `guid` rides along only so the app can tell "same token, different
//! player" apart when it rebuilds.

use std::cmp::Ordering;
use std::collections::HashMap;

use mlua::{Lua, Value};

use super::char_stats::InventorySlots;
use super::Model;

/// The `CheckInteractDistance` threshold table — `{10², 11.1111², 10², 30²}` for the live API's
/// `type ∈ 1..4`, read straight out of the binary (wow-re §5-VERIFIED
/// `PRIMITIVE:check_interact_dist2` @ `0x48ba00`, built from the static `.rdata` at
/// `0x804498`/`0x804490`/`0x80448c`/`0x8044a4`). Type 1 is the *inspect* row's distance, and shares
/// its 100.0 with [`super::inspect`]'s own `CanInspect` threshold.
pub const INTERACT_DIST_SQ: [f64; 4] = [100.0, 123.45678, 100.0, 900.0];

/// The `CanInspect` threshold, **squared** — `DAT_00b4d918`, which the client's own writer builds by
/// squaring the static `.rdata` `10.0` at `0x804498` (wow-re §5-VERIFIED
/// `PRIMITIVE:caninspect_dist2` @ `0x48a1b0`, `ledger.tsv:823`). vmangos enforces the same 10 yards
/// as `INSPECT_DISTANCE` (`ObjectDefines.h:26`), so client and server agree exactly.
pub const CAN_INSPECT_DIST_SQ: f64 = 100.0;

/// What the app resolved about one popup token's unit this frame — the input both range predicates
/// read ([`UiScript::set_inspect_reach`]).
///
/// An entry exists **iff the token resolved to a live unit object**, which is the distinction the
/// reference's own bindings turn on: `0x515940` maps the token to a GUID and then asks the object
/// manager for it, and a NULL there is answered `nil` without any distance being computed. A party
/// member outside the local area is exactly that case — `0x4e81a0` reads their GUID straight out of
/// the roster array, so the GUID is non-zero and the object lookup misses silently.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnitReach {
    /// Squared distance from the player, in the binary's own accumulation shape.
    pub dist_sq: f64,
    /// The unit passes the two NON-distance refusals vmangos makes for an inspect
    /// (`sObjectMgr.GetPlayer` and `!IsValidAttackTarget`, `MiscHandler.cpp:945-956`) — consulted
    /// by `CanInspect` alone. `CheckInteractDistance` is a pure distance test in the binary and
    /// must not read this, or following an enemy player would gray a row the reference leaves live.
    pub inspectable: bool,
}

/// What the app has resolved for the unit currently being inspected (decision 0631).
#[derive(Clone, Debug, PartialEq)]
pub struct InspectView {
    /// The unit token the frame is inspecting — the ref's `InspectFrame.unit` (`"target"`,
    /// `"party3"`, …). The inventory router matches a binding's `unit` argument against this.
    pub unit: String,
    /// The player guid the token resolved to when these slots were read.
    pub guid: u64,
    /// The inspected player's equipment, indexed by live-API inventory slot id exactly like the
    /// self feed (1..=19). Index 0 (ammo) and 20..=23 (bags) stay `None` — a foreign player
    /// exposes neither, and the ref's inspect paper doll has no button for them.
    pub slots: InventorySlots,
}

impl super::UiScript {
    /// Push the per-token **inspect reach** map: unit token → squared distance from the player to
    /// that unit, for every popup token the app could resolve to a live player this frame.
    ///
    /// One app-fed number serves both range predicates, because the binary uses the same d² for
    /// both: `CanInspect` compares it against `100.0`, and `CheckInteractDistance(unit, type)`
    /// against [`INTERACT_DIST_SQ`]`[type-1]`. It lives here rather than on
    /// [`super::UnitState`] because the popup rows need it for **party** tokens too, and those have
    /// no `UnitState` (only `"player"`/`"target"`/`"mouseover"` are fed) — keying it off the unit
    /// snapshot would have left the party menu's rows permanently dead.
    ///
    /// A token absent from the map is a token the object manager holds **no unit for**, and both
    /// predicates answer `nil` there — the reference's own null-object arm (wow-re
    /// `system/ui/scratch/dist2-null-unit-arm.md`, VERIFIED: `0x48babe test ecx,ecx; je` →
    /// `lua_pushnil`). It used to read as *in range*, on the reasoning that missing data must not
    /// gray a row; the binary says the opposite, and that default was report B316 — every distance
    /// row lit up for exactly the party member who was too far away to have an object at all.
    pub fn set_inspect_reach(&mut self, reach: HashMap<String, UnitReach>) {
        self.model_mut().inspect_reach = reach;
    }

    /// Push (or clear, with `None`) the inspected unit's resolved equipment view. The app calls
    /// this whenever the view changes; it fires no event of its own — the app fires
    /// `UNIT_INVENTORY_CHANGED` for the inspected token, which is the signal the ref's
    /// `InspectPaperDollItemSlotButton_OnEvent` actually listens for.
    pub fn set_inspect(&mut self, view: Option<InspectView>) {
        self.model_mut().inspect = view;
    }

    /// Drain the unit tokens `NotifyInspect` queued — the app resolves each to a player guid and
    /// sends `CMSG_INSPECT`.
    pub fn take_inspect_notifies(&mut self) -> Vec<String> {
        std::mem::take(&mut self.model_mut().inspect_notifies)
    }

    /// Whether `ClearInspectPlayer` was called since the last drain (and clear the flag) — the
    /// app drops its inspect target, which stops the per-frame slot resolve.
    pub fn take_inspect_clear(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().inspect_clear)
    }

    /// The inspect model pane's bake yaw in radians — the twin of
    /// [`Self::paperdoll_yaw`], read by the app onto the `"inspect"` booth slot each frame.
    pub fn inspect_yaw(&self) -> f32 {
        self.model_ref().inspect_yaw
    }
}

/// What the app resolved for `token`, or `None` when it resolved to no live unit at all — the
/// reference's NULL-object arm, which both predicates answer `nil` to.
fn reach(lua: &Lua, token: &Option<String>) -> Option<UnitReach> {
    let token = token.as_deref()?;
    let model = lua.app_data_ref::<Model>().expect("model app_data");
    model.inspect_reach.get(token).copied()
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // CanInspect(unit) → 1/nil: the gate the ref's `InspectFrame_Show` opens on
    // (`Blizzard_InspectUI.lua:8`). The real client's `0x48a1b0` is a d² range test — wow-re
    // §5-VERIFIED `PRIMITIVE:caninspect_dist2`: **out of range iff `threshold < d²`**, where
    // `threshold = DAT_00b4d918 = 100.0` (10 yards, the same number vmangos enforces as
    // `INSPECT_DISTANCE`). The operator is theirs too: `test ah,0x41; jne` skips the out-of-range
    // action on `C0|C3` — Less, Equal, *or unordered* — so the three in-range arms below are that
    // mask spelled out, and a NaN distance reads as in RANGE, not out. Only the app-fed distance is
    // consulted here; the is-a-player and not-attackable legs are folded in app-side, since a token
    // that fails them is never entered in the reach map at all. Decision 0631.
    g.set(
        "CanInspect",
        lua.create_function(|lua, unit: Option<String>| {
            let ok = match reach(lua, &unit) {
                Some(r) => {
                    r.inspectable
                        && matches!(
                            r.dist_sq.partial_cmp(&CAN_INSPECT_DIST_SQ),
                            Some(Ordering::Less | Ordering::Equal) | None
                        )
                }
                // No live unit object: the binary reaches its `lua_pushnil` tail through
                // `0x4944a0`'s null-`this` guard rather than an early return, but the value it
                // pushes is the same one — nothing to inspect.
                None => false,
            };
            Ok(if ok { Value::Integer(1) } else { Value::Nil })
        })?,
    )?;

    // CheckInteractDistance(unit, type) → 1/nil, `type ∈ 1..4` (wow-re §5-VERIFIED
    // `PRIMITIVE:check_interact_dist2` @ `0x48ba00`): **in range iff `d² < table[type-1]`** —
    // note the STRICT `<` here against `CanInspect`'s non-strict gate above, which is the
    // binary's own asymmetry (`test ah,0x5; jp` takes the out path unless `d² < thr` ordered), not
    // a transcription slip. The UnitPopup rows' `dist` field indexes it.
    //
    // The three degenerate arms are the binary's too (`dist2-null-unit-arm.md`, VERIFIED), and
    // they are three DIFFERENT answers rather than one permissive default:
    //
    // - **No live unit for the token** → `nil` (`0x48babe test ecx,ecx; je`). This is the party
    //   member outside the local area, and answering `1` here was report B316.
    // - **`type` numeric but outside 1..4** → `nil`. The compare is UNSIGNED on `trunc(type) − 1`
    //   (`0x48bac5 cmp esi,0x4; jae`), so `0`, negatives and `≥ 5` all take it, and a fractional
    //   type truncates toward zero first (`__ftol`: `1.9 → 1`, `0.5 → 0 → nil`).
    // - **`type` missing or not a number** → a **script error**, not `nil`
    //   (`0x48bb48 call luaL_error`, which longjmps and never returns). Same for a non-string
    //   unit. A caller that passes neither argument is not asking a question the reference
    //   answers.
    g.set(
        "CheckInteractDistance",
        lua.create_function(|lua, (unit, kind): (Option<String>, Option<f64>)| {
            let Some(kind) = kind else {
                return Err(mlua::Error::RuntimeError(
                    "Usage: CheckInteractDistance(\"unit\", distIndex)".into(),
                ));
            };
            // `__ftol` chops toward zero; the index is then compared UNSIGNED, so anything that
            // is not exactly 1..=4 falls out of the table lookup and answers nil.
            let thr = usize::try_from(kind.trunc() as i64)
                .ok()
                .and_then(|k| k.checked_sub(1))
                .and_then(|i| INTERACT_DIST_SQ.get(i).copied());
            let ok = match (reach(lua, &unit), thr) {
                (Some(r), Some(thr)) => r.dist_sq < thr,
                _ => false,
            };
            Ok(if ok { Value::Integer(1) } else { Value::Nil })
        })?,
    )?;

    // NotifyInspect(unit) — the ref's request verb (`Blizzard_InspectUI.lua:9`). Queues the token;
    // the app resolves it → guid → CMSG_INSPECT. The window does NOT wait on the reply (see the
    // module doc), so this is fire-and-forget by design, not an unfinished handshake.
    g.set(
        "NotifyInspect",
        lua.create_function(|lua, unit: String| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .inspect_notifies
                .push(unit);
            Ok(())
        })?,
    )?;

    // ClearInspectPlayer() — the ref calls it from `InspectFrame_OnHide` (l.58) to drop the
    // engine's inspected-player state. Ours stops the app's per-frame slot resolve.
    g.set(
        "ClearInspectPlayer",
        lua.create_function(|lua, ()| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .inspect_clear = true;
            Ok(())
        })?,
    )?;

    // BenillaInspectModel_SetFacing(radians) — the inspect pane's own bake yaw, the exact twin of
    // `BenillaPaperDollModel_SetFacing` (decision 0208 §5's rotate-adjusts-the-bake). Two scalars
    // rather than one shared: the two windows can be open at different facings, and the ref's own
    // `InspectModelFrame` carries its own `rotation` independent of the character pane's.
    g.set(
        "BenillaInspectModel_SetFacing",
        lua.create_function(|lua, radians: f32| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .inspect_yaw = radians;
            Ok(())
        })?,
    )?;

    Ok(())
}
