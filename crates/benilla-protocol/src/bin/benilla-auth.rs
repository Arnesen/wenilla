//! `benilla-auth` — Phase 3 CLI: SRP6 logon against a vanilla realmd, print the realm list.
//!
//! Example: `cargo run --bin benilla-auth -- one pone localhost`

use anyhow::Result;
use clap::Parser;

/// Log in to a WoW 1.12.1 auth server and print its realm list.
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
