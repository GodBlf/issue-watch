use axum::{extract::State, response::Html, routing::get, Json, Router};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::Instant,
};

#[derive(Clone)]
pub struct Health(Arc<Inner>);
struct Inner {
    started_at: DateTime<Utc>,
    started: Instant,
    components: RwLock<BTreeMap<String, Component>>,
    secrets: RwLock<Vec<String>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Normal,
    Warning,
    Error,
    Unknown,
}
#[derive(Clone, Serialize)]
pub struct Component {
    pub status: HealthStatus,
    pub details: Value,
}
#[derive(Serialize)]
pub struct Snapshot {
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub uptime_seconds: u64,
    pub status: HealthStatus,
    pub components: BTreeMap<String, Component>,
}
impl Health {
    pub fn new(secrets: Vec<String>) -> Self {
        Self(Arc::new(Inner {
            started_at: Utc::now(),
            started: Instant::now(),
            components: RwLock::new(BTreeMap::new()),
            secrets: RwLock::new(secrets),
        }))
    }
    /// Publish an already computed component observation. This never advances business progress.
    pub fn observe(&self, name: &str, status: HealthStatus, mut details: Value) {
        let secrets = self.0.secrets.read().unwrap();
        sanitize(&mut details, &secrets);
        self.0
            .components
            .write()
            .unwrap()
            .insert(name.to_owned(), Component { status, details });
    }
    /// Register credentials acquired after startup before publishing their related errors.
    pub fn add_secret(&self, secret: String) {
        if !secret.is_empty() {
            self.0.secrets.write().unwrap().push(secret);
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        let components = self.0.components.read().unwrap().clone();
        let status = components
            .get("overall")
            .map(|component| component.status)
            .unwrap_or(HealthStatus::Unknown);
        Snapshot {
            started_at: self.0.started_at,
            updated_at: Utc::now(),
            uptime_seconds: self.0.started.elapsed().as_secs(),
            status,
            components,
        }
    }
}
pub async fn serve(listener: tokio::net::TcpListener, health: Health) -> std::io::Result<()> {
    let app = Router::new()
        .route("/", get(|| async { Html(include_str!("health.html")) }))
        .route(
            "/api/status",
            get(|State(health): State<Health>| async move {
                (
                    [(axum::http::header::CACHE_CONTROL, "no-store")],
                    Json(health.snapshot()),
                )
            }),
        )
        .with_state(health);
    axum::serve(listener, app).await
}

fn credential_marker(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "authorization",
        "token",
        "secret",
        "bearer ",
        "qqbot ",
        "password",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}
fn sanitize(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => {
            for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
                *text = text.replace(secret, "[已隐藏]");
            }
            if credential_marker(text) {
                *text = "错误细节已隐藏（包含凭据信息）".into();
            }
        }
        Value::Array(values) => values.iter_mut().for_each(|value| sanitize(value, secrets)),
        Value::Object(values) => {
            values.retain(|key, _| !credential_marker(key));
            for value in values.values_mut() {
                sanitize(value, secrets);
            }
        }
        _ => {}
    }
}
