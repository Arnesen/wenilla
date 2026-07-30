//! **Which build is this?** — the commit the running binary was built from, stamped in at compile
//! time by `build.rs` (which owns the *how*, including why the stamp can't go stale).
//!
//! A report from someone else's machine — "the water reads wrong here", "it panicked on login" — is
//! only actionable against known code, and nothing else in the binary identifies it: the crate
//! version is a permanent `0.1.0`, and people build from a clone of the public snapshot repo whose
//! HEAD moves with every sync. So the sha is the version.
//!
//! Two surfaces, deliberately:
//!
//! - **The startup log line** ([`banner`], run from [`crate::preflight`]'s banner family) — this is
//!   the one that matters for someone else's run. They launch from a terminal, so the id is already
//!   in the output they paste; nobody has to know a hotkey to answer "what are you on?".
//! - **The debug panel's footer** (`` ` ``) — the same line where the reader already is when they
//!   are looking at a readout, click-to-copy for the full sha.
//!
//! Mapping a reported sha back: a **public** snapshot sha is tagged `pub/<short-sha>` on the private
//! commit it shipped, so `git show pub/<sha>` (or `git rev-parse pub/<sha>^{commit}`) names the code
//! exactly; a **private** sha is just a commit here.

use bevy::prelude::*;

/// The full 40-char sha of the commit this binary was built from. Empty when the build had no git
/// checkout to read (a downloaded source zip, or no `git` on `PATH`).
pub(crate) const SHA: &str = env!("BENILLA_GIT_SHA");
/// Git's own abbreviation of [`SHA`], from the same repo `pubsync` abbreviates in — so a public
/// build's string is literally the suffix of its `pub/<sha>` tag.
pub(crate) const SHORT: &str = env!("BENILLA_GIT_SHORT");
/// The commit date of [`SHA`] (`YYYY-MM-DD`) — a property of the sha, so it can't disagree with it.
pub(crate) const DATE: &str = env!("BENILLA_GIT_DATE");
/// The cargo profile directory this was built in: `debug`, `release`, or `ship` (0736). Half of
/// every "it runs badly" report is a debug build.
pub(crate) const PROFILE: &str = env!("BENILLA_PROFILE");

/// The one-line build id: `f5fd009 · 2026-07-30 · release`.
pub(crate) fn summary() -> String {
    if SHORT.is_empty() {
        format!("unknown (built without a git checkout) · {PROFILE}")
    } else {
        format!("{SHORT} · {DATE} · {PROFILE}")
    }
}

/// Log the build id once at startup — the line that answers "what version are you on?" from a
/// pasted terminal log. Not env-gated, for [`crate::preflight`]'s reason: an id nobody knows to
/// switch on is not an id.
pub(crate) fn banner() {
    info!("benilla build {}", summary());
}
