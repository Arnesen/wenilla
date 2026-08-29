use crate::messages::MovementInfo;
use crate::wire::Vector3d;

/// `MovementFlags::FORWARD` — the only movement flag benilla sets.
pub(super) const MOVEMENT_FLAG_FORWARD: u32 = 0x1;

/// A monotonic **client-uptime** millisecond stamp for the `MovementInfo` time field — the kind the real
/// 1.12 client sends (`GetTickCount()`, ms since the client started), **not** wall-clock. vmangos reads it
/// into `MovementInfo::ctime` and uses it only two ways (VERIFIED, decision 0057): a non-zero guard
/// (`ctime == 0` reads as "not a client packet" and *pauses* its movement extrapolation) and *deltas*
/// (`getMSTimeDiff`, so the epoch cancels). It never compares the value to its own clock, and it
/// overwrites the field with its server clock before relaying to nearby players — so this value never
/// reaches observers. Hence: match the real client's clock kind, wrap cleanly as a full `u32` (~49.7 days,
/// which `getMSTimeDiff` handles), and keep it non-zero. (The old value was wall-clock masked to 31 bits,
/// which folded backwards every ~24.85 days — a delta glitch the clean `u32` wrap avoids.)
pub(super) fn client_uptime_ms() -> u32 {
    use std::sync::OnceLock;
    // `web_time::Instant`, not `std::time::Instant`: the browser has no clock behind the std one
    // (it panics on first use), and this stamp is on every movement packet. A plain re-export of
    // std's on native.
    #[cfg(not(target_arch = "wasm32"))]
    use std::time::Instant;
    #[cfg(target_arch = "wasm32")]
    use web_time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    (START.get_or_init(Instant::now).elapsed().as_millis() as u32).max(1)
}

/// Build a `MovementInfo` stamped with the client-uptime time ([`client_uptime_ms`]).
pub(super) fn movement_info(pos: [f32; 3], orientation: f32, flags: u32) -> MovementInfo {
    MovementInfo {
        flags,
        timestamp: client_uptime_ms(),
        position: Vector3d {
            x: pos[0],
            y: pos[1],
            z: pos[2],
        },
        orientation,
        // benilla never sets ON_TRANSPORT outbound yet (boarding/riding is decision 0438 phase 2) — the
        // writer only emits the transport tail while the flag is set, so this is always dropped.
        transport: None,
        // Level swim (0) by default; the writer only emits the pitch tail while SWIMMING is set, and the
        // controller supplies the live swim pitch through `send_movement` when it does.
        pitch: 0.0,
        fall_time: 0,
        jump: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_uptime_ms_is_nonzero_and_monotonic() {
        // The load-bearing property: vmangos treats `ctime == 0` as "not a client packet" and pauses its
        // extrapolation, so our stamp must never be zero (the bug the old wall-clock value worked around).
        // It must also be non-decreasing so `getMSTimeDiff` deltas stay sane.
        let a = client_uptime_ms();
        let b = client_uptime_ms();
        assert!(a >= 1, "stamp is non-zero: {a}");
        assert!(b >= a, "stamp is monotonic non-decreasing: {a} -> {b}");
    }

    #[test]
    fn movement_info_is_stamped_nonzero() {
        let info = movement_info([1.0, 2.0, 3.0], 0.5, MOVEMENT_FLAG_FORWARD);
        assert_ne!(
            info.timestamp, 0,
            "a movement packet must carry a non-zero time"
        );
    }
}
