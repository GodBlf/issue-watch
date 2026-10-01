use crate::{model::IssueNotification, store::Store};
use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("retryable QQ error: {0}")]
    Retryable(String),
    #[error("permanent QQ error: {0}")]
    Permanent(String),
}

pub fn bind_from_private_message(store: &Store, content: &str, user_openid: &str) -> Result<bool> {
    if content.trim() != "/bind" {
        return Ok(false);
    }
    Ok(store.bind_user(user_openid)?)
}

pub fn render_notification(issue: &IssueNotification) -> String {
    let base = format!(
        "[Issue通知] {} #{}\n标题: {}\n作者: {}\n创建时间: {}\n链接: {}",
        issue.repository,
        issue.number,
        issue.title,
        issue.author,
        issue.created_at.to_rfc3339(),
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
        Self {
            client: Client::new(),
            access_token,
            endpoint: "https://api.sgroup.qq.com/v2/users".into(),
        }
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    message: Option<String>,
}
#[async_trait]
impl MessageSink for QqHttpSender {
    async fn send(&self, user_openid: &str, text: &str) -> std::result::Result<(), SendError> {
        let response = self
            .client
            .post(format!("{}/{}/messages", self.endpoint, user_openid))
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({"content": text}))
            .send()
            .await
            .map_err(|e| SendError::Retryable(e.to_string()))?;
        if response.status().is_success() {
            return Ok(());
        }
        let status = response.status();
        let body = response
            .json::<ErrorBody>()
            .await
            .unwrap_or(ErrorBody { message: None });
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
}
