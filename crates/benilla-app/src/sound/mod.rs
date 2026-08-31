//! sound — WoW's owned audio selection/scheduling over a delegated mixer (decision 0070).
//!
//! Three separable pieces (0070): the **mixer seam** ([`mixer`] — kira behind an FMOD-shaped
//! surface), the **kit player** (WoW's owned selection math off `SoundEntries.dbc` — next slice
//! commit), and the **trigger surface** (zone music/ambience, UI, world emitters — phased in).
//! This module owns the Bevy plumbing: the output resource, the per-frame listener sync from the
//! world camera, and the always-on player config (decision 0026: config is gameplay state, the
//! debug panel only edits it).

use bevy::prelude::*;

use crate::net::Embodied;
use crate::player::{head_height, CameraPivot, Player};
use benilla_world::schedule::WorldStage;
use benilla_world::view::WorldCamera;

mod anim_events;
mod cinematic;
mod combat;
mod creature;
mod emote;
pub(crate) mod footsteps;
mod gameobject;
mod glue;
mod greeting;
mod hal_overload;
pub(crate) mod interior;
mod kit;
mod limiter;
mod liquid_loop;
mod math;
mod meter;
mod missile;
mod mix_tap;
mod mixer;
mod money;
mod mount;
mod probe;
mod reverb;
mod sheathe;
mod spell;
mod ui;
mod water;
mod weather;
mod zone;
pub(crate) use emote::EmoteSounds;
pub(crate) use glue::GlueSound;
pub(crate) use greeting::NpcGreetingRequest;
pub(crate) use mixer::Mixer;
pub(crate) use ui::{AutoEquipSound, LootPickupSound};
pub(crate) use zone::ExplorationSounds;

/// Player-facing audio config — always-on, player-faithful defaults (decision 0026: no
/// gameplay→dev coupling; the debug panel edits this, it doesn't own it).
///
/// Defaults are the client's CVar registration defaults (wow-re `benilla-pins.md` B10,
/// VERIFIED): `MasterVolume` 1.0, `SoundVolume` 1.0, **`MusicVolume` 0.4**,
/// **`AmbienceVolume` 0.6** — a fresh 1.12 install is NOT uniform full volume.
#[derive(Resource)]
pub(crate) struct SoundConfig {
    /// Master enable — 1.12's `MasterSoundEffects` CVar, the Sound options "Enable All Sound"
    /// checkbox (SoundOptionsFrame.lua index 1; registrar default "1", B10). In the binary its
    /// callback sets the engine-wide pause flag (`0x457500` → `0x7a6570` → `DAT_0087cf00`);
    /// benilla zeroes every category through [`Self::category_amp`] instead — channels keep
    /// running silently (the `muted` posture), same audible truth.
    pub enabled: bool,
    /// Quick mute — toggled by the dev chord + `M` (its plane is per-OS: decisions 0585, 0867).
    /// Zeroes the **main track only**: selection and channel life go on untouched, so unmute is
    /// instant (unlike `enabled`, which stops sounds from being picked at all).
    ///
    /// Starts **`false`** — a run you launch has sound, like the real client (decision 1026). It
    /// used to boot muted so that automated runs stayed quiet, but that made the human's every
    /// session start with a chord press to hear anything. The quiet belongs on the automated side
    /// instead: `$WOW_NOSOUND` (agents) and `$WOW_CAPTURE` (the visual harness) open no device at
    /// all — see [`SoundPlugin`].
    pub muted: bool,
    /// Master volume, linear `[0,1]` (the whole mix, applied on the main track).
    pub master: f32,
    /// Per-category sliders `[0,1]` — the client's SFX/music/ambience CVars, multiplied into
    /// each channel by the pump (`0x7a5dc0`'s `cat` factor).
    pub sfx: f32,
    pub music: f32,
    pub ambience: f32,
    /// The per-category enable checkboxes — 1.12's own `EnableMusic` / `EnableAmbience` CVars
    /// (registrar defaults "1", wow-re B10; 1.12 has NO SFX-only toggle — `MasterSoundEffects`
    /// above is the master). Gated in [`Self::category_amp`], so a disable silences the category
    /// everywhere at once. Divergence, disclosed: the reference's `EnableMusic` callback
    /// stops/re-selects the music stream (`0x457490` → `0x45b050`/`0x45aeb0`) — benilla keeps
    /// the stream alive at zero, so re-enabling resumes mid-track where the reference re-picks.
    pub music_enabled: bool,
    pub ambience_enabled: bool,
    /// Zone reverb — 1.12's `SoundReverb` CVar (`0x4573be` registration, callback `0x4574d0`,
    /// flag byte `[0x835a4c]`). The flag gates **both** EAX paths: the zone/environment preset
    /// (`0x45a75b`: flag zero ⇒ `FSOUND_Reverb_SetProperties` is never called) and the
    /// per-channel wet send (`0x458f13` ⇒ `FSOUND_Reverb_SetChannelProperties`). Read by
    /// [`reverb::zone_reverb`].
    ///
    /// **Registrar default is `"1"`; ours is `false`** — the one place benilla's CVar defaults
    /// leave the binary's (decisions 1153, 1155). The reference *emits* both calls on a stock
    /// boot — VERIFIED, and its three writers of `[0x835a4c]` all write 1 — but they are FMOD 3's
    /// EAX API, and its own header says `ONLY SUPPORTED ON WIN32 W/ FSOUND_HW3D FLAG`. The
    /// reference client's `Logs/Sound.log` on this machine reports
    /// `Driver: 0 'Primary Sound Driver' 00000000` (caps 0 — no `HARDWARE`/`EAX2`/`EAX3`) and
    /// `0 3D hardware` channels, and DirectSound lost hardware mixing in Vista. So "emitted, and
    /// rendered as nothing" — **the render half is INFERRED**, not byte-verified (we cannot read
    /// `fmod.dll` from `WoW.exe`); the live capture that would settle it is named in 1155.
    /// benilla is the first 1.12 client to render this DSP in software, so `false` is what the
    /// reference is heard to produce rather than what its registrar says. **This is not what
    /// fixes B236** — that is the `EAXDef` dryness on [`Mixer::play_3d`], which holds whichever
    /// way this flag sits. Turn it on with `/run SetCVar("SoundReverb", 1)` — applies live and
    /// persists — to hear what the DBC data asks for. (The reference's route is
    /// `/console SoundReverb 1`; benilla has no `ConsoleExec` yet.)
    pub reverb: bool,
    /// **The loading cover's audio half** — NOT a CVar, a per-frame live bit fed from
    /// [`crate::loading_screen::LoadingScreen::covering`] by [`feed_world_hold`]. While the cover
    /// is up, no new sound *starts* ([`kit::play_kit_ext`]/[`kit::play_file`] return early) and
    /// the zone beds hard-stop and stay down (`zone::zone_audio`'s hold arm) — the reference
    /// blocks on its world load, so nothing world-side is audible under its loading screen, and
    /// an async client has to build that observable explicitly (0737's argument, in audio).
    /// Sounds already ringing when the cover rises play out — the reference's sound engine keeps
    /// running through the load edge too, which is what lets the glue theme's 2 s fade tail
    /// (1109) ride under the entry cover exactly as the real client's does: its amp is applied
    /// once at start, and a playing stream never passes back through the kit starters.
    pub world_hold: bool,
    /// **The cinematic's music stop** — NOT a CVar, a per-frame live bit fed from
    /// [`crate::cinematic::Cinematic`] by [`feed_music_suppression`], and the exact runtime
    /// counterpart of the reference's `[0xb06cc8]` (wow-re `sound/scratch/cinematic-audio-law.md`,
    /// §5, VERIFIED).
    ///
    /// A cinematic asserts precisely what `/console EnableMusic 0` asserts: both reach the same
    /// setter `0x4603b0` — the CVar handler at `0x4574a4`, the cinematic's own start at `0x48ed83`
    /// — and on the disable edge that setter runs `0x7a5700`, which is **stop-and-destroy and
    /// takes no duration**. So the zone track is CUT, never faded, and the per-tick music pump
    /// bails at its first instruction (`0x460040`: `mov al,[0xb06cc8]; test al,al; jne`) for as
    /// long as the flag is set.
    ///
    /// **Separate from [`Self::music_enabled`] on purpose.** The cinematic writes the runtime
    /// flag, never the CVar — and neither do we, because benilla persists a CVar *diff* to
    /// `config.toml`: asserting `EnableMusic` here would save a player's music off for good if
    /// they quit during a 102-second intro.
    ///
    /// **Ambience is deliberately untouched**, and that is verified rather than assumed: ambience
    /// has the analogous suppress flag (`[0x836424]`, written only from `EnableAmbience`) and the
    /// cinematic never writes it.
    ///
    /// **One thing of the reference's we deliberately do not carry: its restore latch.** It records
    /// at the start whether music was already off (`[0xb4e278] = (flag == 0)`) and re-enables at
    /// the stop only if it was the one that disabled it — because there the flag and the player's
    /// `EnableMusic` CVar are the *same* bit, so restoring blindly would switch a player's music
    /// back on. Here they are two: this is a separate runtime bit, and a player who set
    /// `EnableMusic 0` is still silenced by [`Self::category_amp`] whatever this says. The latch
    /// would guard against nothing, so it is left out rather than transcribed for its own sake.
    pub music_suppressed: bool,
    /// The **output limiter** — benilla's own `SoundOutputLimiter` CVar (decision 1551), default
    /// **on**. Not a 1.12 CVar: the reference has no such DSP and does not need one, because it
    /// hands its whole audible mix to FMOD 3 and carries its headroom elsewhere (the SFX-bus
    /// auto-duck, wow-re `benilla-pins.md` B15). benilla sums into f32 and kira answers an
    /// over-scale sum with a hard clamp, which is audible distortion the moment two full-scale
    /// kits overlap — see [`limiter`] for the measured arithmetic. This exists so the fix can be
    /// A/B'd against what it fixed: `/run SetCVar("SoundOutputLimiter", 0)` applies live.
    pub limiter: bool,
}

impl SoundConfig {
    /// The category slider a channel multiplies in (`0x7a5dc0`'s category pick).
    pub(crate) fn category_amp(&self, cat: kit::SoundCategory) -> f32 {
        if !self.enabled {
            return 0.0;
        }
        match cat {
            kit::SoundCategory::Sfx => self.sfx,
            kit::SoundCategory::Music if self.music_enabled => self.music,
            kit::SoundCategory::Ambience if self.ambience_enabled => self.ambience,
            _ => 0.0,
        }
    }
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            muted: false,
            master: 1.0,
            sfx: 1.0,
            music: 0.4,
            ambience: 0.6,
            music_enabled: true,
            ambience_enabled: true,
            reverb: false,
            world_hold: false,
            music_suppressed: false,
            limiter: true,
        }
    }
}

/// Copy the loading cover's state into [`SoundConfig::world_hold`], once per frame, in
/// `PreUpdate` — before every trigger system, so the whole frame reads one answer. The one-frame
/// lag off the raise (the raise happens in the previous frame's `Present`) is inherent and
/// harmless: the raise frame is the snap frame, and the leaks this closes — units streaming in,
/// footsteps, the fall grunt, the zone beds — all fire on later frames. The lag also keeps the
/// enter-world button's own click audible: it plays on the raise frame, before the bit flips.
fn feed_world_hold(
    mut config: ResMut<SoundConfig>,
    screen: Res<crate::loading_screen::LoadingScreen>,
) {
    let hold = screen.covering();
    if config.world_hold != hold {
        config.world_hold = hold;
    }
}

/// Copy the cinematic's music stop into [`SoundConfig::music_suppressed`], once per frame.
///
/// **Ordered `WorldStage::Stream`, not `PreUpdate` beside [`feed_world_hold`]** — the two look
/// alike and are not. The cover `feed_world_hold` reads is a state that lasts seconds, so reading
/// last frame's answer costs nothing; the cinematic's music cut is a one-frame *edge*, and
/// `crate::cinematic`'s driver asserts it in `WorldStage::Input` — after `PreUpdate` has already
/// run. Read there, [`zone::zone_audio`] (`WorldStage::Present`) saw a stale `false` on exactly
/// the frame a cinematic started: on a first-login race intro that is the first uncovered frame,
/// where the zone's own area-change block starts a track — or, worse, the zone's *intro fanfare*,
/// which is then stamped as played and does not come back for `MinDelayMinutes`. One frame later
/// the suppression cut it again, so the symptom was a click and a consumed fanfare rather than
/// anything you could hear as music. `Stream` sits between the two (`Net → Input → Stream →
/// Present`), so the flag the pump reads is always this frame's.
fn feed_music_suppression(
    mut config: ResMut<SoundConfig>,
    cinematic: Option<Res<crate::cinematic::Cinematic>>,
) {
    let playing = cinematic
        .as_deref()
        .is_some_and(crate::cinematic::Cinematic::is_playing);
    if config.music_suppressed != playing {
        config.music_suppressed = playing;
    }
}

/// The backend output. `mixer` is `None` when no audio device exists (headless/CI) or
/// `$WOW_NOSOUND` is set — every consumer tolerates silence. A **non-Send** resource: the
/// backend's device stream is not `Send` on every platform, so all audio systems run on the
/// main thread (they are cheap parameter feeds).
pub(crate) struct SoundOutput {
    pub(crate) mixer: Option<Mixer>,
    /// Live kit channels, owned and pumped by [`kit::pump_channels`].
    pub(crate) channels: Vec<kit::ActiveChannel>,
    /// The measuring-mode recorder, when `$WOW_SOUND_PROBE` armed one (decision 1556). It rides
    /// here rather than in a resource of its own so the kit player — which already holds `out` —
    /// can stamp every play on the capture's timeline with no new plumbing.
    pub(crate) probe: Option<probe::Probe>,
    /// Live **stream** voices, reported by their owners each frame ([`zone`], [`glue`],
    /// [`cinematic`]).
    ///
    /// These count against the same ceiling as everything else ([`kit::SOFTWARE_CHANNELS`]): the
    /// reference's music, ambience and liquid loops all land on its uncapped bus 0 and occupy
    /// FMOD channels exactly like a sword swing does. A field per owner rather than one shared
    /// counter because each owner **rewrites its own** every frame from its own live handles — a
    /// shared counter with several writers drifts the first time a fade is interrupted, and a
    /// voice budget that drifts is worse than none.
    pub(crate) zone_streams: usize,
    pub(crate) glue_streams: usize,
    /// The cinematic narration's own stream — a third long-lived owner, and for a long time the
    /// one the budget could not see (a 102-second race intro spent all of it one voice short of
    /// the truth). Rewritten each frame by [`cinematic::drive_narration`], like its neighbours.
    pub(crate) cinematic_streams: usize,
    /// One-shots that lost their slot to a louder newcomer, and plays refused because nothing
    /// live was quieter than them (decision 1557). Reported by the probe.
    pub(crate) voices_stolen: u64,
    pub(crate) voices_denied: u64,
    /// Same-kit copies dropped by [`kit::SAME_KIT_MAX`] (decision 1560).
    pub(crate) copies_dropped: u64,
}

impl SoundOutput {
    /// Everything the device is currently mixing — kit channels plus the held streams. This is
    /// the number [`kit::SOFTWARE_CHANNELS`] bounds.
    pub(crate) fn live_voices(&self) -> usize {
        self.channels.len() + self.zone_streams + self.glue_streams + self.cinematic_streams
    }
}

/// The 3D-audio listener pose for this frame — the single authority every sound system reads, in
/// Bevy space. Computed once by [`update_audio_listener`]; consumed by the mixer feed, the channel
/// pump's distance/rolloff math, and each trigger's selection-time audibility gate.
///
/// The client's `SoundListenerAtCharacter` default is `"1"` (wow-re benilla-pins B14): the listener
/// sits at the **character**, not the camera — so 3D volume and pan are independent of zoom and
/// camera orbit. `pos` = the self-avatar's head (feet + [`head_height`]); `rot` = the character's
/// *facing* about world-up (`Quat::from_rotation_y(face_yaw)`), so panning tracks where the body
/// faces, NOT where the camera looks. The camera eye + basis are the fallback (the client's
/// `=0` path): pre-login, in free-fly (`detached`), or before the body attaches.
/// `Material.dbc` — a shared sound fact with two consumers: the armor foley off `$FSD`
/// ([`footsteps`]) and, through the same table's `Flags` column, both the metal/wood split of
/// every weapon impact and the armor slot a player victim presents ([`combat`]). Loaded once
/// here rather than in either, because a second loader over one DBC is how a schema drifts.
#[derive(Resource)]
pub(crate) struct Materials(pub(crate) benilla_formats::MaterialCatalog);

fn load_materials(mut commands: Commands, assets: Option<Res<benilla_assets::WorldAssets>>) {
    use benilla_assets::LockRecover;
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_material_catalog(&mut chain)
    };
    match loaded {
        Ok(cat) => {
            info!("sound: {} material rows", cat.len());
            commands.insert_resource(Materials(cat));
        }
        Err(e) => warn!("sound: materials failed to load: {e:#}"),
    }
}

/// **The material of the body you are wearing** — the chest item's `Material` id, or `None`.
///
/// Two sounds ask this, and both ask it the reference's way, through `[player+0x1d38]` element 4
/// (`EQUIPMENT_SLOT_CHEST`): the armor foley ([`footsteps`], `0x62fa30`) and the impact slot a
/// player victim presents ([`combat`], `0x62fb70`). Shared rather than written twice, because
/// they are one question with one answer and the reach is the subtle part.
///
/// **That reach is self-only, and not by our choice.** The array's count is written 113 when the
/// object's guid matches the local player's and 0 otherwise (`0x5dd454`), so in the reference no
/// other player has a chest material at all. benilla lands there for free: `PLAYER_FIELD_INV_SLOT_*`
/// is a private descriptor field the server sends only to you, so `player_inv_slot` returns
/// `None` for everyone else. Callers therefore need no "is this me" test of their own — but they
/// must ask it of a store that IS the player's.
///
/// `None` covers every one of the reference's own misses: no store, an empty chest, the item
/// object not streamed, and a template still in flight (asked once, answered next frame).
pub(super) fn worn_chest_material(
    store: Option<&crate::net::ObjectStore>,
    items: &mut crate::items::Items,
    net: &crate::net::NetCommands,
) -> Option<u32> {
    /// Index 4 of the inv-slot array — `0x62fa50`/`0x62fb86` read the fifth 8-byte guid.
    const EQUIPMENT_SLOT_CHEST: u8 = 4;
    let guid = store?.0.player_inv_slot(EQUIPMENT_SLOT_CHEST)?;
    let entry = items.object(guid)?.object_entry()?;
    Some(items.held(entry, net)?.material)
}

#[derive(Resource)]
pub(crate) struct AudioListener {
    pub(crate) pos: Vec3,
    pub(crate) rot: Quat,
}

impl Default for AudioListener {
    fn default() -> Self {
        Self {
            pos: Vec3::ZERO,
            rot: Quat::IDENTITY,
        }
    }
}

/// The world soundscape is live: in the world AND seated on the avatar ([`Player::active`]).
/// The state half is the session boundary — the world's followers must not keep tracking (or
/// restarting) its audio from the glue screens after a logout. The seated half covers the edges:
/// after a logout the camera — and with it [`benilla_world::terrain_stream::CurrentArea`] — still sits at
/// the old spot until the next login's take-control, and following it would start the *previous*
/// session's soundscape for those frames.
fn world_audio_live(
    state: Res<State<crate::char_select::ClientState>>,
    player: Res<Player>,
) -> bool {
    *state.get() == crate::char_select::ClientState::InWorld && player.active
}

pub(crate) struct SoundPlugin;

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        // Who gets sound: a run a human launched, and only that. The default posture is audible
        // (decision 1026 — `SoundConfig::muted` starts false), so the silence has to be opt-in by
        // the *automated* callers, both of which are unattended by construction:
        //   $WOW_NOSOUND — an agent opening the client to check something (the dispatch recipe in
        //                  `method.md` sets it; nobody is listening, and a background window that
        //                  starts playing zone music is pure noise in a shared room).
        //   $WOW_CAPTURE — the visual harness. A screenshot has no audio track; opening a device
        //                  per capture is cost and racket for nothing.
        // Both open no device at all rather than muting one, so an unattended run cannot grab the
        // audio hardware or hold it against the session the director is actually listening to.
        let silent = ["WOW_NOSOUND", "WOW_CAPTURE"]
            .into_iter()
            .find(|v| std::env::var_os(v).is_some());
        // Resolved before the mixer: the probe's taps are main-track effects and a kira main
        // track is build-time-only, so "are we recording?" has to be answered before the device
        // opens, not when the director presses the key.
        let probe_dir = if silent.is_some() {
            None
        } else {
            probe::output_dir()
        };
        let mixer = if let Some(var) = silent {
            info!("${var} set — audio disabled");
            None
        } else {
            match Mixer::new(probe_dir.as_deref()) {
                Ok(m) => Some(m),
                Err(e) => {
                    warn!("no audio device — running silent: {e:#}");
                    None
                }
            }
        };
        let probe = probe_dir.zip(mixer.as_ref()).and_then(|(dir, m)| {
            let Some(rate) = m.sample_rate() else {
                warn!("sound probe: device sample rate unknown — not recording");
                return None;
            };
            Some(probe::Probe::start(dir, rate, m.audio_pos()))
        });
        app.insert_non_send_resource(SoundOutput {
            mixer,
            channels: Vec::new(),
            probe,
            zone_streams: 0,
            glue_streams: 0,
            cinematic_streams: 0,
            voices_stolen: 0,
            voices_denied: 0,
            copies_dropped: 0,
        })
        .init_resource::<SoundConfig>()
        .init_resource::<AudioListener>()
        .add_systems(Startup, hal_overload::setup)
        // `Material.dbc` — shared by the foley and the melee impact, so it loads here rather
        // than inside either consumer.
        .add_systems(
            Startup,
            load_materials.after(benilla_assets::AssetSet::Open),
        )
        // The cover's audio hold (see [`SoundConfig::world_hold`]): fed in PreUpdate so every
        // trigger system this frame — whatever stage it runs in — reads one answer.
        .add_systems(PreUpdate, feed_world_hold)
        .add_systems(
            Update,
            feed_music_suppression.in_set(benilla_world::schedule::WorldStage::Stream),
        )
        .add_systems(
            Update,
            (
                // Compute the listener pose in Stream — after Input's `player::control` writes the
                // character pose + camera, before Present's sound consumers read the resource.
                update_audio_listener.in_set(WorldStage::Stream),
                toggle_mute,
                apply_master_volume.after(toggle_mute),
                poll_mix_health,
                hal_overload::poll,
            ),
        );
        probe::plugin(app);
        kit::plugin(app);
        liquid_loop::plugin(app);
        zone::plugin(app);
        cinematic::plugin(app);
        gameobject::plugin(app);
        anim_events::plugin(app);
        spell::plugin(app);
        missile::plugin(app);
        creature::plugin(app);
        combat::plugin(app);
        footsteps::plugin(app);
        mount::plugin(app);
        water::plugin(app);
        weather::plugin(app);
        emote::plugin(app);
        greeting::plugin(app);
        ui::plugin(app);
        glue::plugin(app);
        money::plugin(app);
        reverb::plugin(app);
        interior::plugin(app);
        sheathe::plugin(app);
    }
}

/// Compute this frame's [`AudioListener`] and feed it to the backend. The listener sits at the
/// **character** (`SoundListenerAtCharacter=1`, wow-re benilla-pins B14): position at the avatar's
/// head, orientation at the character's *facing* about world-up — so 3D volume and pan never change
/// with zoom or camera orbit. The camera eye + basis are the fallback (the client's `=0` path): before
/// login, in free-fly (`detached`), or before the body model attaches (no `CameraPivot` yet).
fn update_audio_listener(
    mut listener: ResMut<AudioListener>,
    mut out: NonSendMut<SoundOutput>,
    player: Res<Player>,
    cinematic: Option<Res<crate::cinematic::Cinematic>>,
    self_av: Query<(&Transform, Option<&CameraPivot>), With<Embodied>>,
    cam: Query<&Transform, (With<WorldCamera>, Without<Embodied>)>,
) {
    // **A cinematic takes the listener to the camera, and it OVERRIDES the CVar** — wow-re
    // `sound/scratch/cinematic-audio-law.md` (VERIFIED; it also promoted `benilla-pins.md` B14's
    // `camera+0x50` label from INFERRED to VERIFIED). `0x483112 jne 0x4831f0` takes the camera
    // branch whenever `camera+0x50 != 0`, ahead of and regardless of `SoundListenerAtCharacter`;
    // the flag is armed from the cinematic's own start path (`0x48ee55` → `0x50c870` → `0x50c9f2`
    // → `0x50c740`) and cleared only by `0x50ca50` at the stop.
    //
    // The narration itself is 2D, so what this actually moves is every *other* 3D sound during a
    // fly-by that can travel 1741 yards from the body.
    let flying = cinematic
        .as_deref()
        .is_some_and(crate::cinematic::Cinematic::is_playing);
    // At-character (the default). `player.pos` is the feet; the head offset is the shared
    // model-derived pivot height, and the facing is the aim yaw about world-up (world +Y).
    if player.active && !player.detached && !flying {
        if let Ok((t, pivot)) = self_av.single() {
            listener.pos = player.pos + Vec3::Y * head_height(pivot, t.scale.x);
            listener.rot = Quat::from_rotation_y(player.facing());
            if let Some(mixer) = out.mixer.as_mut() {
                mixer.set_listener(listener.pos, listener.rot);
            }
            return;
        }
    }
    // At-camera (the `SoundListenerAtCharacter=0` path, and the cinematic override above).
    if let Ok(t) = cam.single() {
        listener.pos = t.translation;
        listener.rot = t.rotation;
        if let Some(mixer) = out.mixer.as_mut() {
            mixer.set_listener(listener.pos, listener.rot);
        }
    }
}

/// The dev chord + `M` — flip [`SoundConfig::muted`]. Lives on the dev-chord plane
/// ([`benilla_world::modkeys::dev_chord`], decision 0585) so it can never collide with a game binding
/// and stays reachable with the chat bar open.
fn toggle_mute(keys: Res<ButtonInput<KeyCode>>, mut config: ResMut<SoundConfig>) {
    if crate::run_mode::dev_chord(&keys, KeyCode::KeyM) {
        config.muted = !config.muted;
        info!("sound {}", if config.muted { "muted" } else { "unmuted" });
    }
}

/// Apply the master enable/volume to the backend when the config actually changes (`Local`
/// snapshot — the panel dirties the resource every open frame, so `is_changed` would spam).
fn apply_master_volume(
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    mut last: Local<Option<(bool, f32, bool)>>,
) {
    let cur = (
        config.enabled && !config.muted,
        config.master,
        config.limiter,
    );
    if *last == Some(cur) {
        return;
    }
    *last = Some(cur);
    if let Some(mixer) = out.mixer.as_mut() {
        mixer.set_master(if cur.0 { cur.1 } else { 0.0 });
        mixer.set_limiter(cur.2);
    }
}

/// How long a window of missed deadlines is summarised into one line. Long enough that a sustained
/// problem reports steadily instead of flooding, short enough to still correlate with what the
/// director was doing when they heard it.
const MIX_HEALTH_REPORT: std::time::Duration = std::time::Duration::from_secs(5);

/// Drain the backend's mix-health queues and report deadline misses (decision 1026).
///
/// This is the instrument the crackle investigation had to be run without: kira measures every
/// callback's `elapsed / allotted` and we were throwing it away, so an underrun — the one failure
/// that is *definitionally* audible — left no trace anywhere but the director's ear. The queues
/// are bounded ring buffers, so polling every frame is also what keeps them meaningful.
///
/// A miss is not a maybe: `load >= 1.0` means the mix did not finish before the driver needed it,
/// and the driver played whatever was in the buffer. If this line is quiet during a crackle, the
/// crackle is *not* an underrun and the next suspect is upstream (a stepped parameter, a starved
/// stream decoder) — which is exactly the disambiguation we could not make before. The starved
/// stream decoder has its own meter now — [`mixer::StreamWatch`], fed by the music-stream
/// holders (decision 1109; it registers on *neither* counter here) — and when every meter is
/// quiet while the ear still hears something, `$WOW_MIX_TAP` records the waveform itself
/// (decision 1112: a crackle can live purely in the mix's *content*).
fn poll_mix_health(
    mut out: NonSendMut<SoundOutput>,
    time: Res<Time>,
    mut exit: MessageReader<bevy::app::AppExit>,
    mut since_report: Local<std::time::Duration>,
    mut last_overruns: Local<u64>,
    mut last_refused: Local<u64>,
    mut peak_voices: Local<usize>,
) {
    // While a probing run records, it owns the meters: [`meter::MixLevel::take`] is
    // reset-on-read, so two consumers would each see a fraction of the truth and both would
    // under-report. The probe says everything this says, twenty times a second and to a file
    // (decision 1556).
    if out.probe.is_some() {
        return;
    }
    *peak_voices = (*peak_voices).max(out.channels.len());
    let Some(mixer) = out.mixer.as_mut() else {
        return;
    };
    let health = mixer.poll_health();
    *since_report += time.delta();
    // App exit forces the report out NOW: a session that quits shortly after the interesting
    // moment (log in, hear the crackle, close the window) used to silently lose every miss
    // since the last 5 s boundary — the 1112 hunt found the director's entire post-reveal
    // window unmetered exactly this way.
    let exiting = exit.read().next().is_some();
    if *since_report < MIX_HEALTH_REPORT && !exiting {
        return;
    }
    *since_report = std::time::Duration::ZERO;
    let peak = mixer.take_health_peak();
    let level = mixer.take_level();
    let rate = mixer.sample_rate();
    let voices = std::mem::take(&mut *peak_voices);
    let new_overruns = health.overruns - *last_overruns;
    *last_overruns = health.overruns;
    if new_overruns > 0 {
        warn!(
            "audio: {new_overruns} missed mix deadline(s) in the last {}s (peak load {:.0}% of \
             budget) — this is what a crackle sounds like",
            MIX_HEALTH_REPORT.as_secs(),
            peak * 100.0,
        );
    } else {
        debug!("audio: mix load peak {:.0}% of budget", peak * 100.0);
    }
    let new_refused = health.voices_refused - *last_refused;
    *last_refused = health.voices_refused;
    if new_refused > 0 {
        warn!(
            "audio: {new_refused} 3D sound(s) never played — the spatial-voice arena was full. \
             These are sounds the player should have heard; the ceiling is ours to raise \
             (`SPATIAL_VOICE_CAPACITY`), not the game's to work around.",
        );
    }
    report_level(level, voices, rate);
}

/// The level half of the report (decision 1551) — the amplitude story none of the timing meters
/// can tell. A mix that asks for more than full scale is not a maybe either: the sum did not fit,
/// and without the limiter kira's `clamp` would have squared it off. The line names what the game
/// asked for, how long it was over, what the limiter had to pull, and how many voices were live —
/// which together say *why* (thirty voices at once is a different bug from one voice at 4×).
fn report_level(level: meter::LevelReading, voices: usize, rate: Option<u32>) {
    // Ahead of the level story on purpose: a non-finite sample is not a loud mix, it is a broken
    // one, and it is invisible to every other counter we have — including the limiter's own
    // `peak > CEILING` test, which a NaN passes straight through into the driver (see [`meter`]).
    if level.nonfinite > 0 {
        error!(
            "audio: {} non-finite (NaN/inf) sample(s) reached the mix. This is a defect upstream \
             of the output — the limiter cannot catch it, and it is broadband noise at whatever \
             the hardware makes of the bits.",
            level.nonfinite,
        );
    }
    if level.over == 0 {
        debug!(
            "audio: mix peak {:.2} of full scale, {voices} voice(s) at most",
            level.peak,
        );
        return;
    }
    // Two samples per frame; an unprobeable device leaves the duration out rather than guessing
    // a time axis (the same rule the mix tap follows).
    let over = match rate {
        Some(r) => format!(
            "for ~{:.0} ms",
            level.over as f64 / 2.0 / f64::from(r) * 1000.0
        ),
        None => format!("across {} samples", level.over),
    };
    warn!(
        "audio: the mix asked for {:.2}x full scale ({:+.1} dBFS) {over} of the last {}s, with \
         {voices} voice(s) live at most; the limiter pulled up to {:.1} dB to hold it under. \
         Without it that is hard clipping — the \"dirty, like a speaker breaking\" report.",
        level.peak,
        20.0 * level.peak.max(1e-6).log10(),
        MIX_HEALTH_REPORT.as_secs(),
        20.0 * level.reduction.max(1e-6).log10(),
    );
}
