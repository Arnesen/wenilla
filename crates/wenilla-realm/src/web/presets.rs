//! Dungeon presets on the web. `/g/{token}` is the group's page and its whole admin tool — the
//! role cards (a click signs the browser in as that character and opens the play page), who is
//! online, summon everyone back to the entrance, delete the group. No login: the token in the
//! path is the credential, so these routes sit outside the session layer and are rate-limited
//! per IP. `/admin/presets` is the operator's side: make a group, see every group, delete one.

use std::sync::Arc;

use axum::extract::{Form, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use axum_extra::extract::cookie::CookieJar;

use crate::presets::{self, defs, Group, Preset};
use crate::session::{client_ip, Session};
use crate::web::admin::{back, Flash};
use crate::{accounts, audit, csrf, realmdb, render, session, templates, AppError, AppState};

/// The group page and its three actions, reachable without a session.
pub fn public_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/g/{token}", get(group_page))
        .route("/g/{token}/join/{slot}", post(join))
        .route("/g/{token}/summon", post(summon))
        .route("/g/{token}/delete", post(delete))
}

/// Behind the admin layers.
pub fn admin_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/admin/presets", get(admin_page).post(admin_create))
        .route("/admin/presets/groups/{id}/delete", post(admin_delete))
}

/// Guessing a token is hopeless (256 bits), but every lookup is still counted.
fn limit(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let ip = client_ip(headers).unwrap_or_default();
    if state.limiter.allow(&format!("group:{ip}"), 120, 60) {
        Ok(())
    } else {
        Err(AppError::TooMany)
    }
}

async fn resolve(
    state: &AppState,
    headers: &HeaderMap,
    token: &str,
) -> Result<(Group, &'static Preset), AppError> {
    limit(state, headers)?;
    let g = presets::by_token(&state.db, token)
        .await?
        .ok_or(AppError::NotFound)?;
    let p = defs::get(&g.preset).ok_or(AppError::NotFound)?;
    Ok((g, p))
}

/// The link is the credential: keep it out of caches, referrers and search engines.
fn private(mut r: Response) -> Response {
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    r
}

async fn cards(
    state: &AppState,
    g: &Group,
    p: &Preset,
) -> Result<Vec<templates::PresetCard>, AppError> {
    let members = presets::members(&state.db, g.id).await?;
    let mut accts = Vec::new();
    for m in &members {
        accts.push(
            accounts::get(&state.db, m.user_id)
                .await?
                .map(|a| a.game_username),
        );
    }
    let names: Vec<&str> = accts.iter().flatten().map(String::as_str).collect();
    // Live level/online from the character database; the page still works without it.
    let mut live = realmdb::characters_for_accounts(&state.realmdb, &names)
        .await
        .unwrap_or_default();
    Ok(members
        .into_iter()
        .zip(accts)
        .map(|(m, acct)| {
            let slot = &p.slots[m.slot as usize];
            let ch = acct
                .and_then(|a| live.remove(&a.to_ascii_uppercase()))
                .and_then(|mut v| (!v.is_empty()).then(|| v.remove(0)));
            templates::PresetCard {
                slot: m.slot,
                label: slot.label.clone(),
                role: slot.role.clone(),
                race: realmdb::race_name(i64::from(slot.race)),
                char_name: m.char_name,
                status: m.status,
                detail: m.detail,
                level: ch.as_ref().map_or(i64::from(p.level), |c| c.level),
                online: ch.as_ref().is_some_and(|c| c.online != 0),
            }
        })
        .collect())
}

async fn group_page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Query(flash): Query<Flash>,
) -> Result<Response, AppError> {
    let (g, p) = resolve(&state, &headers, &token).await?;
    let cards = cards(&state, &g, p).await?;
    let (total, ready, in_game) = (
        cards.len(),
        cards.iter().filter(|c| c.status == "ready").count(),
        cards.iter().filter(|c| c.online).count(),
    );
    let mut sections: Vec<(String, Vec<templates::PresetCard>)> = Vec::new();
    for role in ["Tank", "Healer", "Damage"] {
        sections.push((role.to_string(), Vec::new()));
    }
    for c in cards {
        match sections.iter_mut().find(|(r, _)| *r == c.role) {
            Some((_, v)) => v.push(c),
            None => sections.push((c.role.clone(), vec![c])),
        }
    }
    sections.retain(|(_, v)| !v.is_empty());
    Ok(private(render(templates::PresetGroup {
        realm_name: state.realm_name().await,
        preset: p,
        token,
        building: g.status == "building",
        status: g.status,
        raid: total > 5,
        sections,
        total,
        ready,
        in_game,
        notice: flash.notice,
        error: flash.error,
    })))
}

async fn join(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    Path((token, slot)): Path<(String, i64)>,
) -> Result<Response, AppError> {
    let (g, _) = resolve(&state, &headers, &token).await?;
    let m = presets::members(&state.db, g.id)
        .await?
        .into_iter()
        .find(|m| m.slot == slot)
        .ok_or(AppError::NotFound)?;
    if m.status != "ready" {
        return Ok(back(
            &format!("/g/{token}"),
            Err("that character is not ready yet".into()),
        ));
    }
    let ip = client_ip(&headers);
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    // Whoever this browser was signed in as, it is now this character: the play page, the
    // relay and every lock see an ordinary player session.
    if let Some(old) = jar.get(session::COOKIE) {
        let _ = session::delete_by_token(&state.db, old.value()).await;
    }
    let cookie = session::create(&state.db, m.user_id, ip.as_deref(), ua).await?;
    audit::log(
        &state.db,
        Some(m.user_id),
        ip.as_deref(),
        "preset.join",
        m.char_name.as_deref(),
        Some(&format!("group {}", g.id)),
    )
    .await;
    let jar = jar.add(session::cookie(cookie, !state.cfg.cookie_insecure));
    Ok(private((jar, Redirect::to("/")).into_response()))
}

async fn summon(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let (g, p) = resolve(&state, &headers, &token).await?;
    let result = presets::summon(&state, &g)
        .await
        .map(|n| {
            format!(
                "{n} character{} sent to {}",
                if n == 1 { "" } else { "s" },
                p.name
            )
        })
        .map_err(|e| format!("{e:#}"));
    audit::log(
        &state.db,
        None,
        client_ip(&headers).as_deref(),
        "preset.summon",
        Some(&format!("group {}", g.id)),
        result.as_ref().err().map(String::as_str),
    )
    .await;
    Ok(back(&format!("/g/{token}"), result))
}

#[derive(serde::Deserialize)]
pub struct DeleteForm {
    #[serde(default)]
    confirm: String,
}

async fn delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Form(f): Form<DeleteForm>,
) -> Result<Response, AppError> {
    let (g, p) = resolve(&state, &headers, &token).await?;
    if f.confirm != "yes" {
        return Ok(back(
            &format!("/g/{token}"),
            Err("tick the box to confirm — deleting cannot be undone".into()),
        ));
    }
    let result = presets::delete(&state, &g).await;
    audit::log(
        &state.db,
        None,
        client_ip(&headers).as_deref(),
        "preset.delete",
        Some(&format!("group {}", g.id)),
        result.as_ref().err().map(|e| e.to_string()).as_deref(),
    )
    .await;
    match result {
        Ok(()) => Ok(private(render(templates::PresetDeleted {
            realm_name: state.realm_name().await,
            preset_name: p.name.clone(),
        }))),
        Err(e) => Ok(back(&format!("/g/{token}"), Err(e.to_string()))),
    }
}

async fn admin_page(
    session: Session,
    State(state): State<Arc<AppState>>,
    Query(flash): Query<Flash>,
) -> Result<Response, AppError> {
    let mut groups = Vec::new();
    for g in presets::list(&state.db).await? {
        let Some(p) = defs::get(&g.preset) else {
            continue;
        };
        let members = presets::members(&state.db, g.id).await?;
        groups.push(templates::PresetGroupRow {
            id: g.id,
            preset_name: p.name.clone(),
            link: format!(
                "{}/g/{}",
                state.cfg.public_url,
                presets::link_token(&state, &g)?
            ),
            created_at: g.created_at,
            characters: members
                .iter()
                .map(|m| {
                    let slot = &p.slots[m.slot as usize];
                    match &m.char_name {
                        Some(n) => format!("{n} ({})", slot.label),
                        None => format!("{} — {}", slot.label, m.status),
                    }
                })
                .collect(),
            status: g.status,
        });
    }
    Ok(render(templates::AdminPresets {
        realm_name: state.realm_name().await,
        nav: "presets",
        csrf: session.csrf_token.clone(),
        me: session.user,
        presets: defs::all(),
        groups,
        notice: flash.notice,
        error: flash.error,
    }))
}

#[derive(serde::Deserialize)]
pub struct CreateForm {
    _csrf: String,
    preset: String,
}

async fn admin_create(
    session: Session,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<CreateForm>,
) -> Result<Response, AppError> {
    csrf::verify(&session, &f._csrf)?;
    let p = defs::get(&f.preset).ok_or(AppError::BadRequest("unknown preset".into()))?;
    let (id, _) = presets::create(&state, p, Some(session.user.id)).await?;
    audit::log(
        &state.db,
        Some(session.user.id),
        client_ip(&headers).as_deref(),
        "preset.create",
        Some(&format!("group {id}")),
        Some(&p.id),
    )
    .await;
    Ok(back(
        "/admin/presets",
        Ok(format!(
            "{} group created — its characters are being built (about a minute); share its link below",
            p.name
        )),
    ))
}

#[derive(serde::Deserialize)]
pub struct AdminDeleteForm {
    _csrf: String,
}

async fn admin_delete(
    session: Session,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<AdminDeleteForm>,
) -> Result<Response, AppError> {
    csrf::verify(&session, &f._csrf)?;
    let g = presets::by_id(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let result = presets::delete(&state, &g).await.map_err(|e| e.to_string());
    audit::log(
        &state.db,
        Some(session.user.id),
        client_ip(&headers).as_deref(),
        "preset.delete",
        Some(&format!("group {id}")),
        result.as_ref().err().map(String::as_str),
    )
    .await;
    Ok(back(
        "/admin/presets",
        result.map(|()| "group deleted: its accounts and characters are gone".into()),
    ))
}
