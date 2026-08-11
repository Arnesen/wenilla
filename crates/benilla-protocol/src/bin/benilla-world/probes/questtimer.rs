//! `--questtimer`: the timed-quest countdown's whole wire chain, live (decision 1150, B234).
//!
//! The countdown is not on the wire as a duration anywhere. The server writes a timed quest's
//! deadline into the quest-log slot's third field as an **absolute unix stamp** — `time(nullptr) +
//! limitTime`, vmangos `Player::AddQuest` — and the client is expected to subtract its own sample
//! of the server's clock, taken with `CMSG_QUERY_TIME`. Two independent readings, one subtraction,
//! and nothing in the client can tell you it got either one wrong: a mis-decoded field or a missing
//! clock both produce a number that *looks* like a countdown.
//!
//! So this probe reads both ends against the running server and checks the subtraction lands where
//! the quest template says it must:
//!
//! 1. `.quest remove` then `.quest add` [`TIMED_QUEST`] (idempotent across re-runs).
//! 2. Poll the descriptor for the slot the quest landed in and read its **raw** timer field.
//! 3. `CMSG_QUERY_TIME` for the server's wall clock.
//! 4. Assert the subtraction implies the quest was added *just now*, and print the countdown.
//!
//! Step 4 is what makes this more than a decode check. The probe added the quest itself seconds
//! ago, so the remaining time must sit just under the template's full limit — not merely somewhere
//! inside it. A wrong epoch misses that window by decades, a seconds/milliseconds slip by 1000×,
//! and a clock that never arrived by more than the run's own length.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use benilla_protocol::events::decode;
use benilla_protocol::SessionEvent;

use crate::probes::{Ctx, Probe, FIELD_PLAYER_QUEST_LOG_1_1};

/// "Iverron's Antidote" (Teldrassil) — `quest_template.LimitTime = 300`, min level 2, and no
/// prerequisite a GM `.quest add` cares about. Chosen because 5 minutes is long enough that the
/// remaining time is unambiguously mid-window while the probe runs, and short enough that a
/// seconds/milliseconds mix-up would fall outside it immediately.
const TIMED_QUEST: u32 = 3522;
/// [`TIMED_QUEST`]'s `quest_template.LimitTime` (read from the running deploy's `mangos` DB). Not
/// on the wire in 1.12 — `SMSG_QUEST_QUERY_RESPONSE` has no such field, and vmangos reads
/// `GetLimitTime()` only server-side in `Player::AddQuest` — so it is a constant here rather than a
/// second packet's answer.
const LIMIT_TIME_SECS: u32 = 300;
/// How much wall time the probe allows between its `.quest add` and the read below. Generous
/// against a slow login and a full stream window, and still ~100× tighter than any of the failure
/// modes it is bounding (a wrong epoch, a seconds/milliseconds slip, a clock that never arrived).
const ADD_TO_READ_SLACK: f64 = 120.0;

#[derive(Default)]
pub(crate) struct QuestTimer {
    /// The server's wall clock, and when we received it (`SMSG_QUERY_TIME_RESPONSE`).
    clock: Option<(u32, Instant)>,
}

impl Probe for QuestTimer {
    fn stage(&mut self, cx: &mut Ctx) -> Result<()> {
        cx.session
            .send_chat(&format!(".quest remove {TIMED_QUEST}"))?;
        cx.session.send_chat(&format!(".quest add {TIMED_QUEST}"))?;
        println!("questtimer: GM .quest remove/add {TIMED_QUEST}");
        Ok(())
    }

    fn on_event(&mut self, ev: &SessionEvent, _cx: &mut Ctx) -> Result<()> {
        if let SessionEvent::ServerUnixTime { unix_time } = ev {
            self.clock = Some((*unix_time, Instant::now()));
        }
        Ok(())
    }

    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        let Ctx { session, world } = cx;
        let self_guid = world.self_guid;

        // 1) The slot. Poll the descriptor the same way `--questlog` does — the accept's field
        // update can trail the chat ack by a beat.
        let find_slot = |sf: &Option<benilla_protocol::messages::ObjectFields>| {
            sf.as_ref().and_then(|sf| {
                (0..benilla_protocol::messages::PLAYER_QUEST_LOG_SLOTS)
                    .find(|&i| sf.player_quest_log(i).map(|s| s.quest_id) == Some(TIMED_QUEST))
            })
        };
        let mut slot = find_slot(&world.self_fields);
        for _ in 0..6 {
            if slot.is_some() {
                break;
            }
            let drain_until = Instant::now() + Duration::from_secs(1);
            while Instant::now() < drain_until {
                let Ok(msg) = session.recv() else { continue };
                for ev in decode(msg) {
                    match ev {
                        SessionEvent::ObjectValues { guid: g, fields } if g == self_guid => {
                            if let Some(sf) = &mut world.self_fields {
                                sf.merge(fields);
                            }
                        }
                        SessionEvent::ServerUnixTime { unix_time } => {
                            self.clock = Some((unix_time, Instant::now()));
                        }
                        _ => {}
                    }
                }
            }
            slot = find_slot(&world.self_fields);
        }
        let slot = slot.context(
            "--questtimer: the timed quest never landed in a PLAYER_QUEST_LOG slot within the \
             poll window (is `.quest add` refused for this character?)",
        )?;

        // The timer field raw (id-field + 2), beside the decoded view, so a decode that reads the
        // wrong field of the triple is visible as a mismatch rather than a plausible number.
        let raw_timer = world
            .self_fields
            .as_ref()
            .and_then(|sf| {
                sf.raw_fields()
                    .find(|&(idx, _)| idx == FIELD_PLAYER_QUEST_LOG_1_1 + 3 * u16::from(slot) + 2)
                    .map(|(_, v)| v)
            })
            .unwrap_or(0);
        let decoded = world
            .self_fields
            .as_ref()
            .and_then(|sf| sf.player_quest_log(slot))
            .context("--questtimer: the slot vanished between the find and the read")?;
        println!(
            "quest {TIMED_QUEST} occupies slot {slot}; timer field raw {raw_timer} \
             (decoded {}), state byte {:#04x}",
            decoded.timer, decoded.state
        );
        if raw_timer == 0 || decoded.timer != raw_timer {
            bail!(
                "--questtimer: timer field raw {raw_timer}, decoded {} — a timed quest must carry \
                 a nonzero absolute deadline and the decode must agree with the raw word",
                decoded.timer
            );
        }

        // 2) The server's wall clock. Asked for here rather than leaning on a login-time send, so
        // the probe is self-contained and the round trip itself is what's under test.
        session.query_time()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let asked_at = Instant::now();
        while Instant::now() < deadline {
            let Ok(msg) = session.recv() else { continue };
            for ev in decode(msg) {
                if let SessionEvent::ServerUnixTime { unix_time } = ev {
                    self.clock = Some((unix_time, Instant::now()));
                }
            }
            if self.clock.is_some_and(|(_, at)| at > asked_at) {
                break;
            }
        }
        let (base, at) = self
            .clock
            .context("--questtimer: no SMSG_QUERY_TIME_RESPONSE within 5s")?;
        let now = f64::from(base) + at.elapsed().as_secs_f64();
        println!(
            "server wall clock {base} (+{:.1}s since the answer)",
            at.elapsed().as_secs_f64()
        );

        // 3) The subtraction, against a window the probe's own actions bound tightly on BOTH
        // sides. `.quest add` ran seconds ago in `stage`, so the server wrote
        // `now + LIMIT_TIME_SECS` seconds ago — the remaining time must therefore sit just under
        // the full limit, not merely somewhere inside it. That tight upper bound is what makes
        // this a real check: a wrong epoch is out by decades, a seconds/milliseconds slip by
        // 1000×, and an absent or stale clock by more than the run's own length.
        //
        // (The limit is a constant because 1.12's `SMSG_QUEST_QUERY_RESPONSE` does not carry
        // `LimitTime` — verified against the parsed body's field list and vmangos, which reads
        // `GetLimitTime()` only in `Player::AddQuest`. Its provenance is the DB row cited on
        // [`TIMED_QUEST`]; if content ever changes it, this probe fails loudly rather than
        // quietly passing.)
        let remaining = f64::from(raw_timer) - now;
        let elapsed_since_add = f64::from(LIMIT_TIME_SECS) - remaining;
        if !(0.0..=ADD_TO_READ_SLACK).contains(&elapsed_since_add) {
            bail!(
                "--questtimer: {remaining:.1}s remaining of a {LIMIT_TIME_SECS}s limit implies \
                 {elapsed_since_add:.1}s between the `.quest add` and this read — expected \
                 0..{ADD_TO_READ_SLACK}s. The deadline and the clock disagree: wrong field, wrong \
                 epoch, wrong unit, or no clock at all."
            );
        }
        println!(
            "✅ countdown: {remaining:.1}s remaining of {LIMIT_TIME_SECS}s — deadline {raw_timer} \
             minus server now {now:.0}, i.e. {elapsed_since_add:.1}s since the `.quest add`."
        );
        Ok(())
    }
}
