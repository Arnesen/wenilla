//! The HAL overload meter — the crackle detector that lives *downstream* of our mix
//! (decision 1114).
//!
//! The 1112 corner: the director's crackle is in none of our audio — the tap capture is clean
//! by scan and by their own ear, the callback met every deadline, the decoder never starved.
//! What remains is the one hop no in-process meter watched: CoreAudio's **IO cycle** — the HAL
//! collecting every client's buffer and handing the result to the hardware DMA on a hard
//! deadline. When that cycle runs late (a late thread wakeup, a memory-bandwidth storm from a
//! GPU upload burst — a first login's world entry is exactly that), the device plays stale or
//! zero samples and the speaker crackles, while the producing app measures itself healthy: the
//! mix was computed fast and correct, just consumed late. kira's load meter cannot see it
//! (it times `process()`, not the cycle), and macOS does not persist it to the unified log.
//!
//! CoreAudio *does* announce it to clients: the `kAudioDeviceProcessorOverload` property fires
//! a listener on every overloaded cycle. This module registers that listener on the default
//! output device and a Bevy system reports new overloads once per window — so the next live
//! crackle either stamps a `HAL overload` WARN at the moment the ear hears it (confirming the
//! downstream mechanism, and pointing at the device-buffer dial, 1026's residue), or stays
//! silent and rules this hop out too.
//!
//! macOS only by construction; every other platform compiles to a no-op. The listener sticks
//! to the device that was default at registration — a mid-session output swap loses the meter
//! (accepted: kira's cpal backend deliberately never re-queries devices either, 1026).

use bevy::prelude::*;

#[cfg(target_os = "macos")]
mod sys {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Overloaded-cycle count since launch, bumped by the listener (CoreAudio calls it on an
    /// internal thread; the poller reads it on the main thread).
    pub(super) static OVERLOADS: AtomicU64 = AtomicU64::new(0);

    // The tiny slice of the CoreAudio C API this needs — declared here like `thread_qos` does
    // for pthread QoS, so no version-pinned -sys crate enters the graph for four constants.
    type OSStatus = i32;
    type AudioObjectID = u32;

    #[repr(C)]
    struct AudioObjectPropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    /// `kAudioObjectSystemObject`
    const SYSTEM_OBJECT: AudioObjectID = 1;
    /// `kAudioHardwarePropertyDefaultOutputDevice` — 'dOut'
    const DEFAULT_OUTPUT: u32 = u32::from_be_bytes(*b"dOut");
    /// `kAudioDeviceProcessorOverload` — 'over'
    const PROCESSOR_OVERLOAD: u32 = u32::from_be_bytes(*b"over");
    /// `kAudioObjectPropertyScopeGlobal` — 'glob'
    const SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob");
    /// `kAudioObjectPropertyElementMain`
    const ELEMENT_MAIN: u32 = 0;

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyData(
            object: AudioObjectID,
            address: *const AudioObjectPropertyAddress,
            qualifier_size: u32,
            qualifier: *const core::ffi::c_void,
            size: *mut u32,
            data: *mut core::ffi::c_void,
        ) -> OSStatus;
        fn AudioObjectAddPropertyListener(
            object: AudioObjectID,
            address: *const AudioObjectPropertyAddress,
            listener: extern "C" fn(
                AudioObjectID,
                u32,
                *const AudioObjectPropertyAddress,
                *mut core::ffi::c_void,
            ) -> OSStatus,
            client_data: *mut core::ffi::c_void,
        ) -> OSStatus;
    }

    extern "C" fn on_overload(
        _object: AudioObjectID,
        _n: u32,
        _addresses: *const AudioObjectPropertyAddress,
        _client: *mut core::ffi::c_void,
    ) -> OSStatus {
        OVERLOADS.fetch_add(1, Ordering::Relaxed);
        0
    }

    /// Register the overload listener on the current default output device. `Err` carries the
    /// failing call for the log; a machine without a device returns quietly.
    pub(super) fn register() -> Result<(), &'static str> {
        let default_addr = AudioObjectPropertyAddress {
            selector: DEFAULT_OUTPUT,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MAIN,
        };
        let mut device: AudioObjectID = 0;
        let mut size = std::mem::size_of::<AudioObjectID>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &default_addr,
                0,
                core::ptr::null(),
                &mut size,
                (&mut device as *mut AudioObjectID).cast(),
            )
        };
        if status != 0 || device == 0 {
            return Err("default output device lookup failed");
        }
        let overload_addr = AudioObjectPropertyAddress {
            selector: PROCESSOR_OVERLOAD,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MAIN,
        };
        let status = unsafe {
            AudioObjectAddPropertyListener(
                device,
                &overload_addr,
                on_overload,
                core::ptr::null_mut(),
            )
        };
        if status != 0 {
            return Err("overload listener registration failed");
        }
        Ok(())
    }

    pub(super) fn count() -> u64 {
        OVERLOADS.load(Ordering::Relaxed)
    }
}

/// Register the listener at startup (macOS with a live mixer only — silent runs have no device
/// stream to overload).
pub(super) fn setup(out: NonSend<super::SoundOutput>) {
    if out.mixer.is_none() {
        return;
    }
    #[cfg(target_os = "macos")]
    match sys::register() {
        Ok(()) => info!("audio: HAL overload listener armed on the default output device"),
        Err(what) => warn!("audio: HAL overload listener unavailable — {what}"),
    }
}

/// Report new overloads, rate-limited to one line per second — each one is a device cycle the
/// OS glitched *after* our (clean) mix, so a line landing at the moment a crackle is heard is
/// the downstream confirmation 1114 exists to catch.
pub(super) fn poll(time: Res<Time>, mut last: Local<(u64, f32)>) {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (time, last);
    }
    #[cfg(target_os = "macos")]
    {
        let now = time.elapsed_secs();
        let count = sys::count();
        let (reported, last_line_at) = *last;
        if count > reported && now - last_line_at >= 1.0 {
            warn!(
                "audio: {} HAL processor overload(s) — the OS missed the device cycle's hardware \
                 deadline DOWNSTREAM of our mix; the speaker glitched even though the mix is \
                 clean. This is the 1112 crackle signature.",
                count - reported,
            );
            *last = (count, now);
        }
    }
}
