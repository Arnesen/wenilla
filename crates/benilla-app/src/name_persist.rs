//! **The name cache, kept across sessions** (decision 1689) — the load/save half of
//! [`crate::names::NameCache`], and benilla's answer to the reference's `WDB/*.wdb` files.
//!
//! ## Why this exists at all
//!
//! Three of benilla's caches are filled by a server round trip: player names, creature templates
//! and pet names. The reference persists all three (`namecache.wdb`, `creaturecache.wdb`,
//! `petnamecache.wdb`), so on a machine that has played before, an answer is usually *already
//! there* when the packet that needs it arrives. benilla asked fresh every login, which made a
//! race the reference has practically closed wide open for us — the director hit it as a Lua error
//! when unstabling a pet, where `PetStable_Update` hands a nil `UnitName("pet")` to a
//! `GameTooltip:SetText` whose signature requires a string (decision 1688 closed that one path by
//! seeding from the stable list; this closes the class).
//!
//! ## The law, from the carve
//!
//! wow-re carved the whole `DBCache.cpp` machine (`system/dbcache/dbcache.md`, T3 — all 12 record
//! decoders diffed bit-exact). Two of its contracts are the ones a re-implementation must honour,
//! and both are about what the cache does *not* do:
//!
//! - **The header is compared by equality, and carries no checksum, no timestamp and no TTL.** Its
//!   20 bytes are `[FourCC | build 0x16f3 | locale | recordSize | version 1]`. A mismatch discards
//!   the file wholesale. Ours is a header line with the same job ([`NameCache::to_tsv`]).
//! - **Eviction is explicit only** — a high-bit key in a response, or `SMSG_INVALIDATE_PLAYER`
//!   (`0x31C` → remove-by-key `0x556ff0`). Nothing ages out. So a persisted name lives until the
//!   server says otherwise, which is why that opcode had to be wired before anything was written
//!   to disk: in memory a stale name costs a session, on disk it costs forever.
//!
//! ## Where it lives, and the one place we deviate
//!
//! The reference writes `WDB/` **inside the install**, which is precisely where benilla may not
//! write (the contract's read-only rule). Ours goes to `benilla-config/cache/<realm>.tsv` through
//! [`crate::local_state`], like every other thing we persist — and it is **realm-scoped**, which
//! the reference's is not. That is a fix rather than a preference: every key is realm-local (a
//! player guid, a creature entry, a pet number), so one shared file would serve another realm's
//! names to this one.

use std::path::PathBuf;

use bevy::prelude::*;

use crate::char_select::Roster;
use crate::names::NameCache;

/// How long a run of landed answers is allowed to accumulate before it is written. The cache is
/// worth keeping but never worth a stall: names arrive in bursts (a city's worth of creature
/// queries on zone-in), and writing the whole file per answer would turn a burst into hundreds of
/// rewrites. A crash inside the window costs re-asking, which is exactly what the cache is for.
const SAVE_DEBOUNCE: f32 = 10.0;

pub(crate) struct NamePersistPlugin;

impl Plugin for NamePersistPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NameCacheFile>()
            .add_systems(Update, (load_name_cache, save_name_cache).chain());
        // The exit edge (decision 1528): the last burst of answers is worth the one write, and it
        // is the only chance to take it — the debounce above will not fire again.
        crate::shutdown::on_app_exit(app, save_on_exit.into_configs());
    }
}

/// Where this realm's cache lives and what we last wrote there.
#[derive(Resource, Default)]
pub(crate) struct NameCacheFile {
    /// The realm the loaded file belongs to; `None` before the first login. A *change* is what
    /// triggers a load — logging into a second realm must not keep the first's names.
    realm: Option<String>,
    path: Option<PathBuf>,
    /// [`NameCache::generation`] as of the last successful write. The cache's own landed-answer
    /// counter is the dirty bit — it ticks on an arrival and never on an ask, which is exactly the
    /// edge worth writing on.
    saved_generation: u64,
    since_save: f32,
}

/// Load the realm's cache the first frame its identity is known, and on any realm change.
fn load_name_cache(
    roster: Res<Roster>,
    mut file: ResMut<NameCacheFile>,
    mut names: ResMut<NameCache>,
) {
    let Some((realm, _)) = crate::ui_macro::identity(&roster) else {
        return;
    };
    if file.realm.as_deref() == Some(realm.as_str()) {
        return;
    }
    let path = crate::local_state::name_cache_path(&realm);
    file.realm = Some(realm.clone());
    file.path = path.clone();
    file.saved_generation = names.generation();
    let Some(path) = path else {
        return; // a hermetic capture, or no state folder — session-only, exactly as before
    };
    // Through `local_state`, not `std::fs`: on the web the state folder is `localStorage`.
    let Ok(text) = crate::local_state::read_to_string(&path) else {
        return; // no cache yet: the ordinary first-run path, not an error
    };
    match NameCache::from_tsv(&text, &realm) {
        Some(loaded) => {
            let n = loaded.len();
            // Replace rather than merge: this is the *start* of a realm's session, so there is
            // nothing of this realm's in memory to preserve, and a merge would silently keep the
            // previous realm's records alive under their own keys.
            *names = loaded;
            file.saved_generation = names.generation();
            debug!(
                "names: loaded {n} cached records for {realm} from {}",
                path.display()
            );
        }
        // The header did not match — a different build, locale or format. Discarding the whole
        // file is the reference's own rule, and the file is left on disk to be overwritten by the
        // first save rather than deleted out from under a player who may be switching back.
        None => debug!(
            "names: discarding {} — header is not this build/locale/format",
            path.display()
        ),
    }
}

/// Write the cache when answers have landed since the last write and the debounce has elapsed.
fn save_name_cache(time: Res<Time>, mut file: ResMut<NameCacheFile>, names: Res<NameCache>) {
    file.since_save += time.delta_secs();
    if file.since_save < SAVE_DEBOUNCE {
        return;
    }
    file.since_save = 0.0;
    write_now(&mut file, &names);
}

/// The exit-edge write (decision 1528) — unconditional on the debounce, because there is no next
/// frame to defer to.
fn save_on_exit(mut file: ResMut<NameCacheFile>, names: Res<NameCache>) {
    write_now(&mut file, &names);
}

fn write_now(file: &mut NameCacheFile, names: &NameCache) {
    if names.generation() == file.saved_generation {
        return; // nothing landed since the last write
    }
    let (Some(path), Some(realm)) = (file.path.clone(), file.realm.clone()) else {
        return;
    };
    // `write_atomic` creates the folder itself (and has none to create on the web).
    match crate::local_state::write_atomic(&path, &names.to_tsv(&realm)) {
        Ok(()) => {
            file.saved_generation = names.generation();
            debug!("names: wrote {} records to {}", names.len(), path.display());
        }
        // A cache we could not write is a cache we re-fill next session — worth a line, never
        // worth failing a session over.
        Err(e) => warn!("names: cannot write {}: {e}", path.display()),
    }
}
