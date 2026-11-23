//! The local dashboard, served by the agent on 127.0.0.1 only.
//!
//! Requests must carry the dashboard token, as a cookie set by `/login?token=...` (which
//! `globalobject dashboard` opens) or as a bearer token, for scripts and Prometheus. The Host
//! header must name this machine, which stops DNS-rebinding pages from reaching it, and
//! changes need an `x-globalobject` header, which a cross-site form can't send.

use crate::{agent::Agent, config::parse_size};
use axum::{
    extract::{Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc};

const PAGE: &str = include_str!("dashboard.html");
const COOKIE: &str = "globalobject_dashboard";

type S = State<Arc<Agent>>;

pub fn router(agent: Arc<Agent>) -> Router {
    let api = Router::new()
        .route("/api/status", get(status))
        .route("/api/snapshots", get(snapshots))
        .route("/api/events", get(events))
        .route("/api/backup", post(backup))
        .route("/api/restore", post(restore))
        .route("/api/settings", post(settings))
        .route("/metrics", get(metrics))
        .route("/", get(index))
        .layer(axum::middleware::from_fn_with_state(agent.clone(), guard));
    Router::new().route("/login", get(login)).merge(api).layer(axum::middleware::from_fn_with_state(agent.clone(), local_only)).with_state(agent)
}

pub async fn serve(agent: Arc<Agent>, listener: tokio::net::TcpListener) {
    if let Err(e) = axum::serve(listener, router(agent)).await {
        tracing::error!("dashboard stopped: {e}");
    }
}

/// Only `localhost` or a loopback address, on any port, may be named in Host.
pub fn host_is_local(h: &HeaderMap) -> bool {
    let Some(host) = h.get(header::HOST).and_then(|v| v.to_str().ok()) else { return false };
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => host.rsplit_once(':').map_or(host, |(n, p)| if p.chars().all(|c| c.is_ascii_digit()) { n } else { host }),
    };
    name.eq_ignore_ascii_case("localhost") || name.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

async fn local_only(req: Request, next: Next) -> Response {
    if !host_is_local(req.headers()) {
        return (StatusCode::FORBIDDEN, "the dashboard only answers to localhost\n").into_response();
    }
    next.run(req).await
}

fn token_ok(agent: &Agent, h: &HeaderMap) -> bool {
    let want = agent.creds.dashboard_token.as_bytes();
    let cookie = h
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().strip_prefix(&format!("{COOKIE}=")).map(str::to_string))
        .collect::<Vec<_>>();
    let bearer = h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(str::to_string);
    let eq = |given: &str| globalobject_crypto::hash(given.as_bytes()) == globalobject_crypto::hash(want);
    !want.is_empty() && (cookie.iter().any(|c| eq(c)) || bearer.is_some_and(|b| eq(&b)))
}

async fn guard(State(agent): S, req: Request, next: Next) -> Response {
    if !token_ok(&agent, req.headers()) {
        if req.uri().path() == "/" {
            return Html(
                "<!doctype html><meta charset=utf-8><title>GlobalObject</title><body style='font:16px system-ui;margin:3rem'>\
                 <h1>GlobalObject</h1><p>Open the dashboard with <code>globalobject dashboard</code>, which signs you in.</p>",
            )
            .into_response();
        }
        return (StatusCode::UNAUTHORIZED, "sign in with `globalobject dashboard`\n").into_response();
    }
    if req.method() != axum::http::Method::GET && !req.headers().contains_key("x-globalobject") {
        return (StatusCode::FORBIDDEN, "missing x-globalobject header\n").into_response();
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct LoginQuery {
    token: String,
}

async fn login(State(agent): S, Query(q): Query<LoginQuery>) -> Response {
    let mut h = HeaderMap::new();
    h.insert(header::AUTHORIZATION, format!("Bearer {}", q.token).parse().unwrap_or(header::HeaderValue::from_static("")));
    if !token_ok(&agent, &h) {
        return (StatusCode::UNAUTHORIZED, "wrong token: open the dashboard with `globalobject dashboard`\n").into_response();
    }
    let cookie = format!("{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000", q.token);
    ([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response()
}

async fn index() -> Html<&'static str> {
    Html(PAGE)
}

fn err(e: anyhow::Error) -> Response {
    (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": format!("{e:#}")}))).into_response()
}

async fn status(State(agent): S) -> Response {
    let account = agent.client.status().await;
    let cfg = agent.cfg.read().unwrap().clone();
    Json(serde_json::json!({
        "agent": agent.status(),
        "account": account.as_ref().ok(),
        "account_error": account.as_ref().err().map(|e| format!("{e:#}")),
        "control_plane": cfg.control_plane,
        "paths": cfg.backup.paths,
        "every_hours": cfg.backup.every_hours,
        "keep": cfg.backup.keep,
        "slice_dir": cfg.slice.dir,
        "restore_dir": directories::UserDirs::new().map(|d| d.home_dir().join("GlobalObject restores")),
    }))
    .into_response()
}

async fn snapshots(State(agent): S) -> Response {
    match agent.client.snapshot_infos().await {
        Ok(s) => Json(s).into_response(),
        Err(e) => err(e),
    }
}

async fn events(State(agent): S) -> Json<Vec<crate::agent::Event>> {
    Json(agent.events())
}

async fn backup(State(agent): S) -> Response {
    if agent.status().running.is_some() {
        return (StatusCode::CONFLICT, Json(serde_json::json!({"error": "a job is already running"}))).into_response();
    }
    tokio::spawn(async move { agent.backup_now().await });
    (StatusCode::ACCEPTED, Json(serde_json::json!({"ok": true}))).into_response()
}

#[derive(Deserialize)]
struct RestoreReq {
    snapshot: String,
    dest: PathBuf,
}

async fn restore(State(agent): S, Json(r): Json<RestoreReq>) -> Response {
    if !r.dest.is_absolute() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "give a full path to restore into"}))).into_response();
    }
    if r.dest.exists() && std::fs::read_dir(&r.dest).map(|mut d| d.next().is_some()).unwrap_or(true) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "pick an empty or new folder, so nothing gets overwritten"}))).into_response();
    }
    tokio::spawn(async move { agent.restore(r.snapshot, r.dest).await });
    (StatusCode::ACCEPTED, Json(serde_json::json!({"ok": true}))).into_response()
}

#[derive(Deserialize)]
struct SettingsReq {
    slice: Option<String>,
    paths: Option<Vec<PathBuf>>,
    every_hours: Option<u32>,
}

async fn settings(State(agent): S, Json(r): Json<SettingsReq>) -> Response {
    let result = async {
        if let Some(paths) = &r.paths {
            for p in paths {
                anyhow::ensure!(p.is_absolute() && p.exists(), "{} isn't an existing folder (give the full path)", p.display());
            }
        }
        if let Some(s) = &r.slice {
            let n = parse_size(s)?;
            anyhow::ensure!(n >= 1_000_000_000, "share at least 1 GB");
            agent.set_slice(n).await?;
        }
        agent.update_config(|c| {
            if let Some(p) = r.paths {
                c.backup.paths = p;
            }
            if let Some(h) = r.every_hours {
                c.backup.every_hours = h;
            }
        })
    }
    .await;
    match result {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": format!("{e:#}")}))).into_response(),
    }
}

/// A few gauges in Prometheus text format, for people who monitor their machines.
async fn metrics(State(agent): S) -> Response {
    let s = agent.status();
    let last = s.last_backup.as_ref();
    let mut out = String::new();
    let mut g = |name: &str, help: &str, v: f64| {
        out.push_str(&format!("# HELP globalobject_agent_{name} {help}\n# TYPE globalobject_agent_{name} gauge\nglobalobject_agent_{name} {v}\n"));
    };
    g("slice_size_bytes", "Disk offered to the pool", s.slice_size as f64);
    g("slice_used_bytes", "Disk used by other members' pieces", s.slice_used as f64);
    g("last_backup_timestamp_seconds", "When the last backup finished", last.and_then(|r| r.finished).unwrap_or(0.0));
    g("last_backup_success", "1 if the last backup succeeded", if last.and_then(|r| r.ok) == Some(true) { 1.0 } else { 0.0 });
    g("job_running", "1 while a backup or restore runs", if s.running.is_some() { 1.0 } else { 0.0 });
    if let Ok(a) = agent.client.status().await {
        g("quota_bytes", "Backup space this account may use, before redundancy", a.quota_raw as f64);
        g("used_bytes", "Backup space used, including redundancy", a.used_raw as f64);
    }
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], out).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, v.parse().unwrap());
        h
    }

    #[test]
    fn hosts() {
        for ok in ["127.0.0.1:7410", "localhost:7410", "LOCALHOST", "[::1]:7410", "127.0.0.1"] {
            assert!(host_is_local(&host(ok)), "{ok}");
        }
        for bad in ["evil.example:7410", "127.0.0.1.evil.example", "localhost.evil.example:80", "192.168.1.5:7410", "[2001:db8::1]:7410"] {
            assert!(!host_is_local(&host(bad)), "{bad}");
        }
        assert!(!host_is_local(&HeaderMap::new()));
    }
}
