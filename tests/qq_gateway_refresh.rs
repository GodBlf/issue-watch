use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use issue_watch::{
    health::Health,
    qq::{MessageSink, RefreshingQqHttpSender},
    qq_gateway::run_refreshing_gateway_observed,
    qq_health::QqHealth,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::time::{timeout, Duration};
use tokio_tungstenite::tungstenite::{
    protocol::{frame::coding::CloseCode, CloseFrame},
    Message,
};

#[derive(Clone)]
struct Api {
    gateway: String,
    refreshes: Arc<AtomicUsize>,
    gateway_status: StatusCode,
    token_status: StatusCode,
}
async fn token(State(api): State<Api>) -> (StatusCode, Json<serde_json::Value>) {
    api.refreshes.fetch_add(1, Ordering::SeqCst);
    (api.token_status, Json(json!({"access_token":"fresh"})))
}
async fn discovery(
    State(api): State<Api>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    let status = if headers["authorization"] == "QQBot fresh" {
        StatusCode::OK
    } else {
        api.gateway_status
    };
    (status, Json(json!({"url":api.gateway})))
}
async fn message(headers: HeaderMap) -> StatusCode {
    if headers["authorization"] == "QQBot fresh" {
        StatusCode::OK
    } else {
        StatusCode::UNAUTHORIZED
    }
}
async fn api(
    gateway: String,
    gateway_status: StatusCode,
    token_status: StatusCode,
) -> (
    Arc<RefreshingQqHttpSender>,
    String,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let refreshes = Arc::new(AtomicUsize::new(0));
    let state = Api {
        gateway,
        refreshes: refreshes.clone(),
        gateway_status,
        token_status,
    };
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/token", post(token))
                .route("/gateway", get(discovery))
                .route("/{id}/messages", post(message))
                .with_state(state),
        )
        .await
        .unwrap();
    });
    let sender = Arc::new(RefreshingQqHttpSender::with_endpoints(
        "stale".into(),
        "app".into(),
        "secret".into(),
        base.clone(),
        format!("{base}/token"),
    ));
    (sender, format!("{base}/gateway"), refreshes, server)
}

#[tokio::test]
async fn expired_gateway_token_refreshes_identify_and_notification_token() {
    for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
        expired_gateway_token(status).await;
    }
}

async fn expired_gateway_token(status: StatusCode) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let websocket = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(
                json!({"op":10,"d":{"heartbeat_interval":60000}}).to_string(),
            ))
            .await
            .unwrap();
        let identify = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&identify).unwrap()["d"]["token"],
            "QQBot fresh"
        );
        socket
            .send(Message::Text(json!({"op":0,"t":"READY"}).to_string()))
            .await
            .unwrap();
        socket.close(None).await.unwrap();
    });
    let (sender, endpoint, refreshes, server) = api(url, status, StatusCode::OK).await;
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("db.sqlite");
    let health = Health::new(vec![]);
    timeout(
        Duration::from_secs(3),
        run_refreshing_gateway_observed(
            sender.clone(),
            &endpoint,
            None,
            database.to_str().unwrap(),
            &QqHealth::new(health.clone()),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    websocket.await.unwrap();
    sender.send("user", "notification").await.unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        serde_json::to_value(health.snapshot()).unwrap()["components"]["qq_auth"]["status"],
        "normal"
    );
    server.abort();
}

#[tokio::test]
async fn concurrent_notification_refresh_is_reused_by_gateway() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let websocket = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(
                json!({"op":10,"d":{"heartbeat_interval":60000}}).to_string(),
            ))
            .await
            .unwrap();
        let identify = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&identify).unwrap()["d"]["token"],
            "QQBot fresh"
        );
        socket.close(None).await.unwrap();
    });
    let (sender, endpoint, refreshes, server) =
        api(url, StatusCode::UNAUTHORIZED, StatusCode::OK).await;
    let (first, second) = tokio::join!(
        sender.send("first", "notification"),
        sender.send("second", "notification")
    );
    first.unwrap();
    second.unwrap();
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("db.sqlite");
    let health = QqHealth::new(Health::new(vec![]));
    timeout(
        Duration::from_secs(3),
        run_refreshing_gateway_observed(
            sender,
            &endpoint,
            None,
            database.to_str().unwrap(),
            &health,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    websocket.await.unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn gateway_server_failure_does_not_refresh_credentials() {
    let (sender, endpoint, refreshes, server) = api(
        "unused".into(),
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::OK,
    )
    .await;
    let health = QqHealth::new(Health::new(vec![]));
    assert!(
        run_refreshing_gateway_observed(sender, &endpoint, None, "unused", &health)
            .await
            .is_err()
    );
    assert_eq!(refreshes.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn gateway_refresh_failure_remains_visible_and_retries_next_attempt() {
    let (sender, endpoint, refreshes, server) = api(
        "unused".into(),
        StatusCode::UNAUTHORIZED,
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    let health = Health::new(vec![]);
    let qq_health = QqHealth::new(health.clone());
    for _ in 0..2 {
        assert!(run_refreshing_gateway_observed(
            sender.clone(),
            &endpoint,
            None,
            "unused",
            &qq_health
        )
        .await
        .is_err());
    }
    assert_eq!(refreshes.load(Ordering::SeqCst), 2);
    assert_eq!(
        serde_json::to_value(health.snapshot()).unwrap()["components"]["qq_auth"]["status"],
        "error"
    );
    server.abort();
}

#[tokio::test]
async fn configured_gateway_authentication_close_refreshes_next_identify() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let websocket = tokio::spawn(async move {
        for expected in ["QQBot stale", "QQBot fresh"] {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(
                    json!({"op":10,"d":{"heartbeat_interval":60000}}).to_string(),
                ))
                .await
                .unwrap();
            let identify = socket.next().await.unwrap().unwrap().into_text().unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&identify).unwrap()["d"]["token"],
                expected
            );
            if expected == "QQBot stale" {
                socket
                    .close(Some(CloseFrame {
                        code: CloseCode::Library(4004),
                        reason: "expired".into(),
                    }))
                    .await
                    .unwrap();
            } else {
                socket
                    .send(Message::Text(json!({"op":0,"t":"READY"}).to_string()))
                    .await
                    .unwrap();
                socket.close(None).await.unwrap();
            }
        }
    });
    let (sender, endpoint, refreshes, server) =
        api(url.clone(), StatusCode::UNAUTHORIZED, StatusCode::OK).await;
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("db.sqlite");
    let health = QqHealth::new(Health::new(vec![]));
    assert!(timeout(
        Duration::from_secs(3),
        run_refreshing_gateway_observed(
            sender.clone(),
            &endpoint,
            Some(&url),
            database.to_str().unwrap(),
            &health
        )
    )
    .await
    .unwrap()
    .is_err());
    timeout(
        Duration::from_secs(3),
        run_refreshing_gateway_observed(
            sender.clone(),
            &endpoint,
            Some(&url),
            database.to_str().unwrap(),
            &health,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    websocket.await.unwrap();
    sender.send("user", "notification").await.unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    server.abort();
}
