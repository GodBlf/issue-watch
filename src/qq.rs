use crate::{model::IssueNotification, store::Store};
use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("QQ authentication failed: {0}")]
    Authentication(String),
    #[error("retryable QQ error: {0}")]
    Retryable(String),
    #[error("permanent QQ error: {0}")]
    Permanent(String),
}

pub fn bind_from_private_message(store: &Store, content: &str, user_openid: &str) -> Result<bool> {
    if content.trim() != "/bind" {
        return Ok(false);
    }
    store.bind_user(user_openid)
}

pub fn render_notification(issue: &IssueNotification) -> String {
    let base = format!(
        "[Issue通知] {} #{}\n标题: {}\n作者: {}\n时间: {}\n链接: {}",
        issue.repository,
        issue.number,
        issue.title,
        issue.author,
        issue
            .created_at
            .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
            .format("%Y/%-m/%-d %H:%M"),
        issue.url
    );
    base.chars().take(1800).collect()
}

#[async_trait]
pub trait MessageSink: Send + Sync {
    async fn send(&self, user_openid: &str, text: &str) -> std::result::Result<(), SendError>;
}

pub struct QqHttpSender {
    client: Client,
    access_token: String,
    endpoint: String,
}
impl QqHttpSender {
    pub fn new(access_token: String) -> Self {
        Self::with_endpoint(access_token, "https://api.sgroup.qq.com/v2/users".into())
    }
    pub fn with_endpoint(access_token: String, endpoint: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("QQ HTTP client"),
            access_token,
            endpoint,
        }
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    code: Option<i64>,
    message: Option<String>,
}
#[async_trait]
impl MessageSink for QqHttpSender {
    async fn send(&self, user_openid: &str, text: &str) -> std::result::Result<(), SendError> {
        let response = self
            .client
            .post(format!("{}/{}/messages", self.endpoint, user_openid))
            .header("Authorization", format!("QQBot {}", self.access_token))
            .json(&serde_json::json!({"content": text}))
            .send()
            .await
            .map_err(|e| SendError::Retryable(e.to_string()))?;
        let status = response.status();
        if matches!(status.as_u16(), 401 | 403) {
            return Err(SendError::Authentication(format!("HTTP {status}")));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| SendError::Retryable(e.to_string()))?;
        let body = serde_json::from_slice::<ErrorBody>(&bytes).unwrap_or(ErrorBody {
            code: None,
            message: None,
        });
        if status.is_success() && body.code.is_none_or(|code| code == 0) {
            return Ok(());
        }
        if body.code == Some(40054010) {
            let fallback = text
                .lines()
                .filter(|line| !line.starts_with("链接:"))
                .collect::<Vec<_>>()
                .join("\n");
            let retry = self
                .client
                .post(format!("{}/{}/messages", self.endpoint, user_openid))
                .header("Authorization", format!("QQBot {}", self.access_token))
                .json(&serde_json::json!({"content": fallback}))
                .send()
                .await
                .map_err(|e| SendError::Retryable(e.to_string()))?;
            let retry_status = retry.status();
            let retry_bytes = retry
                .bytes()
                .await
                .map_err(|e| SendError::Retryable(e.to_string()))?;
            let retry_body = serde_json::from_slice::<ErrorBody>(&retry_bytes).ok();
            if retry_status.is_success()
                && retry_body
                    .as_ref()
                    .and_then(|body| body.code)
                    .is_none_or(|code| code == 0)
            {
                return Ok(());
            }
            let message = retry_body
                .and_then(|body| body.message)
                .unwrap_or_else(|| format!("HTTP {retry_status}"));
            return Err(if matches!(retry_status.as_u16(), 401 | 403) {
                SendError::Authentication(message)
            } else if retry_status.as_u16() == 429 || retry_status.is_server_error() {
                SendError::Retryable(message)
            } else {
                SendError::Permanent(message)
            });
        }
        let message = body.message.unwrap_or_else(|| format!("HTTP {status}"));
        if status.as_u16() == 429 || status.is_server_error() {
            Err(SendError::Retryable(message))
        } else {
            Err(SendError::Permanent(message))
        }
    }
}

pub async fn fetch_access_token(client: &Client, app_id: &str, app_secret: &str) -> Result<String> {
    let body = client
        .post("https://bots.qq.com/app/getAppAccessToken")
        .timeout(std::time::Duration::from_secs(30))
        .json(&serde_json::json!({"appId": app_id, "clientSecret": app_secret}))
        .send()
        .await
        .context("request QQ access token")?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    body.get("access_token")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .context("QQ access token missing")
}

pub async fn fetch_gateway_url(client: &Client, access_token: &str) -> Result<String> {
    let body = client
        .get("https://api.sgroup.qq.com/gateway")
        .timeout(std::time::Duration::from_secs(30))
        .header("Authorization", format!("QQBot {access_token}"))
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    body.get("url")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .context("QQ gateway URL missing")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    #[test]
    fn renders_bounded_message() {
        let issue = IssueNotification {
            repository: "o/r".into(),
            number: 1,
            title: "x".repeat(3000),
            author: "a".into(),
            created_at: Utc::now(),
            url: "https://example.com".into(),
        };
        assert!(render_notification(&issue).chars().count() <= 1800);
    }

    #[test]
    fn binds_only_the_first_private_target() {
        let store = Store::open_in_memory().unwrap();
        assert!(bind_from_private_message(&store, "/bind", "openid-1").unwrap());
        assert!(!bind_from_private_message(&store, "/bind", "openid-2").unwrap());
        assert_eq!(store.get_bound_user().unwrap().as_deref(), Some("openid-1"));
    }
}
