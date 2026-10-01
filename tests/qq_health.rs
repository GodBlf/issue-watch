use issue_watch::{
    health::{Health, HealthStatus},
    qq_health::QqHealth,
    Store,
};

#[test]
fn startup_distinguishes_unbound_and_unverified_delivery() {
    let store = Store::open_in_memory().unwrap();
    let health = Health::new(vec![]);
    let qq = QqHealth::new(health.clone());
    qq.refresh_queue(&store).unwrap();
    let snapshot = health.snapshot();
    assert_eq!(
        snapshot.components["qq_delivery"].status,
        HealthStatus::Unknown
    );
    assert_eq!(
        snapshot.components["qq_binding"].status,
        HealthStatus::Warning
    );
    assert_eq!(
        snapshot.components["qq_delivery"].details["result"],
        "unverified"
    );
    assert_eq!(
        snapshot.components["notification_queue"].details["pending"],
        0
    );
}

use async_trait::async_trait;
use issue_watch::{
    model::IssueNotification,
    qq::{MessageSink, SendError},
    queue::deliver_pending_observed,
};
struct Sink(std::sync::Mutex<Option<Result<(), SendError>>>);
#[test]
fn enqueue_and_binding_changes_are_visible_before_a_poll_finishes() {
    let health = Health::new(vec![]);
    let _qq = QqHealth::new(health.clone());
    let store = Store::open_in_memory()
        .unwrap()
        .with_health(health.clone())
        .unwrap();
    store.bind_user("private_openid").unwrap();
    store.insert_notification(&issue(1)).unwrap();
    let snapshot = health.snapshot();
    assert_eq!(
        snapshot.components["notification_queue"].details["pending"],
        1
    );
    assert_eq!(snapshot.components["qq_binding"].details["bound"], true);
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("private_openid"));
}
#[test]
fn legacy_queue_times_remain_unknown_instead_of_copying_issue_creation_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.sqlite");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE issue_notifications(repository TEXT NOT NULL,number INTEGER NOT NULL,title TEXT NOT NULL,author TEXT NOT NULL,created_at TEXT NOT NULL,url TEXT NOT NULL,state TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,next_attempt_at TEXT,last_error TEXT,PRIMARY KEY(repository,number)); INSERT INTO issue_notifications(repository,number,title,author,created_at,url,state,last_error) VALUES('o/r',1,'hello','a','2020-01-01T00:00:00Z','https://example.com','permanent_failure','rejected');").unwrap();
    connection.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL); INSERT INTO settings VALUES('qq_user_openid','legacy-user');").unwrap();
    drop(connection);
    let store = Store::open(&path).unwrap();
    let health = Health::new(vec![]);
    QqHealth::new(health.clone()).refresh_queue(&store).unwrap();
    let snapshot = health.snapshot();
    let queue = &snapshot.components["notification_queue"];
    assert_eq!(
        queue.details["failures"][0]["enqueued_at"],
        serde_json::Value::Null
    );
    assert_eq!(
        queue.details["failures"][0]["sent_at"],
        serde_json::Value::Null
    );
    assert_eq!(queue.details["permanent_failed"], 1);
}
#[async_trait]
impl MessageSink for Sink {
    async fn reply(
        &self,
        user_openid: &str,
        text: &str,
        _: &str,
    ) -> std::result::Result<(), SendError> {
        self.send(user_openid, text).await
    }

    async fn send(&self, _: &str, _: &str) -> Result<(), SendError> {
        self.0.lock().unwrap().take().unwrap()
    }
}
fn issue(number: u64) -> IssueNotification {
    IssueNotification {
        repository: "o/r".into(),
        number,
        title: "hello".into(),
        author: "a".into(),
        created_at: chrono::Utc::now(),
        url: "https://example.com".into(),
    }
}
#[tokio::test]
async fn successful_send_does_not_hide_unresolved_permanent_failures() {
    let store = Store::open_in_memory().unwrap();
    store.bind_user("u").unwrap();
    store.insert_notification(&issue(1)).unwrap();
    let health = Health::new(vec!["private".into()]);
    let qq = QqHealth::new(health.clone());
    deliver_pending_observed(
        &store,
        &Sink(std::sync::Mutex::new(Some(Err(SendError::Permanent(
            "private failed".into(),
        ))))),
        &qq,
    )
    .await
    .unwrap();
    store.insert_notification(&issue(2)).unwrap();
    deliver_pending_observed(&store, &Sink(std::sync::Mutex::new(Some(Ok(())))), &qq)
        .await
        .unwrap();
    let snapshot = health.snapshot();
    assert_eq!(snapshot.components["qq_delivery"].details["result"], "sent");
    assert_eq!(
        snapshot.components["notification_queue"].status,
        HealthStatus::Error
    );
    assert_eq!(
        snapshot.components["notification_queue"].details["permanent_failed"],
        1
    );
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("private"));
}
#[tokio::test]
async fn auth_failure_is_explicit_and_disconnect_does_not_invalidate_http_send() {
    let store = Store::open_in_memory().unwrap();
    store.bind_user("u").unwrap();
    store.insert_notification(&issue(1)).unwrap();
    let health = Health::new(vec![]);
    let qq = QqHealth::new(health.clone());
    qq.gateway(false, Some("closed"));
    deliver_pending_observed(
        &store,
        &Sink(std::sync::Mutex::new(Some(Err(SendError::Authentication(
            "HTTP 401".into(),
        ))))),
        &qq,
    )
    .await
    .unwrap();
    assert_eq!(
        health.snapshot().components["qq_auth"].status,
        HealthStatus::Error
    );
    store.insert_notification(&issue(2)).unwrap();
    deliver_pending_observed(&store, &Sink(std::sync::Mutex::new(Some(Ok(())))), &qq)
        .await
        .unwrap();
    let snapshot = health.snapshot();
    assert_eq!(
        snapshot.components["qq_gateway"].status,
        HealthStatus::Warning
    );
    assert_eq!(snapshot.components["qq_delivery"].details["result"], "sent");
}
#[tokio::test]
async fn retry_reason_and_queue_times_survive_restart_but_delivery_observation_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.sqlite");
    let store = Store::open(&path).unwrap();
    store.bind_user("u").unwrap();
    store.insert_notification(&issue(1)).unwrap();
    let health = Health::new(vec![]);
    let qq = QqHealth::new(health.clone());
    deliver_pending_observed(
        &store,
        &Sink(std::sync::Mutex::new(Some(Err(SendError::Retryable(
            "request timed out".into(),
        ))))),
        &qq,
    )
    .await
    .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    let restarted = Health::new(vec![]);
    QqHealth::new(restarted.clone())
        .refresh_queue(&store)
        .unwrap();
    let snapshot = restarted.snapshot();
    let queue = &snapshot.components["notification_queue"];
    assert_eq!(queue.status, HealthStatus::Warning);
    assert_eq!(queue.details["retrying"], 1);
    assert_eq!(queue.details["failures"][0]["error"], "request timed out");
    assert!(queue.details["failures"][0]["enqueued_at"].is_string());
    assert_eq!(
        queue.details["failures"][0]["sent_at"],
        serde_json::Value::Null
    );
    assert_eq!(
        snapshot.components["qq_delivery"].details["result"],
        "unverified"
    );
}
#[tokio::test]
async fn http_auth_denial_is_distinct_from_other_permanent_errors() {
    use axum::{routing::post, Router};
    use issue_watch::qq::QqHttpSender;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/{id}/messages",
                post(|| async { (axum::http::StatusCode::UNAUTHORIZED, "denied") }),
            ),
        )
        .await
        .unwrap()
    });
    let sender = QqHttpSender::with_endpoint("unused".into(), endpoint);
    assert!(matches!(
        sender.send("u", "hello").await,
        Err(SendError::Authentication(_))
    ));
    server.abort();
}

#[tokio::test]
async fn refreshing_sender_retries_after_authentication_denial() {
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
        Json, Router,
    };
    use issue_watch::qq::RefreshingQqHttpSender;
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[derive(Clone)]
    struct TestState(Arc<AtomicUsize>);
    async fn token() -> Json<serde_json::Value> {
        Json(json!({"access_token": "fresh-token"}))
    }
    async fn message(
        State(state): State<TestState>,
        headers: HeaderMap,
    ) -> (StatusCode, &'static str) {
        state.0.fetch_add(1, Ordering::SeqCst);
        if headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some("QQBot fresh-token")
        {
            (StatusCode::OK, "{}")
        } else {
            (StatusCode::UNAUTHORIZED, "denied")
        }
    }

    let requests = Arc::new(AtomicUsize::new(0));
    let state = TestState(requests.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/token", post(token))
                .route("/{id}/messages", post(message))
                .with_state(state),
        )
        .await
        .unwrap()
    });

    let sender = RefreshingQqHttpSender::with_endpoints(
        "stale-token".into(),
        "app-id".into(),
        "app-secret".into(),
        base.clone(),
        format!("{base}/token"),
    );
    sender.send("user", "hello").await.unwrap();
    assert_eq!(sender.send("user", "hello").await.unwrap(), ());
    assert_eq!(requests.load(Ordering::SeqCst), 3);
    server.abort();
}
#[tokio::test]
async fn body_timeout_is_retryable_and_visible_without_real_qq() {
    use axum::{body::Body, routing::post, Router};
    use issue_watch::qq::QqHttpSender;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/{id}/messages",
                post(|| async {
                    Body::from_stream(futures_util::stream::once(async {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        Ok::<_, std::io::Error>("{}")
                    }))
                }),
            ),
        )
        .await
        .unwrap()
    });
    let store = Store::open_in_memory().unwrap();
    store.bind_user("u").unwrap();
    store.insert_notification(&issue(1)).unwrap();
    let health = Health::new(vec![]);
    let qq = QqHealth::new(health.clone());
    let start = std::time::Instant::now();
    deliver_pending_observed(
        &store,
        &QqHttpSender::with_endpoint("unused".into(), endpoint),
        &qq,
    )
    .await
    .unwrap();
    assert!(start.elapsed() < std::time::Duration::from_secs(35));
    assert_eq!(
        health.snapshot().components["notification_queue"].details["retrying"],
        1
    );
    server.abort();
}
#[tokio::test]
async fn gateway_handshake_denial_records_auth_failure() {
    use axum::{routing::get, Router};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/", get(|| async { axum::http::StatusCode::FORBIDDEN })),
        )
        .await
        .unwrap()
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.sqlite");
    let health = Health::new(vec![]);
    let qq = QqHealth::new(health.clone());
    assert!(issue_watch::qq_gateway::run_gateway_observed(
        &url,
        "unused",
        path.to_str().unwrap(),
        &qq
    )
    .await
    .is_err());
    assert_eq!(
        health.snapshot().components["qq_auth"].status,
        HealthStatus::Error
    );
    server.abort();
}
#[tokio::test]
async fn qq_protocol_error_is_not_reported_as_success_or_inferred_auth_failure() {
    use axum::{routing::post, Json, Router};
    use issue_watch::qq::QqHttpSender;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/{id}/messages",
                post(|| async {
                    Json(serde_json::json!({"code":123456,"message":"destination unavailable"}))
                }),
            ),
        )
        .await
        .unwrap()
    });
    assert!(matches!(
        QqHttpSender::with_endpoint("unused".into(), endpoint)
            .send("u", "hello")
            .await,
        Err(SendError::Permanent(_))
    ));
    server.abort();
}
