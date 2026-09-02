//! The non-macOS face of [`super`]: the same surface as `coreaudio.rs`, opening nothing.
//!
//! benilla's device layer is CoreAudio (decision 1857). On any other target the backend
//! reports "no output device" and the client runs silent — the same posture as a headless
//! machine — rather than carrying a second, untestable device layer. Porting means writing
//! this file's surface over the platform's audio API; nothing above it changes.

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Result};

pub(super) const CHANNELS: u32 = 2;

pub(super) fn now_ns() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

#[derive(Clone, Debug)]
pub(super) struct Device {
    pub id: u32,
    pub name: String,
    pub sample_rate: u32,
    pub buffer_range: (u32, u32),
    pub latency_frames: u32,
    pub safety_frames: u32,
}

pub(super) fn default_output() -> Result<Device> {
    bail!("audio output is CoreAudio-only in this build (decision 1857)")
}

pub(super) struct Cycle<'a> {
    pub buffer: &'a mut [f32],
    pub frames: usize,
    pub output_time_ns: u64,
}

pub(super) struct Stream {
    pub buffer_frames: u32,
}

impl Stream {
    pub(super) fn open<F>(device: Device, _buffer_frames: u32, _on_cycle: F) -> Result<Self>
    where
        F: FnMut(Cycle<'_>) + Send + 'static,
    {
        bail!("no audio device layer for this platform ({})", device.name)
    }
}

#[derive(Default)]
pub(super) struct Notices {
    pub default_changed: AtomicBool,
    pub device_died: AtomicBool,
    pub rate_changed: AtomicBool,
    pub overloads: AtomicU64,
    pub last_overload_ns: AtomicU64,
}

pub(super) struct Listeners;

impl Listeners {
    pub(super) fn arm(_device: u32, _notices: Arc<Notices>) -> Self {
        Self
    }
}

pub(super) struct Workgroup;

impl Workgroup {
    pub(super) fn of_device(_device: u32) -> Option<Self> {
        None
    }
}

pub(super) struct Joined;

impl Joined {
    pub(super) fn join(_group: Workgroup) -> Result<Self> {
        bail!("no audio workgroups on this platform")
    }
}

pub(super) fn set_realtime(_period_ns: u64) -> Result<()> {
    bail!("no realtime thread policy on this platform")
}
