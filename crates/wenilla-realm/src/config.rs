//! Process configuration — environment only, so the same binary runs in the compose image and
//! on a developer's box. Every knob has a default that matches the container layout
//! (`/state`, `/config`, `/client/Data`, service names `realmd`/`mangosd`/`mariadb`).

use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    /// The URL players use, e.g. `https://realm.example`. Also the only `Origin` the WebSocket
    /// relay accepts.
    pub public_url: String,
    pub state_dir: PathBuf,
    pub www: PathBuf,
    pub client_data: PathBuf,
    pub config_dir: PathBuf,
    pub mariadb_url: String,
    pub soap_url: String,
    /// The console login used until the wizard has created the service's own (`WRSOAP`). The
    /// classic-db seed is `ADMINISTRATOR/ADMINISTRATOR`.
    pub soap_bootstrap_user: String,
    pub soap_bootstrap_pass: String,
    pub realmd_host: String,
    pub mangosd_host: String,
    /// How long an admin-issued first-login password stays usable, in hours; `0` disables the
    /// expiry. It is handed over out of band (a chat message, a spoken word), so its lifetime is
    /// the real control on who can claim the account — not the password itself, which the
    /// recipient must replace on first login anyway. Also the operator's escape hatch: raise it
    /// if an invite must survive a weekend.
    pub bootstrap_ttl_hours: i64,
    /// Let the page's `?user=&pass=` query log a player in — development only.
    pub dev_query_creds: bool,
    /// Drop the cookie's `Secure` flag — plain-http development only.
    pub cookie_insecure: bool,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let public_url = env_or("REALM_PUBLIC_URL", "https://localhost");
        url::Url::parse(&public_url)
            .with_context(|| format!("REALM_PUBLIC_URL {public_url:?} is not a URL"))?;
        Ok(Self {
            bind: env_or("REALM_BIND", "0.0.0.0:8090"),
            public_url: public_url.trim_end_matches('/').to_string(),
            state_dir: env_or("REALM_STATE_DIR", "/state").into(),
            www: env_or("REALM_WWW", "/app/www").into(),
            client_data: env_or("CLIENT_DATA", "/client/Data").into(),
            config_dir: env_or("CONFIG_DIR", "/config").into(),
            mariadb_url: env_or(
                "MARIADB_URL",
                "mysql://realmweb:realmweb@mariadb:3306/classicrealmd",
            ),
            soap_url: env_or("SOAP_URL", "http://mangosd:7878/"),
            soap_bootstrap_user: env_or("SOAP_BOOTSTRAP_USER", "ADMINISTRATOR"),
            soap_bootstrap_pass: env_or("SOAP_BOOTSTRAP_PASS", "ADMINISTRATOR"),
            realmd_host: env_or("REALMD_HOST", "realmd"),
            mangosd_host: env_or("MANGOSD_HOST", "mangosd"),
            bootstrap_ttl_hours: {
                let raw = env_or("REALM_BOOTSTRAP_TTL_HOURS", "48");
                raw.parse().with_context(|| {
                    format!("REALM_BOOTSTRAP_TTL_HOURS {raw:?} is not a whole number of hours")
                })?
            },
            dev_query_creds: env_or("WENILLA_DEV_QUERY_CREDS", "0") == "1",
            cookie_insecure: env_or("REALM_COOKIE_INSECURE", "0") == "1",
        })
    }

    pub fn sqlite_path(&self) -> PathBuf {
        self.state_dir.join("realm.sqlite")
    }

    /// Host part of `public_url` — what goes into `realmlist.address` (a label on the web: the
    /// client dials the page's own origin, `benilla-protocol::transport::web`).
    pub fn public_host(&self) -> String {
        url::Url::parse(&self.public_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| "localhost".into())
    }
}
