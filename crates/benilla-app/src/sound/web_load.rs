//! wasm32: the zone soundscape's loads, taken off the frame — and since then every other audio
//! load the browser build makes: the one-shot SFX cache (`kit::deferred`), the glue theme
//! ([`super::glue`]) and the cinematic narration ([`super::cinematic`]) all come through
//! [`begin`].
//!
//! ## The freeze this closes
//!
//! Crossing a zone line or a doorway changes the player's area, and [`super::zone`]'s
//! area/interior edge swaps the music bed and the ambience bed on it. On native both halves of
//! that swap are cheap: the chain read is a disk read out of an MPQ, and kira decodes the music
//! on its own decode thread. On wasm both halves were on the **main thread**, because the two
//! primitives the edge reaches for degrade there and nothing said so at the call site:
//!
//! - `Chain::read` is a synchronous `XMLHttpRequest` (`benilla-formats`' `web.rs` — it is called
//!   from ~60 non-async sites, and sync XHR is the one browser primitive that can return bytes
//!   from a function that must return before it does).
//! - `mixer::stream_from_bytes` does not stream on wasm at all: kira drops its streaming module
//!   there (no decode threads), so its wasm arm decodes the **whole file** up front.
//!
//! Measured on the Lion's Pride Inn doorway, over loopback — where the fetch is nearly free and
//! the host answers the 1.1 MB file in 1 ms:
//!
//! ```text
//! blocking audio music fetch:     60 ms — Sound\Music\ZoneMusic\TavernAlliance\TavernAlliance02.mp3
//! blocking audio music decode:    91 ms — …
//! blocking audio ambience fetch:  46 ms — Sound\Ambience\WMOAmbience\Tavern.wav
//! blocking audio ambience decode:  9 ms — …
//! long frame: 220 ms (main thread blocked this long)
//! ```
//!
//! 206 of a 220 ms frame, every doorway and every zone line. Over a real network the two `fetch`
//! halves stop being the cheap ones and carry the whole download.
//!
//! ## The fix: hand both halves to the browser
//!
//! The browser has threads where we do not, and it will use them for exactly these two jobs:
//!
//! - **The fetch** is `fetch`, not sync XHR. [`super::zone`] asks the chain only for the URL
//!   ([`benilla_formats::Chain::url_for_name`]) and drops the lock, so nothing is held across an
//!   await and no request runs on the frame.
//! - **The decode** is `decodeAudioData`, which decodes off the main thread and hands back an
//!   `AudioBuffer`. Copying its planar channels into kira's interleaved [`Frame`]s is a memcpy
//!   where a full MP3 decode used to be.
//!
//! It runs on an [`web_sys::OfflineAudioContext`] rather than the playing `AudioContext`: this
//! context only ever decodes, it is never started, and an offline one is outside the autoplay
//! policy entirely — so nothing here can perturb the output device the mixer owns.
//!
//! **The fallback is load-bearing.** WoW ships IMA/MS-ADPCM WAVs; `decodeAudioData` rejects them
//! where kira's own decoder (its `adpcm` feature) accepts them. A refusal falls back to decoding
//! the fetched bytes in wasm — which does cost a frame, but only for the formats that need it and
//! never for music, which is the expensive one.
//!
//! ## What defers, and why that is safe
//!
//! A load now completes some frames after the edge that asked for it. Both slots absorb that by
//! construction: the ambience bed comes in under its own 5.0 s crossfade (its incoming leg is a
//! per-frame envelope that simply starts later), and the music slot's only fade is the *outgoing*
//! one, which [`super::zone`] still starts at the edge exactly as before. The one disclosed
//! divergence is the intro fanfare's `MinDelayMinutes` stamp: it is taken when the load *starts*
//! rather than when the track plays, so a load that fails consumes the cooldown.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use kira::Frame;
use wasm_bindgen::JsCast;

use super::mixer::StaticSoundData;

/// One soundscape load in flight.
///
/// The fields the completion needs are carried here rather than re-derived: by the time the
/// browser answers, the kit tables may have picked a different variant and the area may have
/// changed again, and a completion must fill the slot with what was *asked for*.
pub(super) struct Pending {
    /// The `SoundEntries` kit this load is for.
    pub(super) kit_id: u32,
    /// The kit's base volume, multiplied under the category slider.
    pub(super) kit_vol: f32,
    /// The chain name — the log line and the error wording, and nothing else.
    pub(super) path: String,
    /// Ambience only: the crossfade this bed was asked to arrive under.
    pub(super) fade_ms: u64,
    /// `Arc<Mutex>` rather than `Rc<RefCell>` so a `Pending` is `Send`: the SFX cache that
    /// holds these is a Bevy `Resource` and the narration's is a `Local`, both of which demand
    /// it. wasm has one thread, so the lock is never contended — it is a type-level formality.
    slot: Arc<Mutex<Option<Result<StaticSoundData, String>>>>,
}

impl Pending {
    /// The result, once the browser has it. `None` = still in flight.
    ///
    /// A `Pending` that is dropped while still running (the player crossed a second border before
    /// the first load landed) leaves its task writing into an `Arc` nobody reads — the last edge
    /// wins, which is the same answer the synchronous path gave.
    pub(super) fn take(&mut self) -> Option<Result<StaticSoundData, String>> {
        self.slot.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
}

/// [`begin`] for a one-shot SFX file (`kit::deferred`): no kit volume or fade rides along —
/// the play that asked carries its own — and a one-shot never loops whole-file.
pub(super) fn begin_sfx(url: String, path: String) -> Pending {
    begin(url, path, 0, 1.0, 0, false)
}

/// Begin a load. Returns immediately — nothing after this point runs on the frame until
/// [`Pending::take`] answers.
///
/// `looping` marks the whole-file loop a bed needs (`mixer::loop_from_bytes`'s job on the
/// synchronous path): music never loops, an ambience bed always does.
pub(super) fn begin(
    url: String,
    path: String,
    kit_id: u32,
    kit_vol: f32,
    fade_ms: u64,
    looping: bool,
) -> Pending {
    let slot: Arc<Mutex<Option<Result<StaticSoundData, String>>>> = Arc::new(Mutex::new(None));
    let write = slot.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let out = fetch_and_decode(&url, looping).await;
        *write.lock().unwrap_or_else(|p| p.into_inner()) = Some(out);
    });
    Pending {
        kit_id,
        kit_vol,
        path,
        fade_ms,
        slot,
    }
}

/// `fetch` the bytes, then decode them — in the browser where possible, in wasm where not.
async fn fetch_and_decode(url: &str, looping: bool) -> Result<StaticSoundData, String> {
    let window = web_sys::window().ok_or("no window: fetch is browser-only")?;
    let resp: web_sys::Response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(js_err)?
        .dyn_into()
        .map_err(|_| "fetch() did not resolve to a Response".to_string())?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let buf: js_sys::ArrayBuffer =
        wasm_bindgen_futures::JsFuture::from(resp.array_buffer().map_err(js_err)?)
            .await
            .map_err(js_err)?
            .dyn_into()
            .map_err(|_| "body did not resolve to an ArrayBuffer".to_string())?;

    // The fallback copy MUST be taken here: `decodeAudioData` DETACHES the buffer it is given, so
    // a copy taken afterwards would be empty. It is one memcpy of the compressed file (~1 MB), not
    // of the decoded PCM, and it is what makes the ADPCM fallback below possible at all.
    let bytes = js_sys::Uint8Array::new(&buf).to_vec();

    let data = match decode_in_browser(&buf).await {
        Ok(data) => data,
        Err(e) => {
            debug!("web_load: decodeAudioData refused ({e}) — decoding in wasm instead");
            super::mixer::sfx_from_bytes(bytes).map_err(|e| format!("{e:#}"))?
        }
    };
    Ok(if looping { data.loop_region(..) } else { data })
}

/// `decodeAudioData` on the decode-only offline context, into kira's PCM.
async fn decode_in_browser(buf: &js_sys::ArrayBuffer) -> Result<StaticSoundData, String> {
    let ctx = decode_context()?;
    let promise = ctx.decode_audio_data(buf).map_err(js_err)?;
    let audio: web_sys::AudioBuffer = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(js_err)?
        .dyn_into()
        .map_err(|_| "decodeAudioData did not resolve to an AudioBuffer".to_string())?;
    // The one part of this that IS on the main thread: copying the browser's planar channels
    // into kira's interleaved frames — a memcpy, measured under the 8 ms floor at every size in
    // the corpus, where the decode it replaces was 91 ms.
    frames_from(&audio)
}

/// The one context this module decodes on, built on first use and kept.
///
/// Offline, and 1 frame long: its only job is to own `decodeAudioData`. The length and rate are
/// the constructor's required arguments, not a choice about the audio — `decodeAudioData` sizes
/// its own output from the file, and we read the rate back off the buffer it returns.
fn decode_context() -> Result<web_sys::OfflineAudioContext, String> {
    thread_local! {
        static CTX: RefCell<Option<web_sys::OfflineAudioContext>> = const { RefCell::new(None) };
    }
    CTX.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            *cell = Some(
                web_sys::OfflineAudioContext::new_with_number_of_channels_and_length_and_sample_rate(
                    2, 1, 44_100.0,
                )
                .map_err(js_err)?,
            );
        }
        Ok(cell.as_ref().expect("just filled").clone())
    })
}

/// An `AudioBuffer`'s planar channels → kira's interleaved [`Frame`]s.
///
/// Mono is duplicated to both sides rather than left half-silent (`Frame::from_mono`, the same
/// answer kira's own decoder gives a mono file); anything above two channels takes the first two,
/// which is every file in the corpus and all the mixer can place anyway.
fn frames_from(audio: &web_sys::AudioBuffer) -> Result<StaticSoundData, String> {
    let left = audio.get_channel_data(0).map_err(js_err)?;
    let frames: Arc<[Frame]> = if audio.number_of_channels() > 1 {
        let right = audio.get_channel_data(1).map_err(js_err)?;
        left.iter()
            .zip(right.iter())
            .map(|(l, r)| Frame {
                left: *l,
                right: *r,
            })
            .collect()
    } else {
        left.iter().map(|s| Frame::from_mono(*s)).collect()
    };
    Ok(StaticSoundData {
        sample_rate: audio.sample_rate() as u32,
        frames,
        settings: Default::default(),
        slice: None,
    })
}

fn js_err(e: wasm_bindgen::JsValue) -> String {
    format!("{e:?}")
}
