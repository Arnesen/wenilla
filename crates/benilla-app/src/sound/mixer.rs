//! The audio-backend seam: kira 0.12 behind a narrow, FMOD-shaped surface (decision 0070).
//!
//! The real client delegates the entire audible mix to FMOD 3.x — spatialization/pan, min/max
//! distance rolloff, doppler, reverb, decode, streaming — and owns only the parameters it feeds
//! (wow-re `system/sound`, T3, pinned by the import-table byte-fact: no `FSOUND_SetPan`, no
//! `FSOUND_PlaySound`). This module is that delegation seam in benilla: everything above it (the
//! kit player, the schedulers) computes WoW's owned parameter math; everything below is the
//! backend's. Keep this surface shaped like the FMOD import contract — play/stop, per-channel
//! volume/pitch, listener + source positions, streamed music — so the backend stays swappable
//! (bevy_seedling is the revisit candidate, decision 0070 "Why").
//!
//! Coordinate convention: kira's listener is X-right/Y-up (its ears sit at ±X of the orientation
//! quat — kira `track/sub.rs::listener_ear_positions`), which is exactly Bevy camera space, so
//! camera `Transform`s feed straight in with **no remap**. The RE'd `(x,y,z)→(−y,z,x)` transform
//! is the WoW↔FMOD convention pair; ours is Bevy↔kira, already unified by the decision-0002
//! world transform upstream.

use anyhow::{Context, Result};
use bevy::prelude::*;
use cpal::traits::{DeviceTrait, HostTrait};
use kira::backend::cpal::CpalBackendSettings;
use kira::effect::reverb::{ReverbBuilder, ReverbHandle};
use kira::listener::ListenerHandle;
use kira::sound::streaming::StreamingSoundData;
use kira::sound::FromFileError;
use kira::track::{SendTrackBuilder, SendTrackHandle, SpatialTrackBuilder};
use kira::{AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Mix, Tween};

pub(crate) use benilla_formats::SoundProvider;

// Types crossing the seam (consumers hold handles to stop/steer a playing channel; dropping a
// *sound* handle does not stop the sound, and dropping a spatial *track* handle marks its track
// for removal — which lands once the track's sounds finish, see `play_3d`'s
// `persist_until_sounds_finish`, so a fade-then-drop still gets to fade).
pub(crate) use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
pub(crate) use kira::sound::streaming::StreamingSoundHandle;
pub(crate) use kira::track::SpatialTrackHandle;

/// An immediate (zero-duration) parameter change. kira requires a tween on every setter; the
/// WoW-side ramps (volume rates, crossfades) are our own math updated per frame, so the backend
/// must not add smoothing of its own on top. Use this for a change that is genuinely a *step* —
/// a reverb preset switch (verified instant, `benilla-pins.md` A2), an initial value. For the
/// **per-frame volume feed** use [`glide`]: a step there is an audible click, not fidelity.
pub(crate) fn snap() -> Tween {
    Tween {
        duration: std::time::Duration::ZERO,
        ..Default::default()
    }
}

/// The per-frame **volume** feed's reconstruction ramp (decision 1026).
///
/// kira applies a track's volume as **one constant gain per internal chunk** — it updates the
/// parameter once per `internal_buffer_size` (128 frames ≈ 2.9 ms) block and multiplies the whole
/// block by it (`track/sub.rs::process`, `backend/renderer.rs`'s `chunks_mut`). Spatial *position*
/// is interpolated across the chunk (`time_in_chunk`); volume is not. So a [`snap`] on the
/// per-frame gain feed is a hard step in the waveform — a click whose loudness scales with the
/// size of the jump.
///
/// At a steady 60 fps the per-frame jumps are small and the clicks stay under the noise floor,
/// which is why this was invisible for so long. The moment frame pacing goes unstable — a
/// background build, an OBS encode, a macOS Space switch (decision 0609's world) — the jumps get
/// big and every live channel steps at once: the reported "crack fest". The frame rate was never
/// supposed to be audible.
///
/// So the feed glides instead of stepping: each frame starts a fresh linear ramp toward the value
/// we just computed. This is **not** backend smoothing layered on WoW's ramps — it is
/// reconstruction of a signal we only sample at frame rate. WoW's owned envelope math upstream is
/// untouched; a ramp shorter than one frame at 60 fps cannot smear a multi-second crossfade, and
/// on a long frame it simply completes early and holds.
pub(crate) fn glide() -> Tween {
    Tween {
        duration: std::time::Duration::from_millis(GLIDE_MS),
        ..Default::default()
    }
}

/// [`glide`]'s ramp, and the de-click fade on a force-stop. Just under one 60 fps frame (16.7 ms):
/// long enough to bridge kira's 2.9 ms gain blocks, short enough that a stop still reads as
/// immediate and a live parameter never audibly lags its source.
const GLIDE_MS: u64 = 15;

/// The de-click fade for a **force-stop** — a `stop()` that cuts a channel which may still be at
/// full amplitude (a tracked loop reaped on despawn, the leave-world blanket stop). Ending a
/// non-zero waveform at an arbitrary sample is a step to zero: the same click [`glide`] fixes on
/// the gain feed. One `GLIDE_MS` ramp removes it without moving the stop in time. The fade
/// survives dropping the handle — `stop_fade_ramps_after_handle_drop` pins exactly that.
pub(crate) fn declick() -> Tween {
    glide()
}

/// A linear fade over `ms` — kira's default easing, matching the client's constant per-tick volume
/// decrement (`0x7a5a50`). Used to fade a stream out before it stops, or a bed out before it swaps
/// (the outgoing side of a music transition / ambience swap).
pub(crate) fn fade(ms: u64) -> Tween {
    Tween {
        duration: std::time::Duration::from_millis(ms),
        ..Default::default()
    }
}

/// Linear amplitude `[0,1]` → the backend's decibel volume. WoW's owned pipeline produces linear
/// amplitudes (the category mix `cat·v·atten`, wow-re `0x7a5dc0`); FMOD consumed them as 0..255
/// levels. kira consumes dB, so the seam converts: `20·log10(amp)`, with kira's `SILENCE` floor
/// (−60 dB) for amp ≤ 10⁻³ (which is below one 1/255 FMOD step anyway).
pub(crate) fn amp_to_db(amp: f32) -> Decibels {
    if amp <= 1e-3 {
        Decibels::SILENCE
    } else {
        Decibels(20.0 * amp.log10())
    }
}

/// The one open audio device + its listener. `Mixer` methods are the only place kira's manager is
/// touched; everything above computes parameters. There is deliberately **no master filter**:
/// the real client applies no DSP beyond FMOD's reverb (53 FSOUND imports, zero `FSOUND_FX_*` —
/// wow-re `benilla-pins.md` B6); underwater is the reverb preset + the ambience swap, both
/// upstream of this seam.
pub(crate) struct Mixer {
    manager: AudioManager<DefaultBackend>,
    listener: ListenerHandle,
    /// Rolling mix-health counters — the crackle instrument ([`Mixer::poll_health`]).
    health: MixHealth,
    /// The zone-reverb send track (wet-only Freeverb). Every 3D world track routes into it at
    /// build; this handle's volume is the zone wet level (SILENCE = reverb off).
    reverb_send: SendTrackHandle,
    /// The Freeverb parameters on the send — retuned per zone preset.
    reverb: ReverbHandle,
}

impl Mixer {
    /// Open the default audio device. Fails cleanly when there is none (headless/CI) — the caller
    /// runs silent with `None` (mirrors the client's `-nosound` gate).
    pub(crate) fn new() -> Result<Self> {
        let (backend_settings, sample_rate) = backend_settings();
        let settings = AudioManagerSettings::<DefaultBackend> {
            backend_settings,
            // The mix tap (decision 1112, `$WOW_MIX_TAP`) rides the main track as an effect —
            // a no-op builder unless the env var names a capture path.
            main_track_builder: super::mix_tap::install(
                kira::track::MainTrackBuilder::new(),
                sample_rate,
            ),
            ..Default::default()
        };
        let mut manager = AudioManager::<DefaultBackend>::new(settings)
            .map_err(|e| anyhow::anyhow!("audio device init: {e}"))?;
        // The zone-reverb send: wet-only (the dry path stays on the source tracks), silent until
        // a zone preset raises it. Effects are build-time-only; parameters retune at runtime.
        let mut send_builder = SendTrackBuilder::new().volume(Decibels::SILENCE);
        let reverb = send_builder.add_effect(
            ReverbBuilder::new()
                .mix(Mix::WET)
                .feedback(0.5)
                .damping(0.5),
        );
        let reverb_send = manager
            .add_send_track(send_builder)
            .map_err(|e| anyhow::anyhow!("reverb send alloc: {e}"))?;
        let listener = manager
            .add_listener(
                mint::Vector3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                mint::Quaternion {
                    v: mint::Vector3 {
                        x: 0.0,
                        y: 0.0,
                        z: 0.0,
                    },
                    s: 1.0,
                },
            )
            .map_err(|e| anyhow::anyhow!("listener alloc: {e}"))?;
        Ok(Self {
            manager,
            listener,
            health: MixHealth::default(),
            reverb_send,
            reverb,
        })
    }

    /// Apply a zone reverb preset — the seam mirror of `FSOUND_Reverb_SetProperties` (the real
    /// client marshals the `SoundProviderPreferences` row into EAX listener properties and hands
    /// them to FMOD; wow-re `0x45a790`/`0x7a5fa0`). `None` = no preset → wet to silence (the
    /// client's silenced-GENERIC default, `0x45a830`). The switch is **instant** — verified, the
    /// client applies the new properties with no ramp (`benilla-pins.md` A2).
    ///
    /// The EAX→Freeverb projection is this backend's own lossy mapping (decision 0078):
    /// - `feedback = 10^(−0.108 / DecayTime)`, from Freeverb's RT60 relation at its ~36 ms mean
    ///   comb delay (`t60 ≈ 3·d̄ / −log10(f)`), clamped to 0.98 so a 20 s hangar can't run away;
    /// - `damping` blends "highs die faster than lows" (`1 − DecayHFRatio/2`, weight 0.7) with
    ///   the wet HF level cut (`−RoomHF/10000`, weight 0.3);
    /// - wet level = `(Room + Reverb)` mB → dB, capped at +6 (Underwater's +700 mB sum);
    ///   ≤ −60 dB (e.g. PRESET_OFF's Room −10000) lands on kira's SILENCE floor.
    pub(crate) fn set_reverb(&mut self, preset: Option<&SoundProvider>) {
        let Some(p) = preset else {
            self.reverb_send.set_volume(Decibels::SILENCE, snap());
            return;
        };
        let (feedback, damping, wet) = freeverb_projection(p);
        self.reverb.set_feedback(feedback, snap());
        self.reverb.set_damping(damping, snap());
        self.reverb_send.set_volume(wet, snap());
    }

    /// Per-frame listener pose from the world camera (Bevy space, no remap — module docs).
    pub(crate) fn set_listener(&mut self, pos: Vec3, rot: Quat) {
        self.listener.set_position(
            mint::Vector3 {
                x: pos.x,
                y: pos.y,
                z: pos.z,
            },
            snap(),
        );
        self.listener.set_orientation(
            mint::Quaternion {
                v: mint::Vector3 {
                    x: rot.x,
                    y: rot.y,
                    z: rot.z,
                },
                s: rot.w,
            },
            snap(),
        );
    }

    /// Master volume as linear amplitude (the whole mix — every track routes to main). Fed every
    /// frame, so it [`glide`]s: a step on the *main* track clicks the entire mix at once, and this
    /// is the knob a slider drag and the mute toggle both move.
    pub(crate) fn set_master(&mut self, amp: f32) {
        self.manager
            .main_track()
            .set_volume(amp_to_db(amp), glide());
    }

    /// Play a decoded (short SFX) sound on the main track — the 2D/UI path.
    pub(crate) fn play_2d(&mut self, data: StaticSoundData) -> Result<StaticSoundHandle> {
        self.manager
            .play(data)
            .map_err(|e| anyhow::anyhow!("play 2d: {e}"))
    }

    /// Play a decoded sound on a fresh spatial track at a world position. Backend attenuation is
    /// **disabled** — gain over distance is WoW's own math (decision 0070; the kit player's pump
    /// computes `rolloff · near_field` and drives the channel volume). The backend contributes
    /// pan only. The returned track handle must live as long as the sound (drop unloads it).
    ///
    /// `reverb_send` is the kit's `SoundEntries.EAXDef` reduced to "does this sound take the wet
    /// send at all" (decision 1155). 3D-open is **necessary but not sufficient** in the reference:
    /// `0x458f1c` hands `EAXDef` to the slot lookup `0x45cdc0`, and because
    /// `SoundSamplePreferences.dbc` holds only ids 1 and 2 — **there is no id 0** — an `EAXDef 0`
    /// kit resolves a NULL slot and `FSOUND_Reverb_SetChannelProperties` (`0x7a5bf0`) skips before
    /// it ever tests the 3D flag. Those channels are dry no matter what the zone preset says.
    /// benilla models neither row's per-channel EAX properties, so the projection is the binary's
    /// own branch and nothing more: populated slot ⇒ send at unity, NULL slot ⇒ no send.
    pub(crate) fn play_3d(
        &mut self,
        data: StaticSoundData,
        pos: Vec3,
        reverb_send: bool,
    ) -> Result<(SpatialTrackHandle, StaticSoundHandle)> {
        // The zone wet level lives on the send's own volume, so per-zone reverb stays one knob
        // and never a per-track update; the per-KIT half is this route existing or not.
        let mut builder = SpatialTrackBuilder::new().attenuation_function(None);
        if reverb_send {
            builder = builder.with_send(&self.reverb_send, Decibels(0.0));
        }
        let mut track = self
            .manager
            .add_spatial_sub_track(
                self.listener.id(),
                mint::Vector3 {
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                },
                builder
                    // Outlive the handle by exactly as long as the sound needs (decision 1026).
                    // kira defaults this to `false` (`track/sub/spatial_builder.rs`), which makes
                    // `should_be_removed()` fire the instant the handle drops — so a channel that
                    // is stopped-with-a-fade and then dropped in the same breath (every reaper
                    // here: cutoff cull, despawn, leave-world) loses its track mid-ramp and gets
                    // cut anyway. `declick()` would have been a no-op on all 3D audio. With this
                    // set, the drop only *marks* the track and kira keeps it until its sounds
                    // finish. Cannot leak: every path that drops a channel stops its sound first,
                    // and a stopped sound finishes.
                    .persist_until_sounds_finish(true),
            )
            .map_err(|e| anyhow::anyhow!("spatial track alloc: {e}"))?;
        let handle = track
            .play(data)
            .map_err(|e| anyhow::anyhow!("play 3d: {e}"))?;
        Ok((track, handle))
    }

    /// Decode-stream a long sound (music/ambience MP3s) on the main track. The data wraps the
    /// *compressed* bytes (a few MB off the MPQ chain) — kira decodes incrementally on its own
    /// thread; nothing is pre-decoded to PCM.
    pub(crate) fn play_stream(
        &mut self,
        data: StreamingSoundData<FromFileError>,
    ) -> Result<StreamingSoundHandle<FromFileError>> {
        self.manager
            .play(data)
            .map_err(|e| anyhow::anyhow!("play stream: {e}"))
    }
}

/// The device buffer we ask for, in frames — the underrun margin (decision 1026).
///
/// kira's cpal backend takes `device.default_output_config().config()` when handed no config of
/// its own, and that carries `BufferSize::Default` — whatever CoreAudio happens to hand us. On
/// macOS the buffer size is a *shared, per-device* property, so another app (an OBS capture, a
/// conferencing tool) can drag it down under us and we would never know. Naming it puts a floor
/// under the callback's deadline instead of inheriting someone else's.
///
/// 2048 frames is ~43 ms at 48 kHz. Latency that size is inaudible for WoW — nothing here is
/// rhythm-critical, the shortest UI click is an order of magnitude longer. The size is set by
/// the *HAL's* deadline, not ours: a confirmed crackle (decision 1115) was the OS missing the
/// device IO-cycle deadline under system pressure while our mix ran clean, and the buffer size
/// is the cycle length — fewer, longer cycles mean twice the slack per cycle for the HAL. Our
/// own mix cost was never the constraint at 1024 (~21 ms, decision 1026) and is even less so
/// here.
const TARGET_BUFFER_FRAMES: u32 = 2048;

/// Build the cpal backend settings: kira's default device, our explicit buffer size. Also
/// reports the negotiated sample rate (the mix tap's WAV header must match the renderer);
/// `None` when the device/config could not be probed — those paths run on kira defaults.
///
/// `device` stays `None` on purpose — that keeps kira's own default-device selection *and* its
/// disconnect/restart handling (`custom_device = false`). We override only the config. Every
/// failure path falls back to kira's defaults, so a machine we can't probe still opens the device
/// exactly as before; [`Mixer::new`]'s caller already tolerates no-device.
fn backend_settings() -> (CpalBackendSettings, Option<u32>) {
    let fallback = CpalBackendSettings::default();
    let Some(device) = cpal::default_host().default_output_device() else {
        return (fallback, None);
    };
    let Ok(supported) = device.default_output_config() else {
        return (fallback, None);
    };
    // Clamp into what the device will actually accept; `Unknown` means cpal can't tell us the
    // range, and asking for a size outside it fails the stream build — so leave those alone.
    let cpal::SupportedBufferSize::Range { min, max } = supported.buffer_size() else {
        info!("audio: device reports no buffer-size range — leaving it to the driver");
        let rate = supported.config().sample_rate;
        return (fallback, Some(rate));
    };
    let frames = TARGET_BUFFER_FRAMES.clamp(*min, *max);
    let mut config = supported.config();
    config.buffer_size = cpal::BufferSize::Fixed(frames);
    info!(
        "audio: {} Hz, {} ch, buffer {frames} frames (~{:.1} ms)",
        config.sample_rate,
        config.channels,
        f64::from(frames) / f64::from(config.sample_rate) * 1000.0,
    );
    let rate = config.sample_rate;
    (
        CpalBackendSettings {
            config: Some(config),
            ..fallback
        },
        Some(rate),
    )
}

/// The mix-health counters — what a crackle actually *is*, in numbers (decision 1026).
///
/// Before this, a crackle was invisible to us: the only report was the director's ear, and we
/// could not tell a missed callback deadline from a stepped parameter from a starved decoder.
/// kira hands us the measurement and we were not reading it.
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct MixHealth {
    /// The most recent callback load: elapsed / allotted, where allotted is `frames / sample_rate`
    /// (kira's `pop_cpu_usage`). **`>= 1.0` means the mix missed its deadline** — that is an
    /// underrun, and an underrun is audible as a crack.
    pub(crate) load: f32,
    /// Worst load seen since the last [`Mixer::take_health_peak`].
    pub(crate) peak_load: f32,
    /// Callbacks at/over the deadline, since launch.
    pub(crate) overruns: u64,
    /// cpal stream errors kira handled, since launch (device loss, `BufferUnderrun`).
    pub(crate) stream_errors: u64,
}

impl Mixer {
    /// Drain the backend's health queues. Cheap and non-blocking (two ring-buffer pops per
    /// frame); the queues are bounded, so *not* draining them is what loses information.
    pub(crate) fn poll_health(&mut self) -> MixHealth {
        let backend = self.manager.backend_mut();
        while let Some(load) = backend.pop_cpu_usage() {
            self.health.load = load;
            self.health.peak_load = self.health.peak_load.max(load);
            if load >= 1.0 {
                self.health.overruns += 1;
            }
        }
        while let Some(err) = backend.pop_error() {
            self.health.stream_errors += 1;
            warn!("audio: stream error — {err}");
        }
        self.health
    }

    /// Read and reset the peak — so a report covers the window since the last one, not all time.
    pub(crate) fn take_health_peak(&mut self) -> f32 {
        std::mem::take(&mut self.health.peak_load)
    }
}

/// Move a live spatial track's emitter — the tracked-channel follow (the client's `0x61fec0`
/// tracked play): the kit pump drives this each frame for a source-tagged loop so the sound
/// rides its unit. Free function (not a `Mixer` method) because the pump holds only the track
/// handle, not the mixer.
pub(crate) fn set_track_position(track: &mut SpatialTrackHandle, pos: Vec3) {
    track.set_position(
        mint::Vector3 {
            x: pos.x,
            y: pos.y,
            z: pos.z,
        },
        snap(),
    );
}

/// EAX listener properties → Freeverb `(feedback, damping, wet level)` — the lossy projection
/// [`Mixer::set_reverb`] documents. Pure so the mapping is pinnable by tests without a device.
fn freeverb_projection(p: &SoundProvider) -> (f64, f64, Decibels) {
    let feedback = if p.decay_time > 0.0 {
        10f64.powf(-0.108 / f64::from(p.decay_time)).min(0.98)
    } else {
        0.0
    };
    let damping = ((1.0 - f64::from(p.decay_hf_ratio) / 2.0).max(0.0) * 0.7
        + f64::from(-p.room_hf) / 10_000.0 * 0.3)
        .clamp(0.0, 1.0);
    let wet_db = ((p.room + p.reverb) as f32 / 100.0).min(6.0);
    let wet = if wet_db <= Decibels::SILENCE.0 {
        Decibels::SILENCE
    } else {
        Decibels(wet_db)
    };
    (feedback, damping, wet)
}

/// Decode audio-file bytes (WAV incl. IMA-ADPCM, MP3) into a ready-to-play SFX sound.
pub(crate) fn sfx_from_bytes(bytes: Vec<u8>) -> Result<StaticSoundData> {
    StaticSoundData::from_cursor(std::io::Cursor::new(bytes)).context("decoding sfx")
}

/// Decode a looping **bed** (zone ambience, weather, underwater) fully and mark it whole-file
/// looping. Loop beds must NOT stream: kira's streaming decoder misreports these 22050 Hz PCM
/// WAVs at 2× their real duration (probe-verified 2026-07-02: ForestNormalDay.wav is 60 s,
/// streaming said 120 s), so `loop_region(..)` lands past EOF and the stream dies mid-file
/// instead of wrapping — the "ambience goes silent after a minute" bug. Statically decoded, the
/// frame count is exact and the loop wraps (probe-verified). Cost: the decoded PCM (~10 MB for a
/// 60 s stereo bed), paid only while the bed plays; music keeps streaming (it never loops).
pub(crate) fn loop_from_bytes(bytes: Vec<u8>) -> Result<StaticSoundData> {
    Ok(sfx_from_bytes(bytes)?.loop_region(..))
}

/// Wrap compressed audio bytes for decode-streaming (music/ambience). The bytes go in behind
/// [`PromotingSource`] — the decode-thread QoS fix (decision 1109) — never a bare cursor.
pub(crate) fn stream_from_bytes(bytes: Vec<u8>) -> Result<StreamingSoundData<FromFileError>> {
    StreamingSoundData::from_media_source(PromotingSource(std::io::Cursor::new(bytes)))
        .context("opening stream")
}

/// Compressed-audio source whose reads promote the calling thread — the stream-decode QoS fix
/// (decision 1109).
///
/// kira decodes each streaming sound on a thread of its own (`decode_scheduler.rs`, a bare
/// `std::thread::spawn`) feeding a 16 384-frame ring buffer (~0.37 s at 44.1 kHz); when that
/// buffer runs dry mid-play the sound zero-fills whole callback blocks (`streaming/sound.rs`,
/// the `slots() < 2` branch) — hard amplitude steps, i.e. a crackle, and one that registers on
/// **neither** [`MixHealth`] meter: the mix met its deadline and no stream error fired. A bare
/// spawn lands at *default* QoS — macOS does not inherit the spawner's class
/// (`thread_qos::tests` pins this) — which is below everything the world-entry burst runs: the
/// compute pool at user-interactive, eight IO workers at user-initiated. First login is the
/// guaranteed collision: the glue theme is mid-fade (a live stream) exactly while every core
/// saturates, so its decoder starves and the loading screen crackles.
///
/// The only place our code runs on that thread is the decoder's reads from the byte source, so
/// the source itself promotes: first touch per thread raises it to user-interactive. The class
/// is justified — the mix's input has the hardest deadline in the app, and the thread sleeps
/// ~99% of its life (MP3 decodes at ~100× real time), so the promotion costs nothing. The latch
/// is thread-local; the open/probe calls kira makes on the *main* thread re-assert that
/// thread's existing class, harmlessly.
struct PromotingSource(std::io::Cursor<Vec<u8>>);

/// Once-per-thread promotion latch for [`PromotingSource`].
fn promote_decode_thread() {
    std::thread_local! {
        static PROMOTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    PROMOTED.with(|p| {
        if !p.get() {
            benilla_world::thread_qos::promote_current_thread(
                benilla_world::thread_qos::QosClass::UserInteractive,
            );
            debug!(
                "audio: stream decode thread {:?} promoted",
                std::thread::current().id()
            );
            p.set(true);
        }
    });
}

impl std::io::Read for PromotingSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        promote_decode_thread();
        self.0.read(buf)
    }
}

impl std::io::Seek for PromotingSource {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        promote_decode_thread();
        self.0.seek(pos)
    }
}

impl symphonia::core::io::MediaSource for PromotingSource {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.0.get_ref().len() as u64)
    }
}

/// How much audible time may go missing per watch window before [`StreamWatch`] reports —
/// over two zero-filled ~43 ms callback blocks: past the ±1-block quantization of the shared
/// position and the sample-clock/wall-clock drift, and far under any real starvation burst
/// (hundreds of ms).
const STREAM_STARVED_MIN_SECS: f64 = 0.1;
/// The accounting window [`StreamWatch`] compares over.
const STREAM_WATCH_WINDOW_SECS: f64 = 1.0;

/// Position-freeze watch over a live stream — the starvation meter [`MixHealth`] lacks (decision
/// 1109; `poll_mix_health`'s docs name this exact blind spot).
///
/// A starved stream is *audible* — kira zero-fills the callback block — but the mix made its
/// deadline, so nothing else measures it. What does move is the stream's shared position: it
/// freezes while the state stays audible. Fed once per frame from the held handle; over each
/// ~[`STREAM_WATCH_WINDOW_SECS`] window it compares wall time elapsed against position advanced
/// and WARNs when more than [`STREAM_STARVED_MIN_SECS`] went missing. Playback rate is always
/// 1.0 here (WoW pitches no music), so the two clocks agree to drift well under the threshold.
///
/// **Known blind spot (1112, deliberate):** the watch samples on the main thread, so a stream
/// that starves *during a main-thread stall* and reaches `Stopped` before the next frame is
/// never accounted — the window is discarded by the reset. Reconstructing it from the stop
/// deadline would trade false positives for coverage; the mix tap (`$WOW_MIX_TAP`) owns that
/// class instead, because the tap records on the audio thread and doesn't care what the main
/// thread was doing.
pub(crate) struct StreamWatch {
    label: &'static str,
    last_pos: Option<f64>,
    expected: f64,
    advanced: f64,
}

impl StreamWatch {
    pub(crate) fn new(label: &'static str) -> Self {
        Self {
            label,
            last_pos: None,
            expected: 0.0,
            advanced: 0.0,
        }
    }

    /// Per-frame feed from the held handle. Call every frame the handle exists.
    pub(crate) fn feed(&mut self, handle: &StreamingSoundHandle<FromFileError>, dt: f64) {
        use kira::sound::PlaybackState as S;
        let audible = matches!(handle.state(), S::Playing | S::Stopping);
        if let Some(lost) = self.observe(audible, handle.position(), dt) {
            warn!(
                "audio: {} stream starved — ~{:.0} ms of injected silence in the last \
                 {STREAM_WATCH_WINDOW_SECS:.0} s (decode thread outrun) — this is what a \
                 crackle sounds like",
                self.label,
                lost * 1000.0,
            );
        }
    }

    /// Drop the baseline — call when the watched handle is dropped/replaced, so the next
    /// stream's position (starting at 0, i.e. *behind* the old one's) can't read as a freeze.
    pub(crate) fn reset(&mut self) {
        self.last_pos = None;
        self.expected = 0.0;
        self.advanced = 0.0;
    }

    /// The accounting core, pure so the tests below can drive it without a device. Returns
    /// `Some(lost_secs)` when a window closes starved.
    fn observe(&mut self, audible: bool, pos: f64, dt: f64) -> Option<f64> {
        // Non-audible states (stopped, paused) drop the baseline: position legitimately holds.
        if !audible {
            self.reset();
            return None;
        }
        let Some(last) = self.last_pos.replace(pos) else {
            return None; // first audible frame — baseline only
        };
        self.expected += dt;
        // `max(0.0)`: a track replaced on the slot mid-window jumps backwards once (the new
        // stream starts at 0); count it as no advance for that one frame, not as negative.
        self.advanced += (pos - last).max(0.0);
        if self.expected < STREAM_WATCH_WINDOW_SECS {
            return None;
        }
        let lost = self.expected - self.advanced;
        self.expected = 0.0;
        self.advanced = 0.0;
        (lost > STREAM_STARVED_MIN_SECS).then_some(lost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(decay: f32, hf_ratio: f32, room: i32, room_hf: i32, reverb: i32) -> SoundProvider {
        SoundProvider {
            id: 0,
            name: String::new(),
            flags: 0,
            decay_time: decay,
            room,
            room_hf,
            decay_hf_ratio: hf_ratio,
            reflections: 0,
            reverb,
            env_diffusion: 1.0,
            env_size: 1.0,
        }
    }

    /// The EAX→Freeverb projection over the byte-verified 5875 rows: longer decay → higher
    /// feedback (monotone), the underwater HF kill lands near-full damping, and PRESET_OFF's
    /// Room −10000 collapses the wet level to the silence floor.
    #[test]
    fn freeverb_projection_orders_the_presets() {
        // PRESET_GENERIC / PRESET_CAVE / PRESET_HANGAR: decay 1.49 / 3.0 / 10.05 s.
        let generic = preset(1.49, 0.86, -1000, -100, 200);
        let cave = preset(3.0, 1.3, -1000, -500, -402);
        let hangar = preset(10.05, 0.26, -1000, -1000, 198);
        let (fg, dg, wg) = freeverb_projection(&generic);
        let (fc, _, _) = freeverb_projection(&cave);
        let (fh, _, _) = freeverb_projection(&hangar);
        assert!(fg < fc && fc < fh, "feedback monotone in decay time");
        assert!(fh <= 0.98, "runaway clamp");
        assert!(
            (wg.0 - -8.0).abs() < 1e-3,
            "GENERIC wet = (−1000+200) mB → −8 dB"
        );
        assert!((0.0..=1.0).contains(&dg));

        // Underwater (11): DecayHFRatio 0.1 + RoomHF −10000 → near-full damping; wet capped +6.
        let underwater = preset(1.0, 0.1, -1000, -10000, 1700);
        let (_, du, wu) = freeverb_projection(&underwater);
        assert!(du > 0.9, "underwater kills the highs (got {du})");
        assert!((wu.0 - 6.0).abs() < 1e-3, "wet cap at +6 dB");

        // PRESET_OFF: Room −10000 → below the −60 dB floor → SILENCE.
        let off = preset(1.0, 1.0, -10000, -10000, 200);
        let (_, _, w_off) = freeverb_projection(&off);
        assert_eq!(w_off, Decibels::SILENCE);
    }

    /// Real 1.12 assets decode through the seam — a short interface WAV as SFX and a zone-music
    /// MP3 as a stream (both paths verified against the 5875 listfile). Needs no audio device
    /// (decode only); skips silently when the gitignored client install isn't present, so CI
    /// without `WoW/Data` stays green.
    #[test]
    fn real_wav_and_mp3_decode() {
        let data = benilla_formats::wow_data_or_skip!();
        let Ok(chain) = benilla_formats::open_chain(&data) else {
            eprintln!("skipping: no client data at {}", data.display());
            return;
        };

        let wav = chain
            .read("Sound\\interface\\LevelUp.wav")
            .expect("LevelUp.wav in the chain");
        let sfx = sfx_from_bytes(wav).expect("WAV decodes");
        assert!(
            sfx.duration().as_millis() > 500,
            "LevelUp.wav should be a real, non-trivial clip"
        );

        let mp3 = chain
            .read("Sound\\Music\\CityMusic\\Darnassus\\Darnassus Walking 1.mp3")
            .expect("Darnassus mp3 in the chain");
        let stream = stream_from_bytes(mp3).expect("MP3 opens for streaming");
        assert!(
            stream.duration().as_secs() > 30,
            "zone music should be minutes long"
        );
    }

    /// The starvation accounting (decision 1109): a stream that advances in lockstep with wall
    /// time stays quiet; one whose position freezes mid-window (kira's zero-fill) reports the
    /// missing time; a slot swap's one-frame backward jump never reads as a freeze; and going
    /// non-audible drops the baseline so a later stream starts clean.
    #[test]
    fn stream_watch_accounts_freezes_not_swaps() {
        let dt = 1.0 / 60.0;

        // Healthy: position tracks wall time exactly — a full window closes clean.
        let mut w = StreamWatch::new("test");
        let mut pos = 0.0;
        for _ in 0..90 {
            assert_eq!(w.observe(true, pos, dt), None, "healthy stream reported");
            pos += dt;
        }

        // Starved: 18 frames (~0.3 s) frozen inside the window → that window reports ~0.3 s.
        let mut w = StreamWatch::new("test");
        let mut pos = 0.0;
        let mut reports = Vec::new();
        for i in 0..90 {
            if !(30..48).contains(&i) {
                pos += dt;
            }
            if let Some(lost) = w.observe(true, pos, dt) {
                reports.push(lost);
            }
        }
        assert_eq!(reports.len(), 1, "one starved window: {reports:?}");
        assert!(
            (reports[0] - 0.3).abs() < 0.05,
            "lost ≈ 0.3 s, got {reports:?}"
        );

        // Slot swap: position jumps backwards once (new track starts at 0) — no report.
        let mut w = StreamWatch::new("test");
        let mut pos = 40.0;
        for i in 0..90 {
            if i == 30 {
                pos = 0.0;
            }
            assert_eq!(
                w.observe(true, pos, dt),
                None,
                "slot swap reported as freeze"
            );
            pos += dt;
        }

        // Non-audible resets the baseline: a stopped-then-restarted stream (position far behind
        // the old one's) opens a fresh window instead of inheriting a phantom freeze.
        let mut w = StreamWatch::new("test");
        let mut pos = 70.0;
        for _ in 0..30 {
            assert_eq!(w.observe(true, pos, dt), None);
            pos += dt;
        }
        assert_eq!(w.observe(false, pos, dt), None);
        let mut pos = 0.0;
        for _ in 0..90 {
            assert_eq!(w.observe(true, pos, dt), None, "restart read as freeze");
            pos += dt;
        }
    }

    /// The fade contract every transition rests on: `stop(tween)` fades the channel to silence
    /// over the tween's duration on the audio thread, and **dropping the handle immediately after
    /// does not cut it short** — the command is already published to the renderer's triple-buffer
    /// (decision 0100's fire-and-forget fade-stop; the director doubted it fires at all). We drive
    /// kira's renderer by hand through a capturing backend and watch a full-amplitude loop after
    /// its handle is dropped: it must ramp *through the middle* — never jump full→0 (an instant
    /// cut) and never stay at full (a command lost with the handle). Device-free (mock renderer),
    /// so it runs in CI.
    #[test]
    fn stop_fade_ramps_after_handle_drop() {
        use kira::backend::{Backend, Renderer};
        use kira::sound::static_sound::StaticSoundData;
        use kira::{AudioManager, AudioManagerSettings, Frame, Tween};
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        // A backend that just holds the renderer and lets the test pump audio out of it.
        struct Capture(Arc<Mutex<Option<Renderer>>>);
        impl Backend for Capture {
            type Settings = ();
            type Error = ();
            fn setup(_: (), _buf: usize) -> Result<(Self, u32), ()> {
                Ok((Capture(Arc::new(Mutex::new(None))), 100)) // 100 Hz → one frame = 10 ms
            }
            fn start(&mut self, renderer: Renderer) -> Result<(), ()> {
                *self.0.lock().unwrap() = Some(renderer);
                Ok(())
            }
        }

        let mut manager =
            AudioManager::<Capture>::new(AudioManagerSettings::default()).expect("manager");
        let slot = manager.backend_mut().0.clone();
        // Render `n` frames; return the mean absolute left-channel amplitude of the block.
        let render = |n: usize| -> f32 {
            let mut guard = slot.lock().unwrap();
            let r = guard.as_mut().expect("renderer started");
            r.on_start_processing();
            let mut out = vec![0.0f32; n * 2];
            r.process(&mut out, 2);
            out.iter().step_by(2).map(|s| s.abs()).sum::<f32>() / n as f32
        };

        // A full-amplitude looping bed, so it can only fall silent by our fade — never by EOF.
        let frames: Arc<[Frame]> = (0..100).map(|_| Frame::from_mono(1.0)).collect();
        let bed = StaticSoundData {
            sample_rate: 100,
            frames,
            settings: Default::default(),
            slice: None,
        }
        .loop_region(..);

        let mut h = manager.play(bed).expect("play");
        assert!(render(5) > 0.9, "bed plays at full before the fade");

        // benilla's exact pattern: fade to silence over 1 s (100 frames), drop the handle now.
        h.stop(Tween {
            duration: Duration::from_secs(1),
            ..Default::default()
        });
        drop(h);

        // Watch the whole fade in 50 ms blocks (1.3 s of coverage over the 1 s fade).
        let series: Vec<f32> = (0..26).map(|_| render(5)).collect();
        let midband = series.iter().filter(|&&a| (0.05..0.9).contains(&a)).count();
        assert!(
            *series.last().unwrap() < 0.05,
            "silent once the fade completes — the stop survived the handle drop ({series:?})"
        );
        assert!(
            midband >= 3,
            "the fade is gradual, not an instant cut — needs blocks mid-ramp ({series:?})"
        );
    }
}
