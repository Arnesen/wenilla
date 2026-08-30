//! Fixed-window counters in memory — enough to blunt password guessing on a ten-player realm.
//! Keys are free-form (`"login:ip:1.2.3.4"`, `"login:user:bob"`, `"play:7"`).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::db::now;

#[derive(Default)]
pub struct Limiter {
    windows: Mutex<HashMap<String, (i64, u32)>>,
}

impl Limiter {
    /// Count one hit; `true` when still within `max` per `window_secs`.
    pub fn allow(&self, key: &str, max: u32, window_secs: i64) -> bool {
        let t = now();
        let mut map = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() > 10_000 {
            map.retain(|_, (start, _)| t - *start < window_secs);
        }
        let entry = map.entry(key.to_string()).or_insert((t, 0));
        if t - entry.0 >= window_secs {
            *entry = (t, 0);
        }
        entry.1 += 1;
        entry.1 <= max
    }
}

#[cfg(test)]
mod tests {
    use super::Limiter;

    #[test]
    fn counts_within_window() {
        let l = Limiter::default();
        assert!(l.allow("k", 2, 60));
        assert!(l.allow("k", 2, 60));
        assert!(!l.allow("k", 2, 60));
        assert!(l.allow("other", 2, 60));
    }
}
