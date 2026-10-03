//! Validated file reloads preserve the last good configuration on any error.
use crate::{config::FileConfig, Store};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

pub struct ConfigReloader {
    path: PathBuf,
    active: FileConfig,
}

impl ConfigReloader {
    pub fn new(path: impl AsRef<Path>, active: FileConfig) -> Self {
        Self {
            path: path.as_ref().into(),
            active,
        }
    }

    pub fn active(&self) -> &FileConfig {
        &self.active
    }

    pub fn check(&mut self, store: &Store, now: DateTime<Utc>) -> Result<bool> {
        let mut candidate = FileConfig::load(&self.path)?;
        anyhow::ensure!(
            candidate.database_path == self.active.database_path,
            "database_path changes require a restart; keeping previous configuration"
        );
        if candidate == self.active {
            return Ok(false);
        }
        candidate.repositories = store.ensure_repositories(&candidate.repositories, now)?;
        if candidate == self.active {
            return Ok(false);
        }
        self.active = candidate;
        Ok(true)
    }
}

/// Apply pending updates before starting another poll when both are ready.
pub async fn next_monitoring_event(
    interval: &mut tokio::time::Interval,
    changes: &mut tokio::sync::watch::Receiver<FileConfig>,
) -> Result<Option<FileConfig>> {
    tokio::select! {
        biased;
        result = changes.changed() => {
            result?;
            Ok(Some(changes.borrow_and_update().clone()))
        }
        _ = interval.tick() => Ok(None),
    }
}

/// Watches file contents rather than metadata so atomic editor saves are detected.
pub async fn watch_config(
    reload: ConfigReloader,
    store: Store,
    updates: tokio::sync::watch::Sender<FileConfig>,
) {
    watch_config_managed(reload, store, updates, None).await
}

pub async fn watch_config_managed(
    mut reload: ConfigReloader,
    store: Store,
    updates: tokio::sync::watch::Sender<FileConfig>,
    management: Option<crate::config_management::ConfigManagement>,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut last_error = None;
    loop {
        interval.tick().await;
        if updates.is_closed() {
            break;
        }
        match reload.check(&store, Utc::now()) {
            Ok(changed) => {
                if let Some(management) = &management {
                    management.accepted(reload.active());
                }
                last_error = None;
                if changed {
                    tracing::info!(
                        repositories = reload.active().repositories.len(),
                        poll_interval_seconds = reload.active().poll_interval_seconds,
                        "Configuration reloaded"
                    );
                    if updates.send(reload.active().clone()).is_err() {
                        break;
                    }
                }
            }
            Err(error) => {
                if let Some(management) = &management {
                    management.rejected();
                }
                let message = format!("{error:#}");
                if last_error.as_ref() != Some(&message) {
                    tracing::warn!(%message, "Configuration reload rejected; keeping previous settings");
                    last_error = Some(message);
                }
            }
        }
    }
}
