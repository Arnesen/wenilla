//! Windows CPU clocks. Pseudo-handles are borrowed and must not be closed.

use std::ffi::c_void;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct FileTime {
    low: u32,
    high: u32,
}

impl FileTime {
    fn ticks(self) -> u64 {
        (u64::from(self.high) << 32) | u64::from(self.low)
    }
}

type TimesFn = unsafe extern "system" fn(
    *mut c_void,
    *mut FileTime,
    *mut FileTime,
    *mut FileTime,
    *mut FileTime,
) -> i32;

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn GetCurrentThread() -> *mut c_void;
    fn GetProcessTimes(
        handle: *mut c_void,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn GetThreadTimes(
        handle: *mut c_void,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
}

fn read(handle: *mut c_void, times: TimesFn) -> Option<f64> {
    let (mut created, mut exited, mut kernel, mut user) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    // SAFETY: the caller supplies a current-process/thread pseudo-handle and its matching
    // Win32 function; all four FILETIME pointers are live, aligned, writable values.
    let ok = unsafe { times(handle, &mut created, &mut exited, &mut kernel, &mut user) };
    (ok != 0).then(|| (kernel.ticks() as f64 + user.ticks() as f64) * 1e-7)
}

pub(super) fn process_cpu_secs() -> Option<f64> {
    // SAFETY: returns a valid borrowed pseudo-handle, with no preconditions.
    read(unsafe { GetCurrentProcess() }, GetProcessTimes)
}

pub(super) fn main_thread_cpu_secs() -> Option<f64> {
    // SAFETY: returns the calling thread's borrowed pseudo-handle, with no preconditions.
    read(unsafe { GetCurrentThread() }, GetThreadTimes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_keeps_high_word() {
        assert_eq!(FileTime { low: 7, high: 2 }.ticks(), (2_u64 << 32) + 7);
    }

    #[test]
    fn clocks_measure_cpu_work() {
        let process = process_cpu_secs().expect("process CPU clock");
        let thread = main_thread_cpu_secs().expect("thread CPU clock");
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(100) {
            std::hint::black_box(
                (0..1000_u64).fold(1_u64, |v, n| v.wrapping_mul(3).wrapping_add(n)),
            );
        }
        let thread_end = main_thread_cpu_secs().unwrap();
        let process_end = process_cpu_secs().unwrap();
        assert!(
            thread_end > thread,
            "busy calling thread must accrue CPU time"
        );
        assert!(process_end > process, "busy process must accrue CPU time");
        assert!(
            process_end >= thread_end,
            "process includes the calling thread"
        );
    }
}
