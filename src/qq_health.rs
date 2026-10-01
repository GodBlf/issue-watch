use crate::{
    health::{Health, HealthStatus},
    store::Store,
};
use anyhow::Result;
use serde_json::json;

#[derive(Clone)]
pub struct QqHealth(pub Health);
impl QqHealth {
    pub fn new(health: Health) -> Self {
        health.observe(
            "qq_delivery",
            HealthStatus::Unknown,
            json!({"result":"unverified", "at":null}),
        );
        health.observe(
            "qq_gateway",
            HealthStatus::Unknown,
            json!({"connected":false, "error":null}),
        );
        health.observe(
            "qq_auth",
            HealthStatus::Unknown,
            json!({"result":"unverified", "error":null}),
        );
        Self(health)
    }
    pub fn refresh_queue(&self, store: &Store) -> Result<()> {
        let subscriber_count = store.bound_users()?.len();
        let bound = subscriber_count > 0;
        self.0.observe(
            "qq_binding",
            if bound {
                HealthStatus::Normal
            } else {
                HealthStatus::Warning
            },
            json!({"bound":bound,"subscriber_count":subscriber_count}),
        );
        let counts = store.notification_summary()?;
        let status = if counts["permanent_failed"].as_u64().unwrap_or(0) > 0 {
            HealthStatus::Error
        } else if counts["retrying"].as_u64().unwrap_or(0) > 0 {
            HealthStatus::Warning
        } else {
            HealthStatus::Normal
        };
        self.0.observe("notification_queue", status, counts);
        Ok(())
    }
    pub fn progress(&self, active: bool, stage: &str) {
        self.0.observe(
            "notification_progress",
            HealthStatus::Normal,
            json!({"active":active,"stage":stage,"last_progress_at":chrono::Utc::now()}),
        );
    }
    pub fn sent(&self, result: &str, error: Option<&str>) {
        self.0.observe(
            "qq_delivery",
            match result {
                "sent" => HealthStatus::Normal,
                "retrying" => HealthStatus::Warning,
                _ => HealthStatus::Error,
            },
            json!({"result":result,"error":error,"at":chrono::Utc::now()}),
        );
    }
    pub fn authentication(&self, known_valid: bool, error: Option<&str>) {
        self.0.observe("qq_auth",if error.is_some(){HealthStatus::Error}else if known_valid{HealthStatus::Normal}else{HealthStatus::Unknown},json!({"result":if error.is_some(){"failed"}else if known_valid{"verified"}else{"unverified"},"error":error}));
    }
    pub fn gateway(&self, connected: bool, error: Option<&str>) {
        self.0.observe(
            "qq_gateway",
            if connected {
                HealthStatus::Normal
            } else {
                HealthStatus::Warning
            },
            json!({"connected":connected,"error":error,"at":chrono::Utc::now()}),
        );
    }
}
