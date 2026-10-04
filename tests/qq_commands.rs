use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use issue_watch::{
    health::Health,
    qq::{MessageSink, QqHttpSender, RefreshingQqHttpSender},
    qq_health::QqHealth,
    Store,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::{
    sync::{mpsc, Notify},
    time::{timeout, Duration},
};
use tokio_tungstenite::tungstenite::Message;

async fn receive<T>(rx: &mut mpsc::UnboundedReceiver<T>) -> T {
    timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn gateway_replies_to_commands_without_blocking_heartbeats_or_losing_bindings_on_reply_failure(
) {
    #[derive(Clone)]
    struct HttpState {
        requests: mpsc::UnboundedSender<(String, Value)>,
        release: Arc<Notify>,
        count: Arc<AtomicUsize>,
    }
    async fn message(
        Path(user): Path<String>,
        State(state): State<HttpState>,
        Json(body): Json<Value>,
    ) -> StatusCode {
        let first = state.count.fetch_add(1, Ordering::SeqCst) == 0;
        state.requests.send((user, body)).unwrap();
        if first {
            state.release.notified().await;
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::OK
        }
    }
    let (requests, mut received) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", http_listener.local_addr().unwrap());
    let state = HttpState {
        requests,
        release: release.clone(),
        count: Arc::new(AtomicUsize::new(0)),
    };
    let http_server = tokio::spawn(async move {
        axum::serve(
            http_listener,
            Router::new()
                .route("/{user}/messages", post(message))
                .with_state(state),
        )
        .await
        .unwrap();
    });

    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_url = format!("ws://{}", ws_listener.local_addr().unwrap());
    let (commands, mut incoming) = mpsc::unbounded_channel::<Value>();
    let (heartbeats, mut heartbeat_rx) = mpsc::unbounded_channel();
    let ws_server = tokio::spawn(async move {
        let (stream, _) = ws_listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(
                json!({"op":10,"d":{"heartbeat_interval":50}}).to_string(),
            ))
            .await
            .unwrap();
        let identify = socket.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(identify.to_text().unwrap()).unwrap()["op"],
            2
        );
        socket
            .send(Message::Text(json!({"op":0,"t":"READY"}).to_string()))
            .await
            .unwrap();
        loop {
            tokio::select! {
                command = incoming.recv() => match command {
                    Some(value) => socket.send(Message::Text(value.to_string())).await.unwrap(),
                    None => {
                        socket.send(Message::Text(json!({"op":7}).to_string())).await.unwrap();
                        let _ = socket.next().await;
                        break;
                    }
                },
                message = socket.next() => {
                    let Some(Ok(Message::Text(text))) = message else { break; };
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["op"] == 1 { heartbeats.send(()).unwrap(); }
                }
            }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("commands.sqlite");
    let database_path = path.to_str().unwrap().to_owned();
    let health = Health::new(vec![]);
    let qq = QqHealth::new(health.clone());
    let gateway = tokio::spawn(async move {
        issue_watch::qq_gateway::run_gateway_with_sender_observed(
            &gateway_url,
            "unused",
            &database_path,
            Arc::new(QqHttpSender::with_endpoint("unused".into(), endpoint)),
            &qq,
        )
        .await
        .unwrap();
    });
    let event = |id: &str, user: &str, content: &str| {
        json!({
            "op":0,"t":"C2C_MESSAGE_CREATE","d":{"id":id,"content":content,"author":{"user_openid":user}}
        })
    };
    commands
        .send(event("bind-alice", "alice", "/bind"))
        .unwrap();
    let (user, body) = receive(&mut received).await;
    assert_eq!(user, "alice");
    assert_eq!(
        body,
        json!({"content":"success","msg_id":"bind-alice","msg_type":0,"msg_seq":1})
    );
    assert_eq!(
        Store::open(&path).unwrap().bound_users().unwrap(),
        ["alice"]
    );
    while heartbeat_rx.try_recv().is_ok() {}
    receive(&mut heartbeat_rx).await; // Reply is still blocked by the mock HTTP server.
    release.notify_one(); // First reply fails; binding must remain committed.

    commands.send(event("bind-bob", "bob", "/bind")).unwrap();
    assert_eq!(
        receive(&mut received).await,
        (
            "bob".into(),
            json!({"content":"success","msg_id":"bind-bob","msg_type":0,"msg_seq":1})
        )
    );
    commands
        .send(event("bind-alice-again", "alice", "/bind"))
        .unwrap();
    assert_eq!(receive(&mut received).await.1["content"], "already bound");
    assert_eq!(
        Store::open(&path).unwrap().bound_users().unwrap(),
        ["alice", "bob"]
    );
    assert_eq!(
        health.snapshot().components["qq_binding"].details["subscriber_count"],
        2
    );
    assert_eq!(
        health.snapshot().components["qq_delivery"].details["result"],
        "unverified"
    );
    commands
        .send(event("unbind-alice", "alice", "/unbind"))
        .unwrap();
    assert_eq!(receive(&mut received).await.1["content"], "success");
    assert_eq!(Store::open(&path).unwrap().bound_users().unwrap(), ["bob"]);
    commands
        .send(event("unbind-alice-again", "alice", "/unbind"))
        .unwrap();
    assert_eq!(receive(&mut received).await.1["content"], "not bound");
    commands
        .send(event("rebind-alice", "alice", "/bind"))
        .unwrap();
    assert_eq!(receive(&mut received).await.1["content"], "success");
    commands
        .send(event(
            "track-denied",
            "alice",
            "/track https://github.com/owner/repo/issues/12",
        ))
        .unwrap();
    let (_, reply) = receive(&mut received).await;
    assert_eq!(reply["msg_id"], "track-denied");
    assert_eq!(
        reply["content"],
        "权限不足：你没有执行此命令的权限，请联系后台管理人员。"
    );
    receive(&mut heartbeat_rx).await;
    drop(commands);
    timeout(Duration::from_secs(3), gateway)
        .await
        .unwrap()
        .unwrap();
    ws_server.await.unwrap();
    http_server.abort();
}

#[tokio::test]
async fn command_reply_keeps_message_reference_after_token_refresh() {
    #[derive(Clone)]
    struct TestState(mpsc::UnboundedSender<(String, Value)>);
    async fn token() -> Json<Value> {
        Json(json!({"access_token":"fresh"}))
    }
    async fn message(
        State(state): State<TestState>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> StatusCode {
        let auth = headers["authorization"].to_str().unwrap().to_owned();
        state.0.send((auth.clone(), body)).unwrap();
        if auth == "QQBot fresh" {
            StatusCode::OK
        } else {
            StatusCode::UNAUTHORIZED
        }
    }
    let (tx, mut rx) = mpsc::unbounded_channel();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/token", post(token))
                .route("/{id}/messages", post(message))
                .with_state(TestState(tx)),
        )
        .await
        .unwrap();
    });
    let sender = RefreshingQqHttpSender::with_endpoints(
        "stale".into(),
        "app".into(),
        "secret".into(),
        endpoint.clone(),
        format!("{endpoint}/token"),
    );
    sender.reply("user", "success", "command-id").await.unwrap();
    let original = receive(&mut rx).await;
    let refreshed = receive(&mut rx).await;
    assert_eq!(original.0, "QQBot stale");
    assert_eq!(refreshed.0, "QQBot fresh");
    assert_eq!(
        original.1,
        json!({"content":"success","msg_id":"command-id","msg_type":0,"msg_seq":1})
    );
    assert_eq!(original.1, refreshed.1);
    sender.send("user", "broadcast").await.unwrap();
    let broadcast = receive(&mut rx).await;
    assert_eq!(broadcast.0, "QQBot fresh");
    assert!(broadcast.1.get("msg_id").is_none());
    server.abort();
}
