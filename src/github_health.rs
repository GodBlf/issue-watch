//! In-memory observations of completed checks, independent of Issue cursors.
use crate::{
    github::{discover_repository, GithubIssue, IssueSource},
    health::{Health, HealthStatus},
    Store,
};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use std::sync::Mutex;

pub struct GithubHealth {
    health: Health,
    repositories: Mutex<Vec<RepositoryHealth>>,
}
#[derive(Clone, Serialize)]
struct RepositoryHealth {
    repository: String,
    status: HealthStatus,
    last_attempt_at: Option<DateTime<Utc>>,
    last_success_at: Option<DateTime<Utc>>,
    consecutive_failures: u64,
    error: Option<String>,
}
impl RepositoryHealth {
    fn new(repository: String) -> Self {
        Self {
            repository,
            status: HealthStatus::Unknown,
            last_attempt_at: None,
            last_success_at: None,
            consecutive_failures: 0,
            error: None,
        }
    }
}
impl GithubHealth {
    pub fn new(health: Health, repositories: &[String]) -> Self {
        let monitor = Self {
            health,
            repositories: Mutex::new(
                repositories
                    .iter()
                    .cloned()
                    .map(RepositoryHealth::new)
                    .collect(),
            ),
        };
        monitor.publish();
        monitor
    }
    fn publish(&self) {
        let repositories = self.repositories.lock().unwrap();
        let status = if repositories.iter().any(|r| r.status == HealthStatus::Error) {
            HealthStatus::Error
        } else if repositories
            .iter()
            .any(|r| r.status == HealthStatus::Warning)
        {
            HealthStatus::Warning
        } else if repositories.is_empty()
            || repositories
                .iter()
                .any(|r| r.status == HealthStatus::Unknown)
        {
            HealthStatus::Unknown
        } else {
            HealthStatus::Normal
        };
        self.health
            .observe("github", status, json!({"repositories": *repositories}));
    }
    /// Apply only the configuration that the business loop has accepted.
    pub fn sync_repositories(&self, active: &[String]) {
        {
            let mut repositories = self.repositories.lock().unwrap();
            *repositories = active
                .iter()
                .map(|name| {
                    repositories
                        .iter()
                        .find(|row| row.repository.eq_ignore_ascii_case(name))
                        .cloned()
                        .unwrap_or_else(|| RepositoryHealth::new(name.clone()))
                })
                .collect();
        }
        self.publish();
    }
    pub async fn poll<S: IssueSource>(
        &self,
        source: &S,
        store: &Store,
        repository: &str,
    ) -> Result<usize> {
        self.poll_with_clock(source, store, repository, Utc::now)
            .await
    }
    /// The clock is an external time seam; attempts and successful completions use wall time.
    pub async fn poll_with_clock<S: IssueSource, C: Fn() -> DateTime<Utc> + Send + Sync>(
        &self,
        source: &S,
        store: &Store,
        repository: &str,
        clock: C,
    ) -> Result<usize> {
        let attempt = clock();
        self.progress(true, repository, "checking_repository", attempt);
        {
            let mut repositories = self.repositories.lock().unwrap();
            if let Some(row) = repositories
                .iter_mut()
                .find(|r| r.repository.eq_ignore_ascii_case(repository))
            {
                row.last_attempt_at = Some(attempt);
            }
        }
        self.publish();
        let observed = ObservedSource {
            source,
            monitor: self,
            clock: &clock,
        };
        let result = discover_repository(&observed, store, repository, attempt).await;
        {
            let mut repositories = self.repositories.lock().unwrap();
            if let Some(row) = repositories
                .iter_mut()
                .find(|r| r.repository.eq_ignore_ascii_case(repository))
            {
                if result.is_ok() {
                    row.status = HealthStatus::Normal;
                    row.last_success_at = Some(clock());
                    row.consecutive_failures = 0;
                    row.error = None;
                } else if let Err(error) = &result {
                    row.consecutive_failures = row.consecutive_failures.saturating_add(1);
                    row.status = if row.consecutive_failures >= 3 {
                        HealthStatus::Error
                    } else {
                        HealthStatus::Warning
                    };
                    row.error = Some(format!("{error:#}"));
                }
            }
        }
        self.publish();
        self.progress(false, repository, "repository_complete", clock());
        result
    }
    fn progress(&self, active: bool, repository: &str, stage: &str, now: DateTime<Utc>) {
        self.health.observe(
            "github_progress",
            HealthStatus::Normal,
            json!({
                "active": active, "repository": repository, "stage": stage, "last_progress_at": now,
            }),
        );
    }
}

struct ObservedSource<'a, S, C> {
    source: &'a S,
    monitor: &'a GithubHealth,
    clock: &'a C,
}
#[async_trait]
impl<S: IssueSource, C: Fn() -> DateTime<Utc> + Send + Sync> IssueSource
    for ObservedSource<'_, S, C>
{
    async fn list_issues(
        &self,
        repository: &str,
        since: Option<DateTime<Utc>>,
        page: u32,
    ) -> Result<Vec<GithubIssue>> {
        let result = self.source.list_issues(repository, since, page).await;
        self.monitor
            .progress(true, repository, "page_complete", (self.clock)());
        result
    }
}
