//! Zone music + ambience — the area-driven schedulers over the kit player (decision 0070 slice 1).
//!
//! The data chain: the player's [`CurrentArea`] (MCNK areaId) → `AreaTable` →
//! `ZoneMusic`/`SoundAmbience`/`ZoneIntroMusicTable` (parent-inherited, `benilla_formats::
//! AreaSoundCatalog::resolve`) → SoundEntries kits → streamed MP3/WAV.
//!
//! **Music transport** (wow-re `0x460040`–`0x460ca0`, `benilla-pins.md` **B15/B16**). Within one
//! zone, tracks are separated by a uniform random **silence interval** per day/night phase. On a
//! zone-music-id CHANGE the outgoing track fades to silence over **4.0 s** then stops (byte-exact:
//! `0x4602e0` → `0x7a5a10(0x40800000 = 4.0f)`) and the incoming track starts **immediately** — the
//! change forces the next-start time to −1 (`0x4602e0`: `DAT_00836400 = 0xffffffff`), so `0x460040`
//! opens the stream that tick (`0x460240`). The randomized silence interval ([`next_track_time`],
//! `0x4601f0`) is ONLY the *same-zone* track-to-track spacing — reusing it on a change (as decision
//! 0100 did) left a newly-entered zone silent for 3–5 min. **There is no cold start**: world entry
//! itself arms the same −1 (`0x401570` → `0x457770` → the zone-music init `0x45ffc0`, which sets
//! `DAT_00836400 = -1` at `0x45ffdb`), so the session's FIRST track is immediate too. `0x4601f0`'s
//! `== 0 → now + 6000 ms` branch is reachable only from the natural-end reap `0x4600b6` and only
//! ever *replaces* the −1, so it cannot fire on an entry — the 6 s wait benilla used to serve there
//! was that branch misapplied (wow-re §5 `glue-music-world-entry.md` §9, decision 1553). The incoming track starts at **full** volume, **no fade-in** — faithful (`0x460240` →
//! `0x7a5dc0`; §5 B16 refuted every music-slot fade-in primitive, and the director confirmed by ear
//! there is none). The reference's *perceived* slightly-soft onset is the delegated audio engine
//! priming the stream to full amplitude over ~a moment (FMOD there, kira's own stream-buffering
//! here), which leaves the 4.0 s fade audible — NOT owned gain math, flagged in wow-re for a live
//! capture if benilla's onset reads too abrupt. (The transition is **not** `0x457960` — that is the
//! SFX-bus auto-duck, decision 0100.) Intro music (`ZoneIntroMusicTable`) plays at full on zone
//! entry, throttled by its per-entry minute delay;
//! higher `Priority` wins when nested areas compete (we resolve one row).
//!
//! **The music slot holds exactly one stream**, and every start goes through
//! [`start_music_stream`], which fades the outgoing one out as it opens the incoming. Dropping a
//! kira handle does *not* stop its sound (that is precisely what the fade-stop rides on), so a
//! start that merely overwrote the handle would leave the old track playing forever, unreapable
//! and deaf to the volume slider.
//!
//! **Server-pushed music** (`SMSG_PLAY_MUSIC`) is a SoundEntries kit for that same slot, and the
//! server **re-pushes the same id on a timer** to keep an event track looping: the Darkmoon Faire's
//! emitter sends it every **5 s** (vmangos `go_darkmoon_faire_music`, commented as a sniffed retail
//! value). So a push for the kit already playing is a **no-op** ([`slot_holds`]) — the client's own
//! already-playing early-out (`0x4602e0` @`0x460342`: `cmp [0xb06cbc],eax; je` → desired == playing
//! → return); when the track *does* end, the next push restarts it, which is what makes the loop.
//! Any other push takes the slot the way a zone-music change does (outgoing 4.0 s fade, incoming at
//! full) — inferred from the slot's one transition idiom, as the `SMSG_PLAY_MUSIC` handler itself
//! is not pinned in wow-re.
//!
//! **Ambience transport** (`0x460b00`, the separate ambience updater; §5 B16). An area / interior /
//! day↔night / ghost ambience-id change is a **5.0 s crossfade**: the old bed fades out over 5.0 s
//! then stops (`0x7a5a10(0x40a00000 = 5.0f)`) while the new bed starts at volume 0 and fades in over
//! 5.0 s (`0x7a5dc0(0)` → `0x7a57b0(5.0f, kit vol)`). The **submerge/emerge** swap is the exception:
//! it takes `0x460b00`'s **instant** no-fade branch (`0x458650` → `0x460af0(param = 1)`) — correcting
//! both the old "250 ms area swap" INTERIM and B6's "5 s underwater fade".
//!
//! The day/night boundary is the server-synced game clock's hard step — **day iff
//! 05:30 ≤ t < 21:00** (minutes 330..1259), re-evaluated per tick, no fade (`0x4578c0` via the
//! `0x642710` range check; wow-re `benilla-pins.md` B4, VERIFIED).

use bevy::prelude::*;

use benilla_formats::AreaSoundCatalog;

use crate::net::{ServerSoundKind, ServerSoundMessage};
use benilla_assets::{AssetSet, LockRecover, WorldAssets};
use benilla_world::lighting::GameClock;
use benilla_world::schedule::WorldStage;

use super::kit::{play_kit_ext, KitRef, Latch, PlayExtras, SoundCategory, SoundKits};
use super::mixer::{self, StreamingSoundHandle};
use super::{AudioListener, SoundConfig, SoundOutput};

/// The zone→audio catalog (AreaTable + ZoneMusic + SoundAmbience + ZoneIntroMusicTable).
/// Absent when the client data didn't load.
#[derive(Resource)]
pub(crate) struct AreaSounds(pub(crate) AreaSoundCatalog);

/// The race → exploration-jingle catalog (`ChrRaces.dbc` column 3) — the discovery sound
/// `net::apply`'s `SMSG_EXPLORATION_EXPERIENCE` arm plays (decision 0829). Absent when the
/// client data didn't load.
#[derive(Resource)]
pub(crate) struct ExplorationSounds(pub(crate) benilla_formats::ExplorationSoundCatalog);

/// Day iff 05:30 ≤ clock < 21:00 (module docs — the client's hard step, B4-verified).
/// Index into the `[day, night]` DBC pairs.
fn phase(clock: &GameClock) -> usize {
    if (330..1260).contains(&clock.minute) {
        0
    } else {
        1
    }
}

/// SoundEntries 4123 `UnderWaterLoop` — the submerged ambience bed the client swaps the
/// ambience slot to (the swap is instant on submerge/emerge — §5 B16).
const UNDERWATER_LOOP_KIT: u32 = 4123;

/// The zone-music transition fade: on a zone-music-id change the outgoing track fades linearly to
/// silence over 4.0 s then stops, byte-exact (`0x4602e0` → `0x7a5a10(0x40800000 = 4.0f)`; wow-re §5
/// **B16**, decision 0100). Both directions. The incoming track starts immediately at FULL volume —
/// the client has no music fade-in (§5 B16, director-confirmed by ear), so this is the only fade on
/// the music slot. It rides the backend fade-stop — drop the handle, the ramp finishes on kira's
/// thread (verified device-free by `mixer::tests::stop_fade_ramps_after_handle_drop`).
const MUSIC_FADE_OUT_MS: u64 = 4000;

/// The zone-ambience crossfade: **5.0 s** for an area / interior / day↔night / ghost swap —
/// `0x460b00` fades the old bed out over 5.0 s (`0x7a5a10(0x40a00000 = 5.0f)`) and fades the new
/// bed in from silence over the same 5.0 s (`0x7a5dc0(0)` → `0x7a57b0(5.0f, kit vol)`). Byte-exact
/// (wow-re §5 B16). The submerge/emerge swap is NOT this — it is instant (see the reconcile).
const AMBIENCE_TRANSITION_FADE_MS: u64 = 5000;

/// A linear fade-in envelope for a freshly-started ambience bed: gain ramps 0→1 over `dur_secs`
/// from `start_secs` (`Time::elapsed_secs_f64`). benilla drives the incoming leg of the ambience
/// crossfade itself, per frame — the per-frame volume sync would otherwise snap the new bed
/// straight to full. (Music has no fade-in — §5 B16 — so only ambience uses this.)
#[derive(Clone, Copy)]
struct FadeIn {
    start: f64,
    dur: f64,
}

impl FadeIn {
    fn new(start: f64, dur_ms: u64) -> Self {
        Self {
            start,
            dur: dur_ms as f64 / 1000.0,
        }
    }

    /// Gain in `[0, 1]` at `now`; ramps linearly and holds at 1 once complete.
    fn gain(&self, now: f64) -> f32 {
        if self.dur <= 0.0 {
            return 1.0;
        }
        (((now - self.start) / self.dur) as f32).clamp(0.0, 1.0)
    }
}

/// The active fade-in gain for a slot, clearing the envelope once it reaches full so the
/// per-frame sync returns to a plain snap.
fn fade_in_gain(fade: &mut Option<FadeIn>, now: f64) -> f32 {
    match fade {
        Some(f) => {
            let g = f.gain(now);
            if g >= 1.0 {
                *fade = None;
            }
            g
        }
        None => 1.0,
    }
}

/// Scheduler state + the held stream handles. Non-Send (the handles aren't `Sync`).
pub(super) struct ZoneAudio {
    /// The `ZoneMusic` row currently scheduled (0 = none).
    zone_music: u32,
    /// The playing music stream (a zone track, an intro, or a server-pushed event track).
    music: Option<StreamingSoundHandle<kira::sound::FromFileError>>,
    /// Starvation watch over the music slot (decision 1109) — a streamed track whose decode
    /// thread is outrun zero-fills the mix, which no other meter sees.
    music_watch: mixer::StreamWatch,
    /// The SoundEntries kit on the music slot (0 = never started one) — the "what is playing" a
    /// repeat `SMSG_PLAY_MUSIC` is compared against ([`slot_holds`]).
    music_kit: u32,
    /// The playing track's kit base volume (multiplied under the category slider + fade).
    music_kit_vol: f32,
    /// When the next zone track starts (Time::elapsed_secs_f64; None = a track is playing or
    /// no music in this zone).
    next_track_at: Option<f64>,
    /// The looping ambience kit currently up (0 = none) + its handle and base volume. A static
    /// loop, not a stream ([`mixer::loop_from_bytes`] — streaming loop beds die at EOF).
    ambience_kit: u32,
    ambience: Option<mixer::StaticSoundHandle>,
    ambience_kit_vol: f32,
    /// The incoming leg of an ambience crossfade — `Some` while the new bed ramps up.
    ambience_fade_in: Option<FadeIn>,
    /// Intro throttle: intro row id → last-played time (secs), against `MinDelayMinutes`.
    intro_last: std::collections::HashMap<u32, f64>,
    /// The last area id acted on (change detection).
    area: Option<u32>,
    /// The last WMO interior acted on (change detection — the override layer, decision 0076).
    interior: Option<super::interior::InteriorAudio>,
    /// Last submersion state — a flip makes the ambience swap INSTANT (the underwater no-fade
    /// branch, wow-re §5 B16), vs the 5.0 s crossfade every other swap gets.
    was_underwater: bool,
    rng: u32,
}

impl Default for ZoneAudio {
    fn default() -> Self {
        Self {
            zone_music: 0,
            music: None,
            music_watch: mixer::StreamWatch::new("zone music"),
            music_kit: 0,
            music_kit_vol: 1.0,
            next_track_at: None,
            ambience_kit: 0,
            ambience: None,
            ambience_kit_vol: 1.0,
            ambience_fade_in: None,
            intro_last: std::collections::HashMap::new(),
            area: None,
            interior: None,
            was_underwater: false,
            rng: 0x1234_5677,
        }
    }
}

impl ZoneAudio {
    fn rand(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    /// Uniform silence interval (ms) in `[min, max]` for the phase.
    fn silence_ms(&mut self, min: u32, max: u32) -> u32 {
        if max > min {
            min + self.rand() % (max - min + 1)
        } else {
            min
        }
    }
}

/// Startup: load the zone→audio catalog.
fn load_area_sounds(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_area_sound_catalog(&mut chain)
    };
    match loaded {
        Ok(cat) => {
            info!("sound: {} areas in the zone-audio catalog", cat.len());
            commands.insert_resource(AreaSounds(cat));
        }
        Err(e) => warn!("sound: zone-audio catalog failed to load: {e:#}"),
    }
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_exploration_sound_catalog(&mut chain)
    };
    match loaded {
        Ok(cat) => {
            info!(
                "sound: {} races in the exploration-sound catalog",
                cat.len()
            );
            commands.insert_resource(ExplorationSounds(cat));
        }
        Err(e) => warn!("sound: exploration-sound catalog failed to load: {e:#}"),
    }
}

/// The per-frame scheduler: react to area/phase changes, arm the transition fade-stop, run the
/// silence timer, start the next track, and keep the stream volumes on the sliders.
#[allow(clippy::too_many_arguments)]
fn zone_audio(
    mut zone: NonSendMut<ZoneAudio>,
    mut out: NonSendMut<SoundOutput>,
    areas: Option<Res<AreaSounds>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    config: Res<SoundConfig>,
    clock: Res<GameClock>,
    time: Res<Time>,
    world: benilla_world::world_point::WorldPoint,
    interior: Res<super::interior::CurrentInterior>,
    weather: Res<super::weather::WeatherAmbience>,
    self_store: Query<&crate::net::ObjectStore, With<crate::net::SelfPlayer>>,
) {
    let (Some(areas), Some(mut kits), Some(assets)) = (areas, kits, assets) else {
        return;
    };
    let now = time.elapsed_secs_f64();
    let phase = phase(&clock);
    let zone = &mut *zone;

    // The cover's audio hold ([`SoundConfig::world_hold`]): the raise kills the world
    // soundscape — the reference's loading screen sits over a torn-down world, so a departure
    // zone's bed playing under the destination's cover is a sound it cannot make. Resetting
    // `area`/`interior` with the beds is what makes the reveal a fresh zone entry: the first
    // uncovered frame re-resolves the destination and starts its music/ambience through the
    // ordinary change block below, exactly like walking into the zone.
    if config.world_hold {
        stop_world_soundscape(zone, "loading cover");
        return;
    }

    // ---- react to an area or interior change (the WMO row overrides the terrain chain where
    // its FKs are nonzero — sound::interior, decision 0076) ----
    let inside = interior.0;
    let area = world.area();
    if area != zone.area || inside != zone.interior {
        zone.area = area;
        zone.interior = inside;
        let resolved = area.and_then(|id| areas.0.resolve(id));
        let (mut music_row, mut intro) = match &resolved {
            Some(r) => (r.music.map(|m| m.id).unwrap_or(0), r.intro),
            None => (0, None),
        };
        if let Some(i) = inside {
            if i.zone_music != 0 {
                music_row = i.zone_music;
            }
            if i.intro_sound != 0 {
                intro = areas.0.intro(i.intro_sound);
            }
        }

        if music_row != zone.zone_music {
            zone.zone_music = music_row;
            // Fade the outgoing track linearly to silence over 4.0 s and let the backend stop it
            // (`0x4602e0` → `0x7a5a10(4.0f)`): hand the fade to kira and drop the handle so the
            // per-frame sync stops fighting it — the ramp finishes on the audio thread.
            if let Some(mut h) = zone.music.take() {
                h.stop(mixer::fade(MUSIC_FADE_OUT_MS));
            }
            zone.next_track_at = None;
            // Intro fanfare preempts the zone track: play immediately at full if off cooldown.
            let mut slot_taken = false;
            if let Some(intro) = intro {
                let ok_at = zone
                    .intro_last
                    .get(&intro.id)
                    .map(|t| t + intro.min_delay_minutes as f64 * 60.0);
                if ok_at.is_none_or(|t| now >= t)
                    && intro.sound_id != 0
                    && start_music_stream(
                        zone,
                        &mut out,
                        &mut kits,
                        &assets,
                        &config,
                        intro.sound_id,
                    )
                {
                    zone.intro_last.insert(intro.id, now);
                    slot_taken = true;
                }
            }
            // Otherwise the incoming zone track starts NOW, at full volume — with no exception.
            // The client starts it immediately on a zone-music change (`0x4602e0` forces the
            // next-start time to −1, so `0x460040` opens the stream that tick at full) AND on a
            // fresh world entry (`0x45ffc0` arms the same −1). The randomized silence gap
            // ([`next_track_time`]) is only the *same-zone* track-to-track spacing: decision 0100
            // wrongly reused it on a change, leaving a newly-entered zone silent for 3–5 min, and
            // the 6 s "cold start" that used to sit here was `0x4601f0`'s unreachable arm applied
            // to an entry that never calls it (1553).
            if !slot_taken {
                if let Some(kit) = zone_music_row(&areas.0, music_row).map(|m| m.sounds[phase]) {
                    if kit != 0 {
                        start_music_stream(zone, &mut out, &mut kits, &assets, &config, kit);
                    }
                }
            }
        }
    }

    // ---- ambience reconciliation. The desired-bed priority is the client's own selector
    // (`FUN_00460bd0`, byte-verified): GHOST > submerged > weather-gated zone day/night. The
    // ghost bed is the named "Ghost" SoundEntries kit, resolved through the name registry like
    // the client's literal; its enter/leave swap rides the standard 5.0 s crossfade (the ghost
    // row of the trigger table — zone-music-ambience-transition.md). ----
    let ghost = self_store
        .single()
        .is_ok_and(|store| store.0.player_is_ghost());
    let desired = if ghost {
        kits.id_by_name("Ghost").unwrap_or(0)
    } else if world.submersion().is_water() {
        UNDERWATER_LOOP_KIT
    } else if weather.0 != 0 && world.area_interior().is_none() {
        // The selector's weather branch (`0x460bd0`, 0x460bf7–0x460c17; wow-re
        // `rf-weather-emission-timeline` ROUND 5 Q-C): while the **zonetext indoor bit** is clear
        // (the keep-flag `[0xb06d44]`, dl=0 from the outdoor area feeder) the raw weather
        // SoundEntries IS the ambience bed; the indoor feeder (dl=1) makes the selector ignore
        // the weather id entirely — the area/interior row below plays instead. Both directions
        // ride the one 5.0 s crossfade: stepping into the inn fades the storm out, never ducks it.
        weather.0
    } else if let Some(kit) = zone
        .interior
        .filter(|i| i.ambience != 0)
        .and_then(|i| areas.0.ambience_row(i.ambience))
        .map(|a| a.kits[phase])
        .filter(|k| *k != 0)
    {
        kit
    } else {
        zone.area
            .and_then(|id| areas.0.resolve(id))
            .and_then(|r| r.ambience.map(|a| a.kits[phase]))
            .unwrap_or(0)
    };
    if desired != zone.ambience_kit {
        // The submerge/emerge swap is INSTANT — the underwater caller takes `0x460b00`'s no-fade
        // branch (`0x458650` → `0x460af0(param=1)`, wow-re §5 B16, correcting the old "5 s
        // underwater" of B6). Every other ambience swap — interior, area, day↔night — is the one
        // 5.0 s crossfade (`0x460b00`'s fade branch).
        let fade_ms = if world.submersion().is_water() != zone.was_underwater {
            0
        } else {
            AMBIENCE_TRANSITION_FADE_MS
        };
        swap_ambience(
            zone, &mut out, &mut kits, &assets, &config, desired, fade_ms, now,
        );
    }
    zone.was_underwater = world.submersion().is_water();

    // ---- music transport ----
    // Reap a finished track → schedule the next after a fresh silence interval.
    if let Some(h) = &zone.music {
        if h.state() == kira::sound::PlaybackState::Stopped {
            zone.music = None;
            let zm = zone.zone_music;
            zone.next_track_at = next_track_time(zone, &areas.0, zm, phase, now);
        }
    }
    // Silence elapsed → start the next track (same-zone cycle, or the cold-start first track).
    if zone.next_track_at.is_some_and(|t| now >= t) && zone.music.is_none() {
        zone.next_track_at = None;
        if let Some(m) = zone_music_row(&areas.0, zone.zone_music) {
            let kit = m.sounds[phase];
            if kit != 0 {
                start_music_stream(zone, &mut out, &mut kits, &assets, &config, kit);
            }
        }
    }

    // ---- per-frame volumes (sliders are live) ----
    // Both feeds `glide` rather than snap (decision 1026): these are the two loudest, longest-lived
    // channels in the mix, so a stepped gain on either is the most audible click there is.
    // Music holds full kit × category volume every frame — its only transition fade is the
    // outgoing 4.0 s fade-stop (on the backend), and the incoming starts at full (faithful).
    if let Some(h) = &mut zone.music {
        h.set_volume(
            mixer::amp_to_db(config.category_amp(SoundCategory::Music) * zone.music_kit_vol),
            mixer::glide(),
        );
    }
    // The starvation watch (1109) over the playing track. A track fading out on a dropped handle
    // isn't watched — under the load bursts that starve a decoder, the *playing* slot's watch is
    // the indicator either way (every stream decoder shares the same scheduling class).
    if let Some(h) = &zone.music {
        zone.music_watch.feed(h, f64::from(time.delta_secs()));
    }
    // Ambience runs the incoming leg of its 5.0 s crossfade as a per-frame fade-in envelope (the
    // per-frame feed would otherwise stomp a handle-level ramp to full); it clears itself at full.
    let ambience_gain = fade_in_gain(&mut zone.ambience_fade_in, now);
    if let Some(h) = &mut zone.ambience {
        h.set_volume(
            mixer::amp_to_db(
                config.category_amp(SoundCategory::Ambience)
                    * zone.ambience_kit_vol
                    * ambience_gain,
            ),
            mixer::glide(),
        );
    }
}

fn zone_music_row(cat: &AreaSoundCatalog, _id: u32) -> Option<&benilla_formats::ZoneMusicEntry> {
    // The catalog keys zone-music rows by id; expose via resolve()'s cached row instead of a
    // second map walk. (Small helper so the call sites read; see AreaSoundCatalog::zone_music.)
    cat.zone_music(_id)
}

/// When the NEXT track should start after this one ends — the client's `0x4601f0`, whose sole
/// caller is the natural-end reap `0x4600b6`: `None` if the zone has no music; otherwise `now +`
/// the row's randomized per-phase silence interval. (`SoundZoneMusicNoDelay`, the immediate path,
/// is a "0" CVar we don't expose.) **Not a cold start** — `0x4601f0`'s `== 0 → now + 6000 ms` arm
/// needs a *cleared* currently-playing row, which end-of-track flow never presents, and an entry
/// never reaches this function at all (it takes the −1 "start now" path; module docs, 1553).
fn next_track_time(
    zone: &mut ZoneAudio,
    cat: &AreaSoundCatalog,
    music_row: u32,
    phase: usize,
    now: f64,
) -> Option<f64> {
    if music_row == 0 {
        return None;
    }
    // Read the min/max out from under the catalog borrow before the rng draw (which needs `zone`).
    let interval =
        zone_music_row(cat, music_row).map(|m| (m.silence_min[phase], m.silence_max[phase]));
    Some(match interval {
        Some((min, max)) => now + zone.silence_ms(min, max) as f64 / 1000.0,
        None => now + 6.0,
    })
}

/// Whether the music slot is *already* running `kit_id` — the client's already-playing early-out
/// (`0x4602e0` @`0x460342`: `cmp [0xb06cbc],eax; je` → desired == playing → return), which is what
/// makes the server's every-5-s re-push of an event track a no-op instead of a restart. A handle
/// the end-of-track reap hasn't collected yet (`Stopped`) does **not** hold the slot: the track is
/// over, and a push is exactly what should start it again.
fn slot_holds(music_kit: u32, kit_id: u32, slot: Option<kira::sound::PlaybackState>) -> bool {
    music_kit == kit_id && slot.is_some_and(|s| s != kira::sound::PlaybackState::Stopped)
}

/// Open a music kit as a stream on the music slot (a zone track, an intro, or a server-pushed
/// track), replacing whatever is on it. The stream starts at **full** kit × category volume for
/// every start — the client has no music fade-in (§5 B16); the slot's only fade is the outgoing
/// 4.0 s fade-stop. Returns whether a stream actually started.
fn start_music_stream(
    zone: &mut ZoneAudio,
    out: &mut SoundOutput,
    kits: &mut SoundKits,
    assets: &WorldAssets,
    config: &SoundConfig,
    kit_id: u32,
) -> bool {
    let Some(mixer_ref) = out.mixer.as_mut() else {
        return false;
    };
    let Some((path, kit_vol)) = kits.pick_stream(kit_id) else {
        return false;
    };
    let bytes = {
        let chain = assets.chain.lock_recover();
        chain.read(&path)
    };
    let data = match bytes.and_then(mixer::stream_from_bytes) {
        Ok(d) => d,
        Err(e) => {
            warn!("zone music: {path} — {e:#}");
            return false;
        }
    };
    let start_amp = config.category_amp(SoundCategory::Music) * kit_vol;
    match mixer_ref.play_stream(data.volume(mixer::amp_to_db(start_amp))) {
        Ok(h) => {
            info!("zone music: {path}");
            // One stream per slot: hand the outgoing track the 4.0 s music fade as the incoming
            // takes over (the same overlap `0x4602e0` arms on a zone-music change). It cannot just
            // be dropped — a dropped kira handle keeps playing, so an overwritten stream would
            // stay in the mix at full volume until its file ran out.
            if let Some(mut outgoing) = zone.music.replace(h) {
                outgoing.stop(mixer::fade(MUSIC_FADE_OUT_MS));
            }
            zone.music_kit = kit_id;
            zone.music_kit_vol = kit_vol;
            true
        }
        Err(e) => {
            warn!("zone music: {path} — {e:#}");
            false
        }
    }
}

/// Crossfade the looping ambience bed to a new kit (0 = stop), over `fade_ms`: the old bed fades
/// out on the backend (drop the handle — the ramp finishes on kira's thread) while the new bed
/// starts silent and fades in under the per-frame envelope. This mirrors the client's `0x460b00`
/// (a 5.0 s crossfade — out `0x7a5a10(5.0f)`, in `0x7a5dc0(0)` → `0x7a57b0(5.0f)`).
#[allow(clippy::too_many_arguments)]
fn swap_ambience(
    zone: &mut ZoneAudio,
    out: &mut SoundOutput,
    kits: &mut SoundKits,
    assets: &WorldAssets,
    config: &SoundConfig,
    kit_id: u32,
    fade_ms: u64,
    now: f64,
) {
    if kit_id == zone.ambience_kit {
        return;
    }
    if let Some(mut h) = zone.ambience.take() {
        h.stop(mixer::fade(fade_ms));
    }
    zone.ambience_kit = kit_id;
    zone.ambience_fade_in = None;
    if kit_id == 0 {
        return;
    }
    let Some(mixer_ref) = out.mixer.as_mut() else {
        return;
    };
    let Some((path, kit_vol)) = kits.pick_stream(kit_id) else {
        return;
    };
    let bytes = {
        let chain = assets.chain.lock_recover();
        chain.read(&path)
    };
    // A static loop, NOT a stream: streaming loop beds die at EOF (mixer::loop_from_bytes docs).
    let data = match bytes.and_then(mixer::loop_from_bytes) {
        Ok(d) => d,
        Err(e) => {
            warn!("ambience: {path} — {e:#}");
            return;
        }
    };
    // Start silent and let the per-frame envelope ramp it up (the fade-in leg of the crossfade).
    let start_amp = if fade_ms > 0 {
        0.0
    } else {
        config.category_amp(SoundCategory::Ambience) * kit_vol
    };
    match mixer_ref.play_2d(data.volume(mixer::amp_to_db(start_amp))) {
        Ok(h) => {
            info!("ambience: {path}");
            zone.ambience = Some(h);
            zone.ambience_kit_vol = kit_vol;
            if fade_ms > 0 {
                zone.ambience_fade_in = Some(FadeIn::new(now, fade_ms));
            }
        }
        Err(e) => warn!("ambience: {path} — {e:#}"),
    }
}

/// Server-pushed sounds (`SMSG_PLAY_SOUND`/`PLAY_MUSIC`/`PLAY_OBJECT_SOUND`, bridged by
/// `net::apply_net_updates`): 2D kits and object-positioned 3D kits go through the kit player;
/// music takes the zone music slot (interrupting the current track — the zone scheduler resumes
/// its own rotation after the pushed track ends, via the normal silence interval).
#[allow(clippy::too_many_arguments)]
fn server_sounds(
    mut msgs: MessageReader<ServerSoundMessage>,
    mut zone: NonSendMut<ZoneAudio>,
    mut out: NonSendMut<SoundOutput>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
    transforms: Query<&Transform, Without<Camera3d>>,
) {
    if msgs.is_empty() {
        return;
    }
    // Under the cover a push is dropped, not deferred (the hold, [`SoundConfig::world_hold`]):
    // the reference's blocking load hears none of these, and the recurring pushes (the Darkmoon
    // Faire emitter re-pushes every 5 s) re-arrive on their own after the reveal.
    if config.world_hold {
        msgs.clear();
        return;
    }
    let (Some(mut kits), Some(assets)) = (kits, assets) else {
        return;
    };
    let listener = listener.pos;
    for m in msgs.read() {
        match m.kind {
            ServerSoundKind::Music => {
                // A push for the track already on the slot is a NO-OP (module docs): the server
                // re-pushes an event track on a timer to loop it — the Darkmoon Faire emitter
                // every 5 s — so restarting on each push stacked a fresh full-volume copy of the
                // faire theme onto the mix every 5 s, a dozen at a time.
                if slot_holds(
                    zone.music_kit,
                    m.sound_id,
                    zone.music.as_ref().map(|h| h.state()),
                ) {
                    continue;
                }
                start_music_stream(&mut zone, &mut out, &mut kits, &assets, &config, m.sound_id);
            }
            ServerSoundKind::Sound2d | ServerSoundKind::ObjectSound => {
                // Object sounds position at the source entity when it's streamed to us;
                // otherwise (or for plain 2D) they play flat.
                let pos = m
                    .source
                    .and_then(|e| transforms.get(e).ok())
                    .map(|t| t.translation);
                // A resolved object sound also REGISTERS on its unit — the reference's
                // `AISOUNDDESC` pool (`0x278` → `0x458fb0` → `0x459120`), which its own vocal
                // gates then query through `0x4591f0` so a scripted voice line is not talked over
                // by the creature's grunts ([`Latch::ObjectSound`]). A plain 2D push, or one
                // whose source is not streamed to us, has no unit to register on.
                let source = match m.kind {
                    ServerSoundKind::ObjectSound => m.source,
                    _ => None,
                };
                let latch = if source.is_some() {
                    Latch::ObjectSound
                } else {
                    Latch::None
                };
                if let Err(e) = play_kit_ext(
                    &mut kits,
                    &assets,
                    &mut out,
                    &config,
                    listener,
                    KitRef::Id(m.sound_id),
                    pos,
                    SoundCategory::Sfx,
                    PlayExtras {
                        source,
                        latch,
                        ..default()
                    },
                ) {
                    warn!("server sound {}: {e:#}", m.sound_id);
                }
            }
        }
    }
}

/// `OnExit(InWorld)` — the world soundscape dies with the world. A logout is a session teardown,
/// not a zone change: the beds hard-stop on a short declick ramp (NOT the 4.0 s / 5.0 s musical
/// transitions — the real client destroys the world's sound state outright and the glue theme
/// takes over), and the scheduler resets to cold so the next login resolves fresh from its own
/// area. Deliberately kept: `intro_last` and the RNG — their client counterparts are process-global
/// statics, not per-session state (the intro throttle survives a relog).
fn leave_world(mut zone: NonSendMut<ZoneAudio>) {
    stop_world_soundscape(&mut zone, "left world");
}

/// Stop the beds and reset the scheduler to cold — shared by the two edges where the world's
/// audio dies: leaving the world ([`leave_world`]) and the loading cover rising over it
/// ([`zone_audio`]'s hold arm, decision 1345 — the cover kills the world soundscape exactly
/// as the reference's blocking load does). Idempotent: with the handles already taken, every
/// line is a no-op and nothing logs.
fn stop_world_soundscape(zone: &mut ZoneAudio, reason: &str) {
    // The teardown declick — fast enough to read as a cut, long enough not to pop.
    const WORLD_TEARDOWN_FADE_MS: u64 = 250;
    let had = zone.music.is_some() || zone.ambience.is_some();
    if let Some(mut h) = zone.music.take() {
        h.stop(mixer::fade(WORLD_TEARDOWN_FADE_MS));
    }
    // The next login's first track starts at position 0, far behind this one's baseline — a
    // stale watch would read that as a freeze (1109).
    zone.music_watch.reset();
    if let Some(mut h) = zone.ambience.take() {
        h.stop(mixer::fade(WORLD_TEARDOWN_FADE_MS));
    }
    zone.zone_music = 0;
    zone.music_kit = 0;
    zone.music_kit_vol = 1.0;
    zone.next_track_at = None;
    zone.ambience_kit = 0;
    zone.ambience_kit_vol = 1.0;
    zone.ambience_fade_in = None;
    zone.area = None;
    zone.interior = None;
    zone.was_underwater = false;
    if had {
        info!("sound: world soundscape stopped ({reason})");
    }
}

/// Registration hook for [`super::SoundPlugin`].
/// Report this module's live stream voices into the global budget (decision 1557).
///
/// Rewritten from the live handles every frame rather than incremented and decremented, so a
/// fade that gets interrupted — or a slot replaced mid-crossfade — cannot leave the budget
/// believing in a voice that stopped. The outgoing leg of an ambience crossfade is deliberately
/// not counted: its handle is released at the swap, so we do not hold it and cannot honestly say
/// whether it is still ringing.
fn report_stream_voices(zone: NonSend<ZoneAudio>, mut out: NonSendMut<super::SoundOutput>) {
    let live = |s: kira::sound::PlaybackState| s != kira::sound::PlaybackState::Stopped;
    out.zone_streams = usize::from(zone.music.as_ref().is_some_and(|h| live(h.state())))
        + usize::from(zone.ambience.as_ref().is_some_and(|h| live(h.state())));
}

pub(super) fn plugin(app: &mut App) {
    app.insert_non_send_resource(ZoneAudio::default())
        .add_systems(Startup, load_area_sounds.after(AssetSet::Open))
        .add_systems(
            Update,
            (server_sounds, zone_audio)
                .chain()
                .run_if(super::world_audio_live)
                .in_set(WorldStage::Present),
        )
        // Unconditional: the budget must fall back to zero when the soundscape stops, and a
        // system gated on `world_audio_live` would freeze the last count instead.
        .add_systems(Update, report_stream_voices)
        .add_systems(
            OnExit(crate::char_select::ClientState::InWorld),
            leave_world,
        );
}

#[cfg(test)]
mod tests {
    use super::slot_holds;
    use kira::sound::PlaybackState;

    /// The repeat-push guard. The server keeps an event track looping by re-pushing its id every
    /// few seconds (the Darkmoon Faire emitter: `SMSG_PLAY_MUSIC` 8440 every 5 s), so a push for
    /// the track *currently playing* must do nothing — without this the faire stacked a new
    /// full-volume copy of its theme into the mix every 5 s. Once the track ends (or never
    /// started), the same id must start it again: that repeat is what loops the music.
    #[test]
    fn repeat_music_push_only_no_ops_while_the_track_plays() {
        assert!(slot_holds(8440, 8440, Some(PlaybackState::Playing)));
        assert!(!slot_holds(8440, 8440, Some(PlaybackState::Stopped)));
        assert!(!slot_holds(8440, 8440, None));
        // A different track always takes the slot (an intro, a scripted track, a zone change).
        assert!(!slot_holds(8440, 4123, Some(PlaybackState::Playing)));
    }
}
