//! The **kit player** — WoW's owned sound selection over the mixer seam (decision 0070).
//!
//! A play request names a `SoundEntries` kit (by id or name, mirroring the client's
//! `PlaySoundById`/`PlaySoundByName` surface `0x458850`/`0x458030`); this module does what the
//! client does between that call and the backend: the audibility gate, duplicate suppression,
//! the weighted variation pick with a depleting pool (`0x45bb70`/`0x45bd40`), per-shot
//! volume/pitch variation (`0x458c60`/`0x458da0`), and the per-frame channel pump (reap finished,
//! distance-cull, recompute `category · v · rolloff · near_field` and feed the channel volume —
//! the `0x7a4ad0`/`0x7a5000`/`0x7a5dc0` loop).
//!
//! Pinned by the 2026-07-03 wow-re dispatch (`benilla-pins.md`, decision 0079): variation gates
//! are separate DBC bits (0x400 pitch / 0x800 volume — raw-copied flag word, B2), the draw is
//! the mulhi scale [`math::variation_draw`] (B1), and **no Type→category table exists** (B3) —
//! the client's volume category is set by which play driver was invoked, so [`play_kit`] takes
//! the category from its caller (SFX for world/UI triggers, ambience/music for the scheduler
//! drivers). Remaining INTERIM: out-of-range looping channels stop (audible again = restart by
//! the trigger) rather than pause/resume-virtualize.

use std::collections::HashMap;

use anyhow::{anyhow, Context, Result};
use bevy::prelude::*;
use kira::sound::PlaybackState;

use benilla_formats::{sound_kit_flags, SoundKitCatalog};

use benilla_assets::{AssetSet, LockRecover, WorldAssets};
use benilla_world::dev_state::DebugState;

use super::math;
use super::mixer::{self, StaticSoundData};
use super::{AudioListener, SoundConfig, SoundOutput};

/// Which config slider scales a channel — the WoW volume categories (master is global, on the
/// main track). A property of the **call site** (which play driver fired — channel flag bits
/// 0x2 SFX / 0x8 ambience / fallback music, set by caller booleans; wow-re `benilla-pins.md`
/// B3), never derived from the kit's `SoundType`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SoundCategory {
    Sfx,
    Music,
    Ambience,
}

/// The depleting variation pool of one kit (`0x45bb70` pick + `0x45bd40` rebalance): each pick
/// decrements the chosen slot's remaining weight; when the pool empties it refills. With the
/// data's typical all-1 weights this is exactly "no repeats until every variation played".
struct PickState {
    remaining: Vec<u32>,
}

/// xorshift32 — a plain deterministic PRNG. The client draws from the shared engine CRandom
/// (cmath-owned); the *transform* of the draw is the fidelity surface, the generator is not.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
}

/// The kit catalog + play-side caches. Present only when the client data loaded.
#[derive(Resource)]
pub(crate) struct SoundKits {
    catalog: SoundKitCatalog,
    /// Decoded SFX by (lowercased) path — kit variations are short files, decoded once and
    /// cheaply cloned per play (the frames are shared). The client's SoundFileDataCache analogue.
    cache: HashMap<String, StaticSoundData>,
    pick: HashMap<u32, PickState>,
    rng: Rng,
}

impl SoundKits {
    /// One 32-bit draw off the kit player's own generator — the input to [`bark_chance_pass`].
    /// Shared with the per-shot volume/pitch variation draws, like the client's single stream.
    pub(super) fn roll(&mut self) -> u32 {
        self.rng.next()
    }
}

/// One playing channel the pump owns — the client's ~0x90-byte channel struct, benilla-shaped.
pub(crate) struct ActiveChannel {
    pub(crate) kit: u32,
    /// The world entity this channel voices, when the trigger is entity-latched (the NPC
    /// greeting's per-unit live-handle latch `[unit+0xb1c]` — wow-re `sound/scratch/
    /// npc-greeting.md`). `None` for every other play. A tagged channel's liveness IS the
    /// latch: the pump reaps it when the sound stops, which is exactly the handle release.
    source: Option<Entity>,
    /// A source-tagged **looping** channel rides its unit (the client's tracked play `0x61fec0`):
    /// the pump refreshes `pos` from the source's transform each frame. One-shots stay where
    /// they fired.
    tracked: bool,
    handle: mixer::StaticSoundHandle,
    /// The spatial track keeping the 3D voice alive; `None` = 2D (main track).
    track: Option<mixer::SpatialTrackHandle>,
    pos: Option<Vec3>,
    /// Kit `MinDistance` — the rolloff knee.
    min_dist: f32,
    /// Kit `DistanceCutoff` — the cull/virtualize radius (0 = never cull).
    cutoff: f32,
    /// The per-shot volume `v` (base + variation) the mix multiplies each frame.
    v: f32,
    /// A driver-animated gain the mix multiplies each frame (default 1.0) — the fade lane for
    /// long-lived loops whose per-frame volume the pump owns (the liquid ambient loops' 5.0 s
    /// in/out ramps, decision 0506; a handle-level fade would be overwritten by the pump's
    /// `set_volume`). Written via [`set_source_kit_gain`].
    gain: f32,
    category: SoundCategory,
    /// The **voice category latched on this channel**, when this play is a unit's one-shot
    /// creature bark — the reference's `[unit+0xb24]` beside the `[unit+0xb20]` handle
    /// ([`unit_voice_playing`]). `None` for every other play, which is what keeps a unit's body
    /// loop, greeting line and footsteps out of the voice slot.
    voice: Option<u8>,
}

/// The **voice categories** of the reference's per-unit one-shot bark dispatch `0x623a40` (its
/// 5-way jump table at `0x623afc`; wow-re `object-layer/scratch/smsg-ai-reaction.md`). Only the
/// one the note pins byte-exactly is named here — the rest of the table, and the priority
/// ordering that decides which category may interrupt which, are an open wow-re question and are
/// deliberately NOT guessed at: today only [`voice_category::HOSTILE`] is routed through the slot.
pub(super) mod voice_category {
    /// `0x623a40(0)` — the HOSTILE aggro bark, `CreatureSoundData` col 10. Byte-verified as the
    /// **lowest** priority in the table: it never interrupts a playing bark, and it is dropped
    /// outright while the unit's voice slot is live.
    pub(in crate::sound) const HOSTILE: u8 = 0;
}

/// The **class-bark chance roll** (wow-re `sound/scratch/creature-vocal-gates.md`, §5): the
/// reference draws `r = MulHi32(101, rand32) ∈ [0, 100]` and admits the bark iff
/// `threshold >= r` — inclusive, so `P = (threshold + 1) / 101`.
///
/// The client's generator is its own shared lagged generator (`0x882664`/`0x882668` over the
/// `.rdata` table `0x802700`), **not** the MSVCRT LCG the animation variation walk uses, and it is
/// reseeded from the millisecond tick at `0x402802`. The note's own guidance follows from that:
/// **reproduce the probability, never the sequence.** So this takes any 32-bit draw — ours is the
/// kit player's own xorshift ([`SoundKits::roll`]) — and only the arithmetic is faithful.
pub(super) fn bark_chance_pass(threshold: u32, roll: u32) -> bool {
    ((101u64 * u64::from(roll)) >> 32) as u32 <= threshold
}

/// The class-5 (`$FDX` stand) chance threshold: `0x8626d4[5] = 40` in the creature table and
/// `0x86424c[5] = 40` in the player twin — identical, so which twin dispatches cannot change the
/// answer. **P = 41/101 ≈ 40.6 %.**
///
/// The rest of the creature table is `{70, 100, 60, 100, 100, 40, 100, …}` (player twin
/// `{35, 100, 30, 100, 100, 40, 100, …}`), and only classes 0, 2 and 5 are ever rolled — every
/// other class carries 100, i.e. `P = 1`, which is why `$WNG`/`$WGG` (classes 7 and 10) and the
/// ALERT bark (class 8) are faithfully unconditional. Classes 0 (exertion) and 2 (injury) are
/// **not** encoded here on purpose: their live trigger route is still unpinned (`$CAH`'s exertion
/// leg, `super::combat`'s own INTERIM), and a threshold is only safe to apply once you know the
/// call actually reaches this gate.
pub(super) const STAND_CHANCE: u32 = 40;

/// The class-5 cooldown: **10 000 ms on ONE global timestamp** (`0x623290`,
/// `GetTickCount − [0xc4e0e4] − 0x2710`). Not per unit and not per class — a single window shared
/// by every creature in the world, so one crocodile's croak silences every other creature's stand
/// vocal for ten seconds. Stamped on an *allowed* attempt and **before** the column-is-zero bail,
/// so a silent or distance-culled `$FDX` burns the window for everyone too.
pub(super) const STAND_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(10);

/// A kit to play, by id or by `PlaySoundByName` name.
pub(crate) enum KitRef<'a> {
    Id(u32),
    Name(&'a str),
}

/// Resolve → gate → pick → decode → play. `pos: None` is the 2D path (UI, self sounds);
/// `category` is the caller's volume bucket (module docs — the client's per-driver flag bits).
/// Silently succeeds without playing when the kit is out of range or duplicate-suppressed
/// (matching the client: gates are not errors).
#[allow(clippy::too_many_arguments)]
pub(crate) fn play_kit(
    kits: &mut SoundKits,
    assets: &WorldAssets,
    out: &mut SoundOutput,
    config: &SoundConfig,
    listener: Vec3,
    kit_ref: KitRef<'_>,
    pos: Option<Vec3>,
    category: SoundCategory,
) -> Result<()> {
    play_kit_ext(
        kits, assets, out, config, listener, kit_ref, pos, category, None, None, false, None,
    )
}

/// [`play_kit`] with the extras the client's full play surface carries (`0x458f90(kit,
/// variant, posPtr)` + the greeting's channel-handle store): an **explicit variation index**
/// (`Some(i)` = play `files[i]`, bypassing the depleting random pool — the client's `variant !=
/// -1`; the NPC-greeting sequence cycler drives this), a **source entity tag** (the played
/// channel is tagged so its liveness serves as that entity's per-unit latch — [`source_playing`]),
/// and **`force_loop`** — loop regardless of the kit's own 0x200 flag, for the drivers whose
/// *column* is the loop authority (the creature body-loop: every CreatureSoundData col-23 kit is
/// authored `*Loop*` yet half omit 0x200 — INTERIM, the client's loop-start `0x461d80` flag
/// handling is unpinned; decision record with the drone build), and a **voice category** — the
/// category latched on the unit's one-shot bark slot ([`ActiveChannel::voice`]); `Some` only for
/// the barks the reference stores in `[unit+0xb20]`.
#[allow(clippy::too_many_arguments)]
pub(super) fn play_kit_ext(
    kits: &mut SoundKits,
    assets: &WorldAssets,
    out: &mut SoundOutput,
    config: &SoundConfig,
    listener: Vec3,
    kit_ref: KitRef<'_>,
    pos: Option<Vec3>,
    category: SoundCategory,
    variant: Option<usize>,
    source: Option<Entity>,
    force_loop: bool,
    voice: Option<u8>,
) -> Result<()> {
    // The cover's audio hold ([`SoundConfig::world_hold`]): while the loading screen is up, no
    // new sound starts — checked before anything allocates, so the per-frame retry drivers (the
    // creature body-loop, the liquid loops) cost one bool while held and start clean at the
    // reveal. A dropped one-shot under an opaque cover is a sound the reference never played:
    // its world load blocks, and the events these sounds voice happen unheard.
    if config.world_hold {
        return Ok(());
    }
    let kit = match kit_ref {
        KitRef::Id(id) => kits.catalog.get(id),
        KitRef::Name(name) => kits.catalog.by_name(name),
    }
    .ok_or_else(|| anyhow!("unknown sound kit"))?;

    let (id, volume, flags, min_dist, cutoff, eax_def) = (
        kit.id,
        kit.volume,
        kit.flags,
        kit.min_distance,
        kit.distance_cutoff,
        kit.eax_def,
    );
    // Selection-time audibility (0x45cdf0): positional kits out of cutoff never allocate —
    // checked first, before the weight pool, so a per-frame retry driver (the creature body-loop
    // reconciler) costs a distance test and nothing more while its unit is out of range.
    let d_sq = pos.map(|p| math::dist_sq(listener, p));
    if let Some(d_sq) = d_sq {
        if cutoff > 0.0 && !math::audible(d_sq, cutoff) {
            return Ok(());
        }
    }

    let weights: Vec<u32> = kit.files.iter().map(|(_, w)| *w).collect();
    if weights.is_empty() {
        return Ok(()); // a kit with no files is playable-as-nothing, not an error
    }

    // Duplicate suppression — byte-verified (wow-re uisound-tables.md, corrected §5): the FMOD
    // pre-play gate 0x7a66a0 drops a play when kit flag 0x20 is set AND a same-kit instance is
    // still audible (0x458f40 lifts SoundEntries +0x7c bit 0x20 into the FMOD flags word). The
    // gate's OTHER arm — an always-on per-category concurrent cap (count[0xcf553c] >=
    // limit[0x87ce60], 13 categories) — stays a deferral until that limit table is read out.
    if flags & sound_kit_flags::NO_DUPLICATES != 0 && out.channels.iter().any(|c| c.kit == id) {
        return Ok(());
    }

    // The variation: an explicit index when the caller drives the cycle (the client's
    // `variant != -1`), else the weighted pick with depletion.
    let pick = match variant {
        Some(i) if i < weights.len() => i,
        Some(_) => return Ok(()), // out-of-range explicit variant: playable-as-nothing
        None => kits.pick_variation(id, &weights),
    };
    let path = kits.catalog.get(id).expect("resolved above").files[pick]
        .0
        .clone();

    // Decode (cached).
    let data = kits.sfx(assets, &path)?;

    // Per-shot variation — separate DBC gates (B2): 0x800 volume, 0x400 pitch (no 5875 kit
    // sets 0x800, so volume variation is dormant in this build's data — the gate is faithful).
    let v = if flags & sound_kit_flags::VARY_VOLUME != 0 {
        let draw = math::variation_draw(kits.rng.next());
        math::variation_volume(Some(draw), volume, 1.0)
    } else {
        math::variation_volume(None, volume, 1.0)
    };

    let atten = d_sq.map_or(1.0, |d| {
        math::fmod_rolloff(d, min_dist) * near_field(d, cutoff)
    });
    let mut data = data.volume(mixer::amp_to_db(config.category_amp(category) * v * atten));
    if flags & sound_kit_flags::VARY_PITCH != 0 {
        let draw = math::variation_draw(kits.rng.next());
        let freq = math::variation_pitch_freq(draw);
        data = data.playback_rate(freq as f64 / data.sample_rate as f64);
    }
    let looping = force_loop || flags & sound_kit_flags::LOOPING != 0;
    if looping {
        data = data.loop_region(..);
    }

    let mixer = out.mixer.as_mut().context("no audio device")?;
    let (track, handle) = match pos {
        Some(p) => {
            // `EAXDef 0` = no `SoundSamplePreferences` row = the reference's NULL-slot skip at
            // `0x45cdc0`/`0x7a5bf0`: the channel never gets reverb properties, so it stays dry
            // however wet the zone is. That is what keeps NPC voice lines (all 275 `SoundType 17`
            // rows are `EAXDef 0`) out of an interior's reverb — decision 1155, bug B236.
            let (t, h) = mixer.play_3d(data, p, eax_def != 0)?;
            (Some(t), h)
        }
        None => (None, mixer.play_2d(data)?),
    };
    // Every actual play, named. The one question the sound subsystem could not answer about itself
    // was "what just made that noise?" — a report of an unexpected sound had no trace to read, only
    // a guess at which trigger fired. `RUST_LOG=benilla_app::sound=debug` now answers it, and since
    // 1155 it also answers "…and does it take the interior's reverb?" — the `EAXDef` wet/dry class
    // is invisible in the audio itself, so a report of an unexpected echo needs it in the trace.
    debug!(
        "sound: play kit {id} ({}) {} {}",
        kits.catalog.get(id).map_or("?", |k| k.name.as_str()),
        match category {
            SoundCategory::Sfx => "sfx",
            SoundCategory::Music => "music",
            SoundCategory::Ambience => "ambience",
        },
        match pos {
            Some(_) if eax_def != 0 => "3d wet",
            Some(_) => "3d dry",
            None => "2d",
        },
    );
    out.channels.push(ActiveChannel {
        kit: id,
        source,
        tracked: looping && source.is_some(),
        handle,
        track,
        pos,
        min_dist,
        cutoff,
        v,
        gain: 1.0,
        category,
        voice,
    });
    Ok(())
}

/// Does `unit` hold a **live one-shot voice channel** — the reference's `[unit+0xb20]` handle,
/// nonzero-gated (wow-re `object-layer/scratch/smsg-ai-reaction.md`)? The slot's liveness IS the
/// gate: the HOSTILE aggro bark is the lowest category in `0x623a40`'s table, so it never
/// interrupts what is sounding and is simply dropped while this is true.
///
/// Scoped to channels that actually latched a category, which is the whole point — the reference
/// keeps this handle separate from the combat drone (`0x623800` carries its own latch) and from
/// the greeting line (`[unit+0xb1c]`), so a humming elemental or a talking quest-giver must not
/// mute its own barks.
pub(super) fn unit_voice_playing(out: &SoundOutput, unit: Entity) -> bool {
    out.channels
        .iter()
        .any(|c| occupies_voice_slot(c.source, c.voice, unit))
}

/// Does one channel, described by its `(source, voice)` identity, occupy `unit`'s **voice** slot
/// (`[unit+0xb20]`) — as opposed to its greeting latch ([`occupies_greeting_latch`],
/// `[unit+0xb1c]`)? The two are disjoint by construction, which is the whole point of separating
/// them: a bark and a greeting line are different handles in the reference and must not mute each
/// other.
pub(super) fn occupies_voice_slot(source: Option<Entity>, voice: Option<u8>, unit: Entity) -> bool {
    source == Some(unit) && voice.is_some()
}

/// The complement — the greeting latch's own test (see [`source_playing`] for what else currently
/// lands in it).
pub(super) fn occupies_greeting_latch(
    source: Option<Entity>,
    voice: Option<u8>,
    unit: Entity,
) -> bool {
    source == Some(unit) && voice.is_none()
}

/// Is a channel tagged with `source` still live? The NPC-greeting per-unit latch (`[unit+0xb1c]`
/// nonzero-gate, `0x60c28c`/`0x60c40a`): a unit with a greeting line still sounding refuses a new
/// one. Release is automatic — the pump reaps the channel when the sound stops.
///
/// **Voice-latched channels are excluded**, because in the reference they are a *different
/// handle*: the greeting owns `[unit+0xb1c]`, the one-shot bark owns `[unit+0xb20]`
/// ([`unit_voice_playing`]). Conflating them would let a unit's aggro roar mute its own greeting
/// line, which the client never does.
///
/// Everything else tagged with `source` is still counted, and that is a **known pre-existing
/// conflation, not a claim of fidelity**: the body loop (`0x623800`'s own latch), the water
/// splash and the spell hold all ride the same one `source` tag, so any of them currently masks
/// a greeting the reference would let through. It bites real data — 6 of the 4 509 displays that
/// carry an `NPCSounds` greeting also resolve a `CreatureSoundData` row with a nonzero
/// `loop_sound` (1303, 10006, 10045, 10699, 11912, 12769; counted at the DBCs this session). The
/// honest fix is a per-latch marker rather than one shared tag, which is wider than this change
/// and is raised as such in decision 1399 — deliberately not slipped in here.
pub(super) fn source_playing(out: &SoundOutput, source: Entity) -> bool {
    out.channels
        .iter()
        .any(|c| occupies_greeting_latch(c.source, c.voice, source))
}

/// Is a channel tagged with `source` playing kit `kit_id`? The kit-scoped latch — the creature
/// body-loop's "not already playing" gate (`0x623800`'s latch is its own channel handle; the
/// channel's liveness here is exactly that). Kit-scoped so a unit's greeting line or spell hold
/// never masks its body loop.
pub(super) fn source_kit_playing(out: &SoundOutput, source: Entity, kit_id: u32) -> bool {
    out.channels
        .iter()
        .any(|c| c.source == Some(source) && c.kit == kit_id)
}

/// Whether kit `id` is a LOOPING kit (`SoundEntries` flag 0x200) — the client's `0x458830` test
/// that splits tracked-loop playback from fire-and-forget one-shots (decision 0107).
pub(super) fn kit_looping(kits: &SoundKits, id: u32) -> bool {
    kits.catalog
        .get(id)
        .is_some_and(|k| k.flags & sound_kit_flags::LOOPING != 0)
}

/// Force-stop `source`'s channels playing kit `kit_id` — the spell-hold loop's reap (decision
/// 0107: a LOOPING kit sound rides the client's *tracked* play `0x61fec0` and dies with its
/// effect at `0x614150`, unlike a fire-and-forget one-shot). Kit-scoped so a caster's other
/// tagged channels (its greeting line) survive the cast ending.
/// Set the driver-animated gain of the channel tagged `(source, kit_id)` — the fade lane for
/// pump-owned loops ([`ActiveChannel::gain`]; the liquid ambient loops' 5.0 s ramps, decision
/// 0506). The pump folds it into the next frame's volume. No-op if the channel is gone.
pub(super) fn set_source_kit_gain(out: &mut SoundOutput, source: Entity, kit_id: u32, gain: f32) {
    for c in &mut out.channels {
        if c.source == Some(source) && c.kit == kit_id {
            c.gain = gain.clamp(0.0, 1.0);
        }
    }
}

pub(super) fn stop_source_kit(out: &mut SoundOutput, source: Entity, kit_id: u32) {
    out.channels.retain_mut(|c| {
        if c.source == Some(source) && c.kit == kit_id {
            c.handle.stop(mixer::declick());
            false
        } else {
            true
        }
    });
}

/// Force-stop every channel tagged with `source` — the unit-teardown stop (`0x5fbb6c`: the client
/// stops + clears the greeting handle when the unit's record is torn down). Called on despawn.
pub(super) fn stop_source(out: &mut SoundOutput, source: Entity) {
    out.channels.retain_mut(|c| {
        if c.source == Some(source) {
            c.handle.stop(mixer::declick());
            false
        } else {
            true
        }
    });
}

/// The `PlaySoundFile` path — a raw file play, no kit: no audibility/duplicate gates, no
/// variation, base volume 1.0, 2D on the caller's category slider. Rides the same decode cache
/// as kit files (addon files are short SFX; a kit id of 0 keeps it invisible to kit dedup).
pub(crate) fn play_file(
    kits: &mut SoundKits,
    assets: &WorldAssets,
    out: &mut SoundOutput,
    config: &SoundConfig,
    path: &str,
    category: SoundCategory,
) -> Result<()> {
    // The cover's audio hold — same gate as [`play_kit_ext`], same reason.
    if config.world_hold {
        return Ok(());
    }
    let data = kits.sfx(assets, path)?;
    let data = data.volume(mixer::amp_to_db(config.category_amp(category)));
    let mixer = out.mixer.as_mut().context("no audio device")?;
    let handle = mixer.play_2d(data)?;
    out.channels.push(ActiveChannel {
        kit: 0,
        source: None,
        tracked: false,
        handle,
        track: None,
        pos: None,
        min_dist: 0.0,
        cutoff: 0.0,
        v: 1.0,
        gain: 1.0,
        category,
        voice: None,
    });
    Ok(())
}

/// Near-field ramp, tolerant of the `cutoff == 0` non-positional sentinel.
fn near_field(d_sq: f32, cutoff: f32) -> f32 {
    if cutoff > 0.0 {
        math::near_field_atten(d_sq, cutoff)
    } else {
        1.0
    }
}

impl SoundKits {
    /// Resolve a SoundEntries kit id by its `PlaySoundByName` key — the client's name-hash
    /// registry lookup (`0x458030` family). The ghost ambience/music tracks are named entries
    /// ("Ghost"/"GhostMusic" — wow-re zone-music-ambience + the 0308 death dispatch).
    pub(crate) fn id_by_name(&self, name: &str) -> Option<u32> {
        self.catalog.by_name(name).map(|k| k.id)
    }

    pub(crate) fn new(catalog: SoundKitCatalog) -> Self {
        Self {
            catalog,
            cache: HashMap::new(),
            pick: HashMap::new(),
            rng: Rng(0x9e37_79b9),
        }
    }

    /// A kit's variation count (0 = unknown or file-less kit). The NPC-greeting sequence cycler
    /// needs the hello kit's size to know when repeat interacts overflow into the pissed line.
    pub(super) fn variations(&self, kit: u32) -> usize {
        self.catalog.get(kit).map_or(0, |k| k.files.len())
    }

    /// Pick a variation for a kit and return `(file path, kit base volume)` — the entry for the
    /// **streaming** consumers (zone music/ambience open the file as a stream instead of a decoded
    /// SFX; same depleting pick, same catalog). `None` when the kit is unknown or file-less.
    pub(super) fn pick_stream(&mut self, kit_id: u32) -> Option<(String, f32)> {
        let kit = self.catalog.get(kit_id)?;
        if kit.files.is_empty() {
            return None;
        }
        let volume = kit.volume;
        let weights: Vec<u32> = kit.files.iter().map(|(_, w)| *w).collect();
        let pick = self.pick_variation(kit_id, &weights);
        let path = self.catalog.get(kit_id)?.files[pick].0.clone();
        Some((path, volume))
    }

    /// `0x45bb70`: weighted-random pick over the kit's *remaining* weight pool, refilled when
    /// exhausted (`0x45bd40`).
    fn pick_variation(&mut self, kit: u32, weights: &[u32]) -> usize {
        if weights.len() == 1 {
            return 0;
        }
        let st = self.pick.entry(kit).or_insert_with(|| PickState {
            remaining: weights.to_vec(),
        });
        let mut total: u32 = st.remaining.iter().sum();
        if total == 0 {
            st.remaining.copy_from_slice(weights);
            total = st.remaining.iter().sum();
        }
        if total == 0 {
            return 0; // all-zero weights: degenerate data, take the first
        }
        let r = self.rng.next() % total;
        let mut acc = 0u32;
        for (i, w) in st.remaining.iter().enumerate() {
            acc += w;
            if acc > r {
                st.remaining[i] -= 1;
                return i;
            }
        }
        st.remaining.len() - 1
    }

    /// Decoded sound for a kit file, via the cache (whole-file chain read + decode on miss —
    /// short SFX; music streams through the scheduler path instead).
    fn sfx(&mut self, assets: &WorldAssets, path: &str) -> Result<StaticSoundData> {
        let key = path.to_ascii_lowercase();
        if let Some(d) = self.cache.get(&key) {
            return Ok(d.clone());
        }
        let bytes = assets
            .chain
            .lock_recover()
            .read_file(path)
            .with_context(|| format!("reading {path}"))?;
        let data = mixer::sfx_from_bytes(bytes)?;
        self.cache.insert(key, data.clone());
        Ok(data)
    }
}

/// Startup: load the kit catalog off the chain (absent → no resource; every play site tolerates
/// that, the same optional-catalog rule as `Creatures`).
pub(super) fn load_sound_kits(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_sound_kit_catalog(&mut chain)
    };
    match loaded {
        Ok(catalog) => {
            info!("sound: {} kits loaded", catalog.len());
            commands.insert_resource(SoundKits::new(catalog));
        }
        Err(e) => warn!("sound: SoundEntries failed to load — kits disabled: {e:#}"),
    }
}

/// The per-frame channel pump (`0x7a4ad0` shape): reap finished channels, follow tracked sources,
/// distance-cull the positional ones, recompute each live channel's `category · v · rolloff ·
/// near_field` volume.
pub(super) fn pump_channels(
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
    transforms: Query<&Transform, Without<Camera3d>>,
) {
    let listener = listener.pos;
    out.channels.retain_mut(|ch| {
        if ch.handle.state() == PlaybackState::Stopped {
            return false;
        }
        // The tracked-channel follow (`0x61fec0`): a source-tagged loop rides its unit — refresh
        // the emitter position from the live transform (a despawned source keeps the last pos for
        // the frame until the despawn reaper stops the channel).
        if ch.tracked {
            if let Some(p) = ch
                .source
                .and_then(|s| transforms.get(s).ok())
                .map(|t| t.translation)
            {
                if ch.pos != Some(p) {
                    ch.pos = Some(p);
                    if let Some(track) = ch.track.as_mut() {
                        mixer::set_track_position(track, p);
                    }
                }
            }
        }
        let Some(p) = ch.pos else {
            // 2D: only the category slider can move under a live channel.
            ch.handle.set_volume(
                mixer::amp_to_db(config.category_amp(ch.category) * ch.v * ch.gain),
                mixer::glide(),
            );
            return true;
        };
        let d_sq = math::dist_sq(listener, p);
        if ch.cutoff > 0.0 && !math::audible(d_sq, ch.cutoff) {
            // Beyond cutoff: the client virtualizes; our one-shots just stop (module docs).
            ch.handle.stop(mixer::declick());
            return false;
        }
        let amp = config.category_amp(ch.category)
            * ch.v
            * ch.gain
            * math::fmod_rolloff(d_sq, ch.min_dist)
            * near_field(d_sq, ch.cutoff);
        // Glides, not snaps (decision 1026): this is the per-frame gain feed, and a step here is a
        // click. It is also the one that scales — every live channel steps together when a frame
        // hitches, which is what a "crack fest" under OBS actually was.
        ch.handle.set_volume(mixer::amp_to_db(amp), mixer::glide());
        true
    });
}

/// Drain the debug panel's "play kit" request (id or name — or a raw file path: a query with a
/// path separator plays through [`play_file`], the `PlaySoundFile` path, so the by-path plumbing
/// is ear-testable without an addon).
pub(super) fn apply_kit_debug(
    mut debug: ResMut<DebugState>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    let s = &mut debug.sound;
    if !std::mem::take(&mut s.play_kit) {
        return;
    }
    let (Some(mut kits), Some(assets)) = (kits, assets) else {
        warn!("sound debug: kit catalog not loaded");
        return;
    };
    let query = s.kit_query.trim().to_owned();
    if query.contains(['\\', '/']) {
        match play_file(
            &mut kits,
            &assets,
            &mut out,
            &config,
            &query,
            SoundCategory::Sfx,
        ) {
            Ok(()) => info!("sound debug: file \"{query}\" played"),
            Err(e) => warn!("sound debug: file \"{query}\" — {e:#}"),
        }
        return;
    }
    let kit_ref = match query.parse::<u32>() {
        Ok(id) => KitRef::Id(id),
        Err(_) => KitRef::Name(&query),
    };
    let listener = listener.pos;
    match play_kit(
        &mut kits,
        &assets,
        &mut out,
        &config,
        listener,
        kit_ref,
        None,
        SoundCategory::Sfx,
    ) {
        Ok(()) => info!("sound debug: kit \"{query}\" played"),
        Err(e) => warn!("sound debug: kit \"{query}\" — {e:#}"),
    }
}

/// `OnExit(InWorld)`: every live kit channel dies with the world — one-shots mid-flight and
/// entity-latched loops alike (their emitters are being torn down in this same transition). The
/// glue screens' own clicks only start after this edge, so the blanket stop is exact.
fn stop_all_channels(mut out: NonSendMut<SoundOutput>) {
    let n = out.channels.len();
    for ch in &mut out.channels {
        ch.handle.stop(mixer::declick());
    }
    out.channels.clear();
    if n > 0 {
        info!("sound: {n} kit channel(s) stopped (left world)");
    }
}

/// Drop the decoded-SFX cache on a cross-map transition (`world_map::MapChange` — see its doc):
/// kit variations are decoded on demand, so a new map's soundscape rebuilds its own working set
/// while the old map's decodes stop occupying RAM forever (the #bugs teleport leak). Playing
/// channels own their frames (`StaticSoundData` clones share them), so nothing audible cuts.
fn evict_kit_cache(
    mut changes: MessageReader<benilla_world::world_map::MapChange>,
    kits: Option<ResMut<SoundKits>>,
) {
    if changes.is_empty() {
        return;
    }
    changes.clear();
    if let Some(mut k) = kits {
        k.cache.clear();
    }
}

/// Registration hook for [`super::SoundPlugin`].
pub(super) fn plugin(app: &mut App) {
    app.add_systems(Startup, load_sound_kits.after(AssetSet::Open))
        .add_systems(
            Update,
            (
                pump_channels.in_set(benilla_world::schedule::WorldStage::Present),
                apply_kit_debug,
                evict_kit_cache,
            ),
        )
        .add_systems(
            OnExit(crate::char_select::ClientState::InWorld),
            stop_all_channels,
        );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The depleting pool (`0x45bb70`/`0x45bd40`): with the data's typical all-1 weights, every
    /// variation plays exactly once per cycle (no repeats until the pool empties), then the pool
    /// refills and the next cycle again covers all of them.
    #[test]
    fn depleting_pick_covers_all_variations_each_cycle() {
        let mut kits = SoundKits::new(benilla_formats::SoundKitCatalog::empty_for_tests());
        let weights = [1u32, 1, 1, 1, 1];
        for cycle in 0..3 {
            let mut seen = [false; 5];
            for _ in 0..5 {
                let i = kits.pick_variation(7, &weights);
                assert!(!seen[i], "repeat within cycle {cycle}: index {i}");
                seen[i] = true;
            }
            assert!(seen.iter().all(|&s| s), "cycle {cycle} covered all 5");
        }
    }

    /// The class-bark chance arithmetic (`r = MulHi32(101, rand32)`, admit iff `threshold >= r`).
    /// Two properties matter and both are byte-derived: the compare is **inclusive**, so
    /// `threshold = 100` is a tautology — which is the whole reason `$WNG`, `$WGG` and the ALERT
    /// bark are faithfully ungated — and `threshold = 40` admits 41 of the 101 buckets.
    #[test]
    fn the_bark_chance_is_inclusive_and_100_is_a_tautology() {
        // 100 never refuses, at either end of the draw space.
        for roll in [0, 1, u32::MAX / 2, u32::MAX - 1, u32::MAX] {
            assert!(bark_chance_pass(100, roll), "threshold 100 refused {roll}");
        }
        // 40 admits exactly buckets 0..=40 of 101 — measured over the bucket boundaries, not
        // sampled, so this pins the arithmetic rather than a distribution.
        let bucket = |r: u32| ((101u64 * u64::from(r)) >> 32) as u32;
        assert_eq!(bucket(0), 0);
        assert_eq!(bucket(u32::MAX), 100);
        let admitted = (0..=100)
            .filter(|&b| {
                // the first draw landing in bucket b
                let r = ((u64::from(b) << 32) / 101) as u32 + 1;
                bucket(r) == b && bark_chance_pass(STAND_CHANCE, r)
            })
            .count();
        assert_eq!(admitted, 41, "P = 41/101 for the class-5 stand vocal");
    }

    /// **The two per-unit handles are disjoint** (decision 1399): the one-shot bark occupies
    /// `[unit+0xb20]`, the greeting line `[unit+0xb1c]`, and neither may answer the other's
    /// question. This is the regression guard for the way the voice slot was added — tagging the
    /// bark's channel with its unit made it visible to the greeting latch, which would have let a
    /// creature's aggro roar silence its own hello.
    #[test]
    fn the_voice_slot_and_the_greeting_latch_are_disjoint() {
        let bear = Entity::from_raw_u32(1).expect("valid entity id");
        let other = Entity::from_raw_u32(2).expect("valid entity id");
        let bark = (Some(bear), Some(voice_category::HOSTILE));
        let greet = (Some(bear), None);

        assert!(occupies_voice_slot(bark.0, bark.1, bear));
        assert!(!occupies_greeting_latch(bark.0, bark.1, bear));
        assert!(occupies_greeting_latch(greet.0, greet.1, bear));
        assert!(!occupies_voice_slot(greet.0, greet.1, bear));

        // Neither slot is world-global: a bark on one unit says nothing about another.
        assert!(!occupies_voice_slot(bark.0, bark.1, other));
        assert!(!occupies_greeting_latch(greet.0, greet.1, other));

        // An untagged channel (every ordinary one-shot) is in neither slot.
        assert!(!occupies_voice_slot(None, None, bear));
        assert!(!occupies_greeting_latch(None, None, bear));
    }
}
