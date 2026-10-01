use crate::model::IssueNotification;
use crate::store::Store;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct GithubIssue {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub created_at: DateTime<Utc>,
    pub url: String,
    pub is_pull_request: bool,
}

#[async_trait]
pub trait IssueSource: Send + Sync {
    async fn list_issues(
        &self,
        repository: &str,
        since: Option<DateTime<Utc>>,
        page: u32,
    ) -> Result<Vec<GithubIssue>>;
}

pub struct GithubClient {
    client: Client,
    token: Option<String>,
}

impl GithubClient {
    pub fn new(token: Option<String>) -> Result<Self> {
        Ok(Self {
            client: Client::builder().user_agent("issue-watch/0.1").build()?,
            token,
        })
    }
}

#[derive(Deserialize)]
struct ApiIssue {
    number: u64,
    title: String,
    html_url: String,
    created_at: DateTime<Utc>,
    user: Option<ApiUser>,
    pull_request: Option<serde_json::Value>,
}
#[derive(Deserialize)]
struct ApiUser {
    login: String,
}

#[async_trait]
impl IssueSource for GithubClient {
    async fn list_issues(
        &self,
        repository: &str,
        since: Option<DateTime<Utc>>,
        page: u32,
    ) -> Result<Vec<GithubIssue>> {
        let url = format!("https://api.github.com/repos/{repository}/issues");
        let mut request = self.client.get(url).query(&[
            ("state", "all"),
            ("sort", "created"),
            ("direction", "asc"),
            ("per_page", "100"),
            ("page", &page.to_string()),
        ]);
        if let Some(since) = since {
            request = request.query(&[("since", &since.to_rfc3339())]);
        }
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .context("request GitHub issues")?
            .error_for_status()
            .context("GitHub issues response")?;
        let issues: Vec<ApiIssue> = response.json().await.context("decode GitHub issues")?;
        Ok(issues
            .into_iter()
            .map(|item| GithubIssue {
                number: item.number,
                title: item.title,
                author: item
                    .user
                    .map(|u| u.login)
                    .unwrap_or_else(|| "unknown".into()),
                created_at: item.created_at,
                url: item.html_url,
                is_pull_request: item.pull_request.is_some(),
            })
            .collect())
    }
}

pub async fn discover_repository<S: IssueSource>(
    source: &S,
    store: &Store,
    repository: &str,
    now: DateTime<Utc>,
) -> Result<usize> {
    let repo = store.ensure_repository(repository, now)?;
    let since = repo.cursor.or(repo.baseline);
    let mut page = 1;
    let mut inserted = 0;
    let mut newest = since;
    loop {
        let page_items = source.list_issues(repository, since, page).await?;
        if page_items.is_empty() {
            break;
        }
        for item in page_items {
            newest = Some(newest.map_or(item.created_at, |value| value.max(item.created_at)));
            if item.is_pull_request || item.created_at <= repo.baseline.unwrap_or(now) {
                continue;
            }
            let notification = IssueNotification {
                repository: repo.name.clone(),
                number: item.number,
                title: item.title,
                author: item.author,
                created_at: item.created_at,
                url: item.url,
            };
            if store.insert_notification(&notification)? {
                inserted += 1;
            }
        }
        page += 1;
    }
    if let Some(cursor) = newest {
        store.set_cursor(&repo.name, cursor)?;
    }
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    struct Fake {
        pages: Arc<Vec<Vec<GithubIssue>>>,
    }
    #[async_trait]
    impl IssueSource for Fake {
        async fn list_issues(
            &self,
            _: &str,
            _: Option<DateTime<Utc>>,
            page: u32,
        ) -> Result<Vec<GithubIssue>> {
            Ok(self
                .pages
                .get(page as usize - 1)
                .cloned()
                .unwrap_or_default())
        }
    }
    fn issue(number: u64, created_at: DateTime<Utc>, pr: bool) -> GithubIssue {
        GithubIssue {
            number,
            title: format!("Issue {number}"),
            author: "alice".into(),
            created_at,
            url: format!("https://github.com/o/r/issues/{number}"),
            is_pull_request: pr,
        }
    }
    #[tokio::test]
    async fn discovers_pages_and_excludes_pull_requests() {
        let store = Store::open_in_memory().unwrap();
        let now = Utc::now();
        let old = now - chrono::Duration::hours(1);
        let fresh = now + chrono::Duration::minutes(1);
        let fake = Fake {
            pages: Arc::new(vec![
                vec![issue(1, old, false), issue(2, fresh, true)],
                vec![issue(3, fresh + chrono::Duration::minutes(1), false)],
                vec![],
            ]),
        };
        assert_eq!(
            discover_repository(&fake, &store, "o/r", now)
                .await
                .unwrap(),
            1
        );
        assert_eq!(store.pending_notifications(now).unwrap()[0].number, 3);
    }
}
