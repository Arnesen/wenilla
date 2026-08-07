//! Glue-screen audio (decision 0423's polish pass) — the two sounds of the pre-world screens:
//!
//! - **Clicks**: the char select/create screens emit [`GlueSound`] messages naming the SoundEntries
//!   kits the 1.12 GlueXML plays verbatim (`gsCharacterCreationClass`, `gsCharacterSelectionEnterWorld`,
//!   …); [`play_glue_sounds`] drains them into the kit player as 2D SFX — the same shape as
//!   [`super::ui`]'s `PlaySound` seam, but from the glue screens (which have no Lua VM).
//! - **Music**: the glue title theme (`GlueParent.lua`'s `CurrentGlueMusic`,
//!   `Sound\Music\GlueScreenMusic\wow_main_theme.mp3`), streamed from the login screen on
//!   (decision 0539) and kept across the glue screens (the ref keeps it through select ⇄ create); entering the world
//!   fades it out on the backend (drop the handle — the ramp finishes on kira's thread, the
//!   [`super::zone`] music-stop shape). Re-entering the glue after a logout starts it again.

use bevy::prelude::*;

use crate::assets::{LockRecover, WorldAssets};
use crate::char_select::ClientState;

use super::kit::{self, KitRef, SoundCategory, SoundKits};
use super::{mixer, SoundConfig, SoundOutput};

/// The glue music file — a frozen fact of the 1.12 GlueXML (`GlueParent.lua`, `CurrentGlueMusic`).
const GLUE_MUSIC: &str = "Sound\\Music\\GlueScreenMusic\\wow_main_theme.mp3";
/// The out-fade on entering the world.
const GLUE_MUSIC_FADE_OUT_MS: u64 = 2000;

/// A glue-screen `PlaySound` — the SoundEntries kit name the 1.12 GlueXML plays for this click.
#[derive(Message)]
pub(crate) struct GlueSound(pub(crate) &'static str);

/// The held glue-music stream (non-`Sync` handle — non-Send state, like [`super::zone`]'s), plus
/// its starvation watch (decision 1109). The handle is held through the stop-fade, not dropped at
/// it: the fade's 2 s ride through the world-entry load burst is exactly the crackle-prone window,
/// and [`watch_glue_music`] can only see a stream it still holds.
struct GlueMusic {
    handle: Option<mixer::StreamingSoundHandle<kira::sound::FromFileError>>,
    watch: mixer::StreamWatch,
}

impl Default for GlueMusic {
    fn default() -> Self {
        Self {
            handle: None,
            watch: mixer::StreamWatch::new("glue music"),
        }
    }
}

/// Drain glue clicks into the kit player (2D SFX, by name — the client's name-registry path).
fn play_glue_sounds(
    mut msgs: MessageReader<GlueSound>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
) {
    let (Some(mut kits), Some(assets)) = (kits, assets) else {
        msgs.clear();
        return;
    };
    for msg in msgs.read() {
        if let Err(e) = kit::play_kit(
            &mut kits,
            &assets,
            &mut out,
            &config,
            Vec3::ZERO,
            KitRef::Name(msg.0),
            None,
            SoundCategory::Sfx,
        ) {
            debug!("sound(glue): {} — {e:#}", msg.0);
        }
    }
}

/// While on the select screen, start the title theme if it isn't up. Runs per-frame (cheap early
/// return once playing) rather than on the state edge: the app *boots* into CharSelect, and the
/// initial `OnEnter` fires before the mixer/chain exist — an edge-triggered start misses it.
fn start_glue_music(
    mut music: NonSendMut<GlueMusic>,
    mut out: NonSendMut<SoundOutput>,
    assets: Option<Res<WorldAssets>>,
    config: Res<SoundConfig>,
) {
    if music.handle.is_some() {
        return; // still playing (select ⇄ create hops re-enter CharSelect)
    }
    let (Some(assets), Some(mixer_ref)) = (assets, out.mixer.as_mut()) else {
        return;
    };
    let bytes = {
        let chain = assets.chain.lock_recover();
        chain.read(GLUE_MUSIC)
    };
    let data = match bytes.and_then(mixer::stream_from_bytes) {
        Ok(d) => d,
        Err(e) => {
            debug!("glue music: {GLUE_MUSIC} — {e:#}");
            return;
        }
    };
    let amp = config.category_amp(SoundCategory::Music);
    match mixer_ref.play_stream(data.volume(mixer::amp_to_db(amp))) {
        Ok(h) => {
            info!("glue music: {GLUE_MUSIC}");
            music.handle = Some(h);
        }
        Err(e) => debug!("glue music: {e:#}"),
    }
}

/// Entering the world: fade the theme out on the backend. The handle is deliberately kept — the
/// [`GlueMusic`] docs say why — and reaped by [`watch_glue_music`] once the fade lands on
/// `Stopped`.
fn stop_glue_music(mut music: NonSendMut<GlueMusic>) {
    if let Some(h) = music.handle.as_mut() {
        h.stop(mixer::fade(GLUE_MUSIC_FADE_OUT_MS));
    }
}

/// Per-frame stream health + handle reaping: the starvation watch (decision 1109) over the held
/// theme, and the drop once it reaches `Stopped` — after the entry fade lands, or at the theme's
/// natural end. On the glue screens that end re-arms [`start_glue_music`], which brings the theme
/// back from the top — the login screen no longer falls silent for good after one play-through
/// (the reference's theme keeps going; it rides the same music machinery as zone tracks,
/// `PlayGlueMusic` → `0x45aeb0`, which re-selects when a track ends).
fn watch_glue_music(mut music: NonSendMut<GlueMusic>, time: Res<Time>) {
    let music = &mut *music;
    let Some(h) = music.handle.as_ref() else {
        return;
    };
    if h.state() == kira::sound::PlaybackState::Stopped {
        music.handle = None;
        music.watch.reset();
        return;
    }
    music.watch.feed(h, f64::from(time.delta_secs()));
}

pub(super) fn plugin(app: &mut App) {
    app.add_message::<GlueSound>()
        .insert_non_send_resource(GlueMusic::default())
        .add_systems(OnEnter(ClientState::InWorld), stop_glue_music)
        .add_systems(
            Update,
            (
                play_glue_sounds,
                // The theme starts at the login screen (the ref's `AccountLogin_OnShow` sets the
                // same `wow_main_theme` — decision 0539) and keeps across the glue screens.
                start_glue_music
                    .run_if(in_state(ClientState::Login).or(in_state(ClientState::CharSelect))),
                // No run condition: the stop-fade rides into `InWorld`, and that leg is exactly
                // the window the watch exists for (1109).
                watch_glue_music.after(start_glue_music),
            ),
        );
}
