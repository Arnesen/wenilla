//! **Posture** — the two things the controller does to how the body *holds* itself, as opposed to
//! where it goes: the **stand state** (the `/sit` family and the `X` key) and the **sheath**
//! toggle (`Z`). One module because they are one mechanism in the reference and they interlock
//! here: entering a stand state ∉ {0 STAND, 2 SIT_CHAIR} force-stows drawn weapons through the
//! same anim-layer setter the `Z` key drives, and the sheath toggle's own guard chain refuses on
//! the stand state this frame just committed.
//!
//! Both halves are **silent-refusal** mechanisms — the reference builds no packet and says
//! nothing when a press is rejected — which is why the stand half writes the `sit` trace tag
//! ([`super::move_trace::posture`]) at every commit *and* every refusal: on screen a granted sit
//! and a refused one are the same picture (decisions 0080, 0881; bug B155).

use bevy::prelude::*;

use super::{move_trace, state, BodyQuery, ClientCommand, NetCommands, Player, StandStateRequest};

/// Run this frame's stand-state decision and the sheath toggle, and return the **committed**
/// stand state — the local commit overlaid on the server's echoed byte, which is what the body
/// pose and the sheath guard both read (decision 0080c).
#[allow(clippy::too_many_arguments)]
pub(super) fn update(
    player: &mut Player,
    body: &BodyQuery,
    binds: &crate::bindings::BindingsState,
    net: &NetCommands,
    sheath: &mut MessageWriter<crate::creature_anim::SheathRequest>,
    asks: &mut MessageReader<StandStateRequest>,
    moving: bool,
    turned: bool,
) -> u8 {
    // Stand state (decision 0080c) — a real field, not a local bool: X volunteers
    // `CMSG_STANDSTATECHANGE` (sit 1 ↔ stand 0) and movement input stands us up; the
    // server's echo into `UNIT_FIELD_BYTES_1` drives the pose — ours *and* every
    // observer's. `stand_pending` is the local commit (the client's `SetStandState`
    // applies immediately and sends, one setter — `0x6127b0`), overlaid on the echoed
    // byte until it lands so the pose never waits on the round-trip.
    let (stand_byte, reads_dead) = body
        .single()
        .ok()
        .and_then(|(.., store, _, _, _, _, _)| {
            // `SetStandState`'s own first two guards, read together with the byte they gate:
            // health ≤ 0, **or** `UNIT_DYNAMIC_FLAGS & 0x20` — a feigner, whose health never moved.
            // Deliberately NOT `unit_reads_dead`, which folds in stand state 7 as a third term the
            // setter does not test (wow-re `local-move-input-gate.md` §6.7; decision 1753).
            store.map(|s| {
                (
                    s.0.unit_stand_state(),
                    s.0.unit_is_dead() || s.0.unit_dynflag_dead(),
                )
            })
        })
        .unwrap_or((0, false));
    if player.stand_pending == Some(stand_byte) {
        player.stand_pending = None; // the echo landed
    }
    let stand_state = player.stand_pending.unwrap_or(stand_byte);
    // The queued asks first (the `/sit` family — decision 0881), then the X key, which is the
    // reference's own precedence: a queued `SetStandState` ran during the frame's message pass,
    // the key is read now. The last writer wins, and every one of them lands on the single
    // commit-and-send below.
    let mut request_stand = asks.read().last().map(|r| r.state);
    if binds.fired(crate::bindings::cmd::SIT_OR_STAND) {
        request_stand = Some(u8::from(stand_state == 0));
    }
    // Any movement input stands the avatar back up (the client volunteers the stand — the
    // server never auto-stands a moving player; verified vmangos MovementHandler). The input
    // set is byte-pinned (wow-re `standstate-movement-trigger.md`, §5 2026-07-14): the net
    // input axes (translation), keyboard turn, and jump all reach the guarded stand wrapper
    // `0x60be30(0)`; a left-drag camera orbit provably does not; sit(1)/chair(2)/sleep(3)
    // all stand identically (the value-agnostic `GetStandState() != 0` gate). The one open
    // corner: no static path was found for a pure right-drag MOUSE turn while seated — the
    // director's ref observation (it stands you up) is the ground truth this keeps; the
    // byte trigger is flagged LIVE-CAPTURE in the wow-re note.
    // **A knockback stands you up too** (decision 1702) — the one entry here that is nobody's
    // input. The reference's knockback apply carries it as a side effect of the launch (the
    // `0x60e139` block, whose indirect `call [edx+0xa4]` resolves to `GetStandState 0x60be50`),
    // which is the same guarded wrapper every trigger above reaches. Read off the armed latch
    // rather than the take-off, because this block runs before the mover: the latch was written
    // by [`super::wire_in::apply_server_moves`] at the top of the frame and is still there.
    let knocked_out_of_it = player.knockback.is_some();
    if (moving || turned || knocked_out_of_it || binds.fired(crate::bindings::cmd::JUMP))
        && stand_state != 0
        && request_stand.is_none()
    {
        request_stand = Some(0);
    }
    // The sit-down gate — the client's own, inside the ONE setter `0x5ed430`
    // ([`state::stand_state_refused`], bug B155): a body the movement layer is already driving
    // cannot be seated, and **swimming is one of the driving states**, so the press is refused
    // for as long as we are in the water — and a body that reads dead is refused in EITHER
    // direction, which is the setter's own first guard (decision 1753). Silently, and before the packet — like the reference,
    // which returns from `SetStandState` without building `CMSG_STANDSTATECHANGE` at all.
    // Placed on the shared commit below rather than on the X key, so it covers the posture
    // emotes (`/sit`, `/sleep`, `/kneel`) in the same stroke — their own `Emotes.dbc` gate
    // does NOT carry the swim bit (`ui_chat::tests::the_posture_emotes_carry_no_swim_suppression_flag`).
    // The word is the live outbound one, a frame old — the same `[[this+0x118]+0x40]` the cast
    // gates read (decision 1056), so all three refusals can never disagree about "am I moving".
    if let Some(s) =
        request_stand.filter(|&s| state::stand_state_refused(reads_dead, player.move_flags(), s))
    {
        debug!(
            "stand state {s} refused (dead {reads_dead}, move flags {:#x} — the client's \
             `0x5ed430` gate)",
            player.move_flags()
        );
        move_trace::posture("REFUSED", s, stand_state, player.move_flags());
        request_stand = None;
    }
    if let Some(s) = request_stand.filter(|&s| s != stand_state) {
        move_trace::posture("commit", s, stand_state, player.move_flags());
        player.stand_pending = Some(s);
        let _ = net.0.send(ClientCommand::StandStateChange {
            state: u32::from(s),
        });
        // The sit-stow rider (the client's SetStandState → SetSheatheState(0, SNAP) —
        // wow-re `sheath-policy.md` §4): entering any stand-state ∉ {0 STAND, 2 SIT_CHAIR}
        // force-stows drawn weapons, through the anim layer's one setter.
        if s != 0 && s != 2 {
            if let Ok((e, _, _, _, drv, _, _, _, _, _, _)) = body.single() {
                if drv.and_then(|d| d.sheath_state()).unwrap_or(0) != 0 {
                    sheath.write(crate::creature_anim::SheathRequest {
                        entity: e,
                        state: 0,
                        ceremony: false,
                    });
                }
            }
        }
    }
    let stand_now = player.stand_pending.unwrap_or(stand_byte);
    // Sheath toggle (Z) — vanilla's draw/stow, through the anim layer's ONE setter
    // ([`crate::creature_anim::SheathRequest`], decision 0080): walk the *committed*
    // client-side state (the setter cache — attacking auto-draws and the anim reconcile
    // force-stows, which a local bool or the raw echo byte would drift from), commit + send
    // `CMSG_SETSHEATHED` there, and play the ceremony — the manual toggle is the ONLY path
    // in the whole client that plays it (`bInstant = 0` at the 4 ToggleSheath sites — wow-re
    // `sheath-policy.md`). No body model yet (no driver) drops the toggle, the client's own
    // refusal.
    if binds.fired(crate::bindings::cmd::TOGGLE_SHEATH) {
        if let Ok((e, _, _, _, Some(drv), store, engaged, _, _, wielded, _)) = body.single() {
            // The manual toggle's guard chain (decision 0080d) — the guards of the client's
            // 12-deep silent-refusal chain (`ToggleSheath` `0x5eb480`) whose states exist
            // today: dead · engaged in combat · not standing (`GetStandState() != 0` —
            // chairs block the toggle too, unlike the *stow rider's* {0, 2} exemption) ·
            // mid-ceremony (the 89/90 clip still playing) · MOUNTED (chain check 4,
            // `UNIT_FIELD_MOUNTDISPLAYID > 0` — wow-re `sheath-policy.md` §2, wired with
            // 0441's mounts). Stunned / channeling join when those states exist. A refused
            // press is simply dropped — no message, like the client.
            let dead = store.is_some_and(|s| s.0.unit_is_dead());
            let mounted = store.is_some_and(|s| s.0.unit_mount_display_id() != 0);
            let refused =
                dead || mounted || engaged || stand_now != 0 || drv.sheath_ceremony_active();
            if refused {
                debug!(
                    "sheath toggle refused (dead {dead}, mounted {mounted}, engaged {engaged}, \
                     stand {stand_now}, mid-ceremony {})",
                    drv.sheath_ceremony_active()
                );
            } else {
                // The cycle proper ([`crate::creature_anim::toggle_sheath_next`], byte-read):
                // melee → ranged → stowed, gated on what is actually worn — never a
                // two-state flip. `None` = the ref makes no call at all (nothing equipped).
                let w = wielded.copied().unwrap_or_default();
                let worn = (w.main.is_some() || w.off.is_some(), w.ranged.is_some());
                let next =
                    crate::creature_anim::toggle_sheath_next(drv.sheath_state().unwrap_or(0), worn);
                if let Some(state) = next {
                    sheath.write(crate::creature_anim::SheathRequest {
                        entity: e,
                        state,
                        ceremony: true,
                    });
                }
            }
        }
    }

    stand_now
}
