use crate::store::Store;
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Debug, Deserialize)]
struct GatewayEvent {
    op: u64,
    d: Option<serde_json::Value>,
    s: Option<u64>,
    t: Option<String>,
}

pub async fn run_gateway(gateway_url: &str, access_token: &str, database_path: &str) -> Result<()> {
    let store = Store::open(database_path)?;
    let (mut socket, _) = connect_async(gateway_url)
        .await
        .context("connect QQ gateway")?;
    let mut sequence: Option<u64> = None;
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + std::time::Duration::from_secs(30),
        std::time::Duration::from_secs(30),
    );
    loop {
        let message = tokio::select! {
            _ = heartbeat.tick() => {
                socket.send(Message::Text(serde_json::json!({"op": 1, "d": sequence}).to_string().into())).await?;
                continue;
            }
            message = socket.next() => match message { Some(message) => message?, None => break }
        };
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
                    socket
                        .send(Message::Text(identify.to_string().into()))
                        .await?;
                }
                0 => {
                    if event.t.as_deref() == Some("READY") {
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
                                .and_then(|v| v.as_str());
                            if content.trim() == "/bind" {
                                if let Some(openid) = openid {
                                    let bound = store.bind_user(openid)?;
                                    tracing::info!(bound, "QQ private target binding processed");
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
