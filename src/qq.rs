use crate::{model::IssueNotification, store::Store};
use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("QQ authentication failed: {0}")]
    Authentication(String),
    #[error("retryable QQ error: {0}")]
    Retryable(String),
    #[error("permanent QQ error: {0}")]
    Permanent(String),
}

/// Commit the subscription change before acknowledging it to the sender.
pub fn private_command(
    store: &Store,
    content: &str,
    user_openid: &str,
) -> Result<Option<&'static str>> {
    match content.trim() {
        "/bind" => Ok(Some(if store.bind_user(user_openid)? {
            "success"
        } else {
            "already bound"
        })),
        "/unbind" => Ok(Some(if store.unbind_user(user_openid)? {
            "success"
        } else {
            "not bound"
        })),
        _ => Ok(None),
    }
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
    async fn reply(
        &self,
        user_openid: &str,
        text: &str,
        message_id: &str,
    ) -> std::result::Result<(), SendError>;
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

    async fn send_with_token(
        &self,
        access_token: &str,
        user_openid: &str,
        text: &str,
        message_id: Option<&str>,
    ) -> std::result::Result<(), SendError> {
        send_message(
            &self.client,
            &self.endpoint,
            access_token,
            user_openid,
            text,
            message_id,
        )
        .await
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
        self.send_with_token(&self.access_token, user_openid, text, None)
            .await
    }
    async fn reply(
        &self,
        user_openid: &str,
        text: &str,
        message_id: &str,
    ) -> std::result::Result<(), SendError> {
        self.send_with_token(&self.access_token, user_openid, text, Some(message_id))
            .await
    }
}

/// Sends notifications with a token that can be refreshed after an authentication failure.
pub struct RefreshingQqHttpSender {
    client: Client,
    access_token: Arc<RwLock<String>>,
    app_id: String,
    app_secret: String,
    endpoint: String,
    token_endpoint: String,
}

impl RefreshingQqHttpSender {
    pub fn new(access_token: String, app_id: String, app_secret: String) -> Self {
        Self::with_endpoints(
            access_token,
            app_id,
            app_secret,
            "https://api.sgroup.qq.com/v2/users".into(),
            "https://bots.qq.com/app/getAppAccessToken".into(),
        )
    }

    pub fn with_endpoint(
        access_token: String,
        app_id: String,
        app_secret: String,
        endpoint: String,
    ) -> Self {
        Self::with_endpoints(
            access_token,
            app_id,
            app_secret,
            endpoint,
            "https://bots.qq.com/app/getAppAccessToken".into(),
        )
    }

    pub fn with_endpoints(
        access_token: String,
        app_id: String,
        app_secret: String,
        endpoint: String,
        token_endpoint: String,
    ) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("QQ HTTP client"),
            access_token: Arc::new(RwLock::new(access_token)),
            app_id,
            app_secret,
            endpoint,
            token_endpoint,
        }
    }
}

impl RefreshingQqHttpSender {
    pub async fn current_token(&self) -> String {
        self.access_token.read().await.clone()
    }

    /// Refresh only if another caller has not already replaced the rejected token.
    pub async fn refresh_rejected_token(&self, rejected: &str) -> Result<String> {
        let mut token = self.access_token.write().await;
        if token.as_str() == rejected {
            *token = fetch_access_token_from(
                &self.client,
                &self.token_endpoint,
                &self.app_id,
                &self.app_secret,
            )
            .await?;
        }
        Ok(token.clone())
    }

    async fn send_or_reply(
        &self,
        user_openid: &str,
        text: &str,
        message_id: Option<&str>,
    ) -> std::result::Result<(), SendError> {
        let token = self.access_token.read().await.clone();
        match send_message(
            &self.client,
            &self.endpoint,
            &token,
            user_openid,
            text,
            message_id,
        )
        .await
        {
            Err(SendError::Authentication(_)) => {
                let refreshed = self
                    .refresh_rejected_token(&token)
                    .await
                    .map_err(|error| SendError::Authentication(error.to_string()))?;
                send_message(
                    &self.client,
                    &self.endpoint,
                    &refreshed,
                    user_openid,
                    text,
                    message_id,
                )
                .await
            }
            result => result,
        }
    }
}

#[async_trait]
impl MessageSink for RefreshingQqHttpSender {
    async fn send(&self, user_openid: &str, text: &str) -> std::result::Result<(), SendError> {
        self.send_or_reply(user_openid, text, None).await
    }
    async fn reply(
        &self,
        user_openid: &str,
        text: &str,
        message_id: &str,
    ) -> std::result::Result<(), SendError> {
        self.send_or_reply(user_openid, text, Some(message_id))
            .await
    }
}

fn message_body(text: &str, message_id: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::json!({"content":text,"msg_type":0});
    if let Some(id) = message_id {
        body["msg_id"] = id.into();
        body["msg_seq"] = 1.into();
    }
    body
}

async fn send_message(
    client: &Client,
    endpoint: &str,
    access_token: &str,
    user_openid: &str,
    text: &str,
    message_id: Option<&str>,
) -> std::result::Result<(), SendError> {
    let response = client
        .post(format!("{endpoint}/{user_openid}/messages"))
        .header("Authorization", format!("QQBot {access_token}"))
        .json(&message_body(text, message_id))
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
        let retry = client
            .post(format!("{endpoint}/{user_openid}/messages"))
            .header("Authorization", format!("QQBot {access_token}"))
            .json(&message_body(&fallback, message_id))
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

pub async fn fetch_access_token(client: &Client, app_id: &str, app_secret: &str) -> Result<String> {
    fetch_access_token_from(
        client,
        "https://bots.qq.com/app/getAppAccessToken",
        app_id,
        app_secret,
    )
    .await
}

async fn fetch_access_token_from(
    client: &Client,
    endpoint: &str,
    app_id: &str,
    app_secret: &str,
) -> Result<String> {
    let body = client
        .post(endpoint)
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
    fetch_gateway_url_from(client, "https://api.sgroup.qq.com/gateway", access_token).await
}

pub(crate) async fn fetch_gateway_url_from(
    client: &Client,
    endpoint: &str,
    access_token: &str,
) -> Result<String> {
    let body = client
        .get(endpoint)
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
    fn private_commands_join_leave_and_acknowledge() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(
            private_command(&store, " /bind ", "openid-1").unwrap(),
            Some("success")
        );
        assert_eq!(
            private_command(&store, "/bind", "openid-2").unwrap(),
            Some("success")
        );
        assert_eq!(
            private_command(&store, "/bind", "openid-1").unwrap(),
            Some("already bound")
        );
        assert_eq!(
            private_command(&store, "/unbind", "openid-1").unwrap(),
            Some("success")
        );
        assert_eq!(
            private_command(&store, "/unbind", "openid-1").unwrap(),
            Some("not bound")
        );
        assert_eq!(private_command(&store, "hello", "openid-3").unwrap(), None);
        assert_eq!(store.bound_users().unwrap(), ["openid-2"]);
    }
}
