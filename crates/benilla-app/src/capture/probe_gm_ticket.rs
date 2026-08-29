//! The GM trouble-ticket live probe (`WOW_PROBE_GMTICKET=1`) — decision 1673's end-to-end
//! instrument: log in, and drive the whole five-opcode ticket wire through the **live Lua VM's own
//! bindings** (`GetGMStatus`, `DeleteGMTicket`, `GetGMTicket`, `NewGMTicket`, `UpdateGMTicket`),
//! observing every answer where the Help window observes it — as an `UPDATE_TICKET` /
//! `UPDATE_GM_STATUS` event fired at a real frame. One
//! `PROBE_GMTICKET: <step> PASS/FAIL/SKIP <detail>` line per step, then a final
//! `PROBE_GMTICKET: DONE pass=<n> fail=<m>`. Modeled on [`super::probe_binder`] and
//! [`super::probe_bank`] — same phase machine, same trace style, same self-terminating exit
//! ([`super::probes::ProbeExitPlugin`]'s pattern), same live-VM observation idiom (a small Lua hook
//! appending to a probe table, read back with `script.eval`).
//!
//! ## Why unit tests cannot close this feature
//!
//! 1673 ships 23 unit tests, byte-exact wire goldens and a DBC oracle, and **not one of them can
//! tell whether vmangos accepts the packet**. The server answers several refusals with *silence*:
//! `HandleGMTicketCreateOpcode` returns with no packet at all when the queue is off, when the
//! player is under `GMTickets.MinLevel`, or when the category is `>= GMTICKET_MAX` (11), and
//! `HandleGMTicketDeleteTicketOpcode` likewise when there is no ticket (vmangos
//! `Handlers/GMTicketHandler.cpp`, read this session). A create the server drops on the floor is
//! byte-identical, from the client's side, to one that worked. Only a live round trip settles it.
//!
//! ## What each step can and cannot conclude
//!
//! 1. **queue** — `GetGMStatus()`, then `UPDATE_GM_STATUS` carrying **1**. `GMTickets.Enable`
//!    defaults to `true` (`World.cpp:684`), so 1 is the expected answer; no event at all means the
//!    opcode round trip is dead and is a FAIL. A `0` is not a client defect — it is the queue
//!    genuinely switched off, and every later step would then be answered with silence, so the
//!    probe SKIPs the rest rather than manufacturing failures out of a server setting.
//! 2. **clean** — `DeleteGMTicket()`, then `GetGMTicket()`, expecting `arg1 == 0`. Clears a
//!    leftover ticket from an earlier run *and* proves the "no ticket" answer decodes
//!    (`GMTICKET_STATUS_DEFAULT`, a 4-byte body). **The trap:** delete-with-no-ticket is answered
//!    with nothing at all, so this waits on the `GetGMTicket` answer that follows, never on a
//!    delete response.
//! 3. **create** — `NewGMTicket(4, <unique text>)` (4 = Item, `GMTicketCategory.dbc` id 4 =
//!    vmangos `GMTICKET_ITEM`), then `GetGMTicket()`, expecting `arg1 == 4` and `arg2` **equal** to
//!    the text sent. The load-bearing step: it proves the `u8` category, the map/position block,
//!    the text and the trailing `"Reserved for future use"` cstring are laid out the way vmangos
//!    reads them. A category widened to `u32` would shift the map id and the position three bytes
//!    down; the server would file the ticket at a garbage spot or reject the packet outright.
//! 4. **db** — the row itself. **A two-part check, and the probe only does the first half**: the
//!    client never reads map/position back, so the echo in step 3 cannot prove they landed. The
//!    probe *prints* what it sent (`PROBE_GMTICKET: db-expect …`) and the operator runs the SQL
//!    after the run and compares. This step therefore always reports SKIP — it is not a pass the
//!    probe is entitled to claim. The line carries a `drift=` figure: how far the body could have
//!    moved between the position sample and the frame the packet was stamped (see [`REST_EPS`]).
//!    `drift=0.000` means the position in the line is exact.
//! 5. **edit** — `UpdateGMTicket(4, <second unique text>)`, then `GetGMTicket()`, expecting `arg2`
//!    **equal** to the new text. This is the step that proves the **category byte** on
//!    `CMSG_GMTICKET_UPDATETEXT` — the emulator fork 1673 records, where cmangos-classic reads a
//!    bare cstring and would swallow our category byte as the first character of the text. The
//!    comparison is equality, never `contains`, because a stray leading control byte is exactly
//!    the failure this step exists to catch.
//! 6. **abandon** — `DeleteGMTicket()`, then `GetGMTicket()`, expecting `arg1 == 0` again.
//!
//! ## Two facts about waiting that this probe is built around
//!
//! **The wait is for the SERVER, never for the drain.** Lua's calls reach the wire in call order:
//! [`crate::ui_gm_ticket`]'s `drain_gm_ticket` walks ONE ordered intent queue
//! (`take_gm_ticket_intents`), so `DeleteGMTicket(); GetGMTicket()` in a single chunk arrives
//! delete-then-get. It did not always — the drain used to walk a counter per verb and put every
//! ask ahead of every write and delete, which inverted exactly that pair and made the get answer
//! with the *pre*-delete state. This probe is what surfaced that (driving the bindings directly is
//! what a third-party addon does and what no shipped-window test exercises); decision 1673's "One
//! ordered queue, not a counter per verb" records the fix. What still has to be waited for is the
//! **round trip**: each step separates its write from the read-back by [`WRITE_GAP_SECS`] and then
//! waits up to [`ANSWER_TIMEOUT_SECS`] for an answer that matches. The gap is no longer a
//! correctness crutch; it keeps one exchange per trace line, which is what makes a failing step
//! readable.
//!
//! **Answers are not correlated to asks, so nothing here waits for "the next event".** Three
//! sources put an `UPDATE_TICKET` on the stream that this probe never explicitly asked for: a GM's
//! `.ticket viewid`/`escalate`/`complete`; vmangos's own delete handler (`SendTicket(this,
//! nullptr)`, right after the delete response); and the **engine's own re-ask**, which fires on
//! every create-ok (2) and update-ok (4) response (`GmTicketState::note_write_landed`, the
//! reference's `0x5e4479`). So after step 3's `NewGMTicket` an answer arrives *before* the probe's
//! own `GetGMTicket()` — expected, not an anomaly. Each step latches the observed event count as a
//! baseline and then waits for **any** answer after it that matches the expectation, failing only
//! at the timeout and printing every answer it did see. Matching on a *unique* text for the two
//! load-bearing steps is what keeps that from being a weaker assertion than "the next one".
//!
//! ## What the DB row looks like afterwards
//!
//! vmangos's abandon is `CloseTicket`, which sets `closed_by` and `SaveToDB()`s — it does **not**
//! delete the row (`GMTicketMgr.cpp:401-410`) — so step 6 leaves the row in place for the operator
//! to read. `HandleGMTicketUpdateTextOpcode` calls `SetTicketType(packet.type)` as well as
//! `SetMessage`, so after step 5 the row's `message` is the **edited** text while `map`/
//! `position_*` still carry the create's values (the row is REPLACEd in place). Both `db-expect`
//! lines say which is which.
//!
//! ## The run recipe
//!
//! ```text
//! WOW_NOSOUND=1 WOW_USER=probe3 WOW_PASS=pprobe3 WOW_CHAR=Probethree \
//!     WOW_PROBE_GMTICKET=1 cargo run -q -p benilla
//! ```
//! (the slot-keyed probe identity — this worktree is `pool-3` → `probe3`/`pprobe3`/`Probethree`;
//! method.md "The local vmangos server"). Non-combat; the probe never drives the body and sends no
//! GM command, so GM mode and position are left exactly as found — but note that the body is not
//! necessarily *still* when a run starts (see [`REST_EPS`]). An outer `timeout` + grep on
//! `PROBE_GMTICKET:` is the whole harness; the probe self-exits once DONE. Nothing here is a
//! timing measurement, so an occluded window costs wall clock and no correctness.
//!
//! Every step SKIPs with a note rather than FAILing for an environmental problem (no UI VM in this
//! build, the queue switched off server-side, a binding that would not run). A genuine wrong value
//! is a FAIL.

use bevy::prelude::*;

use benilla_assets::coords::bevy_to_wow;
use benilla_ui::script::UiScript;

use super::probes::ProbeClock;
use crate::net::SelfPlayer;
use crate::player::Player;

/// `GMTicketCategory.dbc` id 4 = "Item" (decision 1673's table), which is vmangos's
/// `GMTICKET_ITEM = 4` (`SharedDefines.h:1779`) — the same number on both sides, which is the
/// point: the id is the wire value, not a list index.
const CATEGORY_ITEM: u32 = 4;

/// `SMSG_GMTICKETSYSTEMSTATUS`'s "the queue is taking tickets" (`GMTICKET_QUEUE_STATUS_ENABLED`).
///
/// The field is **signed** (`GmTicketState::queue_status`, byte-verified: the reference loads it
/// with `fild dword`), so `-1` — the window's "queue down, and say so" arm — is a value this probe
/// can legitimately read back. vmangos only ever sends 0 or 1.
const QUEUE_ENABLED: f64 = 1.0;

/// The `UPDATE_TICKET` `arg1` that means "you have no ticket" — the window's whole else-branch
/// hangs on `arg1 and arg1 ~= 0`.
const NO_TICKET: f64 = 0.0;

/// Settle before the first ask: lets the body land, the world stream, and any login-time
/// `GetGMTicket()` the shipped Help window fires drain and answer before a baseline is latched.
const SETTLE_SECS: f64 = 3.0;

/// The gap between a write/delete and the `GetGMTicket()` that reads it back.
///
/// Not a correctness requirement — the drain preserves Lua's call order (module doc) — but a
/// readability one: it keeps one exchange per trace line, so a step that fails names the exchange
/// that failed rather than a batch of them.
const WRITE_GAP_SECS: f64 = 1.0;

/// How long a step waits for a matching answer before calling it. Generous on purpose: a starved
/// (occluded) window polls at ~1 fps and a timeout tight enough to trip on that would report a
/// FAIL about the wire, which is the one thing an instrument must never do.
const ANSWER_TIMEOUT_SECS: f64 = 20.0;

/// **The body must be at rest before the ticket is filed.** A ticket records *where you are*, and
/// the first live run of this probe filed one while the body was still falling: it entered the
/// world at z ≈ 94, the preflight banner read 82, and it was through 60 a second later — 21 yd of
/// travel across the create. The row happened to match anyway (the drain sampled the same frame),
/// but "happened to" is not a property an instrument may rest step 4 on, so the create now waits
/// for the body to stop moving. Movement under [`REST_EPS`] yd for [`REST_HOLD_SECS`] counts as
/// stopped.
const REST_EPS: f32 = 0.02;
const REST_HOLD_SECS: f64 = 0.5;
/// The cap on that wait. Timing out is not a failure — the probe files anyway and says the
/// position may be off, because a body that genuinely cannot come to rest (a lift, a swim, a
/// treadmill of terrain streaming) still has a working ticket wire to test.
const REST_TIMEOUT_SECS: f64 = 20.0;

/// How far the body may have travelled between the position sample and the frame the drain stamps
/// the packet, before the db-expect line stops being trustworthy (yd, WoW space).
const STAMP_DRIFT_EPS: f32 = 0.05;

/// The operator's half of step 4, printed verbatim so it can be pasted.
const DB_QUERY: &str = "cd /Users/sam/dev/vmangos-deploy && docker compose exec -T database \
                        mariadb -umangos -pmangos characters -e \"SELECT ticket_id, name, \
                        ticket_type, map, position_x, position_y, position_z, closed_by, message \
                        FROM gm_tickets ORDER BY ticket_id DESC LIMIT 1;\"";

pub(crate) struct ProbeGmTicketPlugin;

impl Plugin for ProbeGmTicketPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GmTicketProbe>()
            .add_systems(Update, gm_ticket_probe);
    }
}

/// The probe's phase machine plus what it discovered along the way (the bank/binder probes' shape:
/// a `Copy` phase snapshotted out of the resource each tick, so an arm can mutate `probe` freely).
#[derive(Resource, Default)]
struct GmTicketProbe {
    phase: Phase,
    /// The text step 3 filed — unique per run, so no stale answer can satisfy step 3 by accident.
    create_text: String,
    /// The text step 5 edited it to.
    edit_text: String,
    /// Map and WoW-space position sampled at the moment `NewGMTicket` was called — what the drain
    /// stamps into `CMSG_GMTICKET_CREATE` in that frame or the next, and what the DB row must
    /// carry.
    create_map: u32,
    create_pos: [f32; 3],
    /// How far the body had travelled by the frame after the create call — the honest bound on
    /// how wrong the db-expect position could be (`None` until that frame has been seen).
    stamp_drift: Option<f32>,
    passes: u32,
    fails: u32,
    /// Latched once [`Phase::Done`] has fired its exit (never re-fire on a later frame).
    exited: bool,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Phase {
    #[default]
    Wait,
    /// Hook installed; settling before the first ask (module doc).
    Settling {
        since: f64,
    },
    /// `GetGMStatus()` called; waiting for `UPDATE_GM_STATUS` (step 1).
    Queue {
        since: f64,
        baseline: usize,
    },
    /// `DeleteGMTicket()` called; letting the drain flush it before the read-back (step 2a).
    CleanGap {
        since: f64,
    },
    /// `GetGMTicket()` called; waiting for `arg1 == 0` (step 2b).
    CleanGet {
        since: f64,
        baseline: usize,
    },
    /// Waiting for the body to stop moving before the ticket is filed ([`REST_EPS`]) — `last` is
    /// the previous sample and `still_since` when it was last seen to move.
    Rest {
        since: f64,
        last: [f32; 3],
        still_since: f64,
    },
    /// `NewGMTicket(4, …)` called; letting the drain flush it (step 3a).
    CreateGap {
        since: f64,
    },
    /// `GetGMTicket()` called; waiting for the filed ticket to come back (step 3b).
    CreateGet {
        since: f64,
        baseline: usize,
    },
    /// `UpdateGMTicket(4, …)` called; letting the drain flush it (step 5a).
    EditGap {
        since: f64,
    },
    /// `GetGMTicket()` called; waiting for the *new* text (step 5b).
    EditGet {
        since: f64,
        baseline: usize,
    },
    /// `DeleteGMTicket()` called; letting the drain flush it (step 6a).
    AbandonGap {
        since: f64,
    },
    /// `GetGMTicket()` called; waiting for `arg1 == 0` again (step 6b).
    AbandonGet {
        since: f64,
        baseline: usize,
    },
    Done,
}

/// The `UPDATE_TICKET` answers the live VM has seen, newest last, as `(arg1, arg2)` — the category
/// (0 for "no ticket") and the description. `arg2` is recorded as `""` whenever `arg1` is 0, since
/// the no-ticket fire carries a single argument and reading a second one would be reading whatever
/// the global happened to hold.
fn ticket_answers(script: &UiScript) -> Vec<(f64, String)> {
    let cats = script
        .eval::<Vec<f64>>("return ProbeGmTicketCat or {}")
        .unwrap_or_default();
    let texts = script
        .eval::<Vec<String>>("return ProbeGmTicketText or {}")
        .unwrap_or_default();
    cats.into_iter().zip(texts).collect()
}

/// The `UPDATE_GM_STATUS` values the live VM has seen, newest last.
fn status_answers(script: &UiScript) -> Vec<f64> {
    script
        .eval::<Vec<f64>>("return ProbeGmStatus or {}")
        .unwrap_or_default()
}

/// The first answer after `baseline` that satisfies `want` — the module doc's match-or-timeout
/// rule. Answers are not correlated to asks, so "the next event" is not a safe read; "any answer
/// after the baseline that matches" is, and the timeout is what turns a never-matching stream into
/// a FAIL that prints everything it saw.
fn matching<T>(answers: &[T], baseline: usize, want: impl Fn(&T) -> bool) -> Option<usize> {
    answers
        .iter()
        .enumerate()
        .skip(baseline)
        .find(|(_, a)| want(a))
        .map(|(i, _)| i)
}

/// A unique-per-run ticket text, so no stale or unsolicited answer can satisfy steps 3/5 by
/// accident. Seconds resolution is plenty — two runs cannot start in the same second and reach the
/// same step, and the suffix distinguishes the two texts within a run.
fn unique_text(suffix: &str) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("benilla probe {secs}{suffix}")
}

fn gm_ticket_probe(
    time: ProbeClock,
    mut probe: ResMut<GmTicketProbe>,
    script: Option<NonSendMut<UiScript>>,
    self_player: Query<(), With<SelfPlayer>>,
    player: Res<Player>,
    map: Option<Res<benilla_world::world_map::CurrentMap>>,
    mut exit: MessageWriter<AppExit>,
) {
    if self_player.is_empty() {
        return; // not in-world yet
    }
    let Some(script) = script else {
        return; // no UI VM this build (headless net-only) — nothing this probe can drive
    };
    let now = time.elapsed_secs_f64();
    let phase = probe.phase;

    match phase {
        Phase::Wait => {
            // The observation channel, installed before anything is asked so it is live for every
            // answer including the unsolicited ones (the binder probe's exact pattern).
            if let Err(e) = script.run(
                r#"
                if not ProbeGmTicketHooked then
                    ProbeGmTicketHooked = true
                    ProbeGmTicketCat = {}
                    ProbeGmTicketText = {}
                    ProbeGmStatus = {}
                    local f = CreateFrame("Frame")
                    f:RegisterEvent("UPDATE_TICKET")
                    f:RegisterEvent("UPDATE_GM_STATUS")
                    f:SetScript("OnEvent", function()
                        if event == "UPDATE_TICKET" then
                            local c = arg1 or 0
                            table.insert(ProbeGmTicketCat, c)
                            if c == 0 then
                                table.insert(ProbeGmTicketText, "")
                            else
                                table.insert(ProbeGmTicketText, arg2 or "")
                            end
                        else
                            table.insert(ProbeGmStatus, arg1 or 0)
                        end
                    end)
                end
                "#,
            ) {
                // Without the hook nothing can be observed, and every step would then FAIL for a
                // reason that has nothing to do with the wire. That is the one failure mode an
                // instrument must never produce, so it stops here instead.
                warn!(
                    "PROBE_GMTICKET: SKIP (0 hook) — the UPDATE_TICKET/UPDATE_GM_STATUS hook would \
                     not install in the live VM: {e}. Nothing can be observed, so no step below \
                     could be trusted (environmental, not a wire failure)."
                );
                probe.phase = Phase::Done;
                return;
            }
            info!("PROBE_GMTICKET: hook installed; settling {SETTLE_SECS}s before the first ask");
            probe.phase = Phase::Settling { since: now };
        }

        Phase::Settling { since } => {
            if now - since < SETTLE_SECS {
                return;
            }
            let baseline = status_answers(&script).len();
            if !run_or_skip(&mut probe, &script, "1 queue", "GetGMStatus()") {
                return;
            }
            probe.phase = Phase::Queue {
                since: now,
                baseline,
            };
        }

        // Step 1 — the queue status. No event at all means the round trip is dead.
        Phase::Queue { since, baseline } => {
            let seen = status_answers(&script);
            let Some(i) = matching(&seen, baseline, |_| true) else {
                if now - since > ANSWER_TIMEOUT_SECS {
                    error!(
                        "PROBE_GMTICKET: FAIL (1 queue) — no UPDATE_GM_STATUS within \
                         {ANSWER_TIMEOUT_SECS}s of GetGMStatus(). The CMSG_GMTICKET_SYSTEMSTATUS \
                         → SMSG_GMTICKETSYSTEMSTATUS round trip is dead: either the send never \
                         reached the wire or the answer never decoded. (values seen this run: \
                         {seen:?})"
                    );
                    probe.fails += 1;
                    probe.phase = Phase::Done;
                }
                return;
            };
            let status = seen[i];
            if status != QUEUE_ENABLED {
                warn!(
                    "PROBE_GMTICKET: SKIP (1 queue) — the round trip WORKS (UPDATE_GM_STATUS \
                     fired carrying {status}), but the server's ticket queue is not enabled. \
                     vmangos then answers create with silence (GMTicketHandler.cpp: \
                     `GetStatus() == GMTICKET_QUEUE_STATUS_DISABLED` → return), so every later \
                     step would fail for a server setting rather than a client defect. Set \
                     GMTickets.Enable = 1 and re-run."
                );
                probe.phase = Phase::Done;
                return;
            }
            info!(
                "PROBE_GMTICKET: PASS (1 queue) — UPDATE_GM_STATUS fired carrying \
                 {QUEUE_ENABLED} (the petition queue is up; the window's PETITION_QUEUE_ACTIVE \
                 gate opens)"
            );
            probe.passes += 1;
            if !run_or_skip(&mut probe, &script, "2 clean", "DeleteGMTicket()") {
                return;
            }
            probe.phase = Phase::CleanGap { since: now };
        }

        // Step 2 — the clean slate. The delete is deliberately NOT waited on: with no ticket
        // vmangos answers it with nothing at all.
        Phase::CleanGap { since } => {
            if now - since < WRITE_GAP_SECS {
                return;
            }
            let baseline = ticket_answers(&script).len();
            if !run_or_skip(&mut probe, &script, "2 clean", "GetGMTicket()") {
                return;
            }
            probe.phase = Phase::CleanGet {
                since: now,
                baseline,
            };
        }
        Phase::CleanGet { since, baseline } => {
            let seen = ticket_answers(&script);
            if matching(&seen, baseline, |(c, _)| *c == NO_TICKET).is_some() {
                info!(
                    "PROBE_GMTICKET: PASS (2 clean) — UPDATE_TICKET fired with arg1 == 0: no open \
                     ticket, and the 4-byte GMTICKET_STATUS_DEFAULT answer decodes"
                );
                probe.passes += 1;
                probe.phase = Phase::Rest {
                    since: now,
                    last: bevy_to_wow(player.pos),
                    still_since: now,
                };
            } else if now - since > ANSWER_TIMEOUT_SECS {
                error!(
                    "PROBE_GMTICKET: FAIL (2 clean) — no UPDATE_TICKET with arg1 == 0 within \
                     {ANSWER_TIMEOUT_SECS}s of DeleteGMTicket() + GetGMTicket(). Answers seen \
                     after the baseline: {:?}. A leftover ticket that refuses to clear, or a \
                     GETTICKET round trip that never answers.",
                    &seen[baseline.min(seen.len())..]
                );
                probe.fails += 1;
                probe.phase = Phase::Done;
            }
        }

        // The rest gate: a ticket records where you are, so file it from a body that has stopped
        // moving (module doc — the first live run filed one mid-fall).
        Phase::Rest {
            since,
            last,
            still_since,
        } => {
            let here = bevy_to_wow(player.pos);
            if distance(here, last) > REST_EPS {
                probe.phase = Phase::Rest {
                    since,
                    last: here,
                    still_since: now,
                };
                return;
            }
            let timed_out = now - since > REST_TIMEOUT_SECS;
            if !timed_out && now - still_since < REST_HOLD_SECS {
                return;
            }
            if timed_out {
                warn!(
                    "PROBE_GMTICKET: (3 create) the body never came to rest within \
                     {REST_TIMEOUT_SECS}s (still at {here:?}); filing anyway — the wire is still \
                     testable, but treat the db-expect position as approximate."
                );
            }
            let text = unique_text("");
            probe.create_pos = here;
            probe.create_map = map.map(|m| m.0).unwrap_or(0);
            if !run_or_skip(
                &mut probe,
                &script,
                "3 create",
                &format!("NewGMTicket({CATEGORY_ITEM}, \"{text}\")"),
            ) {
                return;
            }
            probe.create_text = text;
            probe.phase = Phase::CreateGap { since: now };
        }

        // Step 3 — the load-bearing one: the create body's layout, proven by the echo.
        Phase::CreateGap { since } => {
            // The first tick after the create call is the last frame in which the drain can still
            // stamp the packet, so this is the whole window in which the body could move between
            // the sample and the wire. Measuring it here — rather than a second later, at the
            // db-expect print — is the difference between an honest bound and crying wolf at a
            // body that simply kept falling after the packet had already gone.
            if probe.stamp_drift.is_none() {
                let drift = distance(bevy_to_wow(player.pos), probe.create_pos);
                probe.stamp_drift = Some(drift);
                if drift > STAMP_DRIFT_EPS {
                    warn!(
                        "PROBE_GMTICKET: (3 create) the body moved {drift:.3} yd in the frame the \
                         packet was stamped; the db-expect position below is good only to that."
                    );
                }
            }
            if now - since < WRITE_GAP_SECS {
                return;
            }
            let baseline = ticket_answers(&script).len();
            if !run_or_skip(&mut probe, &script, "3 create", "GetGMTicket()") {
                return;
            }
            probe.phase = Phase::CreateGet {
                since: now,
                baseline,
            };
        }
        Phase::CreateGet { since, baseline } => {
            let seen = ticket_answers(&script);
            let want = probe.create_text.clone();
            if let Some(i) = matching(&seen, baseline, |(_, t)| *t == want) {
                let (cat, text) = &seen[i];
                if *cat != f64::from(CATEGORY_ITEM) {
                    error!(
                        "PROBE_GMTICKET: FAIL (3 create) — the ticket came back with the right \
                         text but category {cat}, wanted {CATEGORY_ITEM}. The category byte is \
                         the head of CMSG_GMTICKET_CREATE; a wrong value there shifts the map id \
                         and the whole position."
                    );
                    probe.fails += 1;
                    probe.phase = Phase::Done;
                    return;
                }
                info!(
                    "PROBE_GMTICKET: PASS (3 create) — the server filed and echoed the ticket: \
                     arg1 == {cat} (Item) and arg2 == {text:?} byte for byte. The u8 category, \
                     the map/position block, the text and the trailing \"Reserved for future \
                     use\" cstring are all laid out the way vmangos reads them."
                );
                probe.passes += 1;
                report_db_expectation(&probe, "after create", &want);
                warn!(
                    "PROBE_GMTICKET: SKIP (4 db) — operator step, by design: the client never \
                     reads map/position back, so only the server's own row proves they landed. \
                     After the run: {DB_QUERY} — and compare against the db-expect line(s)."
                );
                let text = unique_text(" edited");
                if !run_or_skip(
                    &mut probe,
                    &script,
                    "5 edit",
                    &format!("UpdateGMTicket({CATEGORY_ITEM}, \"{text}\")"),
                ) {
                    return;
                }
                probe.edit_text = text;
                probe.phase = Phase::EditGap { since: now };
            } else if now - since > ANSWER_TIMEOUT_SECS {
                error!(
                    "PROBE_GMTICKET: FAIL (3 create) — the ticket never came back within \
                     {ANSWER_TIMEOUT_SECS}s. Wanted arg2 == {want:?}; answers seen after the \
                     baseline: {:?}. vmangos answers a REFUSED create with silence — queue off, \
                     under GMTickets.MinLevel, or category >= 11 — and a malformed body reads as \
                     exactly this. Step 1 already proved the queue is up, so a wrong create body \
                     is the live suspect.",
                    &seen[baseline.min(seen.len())..]
                );
                probe.fails += 1;
                probe.phase = Phase::Done;
            }
        }

        // Step 5 — the edit, and with it the category byte on CMSG_GMTICKET_UPDATETEXT.
        Phase::EditGap { since } => {
            if now - since < WRITE_GAP_SECS {
                return;
            }
            let baseline = ticket_answers(&script).len();
            if !run_or_skip(&mut probe, &script, "5 edit", "GetGMTicket()") {
                return;
            }
            probe.phase = Phase::EditGet {
                since: now,
                baseline,
            };
        }
        Phase::EditGet { since, baseline } => {
            let seen = ticket_answers(&script);
            let want = probe.edit_text.clone();
            // Equality, never `contains`: a leading control byte in the stored text is precisely
            // the cmangos-style failure this step exists to catch (decision 1673's divergence).
            if let Some(i) = matching(&seen, baseline, |(_, t)| *t == want) {
                let (cat, text) = &seen[i];
                info!(
                    "PROBE_GMTICKET: PASS (5 edit) — the edit landed and echoed EXACTLY: arg2 == \
                     {text:?} (arg1 == {cat}). No stray leading byte, so the server read our \
                     category as a category and the text as text — CMSG_GMTICKET_UPDATETEXT's \
                     `u8 type; cstring text` is right for this server."
                );
                probe.passes += 1;
                report_db_expectation(&probe, "after edit", &want);
                if !run_or_skip(&mut probe, &script, "6 abandon", "DeleteGMTicket()") {
                    return;
                }
                probe.phase = Phase::AbandonGap { since: now };
            } else if now - since > ANSWER_TIMEOUT_SECS {
                let stray = seen
                    .iter()
                    .skip(baseline)
                    .any(|(_, t)| t.len() > want.len() && t.ends_with(&want));
                error!(
                    "PROBE_GMTICKET: FAIL (5 edit) — the edited text never came back within \
                     {ANSWER_TIMEOUT_SECS}s. Wanted arg2 == {want:?}; answers seen after the \
                     baseline: {:?}. leading-byte-swallowed={stray} — if that is true, the server \
                     read our category byte as the first character of the text, which is the \
                     cmangos-classic behaviour decision 1673 records.",
                    &seen[baseline.min(seen.len())..]
                );
                probe.fails += 1;
                probe.phase = Phase::Done;
            }
        }

        // Step 6 — abandon. vmangos keeps the row (CloseTicket → closed_by + SaveToDB), so the
        // operator's SQL still finds it afterwards.
        Phase::AbandonGap { since } => {
            if now - since < WRITE_GAP_SECS {
                return;
            }
            let baseline = ticket_answers(&script).len();
            if !run_or_skip(&mut probe, &script, "6 abandon", "GetGMTicket()") {
                return;
            }
            probe.phase = Phase::AbandonGet {
                since: now,
                baseline,
            };
        }
        Phase::AbandonGet { since, baseline } => {
            let seen = ticket_answers(&script);
            if matching(&seen, baseline, |(c, _)| *c == NO_TICKET).is_some() {
                info!(
                    "PROBE_GMTICKET: PASS (6 abandon) — UPDATE_TICKET fired with arg1 == 0 again: \
                     the ticket is gone from the player's view (vmangos marks it closed_by and \
                     keeps the row, so the db-expect rows above still resolve)"
                );
                probe.passes += 1;
            } else if now - since > ANSWER_TIMEOUT_SECS {
                error!(
                    "PROBE_GMTICKET: FAIL (6 abandon) — still holding a ticket \
                     {ANSWER_TIMEOUT_SECS}s after DeleteGMTicket() + GetGMTicket(). Answers seen \
                     after the baseline: {:?}",
                    &seen[baseline.min(seen.len())..]
                );
                probe.fails += 1;
            } else {
                return;
            }
            probe.phase = Phase::Done;
        }

        Phase::Done => {
            if probe.exited {
                return;
            }
            probe.exited = true;
            info!(
                "PROBE_GMTICKET: DONE pass={} fail={}",
                probe.passes, probe.fails
            );
            // The probe self-exit pattern (`ProbeExitPlugin::fire_probe_exit`): a polite AppExit
            // plus a hard backstop thread, so a net/winit teardown hang can't leave a zombie
            // client holding the probe account.
            exit.write(AppExit::Success);
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(5));
                warn!("PROBE_GMTICKET: still alive 5s after AppExit — hard exit");
                std::process::exit(0);
            });
        }
    }
}

/// Run one binding in the live VM, or SKIP the whole probe with the reason.
///
/// A binding that will not run is environmental (no VM, a broken chunk) — never a wire verdict —
/// so it must not print a FAIL. Returns `false` once the probe has been parked in [`Phase::Done`].
fn run_or_skip(probe: &mut GmTicketProbe, script: &UiScript, step: &str, chunk: &str) -> bool {
    match script.run(chunk) {
        Ok(()) => {
            info!("PROBE_GMTICKET: ({step}) {chunk} run in the live VM");
            true
        }
        Err(e) => {
            warn!(
                "PROBE_GMTICKET: SKIP ({step}) — `{chunk}` would not run in the live VM: {e} \
                 (environmental, not a wire failure)"
            );
            probe.phase = Phase::Done;
            false
        }
    }
}

/// Print step 4's half of the check: exactly what the DB row must carry.
///
/// The position is the one sampled at the `NewGMTicket` call — what the drain stamps into the
/// packet, in that frame or the next. How much the body could have moved inside that window is
/// measured once, at the frame after the call, and reported here as `drift=`; a value at or under
/// [`STAMP_DRIFT_EPS`] means the line is exact. The map id and `ticket_type` are exact regardless.
/// **Nothing here is a PASS** — the row is the operator's to read.
fn report_db_expectation(probe: &GmTicketProbe, when: &str, message: &str) {
    let [x, y, z] = probe.create_pos;
    info!(
        "PROBE_GMTICKET: db-expect ({when}) map={} pos={x},{y},{z} ticket_type={CATEGORY_ITEM} \
         message={message:?} drift={:.3}",
        probe.create_map,
        probe.stamp_drift.unwrap_or(0.0)
    );
}

/// Straight-line distance between two WoW-space points, in yards.
fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The match-or-timeout rule: a step accepts **any** answer after its baseline that matches,
    /// because unsolicited answers (a GM's `.ticket viewid`, vmangos's own post-delete push) share
    /// the stream with the solicited ones. It must never accept one from *before* the baseline.
    #[test]
    fn a_step_matches_after_its_baseline_and_never_before_it() {
        let answers = vec![
            (0.0, String::new()),
            (4.0, "mine".to_string()),
            (0.0, String::new()),
            (4.0, "mine".to_string()),
        ];
        assert_eq!(matching(&answers, 0, |(_, t)| t == "mine"), Some(1));
        // Baseline past the first match: the later identical answer is the one taken.
        assert_eq!(matching(&answers, 2, |(_, t)| t == "mine"), Some(3));
        // Nothing after the baseline matches — the caller's timeout is what turns this into a
        // FAIL, and it must not be papered over by an earlier answer.
        assert_eq!(matching(&answers, 4, |(_, t)| t == "mine"), None);
        assert_eq!(matching(&answers, 2, |(c, _)| *c == NO_TICKET), Some(2));
    }

    /// The two texts within a run are distinct — step 5's assertion is "the text CHANGED to this",
    /// and it would be vacuous if the edit reused the create's string.
    #[test]
    fn the_create_and_edit_texts_are_distinct() {
        assert_ne!(unique_text(""), unique_text(" edited"));
        assert!(unique_text(" edited").ends_with(" edited"));
    }
}
