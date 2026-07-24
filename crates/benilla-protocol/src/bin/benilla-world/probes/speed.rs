//! `--speed`: the force-speed-change wire. GM `.modify speed 1.5`, require + ack
//! `SMSG_FORCE_RUN_SPEED_CHANGE`, then `.modify speed 1` and require a SECOND change on a still-live
//! stream (a malformed ack drops the session, so surviving both round trips is the proof).

use anyhow::{ensure, Result};
use benilla_protocol::{SessionEvent, SpeedKind};

use crate::probes::{Ctx, Probe};

#[derive(Default)]
pub(crate) struct Speed {
    second_sent: bool,
}

impl Probe for Speed {
    fn stage(&mut self, cx: &mut Ctx) -> Result<()> {
        cx.session.send_chat(".modify speed 1.5")?;
        println!("sent GM: .modify speed 1.5 (self) — expecting SMSG_FORCE_RUN_SPEED_CHANGE");
        Ok(())
    }

    fn on_event(&mut self, ev: &SessionEvent, cx: &mut Ctx) -> Result<()> {
        if let SessionEvent::ForceSpeedChange { guid, .. } = ev {
            // Fires after World has acked + recorded the change. World acks only when our pose is
            // known (`tracked.get(guid)` Some, else it skips the arm entirely and never records) —
            // mirror that guard (self + tracked-present) here so the second `.modify` is suppressed
            // in exactly the same pose-missing case the old in-arm `continue` suppressed it.
            if *guid == cx.world.self_guid
                && cx.world.tracked.contains_key(guid)
                && !self.second_sent
            {
                cx.session.send_chat(".modify speed 1")?;
                self.second_sent = true;
                println!("sent GM: .modify speed 1 — expecting the second change");
            }
        }
        Ok(())
    }

    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        // --speed verdict: two changes, both Run, the flat speeds the GM rates imply (1.5×7.0 then
        // 1.0×7.0), and an incremented counter — proof both acks were parsed (a malformed body drops
        // the session before the second round trip could complete).
        let speed_changes_seen = &cx.world.speed_changes_seen;
        ensure!(
            speed_changes_seen.len() >= 2,
            "--speed: expected 2 force-speed changes (got {}) — did the first ack drop the session,              or is the account not GM?",
            speed_changes_seen.len()
        );
        let (k1, c1, s1) = speed_changes_seen[0];
        let (k2, c2, s2) = speed_changes_seen[1];
        ensure!(
            k1 == SpeedKind::Run && k2 == SpeedKind::Run,
            "--speed: expected Run changes, got {k1:?} then {k2:?}"
        );
        ensure!(
            (s1 - 10.5).abs() < 0.01 && (s2 - 7.0).abs() < 0.01,
            "--speed: expected flat speeds 10.5 then 7.0 (rates 1.5/1.0 × base 7.0), got {s1} then {s2}"
        );
        ensure!(
            c2 > c1,
            "--speed: movement counter must increment across changes (got {c1} then {c2})"
        );
        println!(
            "\n--speed PASS: {k1:?} 7.0->10.5->7.0 yd/s, counters {c1}->{c2}, both acks accepted              (stream survived)"
        );
        Ok(())
    }
}
