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
        candidate.repositories = store.reconcile_repositories(&candidate.repositories, now)?;
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

pub enum MonitoringEvent {
    Configuration(FileConfig),
    Poll,
    Tracking,
}
pub struct MonitoringSchedule {
    config: FileConfig,
    poll: tokio::time::Interval,
    tracking: tokio::time::Interval,
}
impl MonitoringSchedule {
    pub fn new(config: &FileConfig) -> Self {
        let mut poll =
            tokio::time::interval(std::time::Duration::from_secs(config.poll_interval_seconds));
        let mut tracking = tokio::time::interval(std::time::Duration::from_secs(
            config.tracking_interval_seconds,
        ));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tracking.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Self {
            config: config.clone(),
            poll,
            tracking,
        }
    }
    pub fn apply(&mut self, config: &FileConfig) {
        if config.poll_interval_seconds != self.config.poll_interval_seconds {
            self.poll = delayed_interval(config.poll_interval_seconds);
        }
        if config.tracking_interval_seconds != self.config.tracking_interval_seconds {
            self.tracking = delayed_interval(config.tracking_interval_seconds);
        }
        self.config = config.clone();
    }
    pub async fn next(
        &mut self,
        changes: &mut tokio::sync::watch::Receiver<FileConfig>,
    ) -> Result<MonitoringEvent> {
        tokio::select! {biased;
            result=changes.changed()=>{result?;Ok(MonitoringEvent::Configuration(changes.borrow_and_update().clone()))},
            _=self.poll.tick()=>Ok(MonitoringEvent::Poll),
            _=self.tracking.tick()=>Ok(MonitoringEvent::Tracking),
        }
    }
}
fn delayed_interval(seconds: u64) -> tokio::time::Interval {
    let duration = std::time::Duration::from_secs(seconds);
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + duration, duration);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval
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
    reload: ConfigReloader,
    store: Store,
    updates: tokio::sync::watch::Sender<FileConfig>,
    management: Option<crate::config_management::ConfigManagement>,
) {
    watch_config_observed(reload, store, updates, management, None).await
}

pub async fn watch_config_observed(
    mut reload: ConfigReloader,
    store: Store,
    updates: tokio::sync::watch::Sender<FileConfig>,
    management: Option<crate::config_management::ConfigManagement>,
    github: Option<std::sync::Arc<crate::github_health::GithubHealth>>,
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
                if changed {
                    if let Some(github) = &github {
                        github.sync_repositories(&reload.active().repositories);
                    }
                }
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
