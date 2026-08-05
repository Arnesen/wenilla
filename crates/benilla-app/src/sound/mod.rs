//! sound — WoW's owned audio selection/scheduling over a delegated mixer (decision 0070).
//!
//! Three separable pieces (0070): the **mixer seam** ([`mixer`] — kira behind an FMOD-shaped
//! surface), the **kit player** (WoW's owned selection math off `SoundEntries.dbc` — next slice
//! commit), and the **trigger surface** (zone music/ambience, UI, world emitters — phased in).
//! This module owns the Bevy plumbing: the output resource, the per-frame listener sync from the
//! world camera, and the always-on player config (decision 0026: config is gameplay state, the
//! debug panel only edits it).

use bevy::prelude::*;

use crate::net::SelfPlayer;
use crate::player::{head_height, CameraPivot, Player, WorldCamera};
use crate::schedule::WorldStage;

mod anim_events;
mod combat;
mod creature;
mod emote;
mod footsteps;
mod gameobject;
mod glue;
mod greeting;
pub(crate) mod interior;
mod kit;
mod liquid_loop;
mod math;
mod missile;
mod mixer;
mod money;
mod mount;
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
    /// instant
    /// (unlike `enabled`, which stops sounds from being picked at all). Starts `true`: the client
    /// boots silent until deliberately unmuted.
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
            muted: true,
            master: 1.0,
            sfx: 1.0,
            music: 0.4,
            ambience: 0.6,
            music_enabled: true,
            ambience_enabled: true,
        }
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
/// after a logout the camera — and with it [`crate::terrain_stream::CurrentArea`] — still sits at
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
        let mixer = if std::env::var_os("WOW_NOSOUND").is_some() {
            info!("$WOW_NOSOUND set — audio disabled");
            None
        } else {
            match Mixer::new() {
                Ok(m) => Some(m),
                Err(e) => {
                    warn!("no audio device — running silent: {e:#}");
                    None
                }
            }
        };
        app.insert_non_send_resource(SoundOutput {
            mixer,
            channels: Vec::new(),
        })
        .init_resource::<SoundConfig>()
        .init_resource::<AudioListener>()
        .add_systems(
            Update,
            (
                // Compute the listener pose in Stream — after Input's `player::control` writes the
                // character pose + camera, before Present's sound consumers read the resource.
                update_audio_listener.in_set(WorldStage::Stream),
                toggle_mute,
                apply_master_volume.after(toggle_mute),
            ),
        );
        kit::plugin(app);
        liquid_loop::plugin(app);
        zone::plugin(app);
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
    self_av: Query<(&Transform, Option<&CameraPivot>), With<SelfPlayer>>,
    cam: Query<&Transform, (With<WorldCamera>, Without<SelfPlayer>)>,
) {
    // At-character (the default). `player.pos` is the feet; the head offset is the shared
    // model-derived pivot height, and the facing is the aim yaw about world-up (world +Y).
    if player.active && !player.detached {
        if let Ok((t, pivot)) = self_av.single() {
            listener.pos = player.pos + Vec3::Y * head_height(pivot, t.scale.x);
            listener.rot = Quat::from_rotation_y(player.facing());
            if let Some(mixer) = out.mixer.as_mut() {
                mixer.set_listener(listener.pos, listener.rot);
            }
            return;
        }
    }
    // At-camera fallback (the `SoundListenerAtCharacter=0` path).
    if let Ok(t) = cam.single() {
        listener.pos = t.translation;
        listener.rot = t.rotation;
        if let Some(mixer) = out.mixer.as_mut() {
            mixer.set_listener(listener.pos, listener.rot);
        }
    }
}

/// The dev chord + `M` — flip [`SoundConfig::muted`]. Lives on the dev-chord plane
/// ([`crate::debug_panel::dev_chord`], decision 0585) so it can never collide with a game binding
/// and stays reachable with the chat bar open.
fn toggle_mute(keys: Res<ButtonInput<KeyCode>>, mut config: ResMut<SoundConfig>) {
    if crate::debug_panel::dev_chord(&keys, KeyCode::KeyM) {
        config.muted = !config.muted;
        info!("sound {}", if config.muted { "muted" } else { "unmuted" });
    }
}

/// Apply the master enable/volume to the backend when the config actually changes (`Local`
/// snapshot — the panel dirties the resource every open frame, so `is_changed` would spam).
fn apply_master_volume(
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    mut last: Local<Option<(bool, f32)>>,
) {
    let cur = (config.enabled && !config.muted, config.master);
    if *last == Some(cur) {
        return;
    }
    *last = Some(cur);
    if let Some(mixer) = out.mixer.as_mut() {
        mixer.set_master(if cur.0 { cur.1 } else { 0.0 });
    }
}
