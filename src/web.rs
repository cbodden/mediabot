//! Local control panel (HTTP or HTTPS) + JSON API + Prometheus `/metrics`.

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

use crate::{
    acl::{Role, Sender, irc_lower},
    app::App,
    commands::{self, Ctx},
    config::ChannelConfig,
    irc::IrcCmd,
};

const INDEX: &str = include_str!("../web/index.html");

type AppState = Arc<App>;

#[derive(Debug)]
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::BAD_REQUEST, format!("{e:#}"))
    }
}

type ApiResult<T = Json<Value>> = Result<T, ApiError>;

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn authorized(app: &App, headers: &HeaderMap) -> bool {
    let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(b64) = v.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) else {
        return false;
    };
    let expected = format!(
        "{}:{}",
        app.config.web.username,
        app.config.web.password.as_deref().unwrap_or("")
    );
    ct_eq(&raw, expected.as_bytes())
}

async fn auth(State(app): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let public = path == "/healthz" || (path == "/metrics" && !app.config.web.metrics_auth);
    if !public && !authorized(&app, req.headers()) {
        let mut r = (StatusCode::UNAUTHORIZED, "authentication required").into_response();
        r.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Basic realm="mediabot", charset="UTF-8""#),
        );
        return r;
    }
    // CSRF: browsers attach cached Basic credentials to cross-site requests, so every
    // state-changing call must carry a custom header (which forces a CORS preflight we never allow).
    if req.method() != Method::GET
        && req.method() != Method::HEAD
        && req.headers().get("x-mediabot").is_none()
    {
        return (StatusCode::FORBIDDEN, "missing X-Mediabot header").into_response();
    }
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    h.insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("Referrer-Policy", HeaderValue::from_static("no-referrer"));
    h.insert(
        "Content-Security-Policy",
        HeaderValue::from_static("default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:"),
    );
    resp
}

pub fn router(app: AppState) -> Router {
    Router::new()
        .route("/", get(|| async { Html(INDEX) }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/metrics", get(metrics))
        .route("/api/status", get(status))
        .route("/api/services", get(services))
        .route("/api/services/{name}/test", post(test_service))
        .route("/api/users", get(list_users).post(set_user))
        .route("/api/users/{id}", delete(del_user))
        .route("/api/channels", get(channels).post(join_channel))
        .route("/api/channels/part", post(part_channel))
        .route("/api/say", post(say))
        .route("/api/command", post(command))
        .route("/api/reconnect", post(reconnect))
        .route("/api/logs", get(logs))
        .layer(middleware::from_fn_with_state(app.clone(), auth))
        .with_state(app)
}

pub async fn serve(app: AppState) -> Result<()> {
    let cfg = app.config.web.clone();
    let router = router(app);
    match (&cfg.tls_cert, &cfg.tls_key) {
        (Some(cert), Some(key)) => {
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
                .await
                .context("loading web TLS cert/key")?;
            info!("web panel listening on https://{}", cfg.listen);
            axum_server::bind_rustls(cfg.listen, tls)
                .serve(router.into_make_service())
                .await?;
        }
        _ => {
            let l = tokio::net::TcpListener::bind(cfg.listen)
                .await
                .with_context(|| format!("binding {}", cfg.listen))?;
            info!("web panel listening on http://{}", cfg.listen);
            axum::serve(l, router).await?;
        }
    }
    Ok(())
}

fn web_ctx(app: &App) -> Ctx {
    let user = app.config.web.username.clone();
    Ctx {
        sender: Sender {
            nick: format!("web:{user}"),
            user: user.clone(),
            host: "localhost".into(),
            account: None,
        },
        role: Role::Owner,
        actor: format!("web:{user}"),
    }
}

async fn metrics(State(app): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        app.metrics.render(),
    )
}

async fn status(State(app): State<AppState>) -> Json<Value> {
    let irc = app.irc_status.read().await.clone();
    let svc = app.service_status.read().await;
    let up = svc.values().filter(|s| s.up).count();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "started": app.started,
        "irc": irc,
        "prefix": app.config.bot.prefix,
        "services": { "total": svc.len(), "up": up },
    }))
}

async fn services(State(app): State<AppState>) -> Json<Value> {
    let st = app.service_status.read().await;
    let ordered: Vec<_> = app
        .services
        .iter()
        .filter_map(|s| st.get(&s.name))
        .collect();
    Json(json!(ordered))
}

async fn test_service(State(app): State<AppState>, Path(name): Path<String>) -> ApiResult {
    let svc = app
        .services
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no service {name}")))?;
    let started = std::time::Instant::now();
    let v = svc.version().await?;
    Ok(Json(
        json!({ "ok": true, "version": v, "latency_ms": started.elapsed().as_millis() as u64 }),
    ))
}

async fn list_users(State(app): State<AppState>) -> Json<Value> {
    Json(json!({ "owners": app.store.owners(), "users": app.store.users().await }))
}

#[derive(Deserialize)]
struct SetUser {
    id: String,
    role: String,
}

async fn set_user(State(app): State<AppState>, Json(b): Json<SetUser>) -> ApiResult {
    let role = Role::parse(&b.role).ok_or_else(|| {
        ApiError(
            StatusCode::BAD_REQUEST,
            "role must be readonly or admin".into(),
        )
    })?;
    let ctx = web_ctx(&app);
    let created = app
        .store
        .set_user(&b.id, role, &ctx.actor, Role::Owner)
        .await?;
    app.refresh_user_metrics().await;
    info!(target: "audit", actor = %ctx.actor, user = %b.id, role = role.as_str(), "acl user set");
    Ok(Json(json!({ "ok": true, "created": created })))
}

async fn del_user(State(app): State<AppState>, Path(id): Path<String>) -> ApiResult {
    app.store.remove_user(&id, Role::Owner).await?;
    app.refresh_user_metrics().await;
    info!(target: "audit", actor = %web_ctx(&app).actor, user = %id, "acl user removed");
    Ok(Json(json!({ "ok": true })))
}

async fn channels(State(app): State<AppState>) -> Json<Value> {
    let joined = app.irc_status.read().await.channels.clone();
    let wanted = app.store.channels(&app.config.irc.channels).await;
    let list: Vec<Value> = wanted
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "default_role": c.default_role,
                "has_key": c.key.is_some(),
                "joined": joined.iter().any(|j| irc_lower(j) == irc_lower(&c.name)),
            })
        })
        .collect();
    Json(json!(list))
}

async fn join_channel(State(app): State<AppState>, Json(ch): Json<ChannelConfig>) -> ApiResult {
    if !ch.name.starts_with('#') || ch.name.contains([' ', ',', '\r', '\n']) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid channel name".into(),
        ));
    }
    commands::upsert_channel(&app, ch.clone()).await?;
    info!(target: "audit", actor = %web_ctx(&app).actor, channel = %ch.name, "join");
    app.irc.send(IrcCmd::Join(ch));
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct Name {
    name: String,
}

async fn part_channel(State(app): State<AppState>, Json(b): Json<Name>) -> ApiResult {
    commands::remove_channel(&app, &b.name).await?;
    info!(target: "audit", actor = %web_ctx(&app).actor, channel = %b.name, "part");
    app.irc.send(IrcCmd::Part(b.name));
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct Say {
    target: String,
    text: String,
}

async fn say(State(app): State<AppState>, Json(b): Json<Say>) -> ApiResult {
    if b.target.is_empty() || b.text.trim().is_empty() {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "target and text are required".into(),
        ));
    }
    info!(target: "audit", actor = %web_ctx(&app).actor, to = %b.target, "say");
    app.irc.say(&b.target, b.text);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct Command {
    command: String,
}

/// Run a bot command as an owner and return the output (IRC formatting stripped).
async fn command(State(app): State<AppState>, Json(b): Json<Command>) -> ApiResult {
    let line = b
        .command
        .trim()
        .trim_start_matches(app.config.bot.prefix.as_str());
    let out = commands::execute(&app, &web_ctx(&app), line).await;
    let clean: Vec<String> = out
        .iter()
        .map(|l| l.replace(['\x02', '\x1d', '\x1f', '\x0f'], ""))
        .collect();
    Ok(Json(json!({ "lines": clean })))
}

async fn reconnect(State(app): State<AppState>) -> Json<Value> {
    info!(target: "audit", actor = %web_ctx(&app).actor, "irc reconnect requested");
    app.irc.send(IrcCmd::Reconnect);
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default)]
    since: u64,
}

async fn logs(State(app): State<AppState>, Query(q): Query<LogQuery>) -> Json<Value> {
    Json(json!(app.logs.since(q.since)))
}
