//! The cinematic **narration** — the race intro's voice-over, and the handle that lets an ESC cut
//! it off mid-sentence.
//!
//! Every race fly-by names a `SoundEntries.dbc` id on its `CinematicCamera` row, and all eight are
//! the same thing: a single streamed `Sound\CinematicVoices\<Race>Narration.mp3` (kit type 31,
//! "DwarfFlyByNarration" and its siblings). The two non-intro rows — `PalantirOfAzora` and
//! `Scry_cam` — name id 0 and play nothing.
//!
//! **Why this lives in the sound module rather than in [`crate::cinematic`].** Starting a stream
//! and holding its handle is sound-internal (`pick_stream`, the mixer, the category amp are all
//! `pub(super)` here), and the handle is the whole point: a dropped kira handle keeps playing, so
//! a skipped cinematic whose narration was merely forgotten would keep narrating over the game for
//! another minute. The reference has exactly the same problem and the same answer — it keeps the
//! cinematic's sound in a dedicated global (`[0xb4e274]`) and *releases* it on every stop path
//! (`0x48f050`, and again in the local-abort teardown at `0x490b88`).
//!
//! So this module watches [`Cinematic`]'s published shot and follows it: a new shot starts its
//! narration, and the shot going away stops it. That keeps the coupling one-way — the cinematic
//! plugin never learns what a mixer is.

use bevy::prelude::*;

use benilla_assets::{LockRecover, WorldAssets};
use benilla_world::schedule::WorldStage;

use super::mixer::{self, StreamingSoundHandle};
use super::{kit::SoundCategory, kit::SoundKits, SoundConfig, SoundOutput};
use crate::cinematic::Cinematic;

/// How long a cut-off narration takes to fade. Short: an ESC skip should feel like a cut, not a
/// dissolve, but a hard stop mid-waveform clicks.
const CUT_FADE_MS: u64 = 250;

/// The narration channel: which shot it belongs to, and its live handle.
#[derive(Default)]
pub(super) struct CinematicVoice {
    /// `(sequence id, shot index)` of the shot whose narration is playing — the identity that
    /// decides whether the current shot's audio is already running.
    shot: Option<(u32, usize)>,
    handle: Option<StreamingSoundHandle<kira::sound::FromFileError>>,
}

impl CinematicVoice {
    fn stop(&mut self) {
        self.shot = None;
        if let Some(mut h) = self.handle.take() {
            h.stop(mixer::fade(CUT_FADE_MS));
        }
    }
}

pub(super) fn plugin(app: &mut App) {
    // In `Present`, after the cinematic driver in `Input` has settled this frame's shot: the
    // narration follows the picture rather than racing it.
    app.add_systems(Update, drive_narration.in_set(WorldStage::Present));
}

fn drive_narration(
    cine: Option<Res<Cinematic>>,
    mut voice: Local<CinematicVoice>,
    mut out: NonSendMut<SoundOutput>,
    mut kits: ResMut<SoundKits>,
    assets: Option<Res<WorldAssets>>,
    config: Res<SoundConfig>,
) {
    let playing = cine.as_deref().and_then(Cinematic::playing_shot);
    let Some((sequence, index, sound_id)) = playing else {
        // No cinematic (or it just ended/was skipped) — cut whatever was narrating.
        voice.stop();
        return;
    };
    if voice.shot == Some((sequence, index)) {
        return;
    }
    // A new shot: the previous one's narration, if any, does not carry over.
    voice.stop();
    voice.shot = Some((sequence, index));
    if sound_id == 0 {
        return;
    }
    let Some(assets) = assets else { return };
    let Some(mixer_ref) = out.mixer.as_mut() else {
        return;
    };
    let Some((path, kit_vol)) = kits.pick_stream(sound_id) else {
        warn!("cinematic voice: sound {sound_id} has no file");
        return;
    };
    let bytes = {
        let chain = assets.chain.lock_recover();
        chain.read(&path)
    };
    let data = match bytes.and_then(mixer::stream_from_bytes) {
        Ok(d) => d,
        Err(e) => {
            warn!("cinematic voice: {path} — {e:#}");
            return;
        }
    };
    // Speech rides the effects category, the same one every NPC voice line takes — 1.12 ships no
    // separate dialog slider, so this is where a player's expectation already points.
    let amp = config.category_amp(SoundCategory::Sfx) * kit_vol;
    match mixer_ref.play_stream(data.volume(mixer::amp_to_db(amp))) {
        Ok(h) => {
            info!("cinematic voice: {path}");
            voice.handle = Some(h);
        }
        Err(e) => warn!("cinematic voice: {path} — {e:#}"),
    }
}
