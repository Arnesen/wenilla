//! wasm32: the one-shot SFX cache's misses, taken off the frame.
//!
//! ## The stutter this closes
//!
//! [`super::SoundKits::sfx`] is the decode cache every kit play goes through — weapon swings and
//! impacts, spell casts, creature barks, footsteps, UI clicks. On a miss the native arm reads the
//! file out of the MPQ and decodes it inline, which is cheap. On wasm that same miss was a
//! synchronous `XMLHttpRequest` (`benilla-formats`' `web.rs`) followed by a wasm decode, both on
//! the main thread: one network round trip plus a decode, paid on the frame, **once per file the
//! session has not heard yet**. A fight against a creature type you had not met, a new weapon
//! material, a spell you had not cast — each brought several first plays, each a short stall.
//! The cache is dropped on every map change (`evict_kit_cache`), so the stalls came back after
//! every teleport. It is the same freeze [`crate::sound::web_load`] closed at the zone line,
//! smaller and more often.
//!
//! ## The fix: start the load, replay the shot when it lands
//!
//! A miss now hands the fetch and the decode to the browser through [`web_load::begin_sfx`] and
//! answers `None`. The play that asked is recorded as a [`Deferred`] shot against the in-flight
//! load — the kit, the variation already drawn, the position, the category, the extras — and
//! [`land_sfx_loads`] replays it through the ordinary play path on the frame the browser answers.
//! The replay carries the variation as an explicit index, so it is the file that was asked for
//! and the depleting pool is not drawn twice. Every gate runs again on the replay, which is what
//! you want: a bark whose unit died in the meantime is refused by the same rules that refuse
//! any other play.
//!
//! **A shot that lands late is dropped, not played late.** [`LATE_WINDOW`] bounds how far behind
//! its event a one-shot may sound; past it the file goes into the cache silently and the *next*
//! play of it is instant. A hit sound a whole second after the hit is worse than no hit sound,
//! and this is the reference's own answer for a file its async loader had not delivered.
//!
//! ## Pre-warming
//!
//! [`prewarm_creature_voices`] starts the loads for a creature's vocal kits when the creature
//! appears, so its aggro roar and its wound and death cries are in the cache before the first
//! swing. Nothing plays; the loads are the same off-frame loads a miss would start, only earlier.

use std::time::Duration;

use bevy::platform::time::Instant;
use bevy::prelude::*;

use benilla_assets::{LockRecover, WorldAssets};
use benilla_protocol::events::EntityKind;

use super::{
    play_file, play_kit_ext, KitRef, PlayExtras, SoundCategory, SoundKits, StaticSoundData,
};
use crate::sound::creature::CreatureVoices;
use crate::sound::{web_load, AudioListener, SoundConfig, SoundOutput};

/// How long after its event a one-shot may still sound. Loopback lands a small WAV in one or two
/// frames; a real network in a few more. Past this the shot is dropped and only the cache fills.
pub(super) const LATE_WINDOW: Duration = Duration::from_millis(200);

/// One SFX file in flight, and every play that asked for it while it was.
pub(super) struct PendingSfx {
    load: web_load::Pending,
    shots: Vec<(Instant, Deferred)>,
}

/// A play recorded against a load — everything [`play_kit_ext`] or [`play_file`] needs to be
/// called again exactly as it was, minus the listener position, which the replay reads live.
pub(super) enum Deferred {
    Kit {
        kit: u32,
        /// The variation already drawn from the depleting pool — replayed as an explicit index.
        variant: usize,
        pos: Option<Vec3>,
        category: SoundCategory,
        extras: PlayExtras,
    },
    File {
        path: String,
        category: SoundCategory,
    },
}

impl SoundKits {
    /// The wasm arm of [`SoundKits::sfx`] on a cache miss: begin the browser-side load unless one
    /// is already in flight for this file, and answer `None` either way. `key` is the cache key
    /// (the lowercased path); `path` the chain name as written.
    pub(super) fn sfx_or_begin(
        &mut self,
        assets: &WorldAssets,
        key: &str,
        path: &str,
    ) -> anyhow::Result<Option<StaticSoundData>> {
        if self.pending.contains_key(key) {
            return Ok(None);
        }
        // The lock is held to build the URL and dropped before the request starts.
        let url = assets.chain.lock_recover().url_for_name(path);
        let Some(url) = url else {
            anyhow::bail!("file not in patch chain: {path}");
        };
        self.pending.insert(
            key.to_string(),
            PendingSfx {
                load: web_load::begin_sfx(url, path.to_string()),
                shots: Vec::new(),
            },
        );
        Ok(None)
    }

    /// Record a play against the load in flight for `key`, to be replayed when it lands.
    pub(super) fn defer(&mut self, key: &str, shot: Deferred) {
        if let Some(p) = self.pending.get_mut(key) {
            p.shots.push((Instant::now(), shot));
        }
    }

    /// Start the loads for every file of `kit_id` that is neither cached nor in flight. Plays
    /// nothing. Unknown and file-less kits are ignored, like everywhere else.
    pub(super) fn prewarm(&mut self, assets: &WorldAssets, kit_id: u32) {
        let Some(kit) = self.catalog.get(kit_id) else {
            return;
        };
        let paths: Vec<String> = kit.files.iter().map(|(p, _)| p.clone()).collect();
        for path in paths {
            let key = path.to_ascii_lowercase();
            if self.cache.contains_key(&key) || self.pending.contains_key(&key) {
                continue;
            }
            if let Err(e) = self.sfx_or_begin(assets, &key, &path) {
                debug!("sfx prewarm (kit {kit_id}): {e:#}");
            }
        }
    }
}

/// Once a frame: move every load the browser has finished into the cache, and replay the shots
/// recorded against it that are still inside [`LATE_WINDOW`]. Ordered before the channel pump so
/// a shot replayed here is pumped on the same frame it starts.
pub(super) fn land_sfx_loads(
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    let (Some(mut kits), Some(assets)) = (kits, assets) else {
        return;
    };
    if kits.pending.is_empty() {
        return;
    }
    let mut landed = Vec::new();
    kits.pending.retain(|key, p| match p.load.take() {
        None => true,
        Some(result) => {
            landed.push((key.clone(), result, std::mem::take(&mut p.shots)));
            false
        }
    });
    let now = Instant::now();
    let listener = listener.pos;
    for (key, result, shots) in landed {
        let data = match result {
            Ok(d) => d,
            Err(e) => {
                warn!("sfx: {key} — {e}");
                continue;
            }
        };
        kits.cache.insert(key.clone(), data);
        for (asked, shot) in shots {
            let late = now.duration_since(asked);
            if late > LATE_WINDOW {
                debug!(
                    "sfx: {key} landed {} ms after the play that asked — cached, not played",
                    late.as_millis()
                );
                continue;
            }
            let replayed = match shot {
                Deferred::Kit {
                    kit,
                    variant,
                    pos,
                    category,
                    extras,
                } => play_kit_ext(
                    &mut kits,
                    &assets,
                    &mut out,
                    &config,
                    listener,
                    KitRef::Id(kit),
                    pos,
                    category,
                    PlayExtras {
                        variant: Some(variant),
                        ..extras
                    },
                )
                .map(|_| ()),
                Deferred::File { path, category } => {
                    play_file(&mut kits, &assets, &mut out, &config, &path, category)
                }
            };
            if let Err(e) = replayed {
                debug!("sfx: replay of {key} — {e:#}");
            }
        }
    }
}

/// A creature that has just appeared (or changed its display) starts the loads for its vocal
/// kits — the barks a fight will ask for first. `Changed` covers the spawn and a later display
/// write alike, and fires rarely enough that the per-kit lookups cost nothing.
pub(super) fn prewarm_creature_voices(
    units: Query<&crate::net::NetEntity, Changed<crate::net::NetEntity>>,
    voices: Option<Res<CreatureVoices>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
) {
    let (Some(voices), Some(mut kits), Some(assets)) = (voices, kits, assets) else {
        return;
    };
    for net in &units {
        if !matches!(net.kind, EntityKind::Unit) {
            continue;
        }
        let Some(v) = net.display_id.and_then(|d| voices.0.for_display(d)) else {
            continue;
        };
        let kits_to_warm = [v.aggro, v.alert, v.death, v.stun]
            .into_iter()
            .chain(v.exertion)
            .chain(v.injury)
            .chain(v.custom_attack);
        for kit in kits_to_warm {
            if kit != 0 {
                kits.prewarm(&assets, kit);
            }
        }
    }
}
