use std::sync::Arc;

use anyhow::{Context, Result};
use benilla_formats::Chain;
use clap::{Parser, Subcommand};
use wenilla_realm::{audit, db, mangos_conf, ratelimit, realmdb, secrets, soap, AppState, Config};

#[derive(Parser)]
#[command(
    about = "wenilla-realm: the login-gated web front and admin panel for a browser-only 1.12.1 realm"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve (the default).
    Serve,
    /// Forget the admin's password and print a fresh setup token: the wizard then lets you set a
    /// new admin password without touching anything else.
    ResetAdmin,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let cfg = Config::from_env()?;
    std::fs::create_dir_all(&cfg.state_dir)
        .with_context(|| format!("creating {}", cfg.state_dir.display()))?;
    let sqlite = db::open_sqlite(&cfg.sqlite_path()).await?;
    let keys = secrets::Keyring::load_or_create(&cfg.state_dir)?;

    match cli.cmd.unwrap_or(Cmd::Serve) {
        Cmd::ResetAdmin => {
            sqlx::query("DELETE FROM local_credentials WHERE user_id IN (SELECT id FROM users WHERE role = 'admin')").execute(&sqlite).await?;
            sqlx::query(
                "DELETE FROM sessions WHERE user_id IN (SELECT id FROM users WHERE role = 'admin')",
            )
            .execute(&sqlite)
            .await?;
            db::meta_set(&sqlite, "setup_complete", "0").await?;
            db::meta_set(&sqlite, "setup_mode", "reset").await?;
            let token = write_setup_token(&cfg, &sqlite).await?;
            audit::log(&sqlite, None, None, "admin.reset", None, None).await;
            println!(
                "Admin password cleared. Open {}/setup and use this token:\nSETUP TOKEN: {token}",
                cfg.public_url
            );
            return Ok(());
        }
        Cmd::Serve => {}
    }

    let state = Arc::new(AppState {
        realmdb: realmdb::connect(&cfg.mariadb_url).await?,
        soap: soap::Client::new(
            &cfg.soap_url,
            &cfg.soap_bootstrap_user,
            &cfg.soap_bootstrap_pass,
        ),
        conf: mangos_conf::ConfFiles::in_dir(&cfg.config_dir),
        secrets: keys,
        providers: Vec::new(),
        limiter: ratelimit::Limiter::default(),
        db: sqlite,
        cfg: cfg.clone(),
    });
    state.load_soap_credentials().await?;

    if !db::setup_complete(&state.db).await? {
        let token = write_setup_token(&cfg, &state.db).await?;
        tracing::info!("setup is not complete — open {}/setup", cfg.public_url);
        eprintln!("SETUP TOKEN: {token}");
    }

    let chain = Arc::new(
        Chain::open(&cfg.client_data)
            .with_context(|| format!("opening client data at {}", cfg.client_data.display()))?,
    );
    tracing::info!(data = %cfg.client_data.display(), "patch chain open");

    // Housekeeping: prune old IPs / expired sessions daily.
    {
        let db = state.db.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = audit::prune(&db).await {
                    tracing::warn!(error = %e, "prune");
                }
                tokio::time::sleep(std::time::Duration::from_secs(24 * 3600)).await;
            }
        });
    }

    let app = wenilla_realm::app(Arc::clone(&state), Some(chain), &cfg.www);
    let listener = tokio::net::TcpListener::bind(&cfg.bind)
        .await
        .with_context(|| format!("binding {}", cfg.bind))?;
    tracing::info!(bind = %cfg.bind, public = %cfg.public_url, "wenilla-realm listening");
    axum::serve(listener, app).await.context("serving")
}

/// A one-time token that gates the wizard, printed to the log and written to
/// `<state>/setup-token` (0600) so `realmctl` can show it.
async fn write_setup_token(cfg: &Config, sqlite: &sqlx::SqlitePool) -> Result<String> {
    let token = secrets::random_string(secrets::ALNUM, 24);
    db::meta_set(sqlite, "setup_token", &token).await?;
    secrets::write_private(&cfg.state_dir.join("setup-token"), &token)?;
    Ok(token)
}
