//! Single-administrator authentication at the configured external origin.
mod rate_limit;
mod storage;
use anyhow::{ensure, Context, Result};
use axum::{
    extract::{ConnectInfo, DefaultBodyLimit, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
const COOKIE: &str = "__Host-issue_watch_session";
const LIFETIME: i64 = 365 * 24 * 60 * 60;

#[derive(Clone)]
pub struct Auth {
    path: Arc<PathBuf>,
    origin: Arc<String>,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    trusted_proxy: Option<IpAddr>,
    attempts: Arc<Mutex<rate_limit::Attempts>>,
    workers: Arc<tokio::sync::Semaphore>,
}
impl Auth {
    /// Opens persistent state. HTTP is allowed only for local development.
    pub fn open(path: impl AsRef<Path>, origin: &str) -> Result<Self> {
        let url = url::Url::parse(origin).context("invalid admin origin")?;
        let loopback = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(name)) => name == "localhost",
            None => false,
        };
        ensure!(
            url.scheme() == "https" || (url.scheme() == "http" && loopback),
            "admin origin requires HTTPS"
        );
        ensure!(
            url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
                && url.username().is_empty()
                && url.password().is_none(),
            "admin origin must contain only scheme and authority"
        );
        let auth = Self {
            path: Arc::new(path.as_ref().to_owned()),
            origin: Arc::new(url.origin().ascii_serialization()),
            clock: Arc::new(|| chrono::Utc::now().timestamp()),
            trusted_proxy: None,
            attempts: Arc::new(Mutex::new(rate_limit::Attempts::default())),
            workers: Arc::new(tokio::sync::Semaphore::new(2)),
        };
        auth.connection()?.execute_batch("CREATE TABLE IF NOT EXISTS admin_credentials (id INTEGER PRIMARY KEY CHECK(id=1), password_hash TEXT NOT NULL); CREATE TABLE IF NOT EXISTS admin_sessions (digest TEXT PRIMARY KEY, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);")?;
        Ok(auth)
    }
    /// Overrides wall time for deterministic HTTP expiry tests.
    pub fn with_clock(mut self, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }
    /// Trusts X-Real-IP only from this exact socket peer. Proxies must overwrite that header.
    pub fn with_trusted_proxy(mut self, proxy: IpAddr) -> Self {
        self.trusted_proxy = Some(proxy);
        self
    }
    pub(crate) fn verify_write(&self, headers: &HeaderMap) -> bool {
        headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) == Some(self.origin.as_str())
            && headers.get("x-issue-watch-admin").is_some_and(|h| h == "1")
            && headers
                .get("sec-fetch-site")
                .is_none_or(|h| h == "same-origin")
    }
}
pub(crate) fn protect(router: Router, auth: Auth) -> Router {
    router
        .merge(
            Router::new()
                .route("/api/auth/logout", post(logout))
                .route(
                    "/api/auth/session",
                    get(|| async { Json(serde_json::json!({"authenticated":true})) }),
                )
                .with_state(auth.clone()),
        )
        .layer(axum::middleware::from_fn_with_state(
            auth.clone(),
            require_login,
        ))
        .merge(
            Router::new()
                .route("/login", get(|| async { Html(include_str!("login.html")) }))
                .route(
                    "/login.js",
                    get(|| async {
                        (
                            [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                            include_str!("login.js"),
                        )
                    }),
                )
                .route("/api/auth/login", post(login))
                .layer(DefaultBodyLimit::max(4096))
                .with_state(auth),
        )
        .layer(axum::middleware::map_response(
            |mut response: Response| async move {
                let h = response.headers_mut();
                h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                h.insert(
                    header::X_CONTENT_TYPE_OPTIONS,
                    HeaderValue::from_static("nosniff"),
                );
                h.insert(
                    header::REFERRER_POLICY,
                    HeaderValue::from_static("no-referrer"),
                );
                h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
                response
            },
        ))
}
async fn logout(State(auth): State<Auth>, headers: HeaderMap) -> Response {
    let token = session(&headers);
    match tokio::task::spawn_blocking(move || match token {
        Some(token) => auth.revoke(&token),
        None => Ok(()),
    })
    .await
    {
        Ok(Ok(())) => (
            [(
                header::SET_COOKIE,
                format!("{COOKIE}=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Strict"),
            )],
            Json(serde_json::json!({"authenticated":false})),
        )
            .into_response(),
        Ok(Err(e)) => failed(e),
        Err(e) => failed(e.into()),
    }
}
fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"error":message}))).into_response()
}
fn failed(cause: anyhow::Error) -> Response {
    tracing::warn!(error=%cause, "admin authentication unavailable");
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "认证服务暂不可用，请通过 SSH 检查服务",
    )
}
fn session(headers: &HeaderMap) -> Option<String> {
    let mut found = None;
    for value in headers.get_all(header::COOKIE) {
        for part in value.to_str().ok()?.split(';') {
            if let Some((name, token)) = part.trim().split_once('=') {
                if name == COOKIE {
                    if found.is_some()
                        || token.len() != 64
                        || !token.bytes().all(|b| b.is_ascii_hexdigit())
                    {
                        return None;
                    }
                    found = Some(token.to_owned());
                }
            }
        }
    }
    found
}
async fn require_login(State(auth): State<Auth>, request: Request, next: Next) -> Response {
    let token = session(request.headers());
    let check = auth.clone();
    match tokio::task::spawn_blocking(move || match token {
        Some(token) => check.valid_session(&token),
        None => Ok(false),
    })
    .await
    {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            return if request.uri().path().starts_with("/api/") {
                error(
                    StatusCode::UNAUTHORIZED,
                    "登录已失效，请重新登录；未确认结果的操作请刷新核实",
                )
            } else {
                Redirect::to("/login").into_response()
            }
        }
        Ok(Err(e)) => return failed(e),
        Err(e) => return failed(e.into()),
    }
    if !matches!(*request.method(), Method::GET | Method::HEAD)
        && !auth.verify_write(request.headers())
    {
        return error(StatusCode::FORBIDDEN, "请求来源不受信任，请从管理页面操作");
    }
    next.run(request).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    password: String,
}
async fn login(
    State(auth): State<Auth>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Login>,
) -> Response {
    if !auth.verify_write(&headers) {
        return error(StatusCode::FORBIDDEN, "请求来源不受信任");
    }
    if body.password.len() > 1024 {
        return error(StatusCode::BAD_REQUEST, "密码过长");
    }
    let source = if auth.trusted_proxy == Some(peer.ip()) {
        match headers
            .get("x-real-ip")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<IpAddr>().ok())
        {
            Some(ip) => ip,
            None => return error(StatusCode::BAD_REQUEST, "代理来源地址无效"),
        }
    } else {
        peer.ip()
    };
    let allowed = match auth.attempts.lock() {
        Ok(mut attempts) => attempts.admit(source, (auth.clock)()),
        Err(_) => return failed(anyhow::anyhow!("login limiter unavailable")),
    };
    if !allowed {
        return limited();
    }
    let permit = match auth.workers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return limited(),
    };
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        auth.login(&body.password)
    })
    .await
    {
        Ok(Ok(Some(token))) => {
            let value = format!(
                "{COOKIE}={token}; Path=/; Max-Age={LIFETIME}; Secure; HttpOnly; SameSite=Strict"
            );
            match HeaderValue::from_str(&value) {
                Ok(value) => (
                    [(header::SET_COOKIE, value)],
                    Json(serde_json::json!({"authenticated":true})),
                )
                    .into_response(),
                Err(e) => failed(e.into()),
            }
        }
        Ok(Ok(None)) => error(StatusCode::UNAUTHORIZED, "密码错误或管理员尚未初始化"),
        Ok(Err(e)) => failed(e),
        Err(e) => failed(e.into()),
    }
}
fn limited() -> Response {
    let mut response = error(
        StatusCode::TOO_MANY_REQUESTS,
        "尝试过于频繁，请在一分钟后重试",
    );
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    response
}
