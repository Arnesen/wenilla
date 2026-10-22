//! wasm32: the mixer's kira backend in the browser — Web Audio, scheduled by us.
//!
//! The browser has no audio thread we can run Rust on (no wasm threads), so the mix is rendered on
//! the main thread and handed to Web Audio as a queue of `AudioBufferSourceNode`s, each started at
//! an exact `AudioContext` time. The only thing that decides whether that stutters is how far
//! ahead of the playhead the queue reaches: a main-thread stall longer than the lead (a stream-in
//! burst, a collider build, a long frame) leaves the device with nothing to play.
//!
//! This used to be cpal 0.17's `webaudio` host, which keeps two 2048-frame buffers in flight and
//! refills one when the other ends: about 40 ms of lead (measured 28–45 ms at 44.1 kHz on an idle
//! page), and after an underrun it starts the late buffers "in the past", where they overlap the
//! next ones and garble until its cursor catches up. In-world stalls are routinely longer than
//! that, which is the stutter players heard.
//!
//! [`Scheduler`] keeps the same main-thread mix but:
//! - queues short chunks ([`CHUNK_FRAMES`]) up to a target lead, refilled from every chunk's
//!   `ended` (an audio event, so it keeps running in a background tab where timers are throttled);
//! - adapts the lead: an underrun grows it (×1.5, up to [`MAX_LEAD_S`]); a quiet stretch walks it
//!   back down one chunk at a time to the floor, so latency is only paid while the page is janky;
//! - on an underrun, resyncs the cursor to just ahead of the playhead — one gap, never overlap;
//! - reuses its `AudioBuffer`s (a pool refilled on `ended`) instead of allocating per chunk.
//!
//! `?audiolead=<ms>` (`WOW_AUDIOLEAD`) sets the floor for experiments; `window.__wenilla_audio`
//! reports `{ underruns, lead_ms }` as it changes.
//!
//! Ours rather than kira's own `CpalBackend`: kira 0.12.4's is written against cpal 0.18, whose
//! `alsa-sys` 0.4 cannot share a lockfile with the 0.3 every other cpal in the graph links. cpal
//! stays the device probe in `mixer::backend_settings`; the stream is this file's.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use cpal::StreamConfig;
use kira::backend::{Backend, Renderer};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    AudioBuffer, AudioBufferSourceNode, AudioContext, AudioContextOptions,
    AudioScheduledSourceNode, Event,
};

/// The stream config to open, as `mixer::backend_settings` negotiated it; `None` is the default
/// device's default config. Only its rate and channel count are used: the chunking is ours.
pub(super) type WebBackendSettings = Option<StreamConfig>;

/// Frames per scheduled chunk: ~23 ms at 44.1 kHz. Short enough that the lead is adjustable in
/// fine steps and an underrun costs little; long enough that the per-chunk JS calls (create,
/// connect, start) stay negligible.
const CHUNK_FRAMES: usize = 1024;
/// The lead floor: how far ahead of the playhead the queue reaches on a page that keeps up.
const MIN_LEAD_S: f64 = 0.08;
/// The lead ceiling an underrun-prone page can grow to.
const MAX_LEAD_S: f64 = 0.35;
/// After an underrun, the first chunk starts this far ahead of the playhead.
const RESYNC_MARGIN_S: f64 = 0.01;
/// How long the queue must run without an underrun before the lead steps down one chunk.
const DECAY_AFTER_S: f64 = 8.0;

enum State {
    Empty,
    Uninitialized { config: StreamConfig },
    Initialized { _scheduler: Rc<RefCell<Scheduler>> },
}

/// kira's [`Backend`] over the browser's default output device.
pub(super) struct WebBackend {
    state: State,
}

impl Backend for WebBackend {
    type Settings = WebBackendSettings;
    type Error = anyhow::Error;

    fn setup(
        settings: Self::Settings,
        _internal_buffer_size: usize,
    ) -> Result<(Self, u32), Self::Error> {
        let config = match settings {
            Some(config) => config,
            None => {
                use cpal::traits::{DeviceTrait, HostTrait};
                cpal::default_host()
                    .default_output_device()
                    .ok_or_else(|| anyhow::anyhow!("no default audio output device"))?
                    .default_output_config()?
                    .config()
            }
        };
        let sample_rate = config.sample_rate;
        Ok((
            Self {
                state: State::Uninitialized { config },
            },
            sample_rate,
        ))
    }

    fn start(&mut self, renderer: Renderer) -> Result<(), Self::Error> {
        let State::Uninitialized { config } = std::mem::replace(&mut self.state, State::Empty)
        else {
            anyhow::bail!("the audio backend was started twice");
        };
        let scheduler = Scheduler::start(&config, renderer).map_err(js_err)?;
        self.state = State::Initialized {
            _scheduler: scheduler,
        };
        Ok(())
    }
}

/// The chunk queue: renders the mix and keeps it [`Scheduler::lead`] ahead of the playhead.
struct Scheduler {
    ctx: AudioContext,
    renderer: Renderer,
    channels: usize,
    rate: f32,
    /// Seconds per chunk.
    step: f64,
    /// Context time the next chunk starts at; 0 until the first pump.
    cursor: f64,
    lead: f64,
    min_lead: f64,
    /// Context time of the last underrun or lead step-down.
    settled_since: f64,
    underruns: u32,
    interleaved: Vec<f32>,
    planar: Vec<f32>,
    /// Buffers whose chunk has ended, ready to be refilled.
    free: Vec<AudioBuffer>,
    on_ended: Option<Closure<dyn FnMut(Event)>>,
}

impl Scheduler {
    fn start(config: &StreamConfig, renderer: Renderer) -> Result<Rc<RefCell<Self>>, JsValue> {
        let opts = AudioContextOptions::new();
        opts.set_sample_rate(config.sample_rate as f32);
        let ctx = AudioContext::new_with_context_options(&opts)?;
        let destination = ctx.destination();
        if u32::from(config.channels) <= destination.max_channel_count() {
            destination.set_channel_count(u32::from(config.channels));
        }
        let channels = usize::from(config.channels);
        let min_lead = crate::webenv::var("WOW_AUDIOLEAD")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|ms| ms.is_finite() && *ms > 0.0)
            .map_or(MIN_LEAD_S, |ms| (ms / 1000.0).min(MAX_LEAD_S));
        let scheduler = Rc::new(RefCell::new(Self {
            ctx,
            renderer,
            channels,
            rate: config.sample_rate as f32,
            step: CHUNK_FRAMES as f64 / f64::from(config.sample_rate),
            cursor: 0.0,
            lead: min_lead,
            min_lead,
            settled_since: 0.0,
            underruns: 0,
            interleaved: vec![0.0; CHUNK_FRAMES * channels],
            planar: vec![0.0; CHUNK_FRAMES],
            free: Vec::new(),
            on_ended: None,
        }));
        let weak: Weak<RefCell<Self>> = Rc::downgrade(&scheduler);
        let on_ended = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Some(scheduler) = weak.upgrade() else {
                return;
            };
            // Re-entrancy cannot happen (events are tasks), but a panic elsewhere could leave the
            // cell borrowed; skip the pump rather than abort the page.
            let Ok(mut s) = scheduler.try_borrow_mut() else {
                return;
            };
            if let Some(buffer) = event
                .target()
                .and_then(|t| t.dyn_into::<AudioBufferSourceNode>().ok())
                .and_then(|source| source.buffer())
            {
                s.free.push(buffer);
            }
            s.pump();
        });
        {
            let mut s = scheduler.borrow_mut();
            s.on_ended = Some(on_ended);
            bevy::log::info!(
                "audio: {} Hz, {} ch, {CHUNK_FRAMES}-frame chunks, lead {:.0}–{:.0} ms",
                config.sample_rate,
                channels,
                min_lead * 1000.0,
                MAX_LEAD_S * 1000.0,
            );
            s.pump();
            s.publish();
        }
        Ok(scheduler)
    }

    /// Top the queue up to the target lead, first resyncing (and growing the lead) if the
    /// playhead overtook it.
    fn pump(&mut self) {
        let now = self.ctx.current_time();
        if self.cursor < now + RESYNC_MARGIN_S {
            if self.cursor > 0.0 {
                self.underruns += 1;
                self.lead = (self.lead * 1.5).min(MAX_LEAD_S);
                self.publish();
            }
            self.cursor = now + RESYNC_MARGIN_S;
            self.settled_since = now;
        } else if self.lead > self.min_lead && now - self.settled_since > DECAY_AFTER_S {
            self.lead = (self.lead - self.step).max(self.min_lead);
            self.settled_since = now;
            self.publish();
        }
        while self.cursor < now + self.lead {
            if let Err(e) = self.schedule_chunk() {
                bevy::log::warn!("audio: scheduling a chunk failed: {e:?}");
                return;
            }
        }
    }

    /// Render one chunk of the mix and start it at the cursor.
    fn schedule_chunk(&mut self) -> Result<(), JsValue> {
        self.renderer.on_start_processing();
        self.renderer.process(&mut self.interleaved, self.channels as u16);
        let buffer = match self.free.pop() {
            Some(buffer) => buffer,
            None => self
                .ctx
                .create_buffer(self.channels as u32, CHUNK_FRAMES as u32, self.rate)?,
        };
        for channel in 0..self.channels {
            for (frame, out) in self.planar.iter_mut().enumerate() {
                *out = self.interleaved[frame * self.channels + channel];
            }
            buffer.copy_to_channel(&self.planar, channel as i32)?;
        }
        let source = self.ctx.create_buffer_source()?;
        source.set_buffer(Some(&buffer));
        source.connect_with_audio_node(&self.ctx.destination())?;
        if let Some(on_ended) = &self.on_ended {
            AudioScheduledSourceNode::set_onended(&source, Some(on_ended.as_ref().unchecked_ref()));
        }
        source.start_with_when(self.cursor)?;
        self.cursor += self.step;
        Ok(())
    }

    /// `window.__wenilla_audio = { underruns, lead_ms }`: only written when either changes.
    fn publish(&self) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let stats = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&stats, &"underruns".into(), &self.underruns.into());
        let _ = js_sys::Reflect::set(
            &stats,
            &"lead_ms".into(),
            &(self.lead * 1000.0).round().into(),
        );
        let _ = js_sys::Reflect::set(&window, &"__wenilla_audio".into(), &stats);
    }
}

fn js_err(e: JsValue) -> anyhow::Error {
    anyhow::anyhow!("{e:?}")
}
