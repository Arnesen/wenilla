//! End-to-end through the router with a mock console: wizard → admin creates a player → the
//! player logs in → `/api/play` hands out hidden game credentials; the game routes are locked
//! without a session; CSRF and cross-site posts are refused.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use http_body_util::BodyExt;
use tower::ServiceExt;
use wenilla_realm::{db, mangos_conf, ratelimit, realmdb, secrets, soap, AppState, Config};

/// Stands in for the headless player: names the character after the first candidate, instantly.
struct FakeBuilder;

#[async_trait::async_trait]
impl wenilla_realm::presets::Provisioner for FakeBuilder {
    async fn build(
        &self,
        job: wenilla_realm::presets::Job,
    ) -> anyhow::Result<wenilla_realm::presets::headless::Built> {
        Ok(wenilla_realm::presets::headless::Built {
            name: job.names[0].clone(),
            guid: 1,
            not_equipped: Vec::new(),
        })
    }
}

/// A stand-in for mangosd's SOAP port: records every command, answers `ok`.
async fn mock_soap() -> (String, Arc<Mutex<Vec<String>>>) {
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = Arc::clone(&log);
    let app = axum::Router::new().route(
        "/",
        post(move |body: String| {
            let seen = Arc::clone(&seen);
            async move {
                let cmd = body.split("<command>").nth(1).and_then(|s| s.split("</command>").next()).unwrap_or("").to_string();
                seen.lock().unwrap().push(cmd.clone());
                let reply = if cmd.starts_with("server info") { "Server uptime: 1 minute\nPlayers online: 0" } else { "ok" };
                (StatusCode::OK, format!("<SOAP-ENV:Envelope><SOAP-ENV:Body><ns1:executeCommandResponse><result>{reply}&#xD;\n</result></ns1:executeCommandResponse></SOAP-ENV:Body></SOAP-ENV:Envelope>")).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, log)
}

struct Harness {
    app: axum::Router,
    state: Arc<AppState>,
    soap_log: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let conf_dir = dir.path().join("config");
    let www = dir.path().join("www");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::create_dir_all(&www).unwrap();
    std::fs::write(
        www.join("wenilla.js"),
        "export default async function init() {}",
    )
    .unwrap();
    std::fs::write(conf_dir.join("mangosd.conf"), "Rate.XP.Kill = 1\nRate.XP.Quest = 1\nRate.XP.Explore = 1\nRate.Drop.Item.Poor = 1\nRate.Drop.Item.Normal = 1\nRate.Drop.Item.Uncommon = 1\nRate.Drop.Item.Rare = 1\nRate.Drop.Item.Epic = 1\nRate.Drop.Item.Legendary = 1\nRate.Drop.Item.Artifact = 1\nRate.Drop.Item.Referenced = 1\nRate.Drop.Money = 1\nPlayerLimit = 100\nPlayerSave.Interval = 900000\nMaxPlayerLevel = 60\nMotd = \"Hi\"\n").unwrap();
    std::fs::write(conf_dir.join("aiplayerbot.conf"), "AiPlayerbot.Enabled = 1\nAiPlayerbot.MinRandomBots = 50\nAiPlayerbot.MaxRandomBots = 50\nAiPlayerbot.RandomBotAccountCount = 50\n").unwrap();
    std::fs::write(
        conf_dir.join("ahbot.conf"),
        "AuctionHouseBot.Chance.Sell = 0\nAuctionHouseBot.Chance.Buy = 0\n",
    )
    .unwrap();
    let (soap_url, soap_log) = mock_soap().await;
    std::env::set_var("REALM_PUBLIC_URL", "http://localhost:8090");
    std::env::set_var("REALM_COOKIE_INSECURE", "1");
    let mut cfg = Config::from_env().unwrap();
    cfg.state_dir = state_dir.clone();
    cfg.www = www.clone();
    cfg.config_dir = conf_dir.clone();
    cfg.soap_url = soap_url;
    cfg.mariadb_url = "mysql://nobody:nothing@127.0.0.1:1/classicrealmd".into();
    let sqlite = db::open_sqlite(&cfg.sqlite_path()).await.unwrap();
    db::meta_set(&sqlite, "setup_token", "TESTTOKEN")
        .await
        .unwrap();
    let state = Arc::new(AppState {
        setup_cache: Default::default(),
        realmdb: realmdb::connect(&cfg.mariadb_url).await.unwrap(),
        soap: soap::Client::new(&cfg.soap_url, "ADMINISTRATOR", "ADMINISTRATOR"),
        conf: mangos_conf::ConfFiles::in_dir(&conf_dir),
        secrets: secrets::Keyring::load_or_create(&state_dir).unwrap(),
        providers: Vec::new(),
        limiter: ratelimit::Limiter::default(),
        provisioner: Arc::new(FakeBuilder),
        client_data_error: Some("test: no client data".into()),
        db: sqlite,
        cfg: cfg.clone(),
    });
    let app = wenilla_realm::app(Arc::clone(&state), None, &www);
    Harness {
        app,
        state,
        soap_log,
        _dir: dir,
    }
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

impl Resp {
    fn cookie(&self) -> Option<String> {
        self.headers
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap().to_string())
    }
    fn location(&self) -> &str {
        self.headers
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }
}

async fn send(app: &axum::Router, req: Request<Body>) -> Resp {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    Resp {
        status,
        headers,
        body,
    }
}

fn get(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut b = Request::get(path).header(header::ACCEPT, "text/html");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::empty()).unwrap()
}

fn form(path: &str, cookie: Option<&str>, fields: &[(&str, &str)]) -> Request<Body> {
    let body: String = fields
        .iter()
        .map(|(k, v)| format!("{k}={}", urlenc(v)))
        .collect::<Vec<_>>()
        .join("&");
    let mut b = Request::post(path)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ACCEPT, "text/html");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::from(body)).unwrap()
}

fn urlenc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn csrf_of(html: &str) -> String {
    let i =
        html.find("name=\"_csrf\" value=\"").expect("csrf field") + "name=\"_csrf\" value=\"".len();
    html[i..].split('"').next().unwrap().to_string()
}

async fn run_setup(h: &Harness) -> String {
    let r = send(&h.app, get("/admin", None)).await;
    assert_eq!(
        r.status,
        StatusCode::SEE_OTHER,
        "setup gate redirects: {}",
        r.body
    );
    assert_eq!(r.location(), "/setup");
    let r = send(
        &h.app,
        form(
            "/setup",
            None,
            &[
                ("token", "wrong"),
                ("admin_username", "boss"),
                ("admin_password", "correct horse battery"),
                ("admin_password2", "correct horse battery"),
                ("realm_name", "Test Realm"),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("wrong setup token"));
    let r = send(
        &h.app,
        form(
            "/setup",
            None,
            &[
                ("token", "TESTTOKEN"),
                ("admin_username", "boss"),
                ("admin_password", "correct horse battery"),
                ("admin_password2", "correct horse battery"),
                ("realm_name", "Test Realm"),
                ("xp_rate", "2"),
                ("loot_rate", "1"),
                ("money_rate", "1"),
                ("bots", "10"),
                ("motd", "hello"),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    assert_eq!(r.location(), "/admin/users");
    let cookie = r.cookie().expect("admin session cookie");
    assert!(db::setup_complete(&h.state.db).await.unwrap());
    cookie
}

#[tokio::test]
async fn wizard_bootstraps_console_and_settings() {
    let h = harness().await;
    run_setup(&h).await;
    let log = h.soap_log.lock().unwrap().clone();
    assert!(
        log.iter().any(|c| c.starts_with("account create WRSOAP ")),
        "{log:?}"
    );
    assert!(log.iter().any(|c| c == "account set gmlevel WRSOAP 3 -1"));
    assert!(log
        .iter()
        .any(|c| c.starts_with("account set password ADMINISTRATOR ")));
    assert!(log.iter().any(|c| c == "reload config"));
    assert!(log.iter().any(|c| c == "rndbot reload"));
    assert!(
        log.iter()
            .any(|c| c.starts_with("account create WR000001 ")),
        "admin gets a game account: {log:?}"
    );
    let s = mangos_conf::load(&h.state.conf).unwrap();
    assert_eq!(s.xp_rate, 2.0);
    assert_eq!(s.bots, 10);
    assert_eq!(s.motd, "hello");
    assert_eq!(
        db::meta_get(&h.state.db, "soap_user")
            .await
            .unwrap()
            .as_deref(),
        Some("WRSOAP")
    );
    // The token is single-use.
    assert_eq!(
        db::meta_get(&h.state.db, "setup_token").await.unwrap(),
        None
    );
    let r = send(&h.app, get("/setup", None)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn admin_creates_player_who_can_fetch_hidden_game_credentials() {
    let h = harness().await;
    let admin = run_setup(&h).await;
    let page = send(&h.app, get("/admin/users", Some(&admin))).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("boss"));
    let csrf = csrf_of(&page.body);

    // Wrong CSRF token → refused.
    let r = send(
        &h.app,
        form(
            "/admin/users",
            Some(&admin),
            &[("_csrf", "nope"), ("username", "alice")],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Cross-site post → refused before any handler.
    let mut req = form(
        "/admin/users",
        Some(&admin),
        &[("_csrf", &csrf), ("username", "alice")],
    );
    req.headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    assert_eq!(send(&h.app, req).await.status, StatusCode::FORBIDDEN);

    let r = send(
        &h.app,
        form(
            "/admin/users",
            Some(&admin),
            &[
                ("_csrf", &csrf),
                ("username", "alice"),
                ("display_name", "Alice"),
                ("password", ""),
                ("role", "player"),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let i = r
        .body
        .find("is <code>")
        .expect("generated password shown once")
        + "is <code>".len();
    let password = r.body[i..].split('<').next().unwrap().to_string();
    assert!(password.len() >= 10);
    assert!(h
        .soap_log
        .lock()
        .unwrap()
        .iter()
        .any(|c| c.starts_with("account create WR000002 ")));

    // A player cannot open the admin panel.
    let r = send(
        &h.app,
        form(
            "/login",
            None,
            &[("username", "alice"), ("password", &password)],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    assert_eq!(
        r.location(),
        "/account/password",
        "first login forces a password change"
    );
    let alice = r.cookie().unwrap();

    // …and "forces" means it: until the password is changed, the session opens the change page
    // and nothing else. This assertion is the fix for a real hole — the redirect above used to be
    // the whole of it, so a player who simply typed "/" played on the admin-issued password
    // forever.
    let r = send(&h.app, get("/", Some(&alice))).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.location(), "/account/password");
    for path in ["/api/play", "/data/__index", "/ws/8085", "/wenilla.js"] {
        let r = send(
            &h.app,
            Request::get(path)
                .header(header::COOKIE, &alice)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::FORBIDDEN,
            "{path} is closed until the password changes"
        );
    }

    // The change page itself is reachable, and going through it opens everything else.
    let page = send(&h.app, get("/account/password", Some(&alice))).await;
    assert_eq!(page.status, StatusCode::OK);
    let alice_csrf = csrf_of(&page.body);
    let r = send(
        &h.app,
        form(
            "/account/password",
            Some(&alice),
            &[
                ("_csrf", &alice_csrf),
                ("current", &password),
                ("new", "a much better secret"),
                ("confirm", "a much better secret"),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    assert_eq!(r.location(), "/");

    // A player still cannot open the admin panel.
    assert_eq!(
        send(&h.app, get("/admin", Some(&alice))).await.status,
        StatusCode::FORBIDDEN
    );

    // Hidden credentials over the session, never in a URL.
    let r = send(
        &h.app,
        Request::get("/api/play")
            .header(header::COOKIE, &alice)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(v["user"], "WR000002");
    assert_eq!(v["pass"].as_str().unwrap().len(), 16);
    assert_eq!(v["host"], "localhost");
    assert_eq!(v["realm"], "Test Realm");
    assert!(v.get("dev_query_creds").is_none());
    assert_eq!(r.headers.get(header::CACHE_CONTROL).unwrap(), "no-store");

    // Wrong password and a disabled account fail the same way.
    let r = send(
        &h.app,
        form(
            "/login",
            None,
            &[("username", "alice"), ("password", "nope nope nope")],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("wrong username or password"));
    let r = send(
        &h.app,
        form("/admin/users/2/disable", Some(&admin), &[("_csrf", &csrf)]),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        send(&h.app, get("/", Some(&alice))).await.status,
        StatusCode::SEE_OTHER,
        "disabled: session is gone"
    );
    assert!(h
        .soap_log
        .lock()
        .unwrap()
        .iter()
        .any(|c| c == "ban account WR000002 -1 web"));
}

#[tokio::test]
async fn game_routes_are_locked_without_a_session() {
    let h = harness().await;
    let admin = run_setup(&h).await;
    for path in ["/wenilla.js", "/ws/8085", "/data/__index", "/api/play"] {
        let r = send(&h.app, Request::get(path).body(Body::empty()).unwrap()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path} must be locked");
        assert_eq!(
            r.headers
                .get(header::CACHE_CONTROL)
                .map(|v| v.to_str().unwrap()),
            Some("no-store"),
            "{path}"
        );
    }
    let r = send(
        &h.app,
        Request::get("/wenilla.js")
            .header(header::COOKIE, &admin)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("init"));
    // A page request without a session goes to the login form instead.
    let r = send(&h.app, get("/", None)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.location(), "/login");
    // The play page itself, with a session, boots the client from /api/play.
    let r = send(&h.app, get("/", Some(&admin))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("fetch('/api/play'"));
    assert!(r.body.contains("__wenilla_env"));
}

#[tokio::test]
async fn config_page_saves_and_applies() {
    let h = harness().await;
    let admin = run_setup(&h).await;
    let page = send(&h.app, get("/admin/config", Some(&admin))).await;
    assert_eq!(page.status, StatusCode::OK);
    let csrf = csrf_of(&page.body);
    let r = send(
        &h.app,
        form(
            "/admin/config",
            Some(&admin),
            &[
                ("_csrf", &csrf),
                ("xp_rate", "3"),
                ("loot_rate", "2"),
                ("money_rate", "1"),
                ("player_limit", "50"),
                ("save_interval_secs", "300"),
                ("max_player_level", "60"),
                ("motd", "new motd"),
                ("bots", "10"),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    assert!(r.location().contains("applied+live"), "{}", r.location());
    let s = mangos_conf::load(&h.state.conf).unwrap();
    assert_eq!(
        (
            s.xp_rate,
            s.loot_rate,
            s.player_limit,
            s.save_interval_secs,
            s.motd.as_str()
        ),
        (3.0, 2.0, 50, 300, "new motd")
    );
    assert!(h
        .soap_log
        .lock()
        .unwrap()
        .iter()
        .any(|c| c == "server set motd new motd"));
    // Audit has it all.
    let r = send(&h.app, get("/admin/audit", Some(&admin))).await;
    assert!(
        r.body.contains("config.save")
            && r.body.contains("setup.complete")
            && r.body.contains("login.ok")
            || r.body.contains("setup.complete"),
        "{}",
        r.body
    );
}

/// The admin-issued password is a *bootstrap* credential: it gets someone to their first login
/// and then stops working. It travels out of band — pasted into a chat, read aloud — so its
/// lifetime is the real control on who can claim the account.
#[tokio::test]
async fn an_expired_first_login_password_stops_working() {
    let h = harness().await;
    let admin = run_setup(&h).await;
    let page = send(&h.app, get("/admin/users", Some(&admin))).await;
    let csrf = csrf_of(&page.body);
    let r = send(
        &h.app,
        form(
            "/admin/users",
            Some(&admin),
            &[
                ("_csrf", &csrf),
                ("username", "bob"),
                ("display_name", "Bob"),
                ("password", ""),
                ("role", "player"),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let i = r.body.find("is <code>").expect("generated password") + "is <code>".len();
    let password = r.body[i..].split('<').next().unwrap().to_string();

    // It was issued with an expiry (the default TTL is hours away, so this is not yet in force).
    let exp: (Option<i64>,) =
        sqlx::query_as("SELECT expires_at FROM local_credentials WHERE user_id = 2")
            .fetch_one(&h.state.db)
            .await
            .unwrap();
    assert!(
        exp.0.unwrap() > 0,
        "an admin-issued password carries an expiry"
    );

    // Age it past its deadline — the only way forward in time from out here.
    sqlx::query("UPDATE local_credentials SET expires_at = 1 WHERE user_id = 2")
        .execute(&h.state.db)
        .await
        .unwrap();
    let r = send(
        &h.app,
        form(
            "/login",
            None,
            &[("username", "bob"), ("password", &password)],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "no session is issued");
    assert!(r.body.contains("expired"), "{}", r.body);
    assert!(r.cookie().is_none(), "and no cookie either");

    // The admin re-issues, which is the whole recovery path: a fresh password with a fresh clock.
    let r = send(
        &h.app,
        form(
            "/admin/users/2/reset-web-password",
            Some(&admin),
            &[("_csrf", &csrf)],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let exp: (Option<i64>,) =
        sqlx::query_as("SELECT expires_at FROM local_credentials WHERE user_id = 2")
            .fetch_one(&h.state.db)
            .await
            .unwrap();
    assert!(exp.0.unwrap() > 1, "the reset re-arms the clock");
}

/// A password the user chose for themselves is theirs to keep: no forced change, no expiry.
#[tokio::test]
async fn a_self_chosen_password_never_expires() {
    let h = harness().await;
    let admin = run_setup(&h).await;
    let row: (i64, Option<i64>) =
        sqlx::query_as("SELECT must_change, expires_at FROM local_credentials WHERE user_id = 1")
            .fetch_one(&h.state.db)
            .await
            .unwrap();
    assert_eq!(row, (0, None), "the wizard's own admin password");
    // …and it opens the panel immediately, with no change demanded.
    assert_eq!(
        send(&h.app, get("/admin", Some(&admin))).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn setup_cache_refreshes_after_completion_and_external_reset() {
    let h = harness().await;
    assert!(!h.state.setup_complete().await.unwrap());
    h.state.mark_setup_complete().await.unwrap();
    assert!(h.state.setup_complete().await.unwrap());
    db::meta_set(&h.state.db, "setup_complete", "0")
        .await
        .unwrap();
    // Simulate TTL expiry without a wall-clock sleep.
    *h.state.setup_cache.lock().await = Some((
        std::time::Instant::now() - std::time::Duration::from_secs(6),
        true,
    ));
    assert!(!h.state.setup_complete().await.unwrap());
}

/// Poll until the background build of `group` has finished (the fake builder is instant).
async fn wait_group(h: &Harness, group: i64) -> wenilla_realm::presets::Group {
    for _ in 0..100 {
        let g = wenilla_realm::presets::by_id(&h.state.db, group)
            .await
            .unwrap()
            .unwrap();
        if g.status != "building" {
            return g;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("group {group} never finished building");
}

#[tokio::test]
async fn a_preset_group_link_joins_summons_and_deletes() {
    let h = harness().await;
    let admin = run_setup(&h).await;

    // The admin makes a Deadmines group.
    let page = send(&h.app, get("/admin/presets", Some(&admin))).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("The Deadmines") && page.body.contains("Blackrock Depths"));
    let csrf = csrf_of(&page.body);
    let r = send(
        &h.app,
        form(
            "/admin/presets",
            Some(&admin),
            &[("_csrf", "nope"), ("preset", "deadmines")],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "CSRF still applies");
    let r = send(
        &h.app,
        form(
            "/admin/presets",
            Some(&admin),
            &[("_csrf", &csrf), ("preset", "deadmines")],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let g = wait_group(&h, 1).await;
    assert_eq!(g.status, "ready");
    let members = wenilla_realm::presets::members(&h.state.db, 1)
        .await
        .unwrap();
    assert_eq!(members.len(), 5);
    assert!(members
        .iter()
        .all(|m| m.status == "ready" && m.char_name.is_some()));

    // Each builder got GM for the build and lost it after.
    let log = h.soap_log.lock().unwrap().clone();
    for m in &members {
        let acct = wenilla_realm::accounts::get(&h.state.db, m.user_id)
            .await
            .unwrap()
            .unwrap()
            .game_username;
        assert!(acct.starts_with("WP") && acct.len() == 12, "{acct}");
        assert!(
            log.iter()
                .any(|c| c.starts_with(&format!("account create {acct} "))),
            "{log:?}"
        );
        let up = log
            .iter()
            .position(|c| *c == format!("account set gmlevel {acct} 3 -1"));
        let down = log
            .iter()
            .position(|c| *c == format!("account set gmlevel {acct} 0 -1"));
        assert!(up.is_some() && down > up, "{acct}: {log:?}");
    }

    // The link: shown on the admin page, it opens without a session.
    let page = send(&h.app, get("/admin/presets", Some(&admin))).await;
    let at = page.body.find("/g/").expect("link on the admin page");
    let token: String = page.body[at + 3..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    assert_eq!(token.len(), 43);
    // Preset users stay off the Users page.
    let users = send(&h.app, get("/admin/users", Some(&admin))).await;
    assert!(!users.body.contains("preset-1-"), "{}", users.body);

    let r = send(&h.app, get(&format!("/g/{token}"), None)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Play Warrior") && r.body.contains("Play Priest"));
    assert_eq!(
        r.headers.get("referrer-policy").unwrap(),
        "no-referrer",
        "the link must not leak through a Referer"
    );
    let wrong: String = token.chars().rev().collect();
    assert_eq!(
        send(&h.app, get(&format!("/g/{wrong}"), None)).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&h.app, get("/g/short", None)).await.status,
        StatusCode::NOT_FOUND
    );

    // Joining the tank signs the browser in as that character; /api/play names it.
    let r = send(&h.app, form(&format!("/g/{token}/join/0"), None, &[])).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    assert_eq!(r.location(), "/");
    let player = r.cookie().expect("session cookie");
    let r = send(
        &h.app,
        Request::get("/api/play")
            .header(header::COOKIE, &player)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(v["char"].as_str(), members[0].char_name.as_deref());
    assert!(v["user"].as_str().unwrap().starts_with("WP"));
    // …and it is a player, not an admin.
    assert_eq!(
        send(&h.app, get("/admin", Some(&player))).await.status,
        StatusCode::FORBIDDEN
    );

    // Summon: one console teleport per character, by name, to the preset's entrance.
    let r = send(&h.app, form(&format!("/g/{token}/summon"), None, &[])).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let log = h.soap_log.lock().unwrap().clone();
    for m in &members {
        let want = format!("tele name {} TheDeadmines", m.char_name.as_deref().unwrap());
        assert!(log.contains(&want), "{want}: {log:?}");
    }

    let mut accts = std::collections::HashMap::new();
    for m in &members {
        let a = wenilla_realm::accounts::get(&h.state.db, m.user_id)
            .await
            .unwrap()
            .unwrap();
        accts.insert(m.user_id, a.game_username);
    }
    // Delete needs the confirmation box.
    let r = send(&h.app, form(&format!("/g/{token}/delete"), None, &[])).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(wenilla_realm::presets::by_id(&h.state.db, 1)
        .await
        .unwrap()
        .is_some());
    // A cross-site delete is refused before any handler.
    let mut req = form(&format!("/g/{token}/delete"), None, &[("confirm", "yes")]);
    req.headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    assert_eq!(send(&h.app, req).await.status, StatusCode::FORBIDDEN);

    let r = send(
        &h.app,
        form(&format!("/g/{token}/delete"), None, &[("confirm", "yes")]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(r.body.contains("Group deleted"));
    let log = h.soap_log.lock().unwrap().clone();
    for m in &members {
        let acct = &accts[&m.user_id];
        assert!(
            log.contains(&format!("account delete {acct}")),
            "{acct}: {log:?}"
        );
        assert!(log.contains(&format!("kick {}", m.char_name.as_deref().unwrap())));
    }
    let left: Vec<(i64,)> = sqlx::query_as("SELECT id FROM users WHERE username LIKE 'preset-%'")
        .fetch_all(&h.state.db)
        .await
        .unwrap();
    assert!(left.is_empty());
    // The link is dead, and so is the session it minted.
    assert_eq!(
        send(&h.app, get(&format!("/g/{token}"), None)).await.status,
        StatusCode::NOT_FOUND
    );
    let r = send(
        &h.app,
        Request::get("/api/play")
            .header(header::COOKIE, &player)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_group_cannot_be_deleted_while_it_is_being_built() {
    let h = harness().await;
    run_setup(&h).await;
    let (id, token) = wenilla_realm::presets::create(
        &h.state,
        wenilla_realm::presets::defs::get("brd").unwrap(),
        None,
    )
    .await
    .unwrap();
    wait_group(&h, id).await;
    // Pretend a build is still running.
    sqlx::query("UPDATE preset_groups SET status = 'building' WHERE id = ?")
        .bind(id)
        .execute(&h.state.db)
        .await
        .unwrap();
    let r = send(
        &h.app,
        form(&format!("/g/{token}/delete"), None, &[("confirm", "yes")]),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(r.location().contains("error="), "{}", r.location());
    assert!(wenilla_realm::presets::by_id(&h.state.db, id)
        .await
        .unwrap()
        .is_some());
    // A restart fails the stale build, and then it can go.
    wenilla_realm::presets::recover_interrupted(&h.state.db)
        .await
        .unwrap();
    let r = send(
        &h.app,
        form(&format!("/g/{token}/delete"), None, &[("confirm", "yes")]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(wenilla_realm::presets::by_id(&h.state.db, id)
        .await
        .unwrap()
        .is_none());
}
