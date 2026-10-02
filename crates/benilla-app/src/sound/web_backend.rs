//! wasm32: the mixer's kira backend in the browser — Web Audio, through cpal's `wasm-bindgen`
//! host. Ours rather than kira's own `CpalBackend`: kira 0.12.4's is written against cpal 0.18,
//! whose `alsa-sys` 0.4 cannot share a lockfile with the 0.3 every other cpal in the graph links
//! (the native device layer's, bevy's audio stack's), and a lockfile is resolved for every target
//! at once. This is kira 0.12.1's wasm backend, on the cpal the app already resolves.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use kira::backend::{Backend, Renderer};

/// The stream config to open, as `mixer::backend_settings` negotiated it; `None` is the default
/// device's default config.
pub(super) type WebBackendSettings = Option<StreamConfig>;

enum State {
    Empty,
    Uninitialized {
        device: Device,
        config: StreamConfig,
    },
    Initialized {
        _stream: Stream,
    },
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
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("no default audio output device"))?;
        let config = match settings {
            Some(config) => config,
            None => device.default_output_config()?.config(),
        };
        let sample_rate = config.sample_rate;
        Ok((
            Self {
                state: State::Uninitialized { device, config },
            },
            sample_rate,
        ))
    }

    fn start(&mut self, mut renderer: Renderer) -> Result<(), Self::Error> {
        let State::Uninitialized { device, config } =
            std::mem::replace(&mut self.state, State::Empty)
        else {
            anyhow::bail!("the audio backend was started twice");
        };
        let channels = config.channels;
        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _| {
                renderer.on_start_processing();
                renderer.process(data, channels);
            },
            |e| bevy::log::warn!("audio: stream error: {e}"),
            None,
        )?;
        stream.play()?;
        self.state = State::Initialized { _stream: stream };
        Ok(())
    }
}
