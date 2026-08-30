//! Library half of `wenilla-host` — split out purely so `tests/*` can build the exact routers
//! `main.rs` serves. An integration test can only reach a crate's *library* target (a bin-only
//! package has no seam a `tests/` file can `use`), so `main.rs` stays a thin CLI wrapper over
//! these modules.

pub mod data;
pub mod static_site;
pub mod ws;

/// The only two ports the `/ws/{port}` proxy will ever dial — mangos realmd (login) and worldd
/// (world). Fixed here, not a CLI flag: this host can bind `0.0.0.0` on a Tailscale box, so the
/// allowlist is a security boundary, not a convenience default. Public so a wrapper binary that
/// mounts these routers behind its own auth (wenilla-realm) uses the same set.
pub const ALLOWED_PORTS: [u16; 2] = [3724, 8085];
