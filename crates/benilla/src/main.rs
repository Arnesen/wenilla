//! The `benilla` launcher — a shim whose whole job is to carry `build.rs`'s git stamp.
//!
//! Everything real lives in `benilla-app`; this package exists so the build-id stamp — whose
//! watched git paths make cargo re-dirty the package on **every** commit, rebase, and checkout
//! (see `build.rs`'s header) — invalidates these few lines and a relink instead of the whole app
//! crate, its clippy pass, and its integration-test links. Decision 0993 has the measurements.

use benilla_app::BuildId;

fn main() -> benilla_app::AppExit {
    benilla_app::run(BuildId {
        sha: env!("BENILLA_GIT_SHA"),
        short: env!("BENILLA_GIT_SHORT"),
        date: env!("BENILLA_GIT_DATE"),
        profile: env!("BENILLA_PROFILE"),
    })
}
