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
//! cinematic's sound in a dedicated global (`[0xb4e274]`) and *releases* it on **three** paths,
//! which are all of its references besides the write at `0x48ef3e`: `0x48efef` on the shot
//! advance — and therefore on the ordinary end, since every shipped sequence is a single shot —
//! `0x48f055` on the stop/ESC, and `0x490b8d` in the local-abort teardown. The first was missing
//! from this list; the behaviour was not (a shot change and an ended cinematic both stop the voice
//! below, which is the same three edges).
//!
//! So this module watches [`Cinematic`]'s published shot and follows it: a new shot starts its
//! narration, and the shot going away stops it. That keeps the coupling one-way — the cinematic
//! plugin never learns what a mixer is.

use bevy::prelude::*;

use benilla_assets::{LockRecover, WorldAssets};
use benilla_world::schedule::WorldStage;

use super::mixer::{self, StreamingSoundHandle};
use super::{kit::SoundKits, SoundConfig, SoundOutput};
use crate::cinematic::Cinematic;

/// A cut-off narration is **cut**, with no fade at all — wow-re
/// `sound/scratch/cinematic-audio-law.md` (VERIFIED): every release on this path
/// (`0x48efef` shot-advance, `0x48f055` stop/ESC, `0x490b8d` local abort) is a plain release of
/// `[0xb4e274]`, and there is **no audio fade anywhere in the cinematic** — the 0.25 s fade the
/// reference schedules at both edges (`0x4c0d10`, `[0x804550]`) is a *screen* fade, which is what
/// decision 1695 deferred. benilla's 250 ms declick was a guess at a fade the reference does not
/// have.
const CUT_FADE_MS: u64 = 0;

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
    // **No category slider applies to this channel at all** (wow-re `cinematic-audio-law.md`,
    // VERIFIED). The narration is opened at `0x48ef29` → `0x458a40` → `0x45ce60` with behaviour
    // flags `0x15`: `or eax,4` (`0x45ce9e`) and `or eax,0x10` (`0x45cea8`), while the music bit
    // `or eax,2` at `0x45ceb2` is **skipped**. Bit `0x10` is read at `0x7a5dc0`
    // (`test al,0x10; jne 0x7a5e1a`) and takes the channel around the category multiply entirely —
    // so Sound, Music and Ambience sliders all leave it alone, and its gain is the kit volume flat
    // (`__ftol(0.69 · 1.0 · 255)` = 175/255 on the shipped narrations).
    //
    // It is **not** exempt from the master, though: only bit `0x2` bypasses the
    // `MasterSoundEffects` gate at `0x7a529c`, and this channel does not set it — so unchecking
    // "Enable Sound Effects" silences the narration outright. [`SoundConfig::enabled`] is that
    // checkbox here.
    //
    // The old reading — the Sfx category, "the same one every NPC voice line takes" — was a
    // reasonable guess at where a player's expectation points, and it was wrong: this channel has
    // no category.
    let amp = if config.enabled { kit_vol } else { 0.0 };
    match mixer_ref.play_stream(data.volume(mixer::amp_to_db(amp))) {
        Ok(h) => {
            info!("cinematic voice: {path}");
            voice.handle = Some(h);
        }
        Err(e) => warn!("cinematic voice: {path} — {e:#}"),
    }
}
