//! Server control over the console: status, announcements, MOTD, config reload, restart.
//! There is deliberately no "stop": under `restart: unless-stopped` a clean exit just comes
//! straight back, so stopping the realm is the operator's `realmctl down`.

use anyhow::Result;

use crate::mangos_conf::Apply;
use crate::soap::{self, Client};

#[derive(Debug, Default, serde::Serialize)]
pub struct Status {
    pub reachable: bool,
    pub raw: String,
    pub uptime: Option<String>,
    pub players: Option<String>,
}

/// `server info` as mangosd prints it, lightly parsed.
pub async fn status(soap: &Client) -> Status {
    match soap.exec("server info").await {
        Ok(raw) => {
            let uptime = raw
                .lines()
                .find(|l| l.contains("uptime"))
                .map(|l| l.trim().to_string());
            let players = raw
                .lines()
                .find(|l| l.contains("Players online") || l.contains("online"))
                .map(|l| l.trim().to_string());
            Status {
                reachable: true,
                raw,
                uptime,
                players,
            }
        }
        Err(e) => Status {
            reachable: false,
            raw: e.to_string(),
            ..Default::default()
        },
    }
}

pub async fn announce(soap: &Client, text: &str) -> Result<()> {
    let t = soap::text(text, 200)?;
    soap.exec(&format!("announce {t}")).await?;
    Ok(())
}

pub async fn notify(soap: &Client, text: &str) -> Result<()> {
    let t = soap::text(text, 200)?;
    soap.exec(&format!("notify {t}")).await?;
    Ok(())
}

pub async fn set_motd_live(soap: &Client, text: &str) -> Result<()> {
    let t = soap::text(text, 250)?;
    soap.exec(&format!("server set motd {t}")).await?;
    Ok(())
}

/// Run the apply steps a settings save returned; returns whether a restart is still pending.
pub async fn apply(soap: &Client, steps: &[Apply]) -> Result<bool> {
    let mut restart = false;
    for s in steps {
        match s {
            Apply::Hot => {
                soap.exec("reload config").await?;
            }
            Apply::Bots => {
                soap.exec("rndbot reload").await?;
            }
            Apply::AhBot => {
                soap.exec("ahbot reload").await?;
            }
            Apply::Restart => restart = true,
        }
    }
    Ok(restart)
}

/// `server restart <delay>`: mangosd counts down in chat, saves every character, exits with
/// code 2; the container's restart policy starts it again.
pub async fn restart(soap: &Client, delay_secs: u32) -> Result<()> {
    soap.exec(&format!("server restart {}", delay_secs.clamp(0, 3600)))
        .await?;
    Ok(())
}

pub async fn cancel_restart(soap: &Client) -> Result<()> {
    soap.exec("server restart cancel").await?;
    Ok(())
}

pub async fn pinfo(soap: &Client, character: &str) -> Result<String> {
    let c = soap::arg(character, 12)?;
    Ok(soap.exec(&format!("pinfo {c}")).await?)
}
