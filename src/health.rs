use axum::{extract::State, response::Html, routing::get, Json, Router};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
    time::Instant,
};

#[derive(Clone)]
pub struct Health(Arc<Inner>);
struct Inner {
    started_at: DateTime<Utc>,
    started: Instant,
    components: RwLock<BTreeMap<String, Component>>,
    secrets: RwLock<Vec<String>>,
    poll_interval: AtomicU64,
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
            poll_interval: AtomicU64::new(60),
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
        self.snapshot_at(Utc::now())
    }
    pub fn set_poll_interval(&self, seconds: u64) {
        self.0.poll_interval.store(seconds, Ordering::Relaxed);
    }
    /// Evaluate observations at the external clock boundary without advancing business work.
    pub fn snapshot_at(&self, now: DateTime<Utc>) -> Snapshot {
        let mut components = self.0.components.read().unwrap().clone();
        let threshold = self
            .0
            .poll_interval
            .load(Ordering::Relaxed)
            .saturating_mul(3)
            .max(300);
        let mut active = false;
        let mut stalled = false;
        let mut last_progress = None;
        let mut stage = "waiting";
        for name in ["github_progress", "notification_progress"] {
            if let Some(component) = components.get(name) {
                let details = &component.details;
                if let Some(time) = details["last_progress_at"]
                    .as_str()
                    .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
                    .map(|v| v.with_timezone(&Utc))
                {
                    last_progress = Some(
                        last_progress.map_or(time, |previous: DateTime<Utc>| previous.max(time)),
                    );
                    if details["active"].as_bool() == Some(true) {
                        active = true;
                        stage = details["stage"].as_str().unwrap_or("working");
                        stalled |= now.signed_duration_since(time).num_seconds()
                            > threshold.min(i64::MAX as u64) as i64;
                    }
                }
            }
        }
        let progress = Component {
            status: if stalled {
                HealthStatus::Error
            } else {
                HealthStatus::Normal
            },
            details: serde_json::json!({"active":active,"stage":stage,"last_progress_at":last_progress,"threshold_seconds":threshold,"stalled":stalled}),
        };
        components.insert("progress".into(), progress);
        components.remove("overall");
        let required = [
            "github",
            "qq_gateway",
            "qq_binding",
            "qq_auth",
            "notification_queue",
        ];
        let missing = required.iter().any(|name| !components.contains_key(*name));
        let meaningful: Vec<_> = components
            .iter()
            .filter(|(name, component)| {
                !(name.as_str() == "qq_delivery" && component.details["result"] == "unverified")
            })
            .collect();
        let status = if meaningful
            .iter()
            .any(|(_, c)| c.status == HealthStatus::Error)
        {
            HealthStatus::Error
        } else if meaningful
            .iter()
            .any(|(_, c)| c.status == HealthStatus::Warning)
        {
            HealthStatus::Warning
        } else if missing
            || meaningful
                .iter()
                .any(|(_, c)| c.status == HealthStatus::Unknown)
        {
            HealthStatus::Unknown
        } else {
            HealthStatus::Normal
        };
        let reasons: Vec<_> = meaningful
            .iter()
            .filter(|(_, c)| c.status != HealthStatus::Normal)
            .map(|(name, c)| serde_json::json!({"component":name,"status":c.status}))
            .collect();
        components.insert(
            "overall".into(),
            Component {
                status,
                details: serde_json::json!({"reasons":reasons,"missing_observations":missing}),
            },
        );
        Snapshot {
            started_at: self.0.started_at,
            updated_at: now,
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
