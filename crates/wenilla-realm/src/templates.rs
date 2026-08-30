//! Askama templates — one struct per page; the HTML lives in `templates/`.

use askama::Template;

use crate::session::User;

#[derive(Template)]
#[template(path = "login.html")]
pub struct Login {
    pub error: Option<String>,
    pub realm_name: String,
}

#[derive(Template)]
#[template(path = "password.html")]
pub struct Password {
    pub csrf: String,
    pub error: Option<String>,
    pub forced: bool,
    pub user: User,
}

#[derive(Template)]
#[template(path = "play.html")]
pub struct Play {
    pub user: User,
}

#[derive(Template)]
#[template(path = "about.html")]
pub struct About {
    pub realm_name: String,
}

#[derive(Template)]
#[template(path = "privacy.html")]
pub struct Privacy {
    pub realm_name: String,
}

#[derive(Template)]
#[template(path = "setup.html")]
pub struct Setup {
    pub error: Option<String>,
    pub token: String,
    pub public_url: String,
    pub bot_account_count: i64,
}

pub struct UserRow {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub disabled: bool,
    pub game_username: String,
    pub banned: bool,
    pub characters: Vec<crate::realmdb::Character>,
}

#[derive(Template)]
#[template(path = "admin_users.html")]
pub struct AdminUsers {
    pub realm_name: String,
    pub nav: &'static str,
    pub csrf: String,
    pub me: User,
    pub users: Vec<UserRow>,
    pub notice: Option<String>,
    pub created: Option<(String, String)>,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "admin_dashboard.html")]
pub struct AdminDashboard {
    pub realm_name: String,
    pub nav: &'static str,
    pub csrf: String,
    pub me: User,
    pub status: crate::control::Status,
    pub s: crate::mangos_conf::Settings,
    pub online: Vec<crate::realmdb::OnlineRow>,
    pub players_online: i64,
    pub bots_online: i64,
    pub restart_pending: bool,
    pub notice: Option<String>,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "admin_config.html")]
pub struct AdminConfig {
    pub realm_name: String,
    pub nav: &'static str,
    pub csrf: String,
    pub me: User,
    pub s: crate::mangos_conf::Settings,
    pub restart_pending: bool,
    pub notice: Option<String>,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "admin_audit.html")]
pub struct AdminAudit {
    pub realm_name: String,
    pub nav: &'static str,
    pub me: User,
    pub entries: Vec<crate::audit::Entry>,
    pub next_before: Option<i64>,
}

pub fn race(id: &i64) -> &'static str {
    crate::realmdb::race_name(*id)
}
pub fn class(id: &i64) -> &'static str {
    crate::realmdb::class_name(*id)
}
pub fn when(ts: &i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(*ts)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .map(|s| s.chars().take(19).collect::<String>().replace('T', " "))
        .unwrap_or_default()
}
pub fn hours(secs: &i64) -> String {
    format!("{:.1}", *secs as f64 / 3600.0)
}
