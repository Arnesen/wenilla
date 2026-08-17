//! The CPU clocks the instruments are denominated in — process, main thread, and machine.
//!
//! Three different questions, three different calls. [`process_cpu_secs`] is the campaign's
//! currency (decisions 0711/0736/0717): *work per frame*, immune to the present grant.
//! [`main_thread_cpu_secs`] narrows that to the serialized half — the part a hitch is actually made
//! of. [`system_cpu_ticks`] says how loaded the machine was while we measured, so two legs can be
//! told apart from two moods of the same leg (1157).

/// Whole-process CPU seconds consumed so far — **user + system, summed across every thread**
/// (`getrusage(RUSAGE_SELF)`).
///
/// The perf probes report wall-clock frame time, which on this machine is not a usable regression
/// instrument: parallel session worktrees build on the same 14 cores, and two identical probe runs
/// of the same pin came back 49.6 ms and 28.6 ms apart purely on machine load. CPU-per-frame moves
/// with the work we actually do, not with who else is compiling — and it is the metric the Mac
/// report is written in ("250 % CPU at 59 fps" against 1.12.1's "100 % at 160"), so a probe that
/// prints it can be compared against a reporter's number directly.
///
/// Non-unix returns `None`: the probes print the field only where the platform answers.
pub(crate) fn process_cpu_secs() -> Option<f64> {
    #[cfg(unix)]
    {
        // SAFETY: `getrusage` writes a fully-initialized `rusage` into the out-param and reads
        // nothing from it; zeroed is a valid starting value for a C struct of plain integers.
        unsafe {
            let mut ru: libc::rusage = std::mem::zeroed();
            if libc::getrusage(libc::RUSAGE_SELF, &mut ru) != 0 {
                return None;
            }
            let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 * 1e-6;
            Some(secs(ru.ru_utime) + secs(ru.ru_stime))
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// CPU seconds consumed by **the calling thread** (`CLOCK_THREAD_CPUTIME_ID`).
///
/// The twin [`process_cpu_secs`] cannot answer the question a hitch actually poses. It sums every
/// thread, so a frame in which an asset worker spent 12 ms decompressing reads identically to a
/// frame in which the main thread blocked for 12 ms — the first costs nothing the player can see,
/// the second *is* the stutter. Narrowing to one thread separates them.
///
/// **Only meaningful when called from the thread you mean.** Bevy's executor runs systems across
/// the task pool, so a caller that wants the *main* thread's number must pin itself there with a
/// [`NonSendMarker`](bevy::ecs::system::NonSendMarker) param; without it the reading silently
/// becomes "whichever worker happened to run this system", which is noise shaped like a
/// measurement.
///
/// Non-unix returns `None`, like its twin.
pub(crate) fn main_thread_cpu_secs() -> Option<f64> {
    #[cfg(unix)]
    {
        // SAFETY: `clock_gettime` writes a fully-initialized `timespec` into the out-param and
        // reads nothing from it; zeroed is a valid starting value for two plain integers.
        unsafe {
            let mut ts: libc::timespec = std::mem::zeroed();
            if libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) != 0 {
                return None;
            }
            Some(ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9)
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Cumulative **machine-wide** CPU ticks as `(busy, total)` — every core, every process, us
/// included (`host_statistics64(HOST_CPU_LOAD_INFO)`). Diff two samples and `busy/total` is the
/// fraction of all cores that were doing work between them.
///
/// Why the probes need it, when [`process_cpu_secs`] above already promises load-independence:
/// that promise is **weaker than it reads**, and 1157 caught it. `cpu_ms` is per-frame work rather
/// than wall time, so it shrugs off the scheduling delay that makes frame time useless under load
/// — but it is not immune. The same LBRS pin leg, same binary, same day, read **25.93** while two
/// other slots ran `cargo test --workspace` (load 20-32 on 14 cores) and **18.80-20.12** on a quiet
/// machine: a ~35 % inflation, far outside the ±0.4 band the campaign reasons in (0736). Cache and
/// memory-bandwidth contention is work we genuinely do; `getrusage` counts it and cannot tell us
/// it was someone else's fault.
///
/// So the leg has to say how loaded the machine was, and the reader compares. Deliberately a
/// **stamp, not a gate** (the director's call): no threshold is invented here. Two legs at similar
/// `sys_busy_pct` are comparable; two legs far apart are not, whatever their `cpu_ms` says. That
/// is a relative rule, which is the only kind the four calibration points behind 1157 support.
///
/// **Do not reach for `uptime` instead.** Load average is a 1-minute *decaying* mean and lags
/// badly: a leg stamped 99 % here was taken at load 5.55, seconds after a build burst the average
/// had not caught up with. Cross-checked against an independent out-of-process sampler over the
/// same window — 44 % vs 44.5 % — so where the two disagree, this is the one that is right.
///
/// Non-macOS returns `None`: the probes print the field only where the platform answers.
///
/// The `deprecated` allow is `libc::mach_host_self`, whose deprecation note says "use the `mach2`
/// crate instead". Checked, and it does not apply here: `mach2` 0.5.0 carries `mach_host_self` and
/// **nothing else this call needs** — no `host_statistics64`, no `host_cpu_load_info`, no
/// `HOST_CPU_LOAD_INFO`. Taking the advice would split one call across two crates and add a direct
/// dependency to get the *port* while `libc` still supplies the call it is passed to. Revisit if
/// `mach2` ever grows the host-statistics surface.
#[allow(deprecated)]
pub(crate) fn system_cpu_ticks() -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `host_statistics64` fills at most `count` u32s into the out-param and reads
        // nothing from it; `host_cpu_load_info` is plain integers, so zeroed is a valid starting
        // value. `mach_host_self` returns the global host port — no send right to deallocate.
        unsafe {
            let mut info: libc::host_cpu_load_info = std::mem::zeroed();
            let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
            if libc::host_statistics64(
                libc::mach_host_self(),
                libc::HOST_CPU_LOAD_INFO,
                (&raw mut info).cast(),
                &mut count,
            ) != libc::KERN_SUCCESS
            {
                return None;
            }
            let total: u64 = info.cpu_ticks.iter().map(|&x| u64::from(x)).sum();
            let idle = u64::from(info.cpu_ticks[libc::CPU_STATE_IDLE as usize]);
            Some((total - idle, total))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}
