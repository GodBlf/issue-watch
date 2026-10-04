use crate::{
    qq::{MessageSink, QqHttpSender, SendError},
    store::Store,
};
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Debug, Deserialize)]
struct GatewayEvent {
    op: u64,
    d: Option<serde_json::Value>,
    s: Option<u64>,
    t: Option<String>,
}

/// Run one connection attempt; the caller controls reconnect backoff.
pub async fn run_refreshing_gateway_observed(
    sender: Arc<crate::qq::RefreshingQqHttpSender>,
    gateway_endpoint: &str,
    configured_gateway: Option<&str>,
    database_path: &str,
    health: &crate::qq_health::QqHealth,
) -> Result<()> {
    let mut token = sender.current_token().await;
    health.0.add_secret(token.clone());
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let connection: Result<String> = async {
        match configured_gateway {
            Some(url) => Ok(url.to_owned()),
            None => {
                match crate::qq::fetch_gateway_url_from(&client, gateway_endpoint, &token).await {
                    Err(error) if authentication_rejected(&error) => {
                        health.authentication(false, Some("QQ gateway authentication rejected"));
                        token = sender.refresh_rejected_token(&token).await?;
                        health.0.add_secret(token.clone());
                        crate::qq::fetch_gateway_url_from(&client, gateway_endpoint, &token).await
                    }
                    result => result,
                }
            }
        }
    }
    .await;
    let url = match connection {
        Ok(url) => url,
        Err(error) => {
            health.gateway(false, Some(&error.to_string()));
            if authentication_rejected(&error) {
                health.authentication(false, Some("QQ gateway authentication rejected"));
            }
            return Err(error);
        }
    };
    let result =
        run_gateway_with_sender_observed(&url, &token, database_path, sender.clone(), health).await;
    if result.as_ref().err().is_some_and(authentication_rejected) {
        match sender.refresh_rejected_token(&token).await {
            Ok(refreshed) => health.0.add_secret(refreshed),
            Err(error) => {
                health.authentication(false, Some(&format!("QQ token refresh failed: {error}")));
                return Err(error);
            }
        }
    }
    result
}

#[derive(Debug, thiserror::Error)]
#[error("QQ gateway authentication rejected")]
struct GatewayAuthenticationRejected;

fn authentication_rejected(error: &anyhow::Error) -> bool {
    error.chain().any(|error| {
        error.is::<GatewayAuthenticationRejected>()
            || error.downcast_ref::<reqwest::Error>().is_some_and(|error|
                error.status().is_some_and(|status| matches!(status.as_u16(), 401 | 403)))
            || error.downcast_ref::<tokio_tungstenite::tungstenite::Error>().is_some_and(|error|
                matches!(error, tokio_tungstenite::tungstenite::Error::Http(response) if matches!(response.status().as_u16(), 401 | 403)))
    })
}

pub async fn run_gateway(gateway_url: &str, access_token: &str, database_path: &str) -> Result<()> {
    gateway(
        gateway_url,
        access_token,
        database_path,
        Arc::new(QqHttpSender::new(access_token.into())),
        None,
    )
    .await
}
pub async fn run_gateway_observed(
    gateway_url: &str,
    access_token: &str,
    database_path: &str,
    health: &crate::qq_health::QqHealth,
) -> Result<()> {
    run_gateway_with_sender_observed(
        gateway_url,
        access_token,
        database_path,
        Arc::new(QqHttpSender::new(access_token.into())),
        health,
    )
    .await
}

pub async fn run_gateway_with_sender_observed(
    gateway_url: &str,
    access_token: &str,
    database_path: &str,
    sender: Arc<dyn MessageSink>,
    health: &crate::qq_health::QqHealth,
) -> Result<()> {
    let result = gateway(
        gateway_url,
        access_token,
        database_path,
        sender,
        Some(health),
    )
    .await;
    if let Err(error) = &result {
        if authentication_rejected(error) {
            health.authentication(false, Some("Gateway authentication rejected"));
        }
    }
    health.gateway(
        false,
        Some(&match &result {
            Ok(()) => "Gateway disconnected".into(),
            Err(error) => error.to_string(),
        }),
    );
    result
}
async fn gateway(
    gateway_url: &str,
    access_token: &str,
    database_path: &str,
    sender: Arc<dyn MessageSink>,
    health: Option<&crate::qq_health::QqHealth>,
) -> Result<()> {
    let store = Store::open(database_path)?;
    let (mut socket, _) = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        connect_async(gateway_url),
    )
    .await
    .context("QQ gateway connection timed out")?
    .context("connect QQ gateway")?;
    let mut sequence: Option<u64> = None;
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + std::time::Duration::from_secs(30),
        std::time::Duration::from_secs(30),
    );
    loop {
        let message = tokio::select! {
            _ = heartbeat.tick() => {
                socket.send(Message::Text(serde_json::json!({"op": 1, "d": sequence}).to_string())).await?;
                continue;
            }
            message = socket.next() => match message { Some(message) => message?, None => break }
        };
        if let Message::Close(frame) = &message {
            if let Some(frame) = frame {
                if [4003u16, 4004].contains(&u16::from(frame.code)) {
                    if let Some(health) = health {
                        health.authentication(false, Some("Gateway authentication rejected"));
                    }
                    return Err(GatewayAuthenticationRejected.into());
                }
            }
            break;
        }
        if let Message::Text(text) = message {
            let event: GatewayEvent = serde_json::from_str(&text)?;
            if let Some(seq) = event.s {
                sequence = Some(seq);
            }
            match event.op {
                10 => {
                    if let Some(interval) = event
                        .d
                        .as_ref()
                        .and_then(|d| d.get("heartbeat_interval"))
                        .and_then(|v| v.as_u64())
                    {
                        let duration = std::time::Duration::from_millis(interval.max(1));
                        heartbeat = tokio::time::interval_at(
                            tokio::time::Instant::now() + duration,
                            duration,
                        );
                    }
                    let identify = serde_json::json!({"op": 2, "d": {"token": format!("QQBot {access_token}"), "intents": (1u64 << 25), "shard": [0, 1], "properties": {"$os": "issue-watch", "$browser": "issue-watch", "$device": "issue-watch"}}});
                    socket.send(Message::Text(identify.to_string())).await?;
                }
                0 => {
                    if event.t.as_deref() == Some("READY") {
                        if let Some(health) = health {
                            health.gateway(true, None);
                            health.authentication(true, None);
                        }
                        tracing::info!("QQ gateway READY; private messages can now be received");
                    }
                    if event.t.as_deref() == Some("C2C_MESSAGE_CREATE") {
                        tracing::info!("QQ private message received");
                        if let Some(data) = event.d {
                            let content =
                                data.get("content").and_then(|v| v.as_str()).unwrap_or("");
                            let openid = data
                                .get("author")
                                .and_then(|v| v.get("user_openid"))
                                .and_then(|v| v.as_str())
                                .filter(|id| !id.trim().is_empty());
                            let message_id = data
                                .get("id")
                                .and_then(|v| v.as_str())
                                .filter(|id| !id.is_empty());
                            if let (Some(openid), Some(message_id)) = (openid, message_id) {
                                if matches!(
                                    content.split_whitespace().next(),
                                    Some("/track" | "/untrack" | "/tracking")
                                ) {
                                    let database_path = database_path.to_owned();
                                    let content = content.to_owned();
                                    let openid = openid.to_owned();
                                    let message_id = message_id.to_owned();
                                    let sender = sender.clone();
                                    tokio::spawn(async move {
                                        let outcome: Result<()> = async {
                                            let mut store = Store::open(&database_path)?;
                                            let source =
                                                crate::tracking::GithubTrackingSource::new(
                                                    std::env::var("GITHUB_TOKEN").ok(),
                                                )?;
                                            if let Some(reply) = crate::tracking::tracking_command(
                                                &mut store, &source, &content, &openid,
                                            )
                                            .await?
                                            {
                                                sender.reply(&openid, &reply, &message_id).await?;
                                            }
                                            Ok(())
                                        }
                                        .await;
                                        if let Err(error) = outcome {
                                            tracing::warn!(%error,"QQ tracking command failed");
                                        }
                                    });
                                    continue;
                                }
                                match crate::qq::private_command(&store, content, openid) {
                                    Ok(Some(reply)) => {
                                        if let Some(health) = health {
                                            health.refresh_queue(&store)?;
                                        }
                                        tracing::info!(
                                            command = content.trim(),
                                            "QQ broadcast subscription command processed"
                                        );
                                        let sender = sender.clone();
                                        let openid = openid.to_owned();
                                        let message_id = message_id.to_owned();
                                        let health = health.cloned();
                                        // HTTP replies must not block WebSocket heartbeats.
                                        tokio::spawn(async move {
                                            match sender.reply(&openid, reply, &message_id).await {
                                                Ok(()) => {
                                                    if let Some(health) = health {
                                                        health.authentication(true, None);
                                                    }
                                                }
                                                Err(error) => {
                                                    if matches!(error, SendError::Authentication(_))
                                                    {
                                                        if let Some(health) = health {
                                                            health.authentication(
                                                                false,
                                                                Some(&error.to_string()),
                                                            );
                                                        }
                                                    }
                                                    tracing::warn!(%error, "QQ command reply failed; subscription change remains committed");
                                                }
                                            }
                                        });
                                    }
                                    Ok(None) => {}
                                    Err(error) => {
                                        tracing::warn!(%error, "QQ subscription command failed")
                                    }
                                }
                            }
                        }
                    }
                }
                7 | 9 => break,
                _ => {}
            }
        }
    }
    Ok(())
}
