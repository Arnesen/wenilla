//! `benilla-auth` — Phase 3 CLI: SRP6 logon against a vanilla realmd, print the realm list.
//!
//! Example: `cargo run --bin benilla-auth -- one pone localhost`

// These CLIs are native tools — a browser has no argv, no realmd to dial by hand, and none of the
// blocking `WorldSession` twins the probe harness is written against. The bin target still needs a
// `main` for the web build to link, so it gets an empty one and everything else is gated off.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use anyhow::Result;
#[cfg(not(target_arch = "wasm32"))]
use clap::Parser;

/// Log in to a WoW 1.12.1 auth server and print its realm list.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Parser)]
#[command(name = "benilla-auth", version, about)]
struct Cli {
    /// Account name.
    username: String,
    /// Account password.
    password: String,
    /// Auth (realmd) server host.
    #[arg(default_value = "localhost")]
    host: String,
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    let cli = Cli::parse();

    let logon = benilla_protocol::logon(&cli.host, &cli.username, &cli.password)?;

    println!(
        "authenticated as '{}' (session key {} bytes)",
        cli.username,
        logon.session_key.len()
    );
    if logon.realms.is_empty() {
        println!("no realms advertised");
    }
    for (i, realm) in logon.realms.iter().enumerate() {
        println!(
            "[Realm {}] {} — {} @ {} ({} characters)",
            i + 1,
            realm.name,
            realm.population,
            realm.address,
            realm.characters
        );
    }

    Ok(())
}
