//! Library half of `benilla-webhost` — split out purely so `tests/*` can build the exact routers
//! `main.rs` serves. An integration test can only reach a crate's *library* target (a bin-only
//! package has no seam a `tests/` file can `use`), so `main.rs` stays a thin CLI wrapper over
//! these modules.

pub mod data;
pub mod static_site;
pub mod ws;
