//! The player's cooldown store — a mirror of the client's `SpellHistory` list (decision 0137
//! phase 4). Every law here is the byte-verified mechanism from wow-re `wave-cooldown.md` /
//! `wave-handlers.md` (the SPELLHISTORY node ops `0x6e12c0`/`0x6e13e0`/`0x6e1630`/`0x6e1790`,
//! `StartCooldown 0x6e2c60`, `StartGlobalCooldown 0x6e2de0`, and the SMSG handlers
//! `0x6e9460`/`0x6e95d0`/`0x6e9670`/`0x6e9730`), transcribed onto `Instant`/`Duration`:
//!
//! - A **record** carries three independent timer pairs, exactly the SPELLHISTORY fields: the
//!   spell's own recovery, its category's shared recovery, and the global-cooldown pair
//!   (`startRecoveryCategory`/`startRecoveryTime`). `on_hold` parks the first two until
//!   `SMSG_COOLDOWN_EVENT` starts them (`SPELL_ATTR_COOLDOWN_ON_EVENT` — Stealth, Feign Death).
//! - The **read** ([`Cooldowns::info`], the client's `GetCooldownInfo 0x6e13e0`) resolves a
//!   queried spell against all three: nodes matching its id (+ cast item), nodes matching its
//!   category, and nodes whose GCD category matches its `startRecoveryCategory` — the mechanism
//!   that spreads one cast's GCD onto every other button. The longest remaining wins.
//! - **Who starts what** (byte-VERIFIED, the 2026-07-10 wow-re §5 + follow-up,
//!   `action-button-state-api.md` §7 / `wave-handlers.md` ADDENDUM): the GCD starts locally at
//!   cast-send (`0x6e58fb`); the spell's own recovery is client-computed from `Spell.dbc` and
//!   inserted when **our own `SMSG_SPELL_GO`** arrives (`HandleSpellGo`'s self-insert tail
//!   `0x6e8498`/`0x6e8566`, anchored at the receive-time, onHold from Attributes bit 25);
//!   `SMSG_SPELL_COOLDOWN` is the server *override/refresh* path (school lockouts, pet lists) —
//!   vmangos sends no packet for a plain cast's cooldown. A failed cast (`SMSG_CAST_RESULT`)
//!   never reached its GO, so the fail path clears only the GCD (`0x6e1d83 → 0x6e1630`).
//!
//! The store is generation-counted: every mutation bumps [`Cooldowns::generation`], and the UI
//! feed fires `ACTIONBAR_UPDATE_COOLDOWN` on the change — natural *expiry* bumps nothing (the
//! widget animates itself from `(start, duration)` and hides at the end, the reference
//! `Cooldown.lua` machine).

use std::time::{Duration, Instant};

use bevy::prelude::*;

use benilla_formats::SpellDisplay;
use benilla_protocol::messages::ItemUseSpell;

/// One timer pair: when it started and how long it runs. Zero-duration = not tracked.
#[derive(Clone, Copy, Debug)]
struct Timer {
    start: Instant,
    duration: Duration,
}

impl Timer {
    fn none(now: Instant) -> Self {
        Self {
            start: now,
            duration: Duration::ZERO,
        }
    }

    fn remaining(&self, now: Instant) -> Duration {
        (self.start + self.duration).saturating_duration_since(now)
    }
}

/// One SPELLHISTORY record (wow-re `wave-cooldown.md` `0x6e12c0`'s node, byte-for-byte in
/// spirit: spellID/itemID/recovery pair/category+pair/onHold/GCD pair).
#[derive(Clone, Debug)]
struct Record {
    spell_id: u32,
    /// The cast item's template entry (`0` = a plain spell record) — item-use cooldowns key on
    /// the pair, the client's `[eax+8]==spellId && [eax+0xc]==itemID` match.
    item_id: u32,
    recovery: Timer,
    category: u32,
    category_recovery: Timer,
    /// Parked until `SMSG_COOLDOWN_EVENT` (`SPELL_ATTR_COOLDOWN_ON_EVENT`): the recovery pairs
    /// hold their *durations* but their clocks haven't started.
    on_hold: bool,
    gcd_category: u32,
    gcd: Timer,
}

/// What one queried action's cooldown reads (`GetActionCooldown`'s triple, app-side): the
/// winning timer's **absolute start** + full duration + time remaining, and whether it is
/// actually running (`enabled == false` = an on-hold record — the reference API's `enable == 0`,
/// which the `CooldownFrame_SetTimer` law hides). Carrying the start is the reference's own
/// convention (`GetCooldownInfo 0x6e13e0` returns the record's start, never a "remaining"), and
/// it is what makes the read re-arm-proof: two arms of a same-length cooldown can never alias,
/// because their starts differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CooldownInfo {
    /// When the winning timer started (for an on-hold record: when it was inserted).
    pub start: Instant,
    pub remaining_ms: u32,
    pub duration_ms: u32,
    pub enabled: bool,
}

impl CooldownInfo {
    /// The pushable UI triple `(start_ms on the GetTime clock, duration_ms, enabled)`, or `None`
    /// when cold. `now`/`ui_now` are the same frame's readings of the two clocks (`Instant` vs
    /// the VM's `GetTime`), so the subtraction maps the start across without either side knowing
    /// the other's epoch. Rounded to whole ms so the value is frame-stable: a re-push of the same
    /// running cooldown re-derives the same number, while a RE-ARM always derives a new one — the
    /// property the old `(remaining, duration)` shape lacked (two arms of the same cooldown read
    /// byte-identical and the seam kept the first, elapsed, anchor: the vanished-GCD-pie bug).
    pub(crate) fn ui_triple(&self, now: Instant, ui_now: f64) -> Option<(i64, u32, bool)> {
        (self.remaining_ms > 0).then(|| {
            let start = ui_now - now.saturating_duration_since(self.start).as_secs_f64();
            #[allow(clippy::cast_possible_truncation)] // session-clock ms fit i64
            (
                (start * 1000.0).round() as i64,
                self.duration_ms,
                self.enabled,
            )
        })
    }
}

/// The player's cooldown list (the client's self `SpellHistory` @0xcecaec; the pet list has no
/// benilla consumer). One resource, written by the net bridge + the cast-send path, read by the
/// action-bar feed.
#[derive(Resource, Default)]
pub(crate) struct Cooldowns {
    records: Vec<Record>,
    /// Bumped on every mutation — the UI feed's `ACTIONBAR_UPDATE_COOLDOWN` edge.
    pub(crate) generation: u64,
}

impl Cooldowns {
    fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// The insert/refresh primitive (`AddCooldown 0x6e12c0`): nothing to track → no-op; an
    /// existing `(spell, item)` record is refreshed in place, else a fresh one is appended.
    #[allow(clippy::too_many_arguments)] // the SPELLHISTORY node's own field list
    fn add(
        &mut self,
        spell_id: u32,
        item_id: u32,
        recovery: Timer,
        category: u32,
        category_recovery: Timer,
        on_hold: bool,
        gcd_category: u32,
        gcd: Timer,
    ) {
        // The client's early-out: no recovery, no category recovery, not on hold, no GCD —
        // nothing to track (6e12c3).
        if recovery.duration.is_zero()
            && category_recovery.duration.is_zero()
            && !on_hold
            && gcd.duration.is_zero()
        {
            return;
        }
        let node = Record {
            spell_id,
            item_id,
            recovery,
            category,
            category_recovery,
            on_hold,
            gcd_category,
            gcd,
        };
        match self
            .records
            .iter_mut()
            .find(|r| r.spell_id == spell_id && r.item_id == item_id)
        {
            Some(existing) => *existing = node,
            None => self.records.push(node),
        }
        self.bump();
    }

    /// Prune records with nothing left to say: every timer elapsed and not on hold. Behaviorally
    /// invisible (an elapsed record contributes zero remaining) — bounds the list without the
    /// client's event-driven sweep sites.
    pub(crate) fn prune(&mut self, now: Instant) {
        self.records.retain(|r| {
            r.on_hold
                || !r.recovery.remaining(now).is_zero()
                || !r.category_recovery.remaining(now).is_zero()
                || !r.gcd.remaining(now).is_zero()
        });
    }

    /// Start a spell's own cooldown (`StartCooldown 0x6e2c60`, spell-only path): recovery /
    /// category from `Spell.dbc`, on-hold when the spell is `SPELL_ATTR_COOLDOWN_ON_EVENT`.
    /// No GCD here — that's [`Self::start_gcd`]'s separate insert.
    ///
    /// `ranged_attack_time_ms` is the ranged-shot pad (the category scaler `0x6e2b60`'s
    /// `add [categoryRecoveryTime], [player+0x110]+0x1e8`, byte-verified — wow-re
    /// `ranged-cooldown-sweep.md`, decision 0378): the caster's live `UNIT_FIELD_RANGEDATTACKTIME`
    /// when [`SpellDisplay::ranged_speed_cooldown`], else 0. It folds into the CATEGORY timer —
    /// the Throw/wand-Shoot sweep with all-zero DBC recovery — and rides the insert even for
    /// category 0 (Auto Shot), where no read surfaces it (the client's SpellCategory[0]-is-NULL
    /// asymmetry: Auto Shot never sweeps).
    pub(crate) fn start_spell(
        &mut self,
        spell_id: u32,
        spell: &SpellDisplay,
        ranged_attack_time_ms: u32,
        now: Instant,
    ) {
        self.add(
            spell_id,
            0,
            Timer {
                start: now,
                duration: Duration::from_millis(u64::from(spell.recovery_ms)),
            },
            spell.category,
            Timer {
                start: now,
                duration: Duration::from_millis(u64::from(
                    spell.category_recovery_ms + ranged_attack_time_ms,
                )),
            },
            spell.cooldown_on_event(),
            0,
            Timer::none(now),
        );
    }

    /// Start an item use's cooldown (`StartCooldown 0x6e2c60` with an item record): the wire's
    /// server-resolved triple, each negative falling back to the spell's own `Spell.dbc` value
    /// (the client's `>= 0` pick on the item slots).
    pub(crate) fn start_item(
        &mut self,
        item_entry: u32,
        use_spell: &ItemUseSpell,
        spell: Option<&SpellDisplay>,
        now: Instant,
    ) {
        let recovery_ms = if use_spell.cooldown_ms >= 0 {
            use_spell.cooldown_ms as u32
        } else {
            spell.map_or(0, |s| s.recovery_ms)
        };
        let category_ms = if use_spell.category_cooldown_ms >= 0 {
            use_spell.category_cooldown_ms as u32
        } else {
            spell.map_or(0, |s| s.category_recovery_ms)
        };
        self.add(
            use_spell.spell_id,
            item_entry,
            Timer {
                start: now,
                duration: Duration::from_millis(u64::from(recovery_ms)),
            },
            use_spell.category,
            Timer {
                start: now,
                duration: Duration::from_millis(u64::from(category_ms)),
            },
            spell.is_some_and(|s| s.cooldown_on_event()),
            0,
            Timer::none(now),
        );
    }

    /// Arm the global cooldown at cast-send (`StartGlobalCooldown 0x6e2de0` ← the cast-send arm
    /// `0x6e58fb`): the spell's `startRecoveryCategory`/`startRecoveryTime` pair; both zero →
    /// nothing (Attack, Auto Shot, wand Shoot carry no GCD).
    pub(crate) fn start_gcd(&mut self, spell_id: u32, spell: &SpellDisplay, now: Instant) {
        if spell.start_recovery_category == 0 && spell.start_recovery_ms == 0 {
            return;
        }
        self.add(
            spell_id,
            0,
            Timer::none(now),
            0,
            Timer::none(now),
            false,
            spell.start_recovery_category,
            Timer {
                start: now,
                duration: Duration::from_millis(u64::from(spell.start_recovery_ms)),
            },
        );
    }

    /// Clear only the GCD fields of a spell's record(s) (`0x6e1630` — the cast-fail path): a
    /// rejected cast opens the global cooldown again immediately.
    pub(crate) fn clear_gcd(&mut self, spell_id: u32, now: Instant) {
        let mut touched = false;
        for r in &mut self.records {
            if r.spell_id == spell_id && !r.gcd.duration.is_zero() {
                r.gcd = Timer::none(now);
                touched = true;
            }
        }
        if touched {
            self.prune(now);
            self.bump();
        }
    }

    /// `SMSG_COOLDOWN_EVENT` (`0x6e1790`, force=0): an on-hold record's parked timers start
    /// **now**; a running record is left alone.
    pub(crate) fn cooldown_event(&mut self, spell_id: u32, now: Instant) {
        let mut touched = false;
        for r in &mut self.records {
            if r.spell_id == spell_id && r.on_hold {
                r.recovery.start = now;
                r.category_recovery.start = now;
                r.on_hold = false;
                touched = true;
            }
        }
        if touched {
            self.bump();
        }
    }

    /// `SMSG_CLEAR_COOLDOWN` / the cast-fail revert (`0x6e1790`, force=1): remove the spell's
    /// record(s) outright.
    pub(crate) fn clear_spell(&mut self, spell_id: u32) {
        let before = self.records.len();
        self.records.retain(|r| r.spell_id != spell_id);
        if self.records.len() != before {
            self.bump();
        }
    }

    /// `SMSG_COOLDOWN_CHEAT` (`0x6e9700`): drain the whole list.
    pub(crate) fn wipe(&mut self) {
        if !self.records.is_empty() {
            self.records.clear();
            self.bump();
        }
    }

    /// One `SMSG_SPELL_COOLDOWN` pair (`0x6e9460`'s per-entry law): a nonzero wire duration is
    /// the spell recovery verbatim (category untracked); zero means "the spell's own Spell.dbc
    /// recovery + category recovery". `SPELL_ATTR_COOLDOWN_ON_EVENT` parks it and suppresses the
    /// GCD pair; otherwise the spell's GCD pair rides along.
    pub(crate) fn apply_wire_cooldown(
        &mut self,
        spell_id: u32,
        cooldown_ms: u32,
        spell: Option<&SpellDisplay>,
        now: Instant,
    ) {
        let on_hold = spell.is_some_and(|s| s.cooldown_on_event());
        let (recovery_ms, category, category_ms) = if cooldown_ms != 0 {
            (cooldown_ms, spell.map_or(0, |s| s.category), 0)
        } else {
            match spell {
                Some(s) => (s.recovery_ms, s.category, s.category_recovery_ms),
                None => (0, 0, 0),
            }
        };
        let (gcd_category, gcd_ms) = if on_hold {
            (0, 0)
        } else {
            match spell {
                Some(s) => (s.start_recovery_category, s.start_recovery_ms),
                None => (0, 0),
            }
        };
        self.add(
            spell_id,
            0,
            Timer {
                start: now,
                duration: Duration::from_millis(u64::from(recovery_ms)),
            },
            category,
            Timer {
                start: now,
                duration: Duration::from_millis(u64::from(category_ms)),
            },
            on_hold,
            gcd_category,
            Timer {
                start: now,
                duration: Duration::from_millis(u64::from(gcd_ms)),
            },
        );
    }

    /// `SMSG_ITEM_COOLDOWN` (`0x6e95d0`): the fixed 30 000 ms use cooldown on the item's on-use
    /// spell — the 30 s is the client's hardcode, nothing else rides the wire.
    pub(crate) fn apply_wire_item_cooldown(
        &mut self,
        item_entry: u32,
        spell_id: u32,
        now: Instant,
    ) {
        self.add(
            spell_id,
            item_entry,
            Timer {
                start: now,
                duration: Duration::from_millis(30_000),
            },
            0,
            Timer::none(now),
            false,
            0,
            Timer::none(now),
        );
    }

    /// One `SMSG_INITIAL_SPELLS` cooldown entry: the wire carries **remaining** ms (vmangos
    /// computes them at send), so the record starts now and runs that remainder — the client
    /// can't know the original start either. A *permanent* cooldown (`spell_cd_ms == 1`, the
    /// category word's top bit) re-arms server-side; its 1 ms is carried verbatim (harmless — the
    /// server refuses the cast regardless).
    pub(crate) fn seed_initial(
        &mut self,
        cd: &benilla_protocol::messages::SpellCooldown,
        now: Instant,
    ) {
        self.add(
            u32::from(cd.spell_id),
            u32::from(cd.item_id),
            Timer {
                start: now,
                duration: Duration::from_millis(u64::from(cd.spell_cd_ms)),
            },
            u32::from(cd.category & 0x7FFF),
            Timer {
                start: now,
                duration: Duration::from_millis(u64::from(cd.category_cd_ms)),
            },
            false,
            0,
            Timer::none(now),
        );
    }

    /// Whether the spell (or its category) is on a *tracked* cooldown — the client's
    /// `IsSpellOnCooldown 0x6e1690`, the local not-ready refusal at cast-send. Faithfully
    /// **excludes** the GCD fields (the byte law tests only `duration != 0 || onHold` on the
    /// spell/category pairs): a pure-GCD lock does not read as "not ready".
    pub(crate) fn is_on_cooldown(
        &self,
        spell_id: u32,
        spell: Option<&SpellDisplay>,
        now: Instant,
    ) -> bool {
        let category = spell.map_or(0, |s| s.category);
        self.records.iter().any(|r| {
            // The id match reads the spell timer + hold only; a record's CATEGORY timer is
            // reachable solely through the category resolve below — the client's own shape
            // (`GetCooldownInfo 0x6e13e0` surfaces the category pair only on the category
            // compare at `0x6e155e`, and `SpellCategory[0]` is NULL). Concretely: Auto Shot's
            // ranged-speed pad (decision 0378) sits on its category-0 record, invisible — a
            // re-press is never locally refused, matching the ref.
            (r.spell_id == spell_id && (!r.recovery.remaining(now).is_zero() || r.on_hold))
                || (category != 0
                    && r.category == category
                    && !r.category_recovery.remaining(now).is_zero()
                    && !r.on_hold)
        })
    }

    /// Whether a **GCD-carrying** spell is locked by a running global cooldown — its
    /// `startRecoveryCategory` has remaining time on some record's GCD pair. The send seam's
    /// second refusal leg (after [`Self::is_on_cooldown`], which byte-faithfully EXCLUDES the
    /// GCD): a locked press refuses locally with "not ready" and never wires a cast — sending
    /// instead draws the server's NOT_READY fail (vmangos `Spell.cpp:5387` enforces the GCD),
    /// whose faithful revert [`Self::clear_gcd`] then wipes the RUNNING GCD — the spam-press
    /// vanished-pie bug. GCD-free spells (`startRecoveryTime == 0` — Attack, Auto Shot, Heroic
    /// Strike) are never locked: they queue/fire during the GCD, the vanilla feel. INTERIM: the
    /// ref's exact refusal site for a GCD-locked press is unpinned in wow-re (`0x6e1690` skips
    /// the GCD fields, yet the ref demonstrably refuses locally and never casts during the GCD);
    /// the observable this reproduces is the reference's.
    pub(crate) fn gcd_locked(&self, spell: &SpellDisplay, now: Instant) -> bool {
        spell.start_recovery_ms > 0
            && spell.start_recovery_category != 0
            && self.records.iter().any(|r| {
                r.gcd_category == spell.start_recovery_category && !r.gcd.remaining(now).is_zero()
            })
    }

    /// The per-spell read (`GetCooldownInfo 0x6e13e0`): resolve `spell_id` (as cast from
    /// `item_entry`, `0` for a plain spell) against every record — id (+item) match takes the
    /// spell pair, category match the category pair, and a GCD-category match the GCD pair (how
    /// one cast's GCD reaches every button sharing `startRecoveryCategory`). Longest remaining
    /// wins; an on-hold winner reads `enabled == false` (the parked "cooldown hasn't begun").
    pub(crate) fn info(
        &self,
        spell_id: u32,
        item_entry: u32,
        spell: Option<&SpellDisplay>,
        now: Instant,
    ) -> CooldownInfo {
        let category = spell.map_or(0, |s| s.category);
        let start_recovery_category = spell.map_or(0, |s| s.start_recovery_category);
        let mut best = CooldownInfo {
            start: now,
            remaining_ms: 0,
            duration_ms: 0,
            enabled: true,
        };
        let mut consider = |timer: &Timer, remaining: Duration, enabled: bool| {
            let remaining_ms = remaining.as_millis().min(u128::from(u32::MAX)) as u32;
            if remaining_ms > best.remaining_ms {
                best = CooldownInfo {
                    start: timer.start,
                    remaining_ms,
                    duration_ms: timer.duration.as_millis().min(u128::from(u32::MAX)) as u32,
                    enabled,
                };
            }
        };
        for r in &self.records {
            if r.spell_id == spell_id && r.item_id == item_entry {
                if r.on_hold {
                    // Parked: full duration remaining, not running (enable == 0 hides the sweep).
                    consider(&r.recovery, r.recovery.duration, false);
                } else {
                    consider(&r.recovery, r.recovery.remaining(now), true);
                }
            }
            if category != 0 && r.category == category && !r.on_hold {
                consider(
                    &r.category_recovery,
                    r.category_recovery.remaining(now),
                    true,
                );
            }
            if start_recovery_category != 0 && r.gcd_category == start_recovery_category {
                consider(&r.gcd, r.gcd.remaining(now), true);
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spell(
        category: u32,
        recovery_ms: u32,
        category_recovery_ms: u32,
        gcd: (u32, u32),
        attributes: u32,
    ) -> SpellDisplay {
        SpellDisplay {
            category,
            recovery_ms,
            category_recovery_ms,
            start_recovery_category: gcd.0,
            start_recovery_ms: gcd.1,
            attributes,
            ..Default::default()
        }
    }

    /// Fireball-shaped: no own cooldown, the ordinary 133/1500 GCD.
    fn fireball() -> SpellDisplay {
        spell(0, 0, 0, (133, 1500), 0x10000)
    }

    /// Charge-shaped: category 44, 15 s category cooldown, NO GCD pair.
    fn charge() -> SpellDisplay {
        spell(44, 0, 15_000, (0, 0), 0)
    }

    #[test]
    fn the_gcd_spreads_to_every_spell_sharing_the_start_recovery_category() {
        let t0 = Instant::now();
        let mut cds = Cooldowns::default();
        cds.start_gcd(133, &fireball(), t0);

        // The cast spell itself and a DIFFERENT spell with the same startRecoveryCategory both
        // read the GCD; Charge (no GCD pair) reads nothing.
        let mid = t0 + Duration::from_millis(500);
        let fb = cds.info(133, 0, Some(&fireball()), mid);
        assert_eq!(
            (fb.remaining_ms, fb.duration_ms, fb.enabled),
            (1000, 1500, true)
        );
        let frostbolt = fireball(); // same shape, different id
        let other = cds.info(116, 0, Some(&frostbolt), mid);
        assert_eq!((other.remaining_ms, other.duration_ms), (1000, 1500));
        let ch = cds.info(100, 0, Some(&charge()), mid);
        assert_eq!(ch.remaining_ms, 0, "no startRecoveryCategory — no GCD read");

        // …and the GCD is NOT a "not ready" lock (0x6e1690 skips the GCD fields).
        assert!(!cds.is_on_cooldown(133, Some(&fireball()), mid));
    }

    #[test]
    fn a_failed_cast_clears_the_gcd_and_the_optimistic_recovery() {
        let t0 = Instant::now();
        let mut cds = Cooldowns::default();
        let fd = spell(0, 30_000, 0, (133, 1500), 0); // Feign-Death-shaped minus on-event
        cds.start_gcd(5384, &fd, t0);
        cds.start_spell(5384, &fd, 0, t0);
        let mid = t0 + Duration::from_millis(100);
        assert!(cds.is_on_cooldown(5384, Some(&fd), mid));
        assert!(cds.info(5384, 0, Some(&fd), mid).remaining_ms > 0);

        // The 0x6e1a00 fail path: GCD cleared (0x6e1630) + the record force-removed (0x6e3050).
        cds.clear_gcd(5384, mid);
        cds.clear_spell(5384);
        assert!(!cds.is_on_cooldown(5384, Some(&fd), mid));
        assert_eq!(cds.info(5384, 0, Some(&fd), mid).remaining_ms, 0);
    }

    #[test]
    fn category_cooldowns_reach_category_siblings_but_not_others() {
        let t0 = Instant::now();
        let mut cds = Cooldowns::default();
        cds.start_spell(100, &charge(), 0, t0); // Charge: category 44, 15 s

        let mid = t0 + Duration::from_secs(5);
        // A different spell in category 44 reads the shared remainder…
        let sibling = spell(44, 0, 15_000, (0, 0), 0);
        let s = cds.info(999, 0, Some(&sibling), mid);
        assert_eq!((s.remaining_ms, s.duration_ms), (10_000, 15_000));
        assert!(
            cds.is_on_cooldown(999, Some(&sibling), mid),
            "category lock is a not-ready"
        );
        // …an unrelated spell reads nothing.
        assert_eq!(cds.info(133, 0, Some(&fireball()), mid).remaining_ms, 0);
    }

    #[test]
    fn an_on_event_cooldown_parks_until_the_event_starts_it() {
        let t0 = Instant::now();
        let mut cds = Cooldowns::default();
        // Feign Death: 30 s recovery, SPELL_ATTR_COOLDOWN_ON_EVENT (bit 25).
        let fd = spell(0, 30_000, 0, (0, 0), 0x0200_0000);
        cds.start_spell(5384, &fd, 0, t0);

        // Parked: full duration, enabled == false (the sweep is hidden), but "not ready" holds.
        let parked = cds.info(5384, 0, Some(&fd), t0 + Duration::from_secs(60));
        assert_eq!(
            (parked.remaining_ms, parked.enabled),
            (30_000, false),
            "an on-hold record never elapses on its own"
        );
        assert!(cds.is_on_cooldown(5384, Some(&fd), t0 + Duration::from_secs(60)));

        // SMSG_COOLDOWN_EVENT starts the clocks NOW.
        let event_at = t0 + Duration::from_secs(60);
        cds.cooldown_event(5384, event_at);
        let running = cds.info(5384, 0, Some(&fd), event_at + Duration::from_secs(10));
        assert_eq!(
            (running.remaining_ms, running.duration_ms, running.enabled),
            (20_000, 30_000, true)
        );
    }

    #[test]
    fn wire_cooldowns_take_the_server_duration_or_fall_back_to_the_dbc() {
        let t0 = Instant::now();
        let mut cds = Cooldowns::default();
        // A school lockout: nonzero wire ms is the recovery verbatim.
        cds.apply_wire_cooldown(133, 8_000, Some(&fireball()), t0);
        let locked = cds.info(133, 0, Some(&fireball()), t0 + Duration::from_secs(3));
        assert_eq!((locked.remaining_ms, locked.duration_ms), (5_000, 8_000));

        // Zero wire ms: the spell's own Spell.dbc recovery/category pair.
        let mut cds = Cooldowns::default();
        cds.apply_wire_cooldown(100, 0, Some(&charge()), t0);
        let ch = cds.info(100, 0, Some(&charge()), t0 + Duration::from_secs(5));
        assert_eq!((ch.remaining_ms, ch.duration_ms), (10_000, 15_000));
    }

    #[test]
    fn item_use_cooldowns_key_on_the_item_and_respect_the_wire_triple() {
        let t0 = Instant::now();
        let mut cds = Cooldowns::default();
        // A potion: the wire triple resolved category 4 / 60 s, use-cooldown "the spell's own"
        // (-1 → the spell has none).
        let use_spell = ItemUseSpell {
            spell_id: 439,
            cooldown_ms: -1,
            category: 4,
            category_cooldown_ms: 60_000,
        };
        let potion_spell = spell(4, 0, 60_000, (133, 1500), 0);
        cds.start_item(118, &use_spell, Some(&potion_spell), t0);

        let mid = t0 + Duration::from_secs(15);
        // The action-bar read for the potion action (spell 439 as cast from item 118) sees the
        // category remainder…
        let info = cds.info(439, 118, Some(&potion_spell), mid);
        assert_eq!((info.remaining_ms, info.duration_ms), (45_000, 60_000));
        // …and so does any other category-4 potion.
        let other = cds.info(440, 929, Some(&potion_spell), mid);
        assert_eq!(other.remaining_ms, 45_000);
    }

    #[test]
    fn prune_drops_only_fully_elapsed_records() {
        let t0 = Instant::now();
        let mut cds = Cooldowns::default();
        cds.start_gcd(133, &fireball(), t0);
        cds.start_spell(100, &charge(), 0, t0);
        assert_eq!(cds.records.len(), 2);

        // After the GCD (1.5 s) but inside Charge's 15 s: only the GCD record goes.
        cds.prune(t0 + Duration::from_secs(5));
        assert_eq!(cds.records.len(), 1);
        assert_eq!(cds.records[0].spell_id, 100);

        cds.prune(t0 + Duration::from_secs(20));
        assert!(cds.records.is_empty());
    }

    /// The vanished-GCD-pie regression (spam-press: fail-clear + re-arm inside one inter-feed
    /// gap): the UI triple must be (a) frame-stable for one running cooldown — re-reading the
    /// same arm later yields the same start — and (b) distinct across two arms, so the seam can
    /// never mistake a fresh GCD for the elapsed one it replaced.
    #[test]
    fn the_ui_triple_is_stable_per_arm_and_distinct_across_arms() {
        let t0 = Instant::now();
        let mut cds = Cooldowns::default();
        cds.start_gcd(772, &fireball(), t0); // press #1 arms the GCD

        // Two reads of the SAME arm, frames apart, on both clocks in lockstep → identical triple.
        let read1 = cds
            .info(772, 0, Some(&fireball()), t0 + Duration::from_millis(16))
            .ui_triple(t0 + Duration::from_millis(16), 10.016);
        let read2 = cds
            .info(772, 0, Some(&fireball()), t0 + Duration::from_millis(160))
            .ui_triple(t0 + Duration::from_millis(160), 10.160);
        assert_eq!(read1, Some((10_000, 1500, true)));
        assert_eq!(read1, read2, "one arm reads one start, every frame");

        // The spam cycle: the fail clears the GCD, the re-press re-arms 200 ms later — the feed
        // never observes the cleared gap, but the fresh arm carries a fresh start regardless.
        cds.clear_gcd(772, t0 + Duration::from_millis(200));
        cds.start_gcd(772, &fireball(), t0 + Duration::from_millis(200));
        let rearmed = cds
            .info(772, 0, Some(&fireball()), t0 + Duration::from_millis(216))
            .ui_triple(t0 + Duration::from_millis(216), 10.216);
        assert_eq!(
            rearmed,
            Some((10_200, 1500, true)),
            "a re-arm never aliases"
        );
    }

    /// The spam-press vanished-pie bug, leg 2 (the send gate): a press during the running GCD
    /// must be refused LOCALLY — if it were sent, the server's NOT_READY fail would `clear_gcd`
    /// the running GCD and kill the sweep. GCD-free spells (Heroic Strike's 0 ms pair) are never
    /// locked, and the lock lifts when the GCD elapses.
    #[test]
    fn a_running_gcd_locks_gcd_carrying_presses_locally() {
        let t0 = Instant::now();
        let mut cds = Cooldowns::default();
        assert!(
            !cds.gcd_locked(&fireball(), t0),
            "no GCD running — nothing locks"
        );

        cds.start_gcd(772, &fireball(), t0); // the successful Rend arms the 133/1500 GCD
        let mid = t0 + Duration::from_millis(200);
        assert!(
            cds.gcd_locked(&fireball(), mid),
            "the spam press 200 ms later is locked — refused, never sent, the GCD lives"
        );
        // A GCD-free spell (Heroic Strike shape: category 133, 0 ms) queues through freely.
        let heroic = spell(0, 0, 0, (133, 0), 0x10000);
        assert!(!cds.gcd_locked(&heroic, mid), "0 ms GCD pair never locks");
        // Charge (no GCD pair at all) is untouched.
        assert!(!cds.gcd_locked(&charge(), mid));
        // The lock lifts exactly when the GCD elapses.
        assert!(!cds.gcd_locked(&fireball(), t0 + Duration::from_millis(1_501)));
    }

    #[test]
    fn cheat_wipe_and_clear_bump_the_generation_only_on_change() {
        let t0 = Instant::now();
        let mut cds = Cooldowns::default();
        let g0 = cds.generation;
        cds.clear_spell(133); // nothing tracked — no bump
        cds.wipe();
        assert_eq!(cds.generation, g0);

        cds.start_gcd(133, &fireball(), t0);
        assert_ne!(cds.generation, g0);
        let g1 = cds.generation;
        cds.wipe();
        assert_ne!(cds.generation, g1);
        assert_eq!(cds.info(133, 0, Some(&fireball()), t0).remaining_ms, 0);
    }

    /// The ranged-shot pad (decision 0378, wow-re `ranged-cooldown-sweep.md`): a Throw-shaped
    /// spell (category 76, all-zero DBC recovery) sweeps the weapon's attack time via its
    /// CATEGORY timer, and refuses a recast within it.
    #[test]
    fn throw_sweeps_the_ranged_attack_time_via_its_category() {
        let t0 = Instant::now();
        let mut cds = Cooldowns::default();
        let throw = spell(76, 0, 0, (0, 0), 0x410012);
        cds.start_spell(2764, &throw, 2200, t0);
        let mid = t0 + Duration::from_millis(1000);
        let info = cds.info(2764, 0, Some(&throw), mid);
        assert_eq!(info.duration_ms, 2200, "the sweep is the weapon speed");
        assert_eq!(info.remaining_ms, 1200);
        assert!(info.enabled, "running, not parked");
        assert!(cds.is_on_cooldown(2764, Some(&throw), mid));
        assert!(
            !cds.is_on_cooldown(2764, Some(&throw), t0 + Duration::from_millis(2300)),
            "free again after the weapon speed elapses"
        );
    }

    /// Auto Shot (category 0) inserts the same padded timer but NO read surfaces it — the
    /// client's `SpellCategory[0]`-is-NULL asymmetry: no sweep, no local recast refusal.
    #[test]
    fn auto_shot_category_zero_never_surfaces_its_pad() {
        let t0 = Instant::now();
        let mut cds = Cooldowns::default();
        let auto_shot = spell(0, 0, 0, (0, 0), 0x50012);
        cds.start_spell(75, &auto_shot, 3200, t0);
        let mid = t0 + Duration::from_millis(100);
        assert_eq!(
            cds.info(75, 0, Some(&auto_shot), mid).remaining_ms,
            0,
            "no sweep for a category-0 record"
        );
        assert!(
            !cds.is_on_cooldown(75, Some(&auto_shot), mid),
            "no local refusal either"
        );
    }
}
