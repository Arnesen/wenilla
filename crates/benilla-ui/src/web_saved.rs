//! wenilla carry: where an addon's saved-variables file is read from in the browser.
//!
//! `script::addon::load_saved_variables` reads `<saved dir>/<Addon>.lua` from disk. The browser
//! build has no filesystem: `benilla-app`'s state folder (`/benilla-config/…`) lives in
//! `localStorage`, which is also where the shutdown write puts these files. `std::fs::read` there
//! answers `Unsupported`, not `NotFound`, so every load-on-demand addon (`Blizzard_CombatText` at
//! world entry) raised a "operation not supported on this platform" error dialog and its saved
//! values never came back. `benilla-ui` cannot reach that store itself, so `benilla-app` installs
//! its reader here at boot; until it does, a file reads as absent.

use std::io::{Error, ErrorKind, Result};
use std::path::Path;
use std::sync::OnceLock;

static READER: OnceLock<fn(&Path) -> Result<Vec<u8>>> = OnceLock::new();

/// Install the state folder's reader; the first call wins.
pub fn set_reader(read: fn(&Path) -> Result<Vec<u8>>) {
    let _ = READER.set(read);
}

/// A saved-variables file's bytes; `NotFound` when absent, as on disk.
pub(crate) fn read(path: &Path) -> Result<Vec<u8>> {
    match READER.get() {
        Some(read) => read(path),
        None => Err(Error::new(
            ErrorKind::NotFound,
            "no state folder reader installed",
        )),
    }
}
