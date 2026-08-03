//! Ground-target casting — the targeting-cursor machine's **location half** (decision 0792,
//! closing B132: "ground-targeted AOE all Invalid target").
//!
//! The reference's targeting mode IS a nonzero flag_word (`IsTargeting 0x6e48a0`); this module
//! holds benilla's mirror of that state and the systems around it, each transcribing a
//! byte-verified piece (wow-re `wave-cast.md` + `cursor-system.md` §5 + the world-click path in
//! `world-click-targeting.md`, the 0792 dispatch's synthesis):
//!
//! - **Cursor** ([`drive_targeting_cursor`]): while targeting, the world classifier is
//!   pre-empted (the ref's dispatcher step 2 runs before any object resolve) — **Cast** when the
//!   hovered ground point passes the range gate, **UnableCast** otherwise (`0x4820f0`'s split,
//!   computed by `CheckGroundPointInRange 0x6e6810` over `GetMinMaxRange`).
//! - **Commit** ([`commit_ground_cast_on_click`]): the terrain leg's action-1 arm tries the
//!   ground commit first, gated only on the word (`0x492580` → `BindLocation 0x6e60f0`): bind
//!   the clicked point, and with the word cleared, send — mask `0x40` + the point in WoW coords
//!   (`SendCast 0x6e54f0`), arming the pending cast + the GCD exactly like the unit send.
//! - **The ESC chain** ([`feed_targeting_to_vm`] / [`drain_stop_targeting`]): the real
//!   `UIParent.lua:1490` rung (`elseif ( SpellStopTargeting() ) then`) runs in our live VM; the
//!   feed pushes the state its `SpellIsTargeting`/`SpellStopTargeting` bindings read, the drain
//!   commits the cancel. AbortCast in targeting mode clears the word and sends **nothing**.
//!
//! Entry and the two press-cancel shapes live in the cast path itself: the resolver yields
//! [`super::cast_target::CastWireTarget::GroundTargeting`] (arm 16 / the bare DEST word), the
//! one cast-send path enters the mode here, a NEW spell's press aborts-and-proceeds (`TryCast
//! 6e4d62`), and the action bar's re-press of the SAME spell toggles the mode off (`UseAction
//! 0x4e5ee0`'s `GetTargetingSpellId`+`StopTargeting` — [`super::drain`]).
//!
//! The click path is byte-pinned by wow-re's `world-click-targeting.md` (the 0792 dispatch's
//! answers): the terrain-leg commit `0x492580` has **no range gate and no error path** — it
//! binds and sends regardless, and the server judges range (`CheckGroundPointInRange 0x6e6810`
//! has exactly ONE caller binary-wide, the hover classifier `0x4820f0`: its verdict colours the
//! cursor and nothing else). While targeting, the pick flags come from the pending spell's mask
//! alone — for a dest-only word a unit is not pickable, so a click over one commits on the
//! ground behind it ([`crate::target::click::select_on_click`]'s gate transcribes the
//! unreachable select). Right-click cancels on the DOWN edge
//! ([`cancel_targeting_on_right_press`]); movement never cancels (`0x515090`'s explicit
//! IsTargeting-skip). The ground reticle draws in [`crate::target`]'s `reticle` module
//! (decision 0797) off [`ground_cast_radius`] + the cursor's range verdict.

use std::time::Instant;

use bevy::prelude::*;

use benilla_assets::coords::bevy_to_wow;
use benilla_formats::SpellRange;

use crate::interact::{WorldClick, WorldRightPress};
use crate::net::{ClientCommand, NetCommands, SelfPlayer};
use crate::target::{CursorKind, PickOcclusion, WorldCursor};

use super::Spells;

/// The targeting-cursor mode — benilla's `flag_word != 0` mirror for the location half: `Some`
/// while a ground-targeted cast awaits its world click. Entered by the one cast-send path
/// ([`super::send_spell_cast`]'s `GroundTargeting` arm), cleared by the commit, the two press
/// cancels, and the ESC drain.
#[derive(Resource, Default)]
pub(crate) struct SpellTargeting(Option<GroundTargeting>);

struct GroundTargeting {
    spell_id: u32,
    /// What the world click will commit. The ref keeps the whole pending-cast block across the
    /// cursor — the cast **item's** guid at `0xceac48` included — so `0x6e54f0`'s discriminator
    /// still picks `CMSG_USE_ITEM` when the click lands: a thrown grenade, a stick of dynamite, a
    /// Goblin Mortar (decision 0914; 46 of the 1.12 on-use item spells arm this cursor).
    commit: super::cast_send::CastCommit,
}

impl SpellTargeting {
    /// `IsTargeting 0x6e48a0` — the canonical predicate.
    pub(crate) fn active(&self) -> bool {
        self.0.is_some()
    }

    /// `GetTargetingSpellId 0x6e48e0` — the spell awaiting its click, for the action bar's
    /// press-again toggle.
    pub(crate) fn spell(&self) -> Option<u32> {
        self.0.as_ref().map(|t| t.spell_id)
    }

    pub(crate) fn enter(&mut self, spell_id: u32, commit: super::cast_send::CastCommit) {
        self.0 = Some(GroundTargeting { spell_id, commit });
    }

    /// What the pending ground cast will commit as when the click lands.
    fn commit(&self) -> Option<super::cast_send::CastCommit> {
        self.0.as_ref().map(|t| t.commit)
    }

    pub(crate) fn clear(&mut self) {
        self.0 = None;
    }
}

/// `CheckGroundPointInRange 0x6e6810` — min²/max² from the spell's `SpellRange` row against the
/// squared caster↔point distance. Its ONE caller binary-wide is the hover-cursor classifier
/// (`0x4820f0` — wow-re `world-click-targeting.md` Q1's caller census): the verdict colours
/// Cast/UnableCast and nothing else. The click never consults it, so neither does ours. No row
/// (a failed DBC, an unknown spell) is permissive — the server validates every send anyway.
fn ground_point_in_range(row: Option<&SpellRange>, self_pos: Vec3, point: Vec3) -> bool {
    let Some(row) = row else {
        return true;
    };
    let dist_sq = self_pos.distance_squared(point);
    if row.min > 0.0 && dist_sq < row.min * row.min {
        return false;
    }
    dist_sq <= row.max * row.max
}

/// The targeting spell's `SpellRange` row, through the catalogs.
fn range_row(spells: Option<&Spells>, spell_id: u32) -> Option<&SpellRange> {
    let spells = spells?;
    spells.ranges.get(spells.catalog.get(spell_id)?.range_index)
}

/// `GetCurrentCastRadius 0x6e6350` (wow-re `ground-target-reticle.md` B2) — the reticle's
/// radius: per-effect `radius + casterLevel × perLevel` over **EffectRadiusIndex[0] and [1]
/// only** (slot 2 is never read by the client), the max with candidate 1 winning ties/NaN,
/// clamped to 20.0 (`0x4820f0`'s `[0x804478]` literal — `min`, NaN → 20). `0.0` = no radius
/// rows; the reticle then draws at its literal default size. Class-6 spell modifiers are
/// unmodelled (the 0792 residual, same as the range gate).
pub(crate) fn ground_cast_radius(spells: Option<&Spells>, spell_id: u32, level: u32) -> f32 {
    let Some(spells) = spells else { return 0.0 };
    let Some(d) = spells.catalog.get(spell_id) else {
        return 0.0;
    };
    let candidate = |slot: usize| -> f32 {
        let idx = d.effect_radius_index[slot];
        if idx == 0 {
            return 0.0;
        }
        spells
            .radii
            .get(idx)
            .map_or(0.0, |r| r.radius + level as f32 * r.per_level)
    };
    let (c0, c1) = (candidate(0), candidate(1));
    // Strict > for candidate 0; a tie or a NaN c0 falls to candidate 1 — the byte order.
    let r = if c0 > c1 { c0 } else { c1 };
    r.min(20.0)
}

/// While targeting, the world cursor is the classifier's pre-empt (`0x4820f0`, cursor-system
/// §5): **Cast** over an in-range ground point, **UnableCast** out of range / too close / no
/// ground hit at all (sky, mouselook). Runs right after [`crate::target`]'s classifier in the
/// target chain and overwrites its verdict — the ref runs this branch before the object
/// classifier ever executes, and the visible result is identical.
pub(crate) fn drive_targeting_cursor(
    targeting: Res<SpellTargeting>,
    occlusion: Res<PickOcclusion>,
    spells: Option<Res<Spells>>,
    self_tf: Query<&Transform, With<SelfPlayer>>,
    mut cursor: ResMut<WorldCursor>,
) {
    let Some(spell_id) = targeting.spell() else {
        return;
    };
    let in_range = match (occlusion.point, self_tf.single()) {
        (Some(point), Ok(tf)) => ground_point_in_range(
            range_row(spells.as_deref(), spell_id),
            tf.translation,
            point,
        ),
        _ => false,
    };
    *cursor = WorldCursor {
        kind: CursorKind::Cast,
        unable: !in_range,
    };
}

/// The world click's ground commit — the terrain leg's action-1 arm (`0x492580`, tried before
/// anything else the click could mean; [`crate::target::click::select_on_click`] holds its gate
/// while this mode is active, so the click neither selects nor deselects). Binds the frame's
/// pick-occlusion point and sends **unconditionally** — the leg's complete callee set has no
/// range check and no error path (wow-re `world-click-targeting.md` Q1; C2 REFUTED: the click
/// never gates on range, the server judges it, and its refusing `SMSG_CAST_RESULT` is the red
/// line) — `CMSG_CAST_SPELL` mask `0x40` + the point (WoW coords), arming the pending cast +
/// the GCD (the `SendCast 0x6e54f0` tail's two live pieces for a ground cast); the mode ends
/// with the send. No world hit (sky) → the nothing leg: no commit, mode kept.
///
/// Runs AFTER `select_on_click` in the target chain: the selection gate reads the mode's state,
/// so the commit that clears it must come later in the same frame.
pub(crate) fn commit_ground_cast_on_click(
    mut clicks: MessageReader<WorldClick>,
    mut targeting: ResMut<SpellTargeting>,
    occlusion: Res<PickOcclusion>,
    spells: Option<Res<Spells>>,
    net: Res<NetCommands>,
    mut pending: ResMut<crate::ui_cast::PendingCast>,
    mut cooldowns: ResMut<crate::cooldowns::Cooldowns>,
) {
    if !targeting.active() {
        // Keep the reader current so a click buffered while idle can never replay as a commit
        // the frame the mode turns on.
        clicks.clear();
        return;
    }
    if clicks.read().last().is_none() {
        return;
    }
    let (Some(spell_id), Some(commit)) = (targeting.spell(), targeting.commit()) else {
        return;
    };
    let Some(point) = occlusion.point else {
        // The ray hit nothing (sky) — the ref's nothing-leg has no ground commit; the mode
        // stays, exactly like the UnableCast cursor said it would.
        return;
    };
    let dest = bevy_to_wow(point);
    debug!(
        "ui_action: ground cast {spell_id} committed at wow ({:.2}, {:.2}, {:.2})",
        dest[0], dest[1], dest[2]
    );
    // Same block, two opcodes — `SendCast 0x6e54f0`'s one discriminator survives the cursor
    // (decision 0914): a thrown grenade commits as `CMSG_USE_ITEM` with the DEST block.
    let _ = net.0.send(match commit {
        super::cast_send::CastCommit::Spell => ClientCommand::CastSpellAtDest { spell_id, dest },
        super::cast_send::CastCommit::Item {
            bag_index,
            slot,
            spell_index,
            ..
        } => ClientCommand::UseItem {
            bag_index,
            slot,
            spell_index,
            target: benilla_protocol::messages::UseItemTarget::Dest(dest),
        },
    });
    let now = Instant::now();
    if commit.is_item() {
        pending.arm_item(spell_id, now);
    } else {
        pending.arm(spell_id, now);
    }
    if let Some(d) = spells.as_ref().and_then(|s| s.catalog.get(spell_id)) {
        cooldowns.start_gcd(spell_id, d, now);
    }
    targeting.clear();
}

/// Right-click cancels targeting — on the **DOWN edge**, the reference's WorldFrame
/// `OnMouseDown 0x483c40` → `0x492c20`: right button ∧ `IsTargeting` → `StopTargeting
/// 0x6e4900`, no packet — and the handler returns 0, so the press keeps doing everything else
/// it did (the turn-drag, the release's context click; we consume nothing either). Byte-pinned
/// by wow-re `world-click-targeting.md` Q3, whose caller census is complete: this and the
/// ESC/UseAction/TryCast paths are the ONLY input-band cancels — no keyboard caller exists.
///
/// Two qualifications, transcribed: a held cursor payload pre-empts the cancel (`0x492b50`
/// clears the payload and returns before the WorldFrame virtuals dispatch — our payload keeps
/// its own clean-click clear in [`crate::target::click::world_right_click_payload`]); and a
/// press over a UI frame never reaches the WorldFrame — [`WorldRightPress`]'s world gate
/// transcribes the certain half of wow-re's one DEFERRED (whether a UI-frame right-click also
/// cancels is unpinned there). The `0x51`-effect placement-rotate skip (`[0xceca90]`) is
/// unmodelled along with the flag itself (named residual, 0792).
pub(crate) fn cancel_targeting_on_right_press(
    mut presses: MessageReader<WorldRightPress>,
    payload_held: Res<crate::ui_script::CursorPayloadHeld>,
    mut targeting: ResMut<SpellTargeting>,
) {
    if !targeting.active() {
        // Reader hygiene, like the commit's: a press buffered while idle never replays as a
        // cancel the frame the mode turns on.
        presses.clear();
        return;
    }
    if presses.read().last().is_none() || payload_held.0 {
        return;
    }
    debug!("ui_action: targeting cancelled (right-click)");
    targeting.clear();
}

/// Push the targeting state into the live VM each frame, **before** the input pass runs the ESC
/// chain — what `SpellIsTargeting()` reads and `SpellStopTargeting()` gates on.
pub(crate) fn feed_targeting_to_vm(
    targeting: Res<SpellTargeting>,
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
) {
    if let Some(mut script) = script {
        script.set_spell_targeting(targeting.active());
    }
}

/// Drain the ESC chain's `SpellStopTargeting()` trigger (**after** the input pass) and clear
/// the mode — the ref's `StopTargeting 0x6e4900` → AbortCast-in-targeting: word cleared, no
/// packet.
pub(crate) fn drain_stop_targeting(
    mut targeting: ResMut<SpellTargeting>,
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
) {
    let Some(mut script) = script else {
        return;
    };
    if script.take_stop_targeting() {
        debug!("ui_action: targeting cancelled (ESC chain)");
        targeting.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `0x6e6810` mirror: min²/max² against the squared caster↔point distance — the
    /// CURSOR's verdict and nothing else (its one caller binary-wide is the hover classifier;
    /// the click never asks). Permissive with no row (Blizzard's row 4 is 0–30 yd; a synthetic
    /// min exercises the too-close arm the real row can't).
    #[test]
    fn ground_point_in_range_mirrors_check_ground_point_in_range() {
        let row = |min: f32, max: f32| SpellRange { min, max, flags: 0 };
        let origin = Vec3::ZERO;
        let at = |d: f32| Vec3::new(d, 0.0, 0.0);
        let blizzard = row(0.0, 30.0);
        assert!(ground_point_in_range(Some(&blizzard), origin, at(29.9)));
        assert!(!ground_point_in_range(Some(&blizzard), origin, at(30.1)));
        let banded = row(8.0, 35.0);
        assert!(!ground_point_in_range(Some(&banded), origin, at(5.0)));
        assert!(ground_point_in_range(Some(&banded), origin, at(20.0)));
        // No row → permissive (the server still validates).
        assert!(ground_point_in_range(None, origin, at(500.0)));
    }

    /// `GetCurrentCastRadius 0x6e6350` + the `0x4820f0` clamp: slots 0/1 only (slot 2 is never
    /// read), max with candidate-1 winning ties, per-level scaling, min(r, 20). Fixture rows
    /// mirror the real table (row 14 = 8.0 Blizzard, row 8 = 5.0 Flamestrike).
    #[test]
    fn ground_cast_radius_mirrors_get_current_cast_radius() {
        use benilla_formats::{SpellDisplay, SpellRadius};
        use std::collections::HashMap;
        let mut spells = super::super::Spells::empty_for_tests();
        let display = |idx: [u32; 3]| SpellDisplay {
            effect_radius_index: idx,
            ..SpellDisplay::default()
        };
        spells.catalog = benilla_formats::SpellCatalog::from_displays(HashMap::from([
            (10, display([14, 0, 0])),
            (2120, display([8, 8, 0])),
            (777, display([0, 0, 13])), // slot 2 only — the client never reads it
            (778, display([90, 8, 0])), // per-level row in slot 0
            (779, display([10, 0, 0])), // row 10 = 30.0 — the 20.0 clamp
        ]));
        spells.radii = benilla_formats::SpellRadiusCatalog::from_rows(HashMap::from([
            (
                14,
                SpellRadius {
                    radius: 8.0,
                    per_level: 0.0,
                    max: 0.0,
                },
            ),
            (
                8,
                SpellRadius {
                    radius: 5.0,
                    per_level: 0.0,
                    max: 0.0,
                },
            ),
            (
                13,
                SpellRadius {
                    radius: 10.0,
                    per_level: 0.0,
                    max: 0.0,
                },
            ),
            (
                10,
                SpellRadius {
                    radius: 30.0,
                    per_level: 0.0,
                    max: 0.0,
                },
            ),
            (
                90,
                SpellRadius {
                    radius: 2.0,
                    per_level: 0.1,
                    max: 0.0,
                },
            ),
        ]));
        let s = Some(&spells);
        assert_eq!(ground_cast_radius(s, 10, 60), 8.0);
        assert_eq!(ground_cast_radius(s, 2120, 60), 5.0);
        // Slot 2 is invisible to the reticle — no rows in 0/1 reads 0 (→ the default size).
        assert_eq!(ground_cast_radius(s, 777, 60), 0.0);
        // Per-level: 2.0 + 60 × 0.1 = 8.0 beats slot 1's 5.0.
        assert_eq!(ground_cast_radius(s, 778, 60), 8.0);
        // The 20.0 clamp (`[0x804478]`).
        assert_eq!(ground_cast_radius(s, 779, 60), 20.0);
        // Unknown spell / no data at all → 0 (default size).
        assert_eq!(ground_cast_radius(s, 9999, 60), 0.0);
        assert_eq!(ground_cast_radius(None, 10, 60), 0.0);
    }
}
