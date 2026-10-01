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

pub async fn run_gateway(gateway_url: &str, access_token: &str, store: &Store) -> Result<()> {
    let (mut socket, _) = connect_async(gateway_url)
        .await
        .context("connect QQ gateway")?;
    let mut sequence = serde_json::Value::Null;
    while let Some(message) = socket.next().await {
        let message = message?;
        if let Message::Text(text) = message {
            let event: GatewayEvent = serde_json::from_str(&text)?;
            if let Some(seq) = event.s {
                sequence = serde_json::json!(seq);
            }
            match event.op {
                10 => {
                    let identify = serde_json::json!({"op": 2, "d": {"token": format!("QQBot {access_token}"), "intents": (1u64 << 25), "shard": [0, 1], "properties": {"$os": "issue-watch", "$browser": "issue-watch", "$device": "issue-watch"}}});
                    socket
                        .send(Message::Text(identify.to_string().into()))
                        .await?;
                }
                0 => {
                    if event.t.as_deref() == Some("C2C_MESSAGE_CREATE") {
                        if let Some(data) = event.d {
                            let content =
                                data.get("content").and_then(|v| v.as_str()).unwrap_or("");
                            let openid = data
                                .get("author")
                                .and_then(|v| v.get("user_openid"))
                                .and_then(|v| v.as_str());
                            if content.trim() == "/bind" {
                                if let Some(openid) = openid {
                                    store.bind_user(openid)?;
                                }
                            }
                        }
                    }
                }
                1 => {
                    socket
                        .send(Message::Text(
                            serde_json::json!({"op": 1, "d": sequence})
                                .to_string()
                                .into(),
                        ))
                        .await?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
