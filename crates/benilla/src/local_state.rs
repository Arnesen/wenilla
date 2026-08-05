//! Where benilla keeps **local state** — THE LAW (decision 0954). Every file benilla persists on
//! a player's machine — config today; realmlist, caches, per-character state as they arrive —
//! lives in **one visible folder, `benilla/`, beside the WoW install's own `Data/`**. Never
//! scattered into the install, never hidden in a platform config dir: the 1.12 client is itself
//! fully portable (`WTF/`, `realmlist.wtf`, `Cache/` all in-folder — verified on the live
//! install), its community expects the drop-in-a-folder shape, and one folder answers "what here
//! is benilla's?" at a glance (the director's call, 2026-08-04; prior art: Dolphin's
//! `portable.txt`, DREAMM's config-next-to-exe — we skip the marker file because benilla always
//! knows the install root from `$WOW_DATA`).
//!
//! **Every feature with local state resolves its paths through this module** — nothing else in
//! the tree may compute a persistence path (grep `local_state::` to see every resident). New
//! files get a name here and a line in decision 0954's layout table.
//!
//! Resolution, in order:
//! 1. **`$BENILLA_HOME`** — explicit override (tests point it at a tempdir; a second install or
//!    a shared-config setup points it wherever it likes).
//! 2. **`<install root>/benilla/`** — the parent of `$WOW_DATA` (default `WoW/Data` → `WoW/
//!    benilla/`). Gated on the Data dir actually existing, so a dev run without an install never
//!    plants a phantom folder in some working directory.
//!
//! **Capture/probe runs are hermetic**: with `$WOW_CAPTURE` set every path resolves to `None` —
//! a deterministic capture must not read one machine's saved settings or write anything back
//! (the harness owns its inputs; decision 0008's reproducibility posture). `None` is also the
//! honest answer when no install exists: features degrade to session-only state, warn once, and
//! keep running.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The one benilla-state folder, or `None` when persistence is off (hermetic capture run, or no
/// resolvable install). Existence is NOT guaranteed — [`write_atomic`] creates it on first write;
/// readers just try their file and treat absent as defaults.
pub(crate) fn home() -> Option<PathBuf> {
    if std::env::var_os("WOW_CAPTURE").is_some() {
        return None; // hermetic: captures neither read nor write player state
    }
    if let Some(over) = std::env::var_os("BENILLA_HOME") {
        return Some(PathBuf::from(over));
    }
    let data = PathBuf::from(std::env::var("WOW_DATA").unwrap_or_else(|_| "WoW/Data".into()));
    if !data.is_dir() {
        return None; // no install, nothing to sit beside — session-only state
    }
    Some(data.parent().unwrap_or(Path::new(".")).join("benilla"))
}

/// `benilla/config.toml` — the CVar overrides (the `Config.wtf` analog; see `crate::cvars`).
pub(crate) fn config_path() -> Option<PathBuf> {
    home().map(|h| h.join("config.toml"))
}

/// `benilla/macros/account.txt` — the account-wide macro tab (indices 1..=18; decision 0983). One
/// file per scope rather than one file with two sections, because the two have different lifetimes:
/// the account tab follows the install, a character tab dies with its character.
pub(crate) fn macros_account_path() -> Option<PathBuf> {
    home().map(|h| h.join("macros/account.txt"))
}

/// `benilla/macros/<realm>-<character>.txt` — the per-character macro tab (indices 19..=36). The
/// reference nests these (`WTF/Account/<ACC>/<REALM>/<CHAR>/macros-cache.txt`); benilla flattens to
/// one `macros/` folder because [`home`] is already account-scoped by the install it sits beside.
pub(crate) fn macros_character_path(realm: &str, character: &str) -> Option<PathBuf> {
    let key = format!("{}-{}", file_token(realm), file_token(character));
    home().map(|h| h.join("macros").join(format!("{key}.txt")))
}

/// Make an arbitrary realm/character name safe as one path component: anything outside
/// `[A-Za-z0-9_]` becomes `_`, so a realm called `Hydraxian Waterlords` or one with a slash cannot
/// escape the folder or collide with the path separator.
fn file_token(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if t.is_empty() {
        "unknown".into()
    } else {
        t
    }
}

/// Write a state file atomically: parent dirs created, contents to `<path>.tmp`, then rename —
/// a crash mid-write leaves the old file intact, never a truncated one. The one write path for
/// every resident of the folder.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Shared test plumbing for anything that toggles the persistence env vars: `std::env::set_var`
/// is process-global and cargo runs tests in threads, so every such test takes [`ENV_LOCK`] and
/// scopes its overrides in [`EnvGuard`]s (restore-on-drop). Used here and by `crate::cvars`'s
/// end-to-end test.
#[cfg(test)]
pub(crate) mod test_env {
    pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) struct EnvGuard(&'static str, Option<std::ffi::OsString>);
    impl EnvGuard {
        pub(crate) fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var_os(key);
            std::env::set_var(key, value);
            Self(key, old)
        }
        pub(crate) fn unset(key: &'static str) -> Self {
            let old = std::env::var_os(key);
            std::env::remove_var(key);
            Self(key, old)
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.1 {
                Some(v) => std::env::set_var(self.0, v),
                None => std::env::remove_var(self.0),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_env::{EnvGuard, ENV_LOCK};
    use super::*;

    #[test]
    fn the_home_law_override_then_install_root_and_hermetic_captures() {
        let _l = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("benilla-ls-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("Data")).unwrap();

        // 2 · install root: home = parent-of-Data /benilla.
        let _c = EnvGuard::unset("WOW_CAPTURE");
        let _h = EnvGuard::unset("BENILLA_HOME");
        let _d = EnvGuard::set("WOW_DATA", tmp.join("Data").to_str().unwrap());
        assert_eq!(home(), Some(tmp.join("benilla")));
        assert_eq!(config_path(), Some(tmp.join("benilla/config.toml")));
        // The macro residents (decision 0983): one account file, one per character, and every
        // realm/character name reduced to a safe single path component.
        assert_eq!(
            macros_account_path(),
            Some(tmp.join("benilla/macros/account.txt"))
        );
        assert_eq!(
            macros_character_path("Hydraxian Waterlords", "Probeone"),
            Some(tmp.join("benilla/macros/Hydraxian_Waterlords-Probeone.txt"))
        );
        assert_eq!(
            macros_character_path("../evil", "a/b"),
            Some(tmp.join("benilla/macros/___evil-a_b.txt")),
            "no name can escape the folder"
        );

        // 1 · the explicit override wins over the install root.
        let _h2 = EnvGuard::set("BENILLA_HOME", tmp.join("elsewhere").to_str().unwrap());
        assert_eq!(home(), Some(tmp.join("elsewhere")));

        // 0 · hermetic: a capture run resolves nothing, even with an override set.
        let _c2 = EnvGuard::set("WOW_CAPTURE", "ui-options");
        assert_eq!(home(), None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn no_install_means_no_phantom_folder() {
        let _l = ENV_LOCK.lock().unwrap();
        let _c = EnvGuard::unset("WOW_CAPTURE");
        let _h = EnvGuard::unset("BENILLA_HOME");
        let _d = EnvGuard::set("WOW_DATA", "/nonexistent/benilla-test/Data");
        assert_eq!(home(), None);
    }

    #[test]
    fn write_atomic_creates_dirs_and_replaces_whole_files() {
        let _l = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("benilla-wa-{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        let path = tmp.join("nested/config.toml");
        write_atomic(&path, "a = \"1\"\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = \"1\"\n");
        write_atomic(&path, "a = \"2\"\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = \"2\"\n");
        assert!(!path.with_extension("tmp").exists(), "tmp renamed away");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
