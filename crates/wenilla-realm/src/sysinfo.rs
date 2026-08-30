//! Memory pressure for the dashboard, read from `/proc`. Inside the container `/proc/meminfo`
//! is the machine's (Docker does not virtualise it), so total/available are what the operator
//! sizes the VM by. Per-process RSS is visible only for processes in the same PID namespace —
//! the packaging runs the realm container with `pid: "service:mangosd"` so the world server
//! shows up; natively (a dev box) every process does.

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Memory {
    pub total_mb: u64,
    pub available_mb: u64,
    pub used_mb: u64,
    pub used_pct: u64,
    pub swap_total_mb: u64,
    pub swap_used_mb: u64,
    /// `(process name, RSS in MB)` for the game processes that are visible.
    pub processes: Vec<(String, u64)>,
}

impl Memory {
    pub fn low(&self) -> bool {
        self.total_mb > 0 && self.available_mb * 100 / self.total_mb < 15
    }
}

fn kb(line: &str) -> Option<u64> {
    line.split_whitespace().nth(1)?.parse().ok()
}

pub fn read() -> Memory {
    let mut m = Memory::default();
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        let (mut total, mut avail, mut stotal, mut sfree) = (0u64, 0u64, 0u64, 0u64);
        for l in text.lines() {
            if l.starts_with("MemTotal:") {
                total = kb(l).unwrap_or(0)
            }
            if l.starts_with("MemAvailable:") {
                avail = kb(l).unwrap_or(0)
            }
            if l.starts_with("SwapTotal:") {
                stotal = kb(l).unwrap_or(0)
            }
            if l.starts_with("SwapFree:") {
                sfree = kb(l).unwrap_or(0)
            }
        }
        m.total_mb = total / 1024;
        m.available_mb = avail / 1024;
        m.used_mb = m.total_mb.saturating_sub(m.available_mb);
        m.used_pct = if m.total_mb > 0 {
            m.used_mb * 100 / m.total_mb
        } else {
            0
        };
        m.swap_total_mb = stotal / 1024;
        m.swap_used_mb = stotal.saturating_sub(sfree) / 1024;
    }
    if let Ok(dir) = std::fs::read_dir("/proc") {
        for e in dir.flatten() {
            let Ok(status) = std::fs::read_to_string(e.path().join("status")) else {
                continue;
            };
            let mut name = None;
            let mut rss = None;
            for l in status.lines() {
                if let Some(n) = l.strip_prefix("Name:") {
                    name = Some(n.trim().to_string())
                }
                if l.starts_with("VmRSS:") {
                    rss = kb(l)
                }
            }
            if let (Some(n), Some(r)) = (name, rss) {
                if matches!(
                    n.as_str(),
                    "mangosd" | "realmd" | "mariadbd" | "mysqld" | "wenilla-realm" | "caddy"
                ) {
                    m.processes.push((n, r / 1024));
                }
            }
        }
        m.processes.sort_by(|a, b| b.1.cmp(&a.1));
    }
    m
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_something_on_linux() {
        let m = super::read();
        assert!(m.total_mb > 0);
        assert!(m.used_pct <= 100);
    }
}
