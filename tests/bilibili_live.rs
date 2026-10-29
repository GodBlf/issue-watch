mod support;

use axum::{extract::Query, routing::get, Json, Router};
use issue_watch::{admin::Admin, health::Health, Store};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

struct Fixture {
    _dir: tempfile::TempDir,
    database: std::path::PathBuf,
    base: String,
    client: reqwest::Client,
    source_base: String,
    snapshot: Arc<Mutex<Value>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    requests: Arc<std::sync::atomic::AtomicUsize>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("state.sqlite");
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "repositories=['o/r']\ndatabase_path={:?}",
                database.to_str().unwrap()
            ),
        )
        .unwrap();
        let snapshot = Arc::new(Mutex::new(
            json!({"room_id":7734200,"short_id":6,"uid":42,"live_status":0,"live_time":0,"title":"测试直播"}),
        ));
        let data = snapshot.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_base = format!("http://{}", listener.local_addr().unwrap());
        let info_data = snapshot.clone();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gate_entered = entered.clone();
        let gate_release = release.clone();
        let request_count = requests.clone();
        let source = tokio::spawn(async move {
            let router =
                Router::new()
                    .route(
                        "/room/v1/Room/room_init",
                        get(move |Query(q): Query<HashMap<String, String>>| {
                            let data = data.clone();
                            let entered = gate_entered.clone();
                            let release = gate_release.clone();
                            let count = request_count.clone();
                            async move {
                                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                let snapshot = data.lock().unwrap().clone();
                                if snapshot["hold"] == true {
                                    entered.notify_one();
                                    release.notified().await;
                                }
                                if snapshot["fail"] == true {
                                    return Json(json!({"code":-412}));
                                }
                                if q.get("id").is_some_and(|id| id == "999") {
                                    Json(json!({"code":1,"message":"missing"}))
                                } else {
                                    Json(json!({"code":0,"data":snapshot}))
                                }
                            }
                        }),
                    )
                    .route(
                        "/room/v1/Room/get_info",
                        get({
                            let data = info_data.clone();
                            move || {
                                let data = data.clone();
                                async move {
                                    Json(json!({"code":0,"data":data.lock().unwrap().clone()}))
                                }
                            }
                        }),
                    )
                    .route(
                        "/live_user/v1/Master/info",
                        get(|| async {
                            Json(json!({"code":0,"data":{"info":{"uname":"测试主播"}}}))
                        }),
                    );
            axum::serve(listener, router).await.unwrap();
        });
        // The source is a real local HTTP boundary; no production credentials are used.
        let health = Health::new(vec![]);
        let admin = Admin::new(&config, health.clone())
            .unwrap()
            .with_live_source(Arc::new(
                issue_watch::bilibili::BilibiliClient::with_endpoint(&source_base).unwrap(),
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (client, server) =
            support::authenticated_server(listener, health, admin, &database).await;
        let server = tokio::spawn(async move {
            server.await.unwrap().unwrap();
        });
        Self {
            _dir: dir,
            database,
            base,
            client,
            source_base,
            snapshot,
            tasks: vec![source, server],
            entered,
            release,
            requests,
        }
    }
    async fn write(&self, method: reqwest::Method, path: &str, body: Value) -> reqwest::Response {
        self.client
            .request(method, format!("{}{path}", self.base))
            .header("origin", &self.base)
            .header("x-issue-watch-admin", "1")
            .json(&body)
            .send()
            .await
            .unwrap()
    }
    async fn rows(&self, path: &str) -> Value {
        self.client
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn target(&self, user: &str) -> i64 {
        self.write(
            reqwest::Method::POST,
            "/api/admin/subscriptions",
            json!({"user_openid":user}),
        )
        .await
        .error_for_status()
        .unwrap();
        self.rows("/api/admin/subscriptions")
            .await
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["user_openid"] == user)
            .unwrap()["id"]
            .as_i64()
            .unwrap()
    }
}

#[tokio::test]
async fn administrator_manages_normalized_live_subscriptions_without_affecting_repositories() {
    let f = Fixture::new().await;
    let id = f.target("alice").await;
    let path = format!("/api/admin/subscriptions/{id}/bilibili");
    for input in ["6", "7734200", "https://live.bilibili.com/6"] {
        let response = f
            .write(reqwest::Method::POST, &path, json!({"room":input}))
            .await;
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
    }
    let rows = f.rows(&path).await;
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["room_id"], 7734200);
    assert_eq!(rows[0]["anchor"], "测试主播");
    assert!(!f
        .write(reqwest::Method::POST, &path, json!({"room":"999"}))
        .await
        .status()
        .is_success());
    assert_eq!(f.rows(&path).await.as_array().unwrap().len(), 1);
    let store = Store::open(&f.database).unwrap();
    assert!(store.subscriptions().unwrap()[0].repositories.is_empty());
    f.write(
        reqwest::Method::DELETE,
        &format!("{path}/7734200"),
        json!({}),
    )
    .await
    .error_for_status()
    .unwrap();
    assert_eq!(f.rows(&path).await, json!([]));
}

#[derive(Default)]
struct Sink(Mutex<Vec<(String, String)>>);
#[async_trait::async_trait]
impl issue_watch::qq::MessageSink for Sink {
    async fn send(&self, user: &str, text: &str) -> Result<(), issue_watch::qq::SendError> {
        self.0.lock().unwrap().push((user.into(), text.into()));
        Ok(())
    }
    async fn reply(&self, _: &str, _: &str, _: &str) -> Result<(), issue_watch::qq::SendError> {
        unreachable!()
    }
}
#[tokio::test]
async fn shared_monitoring_notifies_existing_subscribers_once_per_session_and_skips_late_joiners() {
    let f = Fixture::new().await;
    let alice = f.target("alice").await;
    let path = format!("/api/admin/subscriptions/{alice}/bilibili");
    f.write(reqwest::Method::POST, &path, json!({"room":"6"}))
        .await
        .error_for_status()
        .unwrap();
    let live = issue_watch::bilibili::LiveMonitoring::new(
        &f.database,
        Arc::new(issue_watch::bilibili::BilibiliClient::with_endpoint(&f.source_base).unwrap()),
    );
    let sink = Sink::default();
    f.snapshot.lock().unwrap()["live_status"] = json!(1);
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000000);
    live.check().await.unwrap();
    live.deliver(&sink).await.unwrap();
    live.check().await.unwrap();
    live.deliver(&sink).await.unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    assert_eq!(sink.0.lock().unwrap()[0].0, "alice");
    assert!(sink.0.lock().unwrap()[0].1.contains("测试主播"));
    let bob = f.target("bob").await;
    f.write(
        reqwest::Method::POST,
        &format!("/api/admin/subscriptions/{bob}/bilibili"),
        json!({"room":"6"}),
    )
    .await
    .error_for_status()
    .unwrap();
    live.check().await.unwrap();
    live.deliver(&sink).await.unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000100);
    live.check().await.unwrap();
    live.deliver(&sink).await.unwrap();
    let received: Vec<_> = sink.0.lock().unwrap().iter().map(|m| m.0.clone()).collect();
    assert_eq!(received, ["alice", "alice", "bob"]);
}

#[tokio::test]
async fn qq_commands_create_targets_only_after_validation_and_share_admin_subscriptions() {
    let f = Fixture::new().await;
    let live = issue_watch::bilibili::LiveMonitoring::new(
        &f.database,
        Arc::new(issue_watch::bilibili::BilibiliClient::with_endpoint(&f.source_base).unwrap()),
    );
    let failed = live
        .command("/bilibili 999", "new-user")
        .await
        .unwrap()
        .unwrap();
    assert!(failed.contains("失败"));
    assert_eq!(f.rows("/api/admin/subscriptions").await, json!([]));
    let reply = live
        .command("/bilibili https://live.bilibili.com/6", "alice")
        .await
        .unwrap()
        .unwrap();
    assert!(reply.contains("已订阅"));
    assert!(live
        .command("/bilibili", "alice")
        .await
        .unwrap()
        .unwrap()
        .contains("7734200"));
    let rows = f.rows("/api/admin/subscriptions").await;
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["live_subscriptions"].as_array().unwrap().len(), 1);
    assert!(live
        .command("/bilibili", "bob")
        .await
        .unwrap()
        .unwrap()
        .contains("未订阅"));
    live.command("/unbilibili 6", "alice").await.unwrap();
    assert!(live
        .command("/bilibili", "alice")
        .await
        .unwrap()
        .unwrap()
        .contains("未订阅"));
    live.command("/bilibili 6", "alice").await.unwrap();
    issue_watch::qq::private_command(&Store::open(&f.database).unwrap(), "/unbind", "alice")
        .unwrap();
    assert_eq!(f.rows("/api/admin/subscriptions").await, json!([]));
    live.command("/bilibili 6", "alice").await.unwrap();
    assert_eq!(
        f.rows("/api/admin/subscriptions").await[0]["live_subscriptions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

struct PartialSink {
    sent: Mutex<Vec<String>>,
    fail_bob: std::sync::atomic::AtomicBool,
}
#[async_trait::async_trait]
impl issue_watch::qq::MessageSink for PartialSink {
    async fn send(&self, user: &str, _: &str) -> Result<(), issue_watch::qq::SendError> {
        if user == "bob" && self.fail_bob.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(issue_watch::qq::SendError::Retryable("temporary".into()));
        }
        self.sent.lock().unwrap().push(user.into());
        Ok(())
    }
    async fn reply(&self, _: &str, _: &str, _: &str) -> Result<(), issue_watch::qq::SendError> {
        unreachable!()
    }
}
#[tokio::test]
async fn retry_recovery_preserves_recipient_results_and_original_expiration() {
    use std::sync::atomic::{AtomicI64, Ordering};
    let f = Fixture::new().await;
    let now = Arc::new(AtomicI64::new(1000));
    let live = issue_watch::bilibili::LiveMonitoring::new(
        &f.database,
        Arc::new(issue_watch::bilibili::BilibiliClient::with_endpoint(&f.source_base).unwrap()),
    )
    .with_clock({
        let now = now.clone();
        move || now.load(Ordering::SeqCst)
    });
    live.command("/bilibili 6", "alice").await.unwrap();
    live.command("/bilibili 6", "bob").await.unwrap();
    f.snapshot.lock().unwrap()["live_status"] = json!(1);
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000000);
    live.check().await.unwrap();
    let sink = PartialSink {
        sent: Mutex::new(vec![]),
        fail_bob: std::sync::atomic::AtomicBool::new(true),
    };
    live.deliver(&sink).await.unwrap();
    assert_eq!(*sink.sent.lock().unwrap(), ["alice"]);
    sink.fail_bob.store(false, Ordering::SeqCst);
    now.store(1029, Ordering::SeqCst);
    live.deliver(&sink).await.unwrap();
    assert_eq!(sink.sent.lock().unwrap().len(), 1);
    let recovered = issue_watch::bilibili::LiveMonitoring::new(
        &f.database,
        Arc::new(issue_watch::bilibili::BilibiliClient::with_endpoint(&f.source_base).unwrap()),
    )
    .with_clock({
        let now = now.clone();
        move || now.load(Ordering::SeqCst)
    });
    recovered.recover().await.unwrap();
    now.store(1030, Ordering::SeqCst);
    recovered.check().await.unwrap();
    recovered.deliver(&sink).await.unwrap();
    assert_eq!(*sink.sent.lock().unwrap(), ["alice", "bob"]);
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000100);
    recovered.check().await.unwrap();
    now.store(1631, Ordering::SeqCst);
    recovered.recover().await.unwrap();
    recovered.deliver(&sink).await.unwrap();
    assert_eq!(
        sink.sent.lock().unwrap().len(),
        2,
        "old discovered-at must survive recovery"
    );
}

#[tokio::test]
async fn offline_new_session_and_unsubscribe_cancel_pending_notifications_and_errors_preserve_state(
) {
    let f = Fixture::new().await;
    let live = issue_watch::bilibili::LiveMonitoring::new(
        &f.database,
        Arc::new(issue_watch::bilibili::BilibiliClient::with_endpoint(&f.source_base).unwrap()),
    );
    live.command("/bilibili 6", "alice").await.unwrap();
    let sink = Sink::default();
    f.snapshot.lock().unwrap()["live_status"] = json!(1);
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000000);
    live.check().await.unwrap();
    f.snapshot.lock().unwrap()["live_status"] = json!(0);
    live.check().await.unwrap();
    live.deliver(&sink).await.unwrap();
    assert!(sink.0.lock().unwrap().is_empty());
    f.snapshot.lock().unwrap()["live_status"] = json!(1);
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000100);
    live.check().await.unwrap();
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000200);
    live.check().await.unwrap();
    live.deliver(&sink).await.unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    f.snapshot.lock().unwrap()["live_status"] = json!(99);
    live.check().await.unwrap();
    let diagnostics = f.rows("/api/admin/bilibili").await;
    assert_eq!(diagnostics["rooms"][0]["live_status"], 1);
    assert!(diagnostics["rooms"][0]["error"].is_string());
    f.snapshot.lock().unwrap()["live_status"] = json!(1);
    live.check().await.unwrap();
    live.deliver(&sink).await.unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000300);
    live.check().await.unwrap();
    live.command("/unbilibili 6", "alice").await.unwrap();
    live.deliver(&sink).await.unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    live.command("/bilibili 6", "alice").await.unwrap();
    live.check().await.unwrap();
    live.deliver(&sink).await.unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    issue_watch::qq::private_command(&Store::open(&f.database).unwrap(), "/unbind", "alice")
        .unwrap();
    assert_eq!(f.rows("/api/admin/bilibili").await["rooms"], json!([]));
}

#[tokio::test]
async fn check_failure_keeps_live_state_and_last_unsubscribe_stops_external_queries() {
    use std::sync::atomic::Ordering;
    let f = Fixture::new().await;
    let live = issue_watch::bilibili::LiveMonitoring::new(
        &f.database,
        Arc::new(issue_watch::bilibili::BilibiliClient::with_endpoint(&f.source_base).unwrap()),
    );
    live.command("/bilibili 6", "alice").await.unwrap();
    f.snapshot.lock().unwrap()["live_status"] = json!(1);
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000000);
    live.check().await.unwrap();
    let sink = Sink::default();
    live.deliver(&sink).await.unwrap();
    f.snapshot.lock().unwrap()["fail"] = json!(true);
    live.check().await.unwrap();
    let data = f.rows("/api/admin/bilibili").await;
    assert_eq!(data["rooms"][0]["live_status"], 1);
    assert!(data["rooms"][0]["error"].is_string());
    f.snapshot.lock().unwrap()["fail"] = json!(false);
    live.check().await.unwrap();
    live.deliver(&sink).await.unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    live.command("/unbilibili 6", "alice").await.unwrap();
    let count = f.requests.load(Ordering::SeqCst);
    live.check().await.unwrap();
    assert_eq!(f.requests.load(Ordering::SeqCst), count);
}

struct GatedSink {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    sent: Mutex<Vec<String>>,
}
#[async_trait::async_trait]
impl issue_watch::qq::MessageSink for GatedSink {
    async fn wait_until_ready(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }
    async fn send(&self, user: &str, _: &str) -> Result<(), issue_watch::qq::SendError> {
        self.sent.lock().unwrap().push(user.into());
        Ok(())
    }
    async fn reply(&self, _: &str, _: &str, _: &str) -> Result<(), issue_watch::qq::SendError> {
        unreachable!()
    }
}
#[tokio::test]
async fn cancelling_while_waiting_to_send_prevents_submission_and_interrupted_claim_recovers() {
    let f = Fixture::new().await;
    let live = issue_watch::bilibili::LiveMonitoring::new(
        &f.database,
        Arc::new(issue_watch::bilibili::BilibiliClient::with_endpoint(&f.source_base).unwrap()),
    );
    live.command("/bilibili 6", "alice").await.unwrap();
    f.snapshot.lock().unwrap()["live_status"] = json!(1);
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000000);
    live.check().await.unwrap();
    let gate = Arc::new(GatedSink {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        sent: Mutex::new(vec![]),
    });
    let task = tokio::spawn({
        let live = live.clone();
        let gate = gate.clone();
        async move { live.deliver(gate.as_ref()).await.unwrap() }
    });
    gate.entered.notified().await;
    live.command("/unbilibili 6", "alice").await.unwrap();
    gate.release.notify_one();
    task.await.unwrap();
    assert!(gate.sent.lock().unwrap().is_empty());
    live.command("/bilibili 6", "alice").await.unwrap();
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000100);
    live.check().await.unwrap();
    let task = tokio::spawn({
        let live = live.clone();
        let gate = gate.clone();
        async move { live.deliver(gate.as_ref()).await.unwrap() }
    });
    gate.entered.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    live.recover().await.unwrap();
    let sink = Sink::default();
    live.deliver(&sink).await.unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn older_inflight_check_cannot_replace_a_newer_observation_or_resurrect_retired_deliveries() {
    let f = Fixture::new().await;
    let live = issue_watch::bilibili::LiveMonitoring::new(
        &f.database,
        Arc::new(issue_watch::bilibili::BilibiliClient::with_endpoint(&f.source_base).unwrap()),
    );
    live.command("/bilibili 6", "alice").await.unwrap();
    f.snapshot.lock().unwrap()["live_status"] = json!(1);
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000000);
    f.snapshot.lock().unwrap()["hold"] = json!(true);
    let task = tokio::spawn({
        let live = live.clone();
        async move { live.check().await.unwrap() }
    });
    f.entered.notified().await;
    f.snapshot.lock().unwrap()["hold"] = json!(false);
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000100);
    live.check().await.unwrap();
    f.release.notify_one();
    task.await.unwrap();
    let sink = Sink::default();
    live.deliver(&sink).await.unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    assert_eq!(
        f.rows("/api/admin/bilibili").await["rooms"][0]["live_time"],
        1700000100
    );
}

struct PermanentSink;
#[async_trait::async_trait]
impl issue_watch::qq::MessageSink for PermanentSink {
    async fn send(&self, _: &str, _: &str) -> Result<(), issue_watch::qq::SendError> {
        Err(issue_watch::qq::SendError::Permanent(
            "platform denied private message".into(),
        ))
    }
    async fn reply(&self, _: &str, _: &str, _: &str) -> Result<(), issue_watch::qq::SendError> {
        unreachable!()
    }
}
#[tokio::test]
async fn permanent_delivery_failure_is_visible_to_authenticated_admin_and_does_not_retry() {
    let f = Fixture::new().await;
    let live = issue_watch::bilibili::LiveMonitoring::new(
        &f.database,
        Arc::new(issue_watch::bilibili::BilibiliClient::with_endpoint(&f.source_base).unwrap()),
    );
    live.command("/bilibili 6", "alice").await.unwrap();
    f.snapshot.lock().unwrap()["live_status"] = json!(1);
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000000);
    live.check().await.unwrap();
    live.deliver(&PermanentSink).await.unwrap();
    let sink = Sink::default();
    live.deliver(&sink).await.unwrap();
    assert!(sink.0.lock().unwrap().is_empty());
    let data = f.rows("/api/admin/bilibili").await;
    assert_eq!(data["failures"].as_array().unwrap().len(), 1);
    assert_eq!(data["failures"][0]["room_id"], 7734200);
    assert!(data["failures"][0]["error"]
        .as_str()
        .unwrap()
        .contains("永久"));
    assert_eq!(
        reqwest::Client::new()
            .get(format!("{}/api/admin/bilibili", f.base))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let id = f.rows("/api/admin/subscriptions").await[0]["id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        f.client
            .post(format!("{}/api/admin/subscriptions/{id}/bilibili", f.base))
            .json(&json!({"room":"6"}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
}

struct ClockedSource(tokio::sync::mpsc::UnboundedSender<tokio::time::Instant>);
#[async_trait::async_trait]
impl issue_watch::bilibili::LiveSource for ClockedSource {
    async fn snapshot(&self, _: u64) -> anyhow::Result<issue_watch::bilibili::LiveSnapshot> {
        self.0.send(tokio::time::Instant::now()).unwrap();
        Ok(issue_watch::bilibili::LiveSnapshot {
            room_id: 6,
            anchor: "主播".into(),
            title: "标题".into(),
            live_status: 0,
            live_time: None,
        })
    }
}
#[tokio::test(start_paused = true)]
async fn monitoring_runs_one_shared_check_every_two_minutes_and_stops_after_last_exit() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("live.sqlite");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let live = issue_watch::bilibili::LiveMonitoring::new(&database, Arc::new(ClockedSource(tx)));
    live.command("/bilibili 6", "alice").await.unwrap();
    live.command("/bilibili 6", "bob").await.unwrap();
    rx.try_recv().unwrap();
    rx.try_recv().unwrap();
    let task = tokio::spawn({
        let live = live.clone();
        async move { live.run_checks().await.unwrap() }
    });
    let first = rx.recv().await.unwrap();
    tokio::time::advance(std::time::Duration::from_secs(119)).await;
    tokio::task::yield_now().await;
    assert!(rx.try_recv().is_err());
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert_eq!(
        rx.recv().await.unwrap() - first,
        std::time::Duration::from_secs(120)
    );
    assert!(rx.try_recv().is_err());
    let store = Store::open(&database).unwrap();
    store.unbind_user("alice").unwrap();
    store.unbind_user("bob").unwrap();
    tokio::time::advance(std::time::Duration::from_secs(120)).await;
    tokio::task::yield_now().await;
    assert!(rx.try_recv().is_err());
    task.abort();
}

struct ReplySink(tokio::sync::mpsc::UnboundedSender<(String, String, String)>);
#[async_trait::async_trait]
impl issue_watch::qq::MessageSink for ReplySink {
    async fn send(&self, _: &str, _: &str) -> Result<(), issue_watch::qq::SendError> {
        unreachable!()
    }
    async fn reply(
        &self,
        user: &str,
        text: &str,
        message: &str,
    ) -> Result<(), issue_watch::qq::SendError> {
        self.0
            .send((user.into(), text.into(), message.into()))
            .unwrap();
        Err(issue_watch::qq::SendError::Retryable("reply failed".into()))
    }
}
#[tokio::test]
async fn gateway_keeps_heartbeats_while_live_validation_is_slow_and_commits_before_reply_failure() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let f = Fixture::new().await;
    f.snapshot.lock().unwrap()["hold"] = json!(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (heartbeats, mut heartbeat_rx) = tokio::sync::mpsc::unbounded_channel();
    let ws = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(
                json!({"op":10,"d":{"heartbeat_interval":25}}).to_string(),
            ))
            .await
            .unwrap();
        let identify = socket.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(identify.to_text().unwrap()).unwrap()["op"],
            2
        );
        socket.send(Message::Text(json!({"op":0,"t":"C2C_MESSAGE_CREATE","d":{"id":"live-message","content":"/bilibili 6","author":{"user_openid":"alice"}}}).to_string())).await.unwrap();
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let v: Value = serde_json::from_str(&text).unwrap();
            if v["op"] == 1 {
                heartbeats.send(()).unwrap();
            }
        }
    });
    let (tx, mut replies) = tokio::sync::mpsc::unbounded_channel();
    let gateway = tokio::spawn({
        let database = f.database.clone();
        let source_base = f.source_base.clone();
        async move {
            issue_watch::qq_gateway::run_gateway_with_live_source(
                &url,
                "unused",
                database.to_str().unwrap(),
                Arc::new(ReplySink(tx)),
                Arc::new(
                    issue_watch::bilibili::BilibiliClient::with_endpoint(&source_base).unwrap(),
                ),
            )
            .await
            .unwrap();
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), f.entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), heartbeat_rx.recv())
        .await
        .unwrap()
        .unwrap();
    f.release.notify_one();
    let (user, text, message) =
        tokio::time::timeout(std::time::Duration::from_secs(3), replies.recv())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(user, "alice");
    assert_eq!(message, "live-message");
    assert!(text.contains("已订阅"));
    let rows = f.rows("/api/admin/subscriptions").await;
    assert_eq!(rows[0]["live_subscriptions"][0]["room_id"], 7734200);
    gateway.abort();
    ws.abort();
}

#[tokio::test]
async fn cancelling_saved_real_or_short_room_does_not_require_bilibili_to_be_available() {
    let f = Fixture::new().await;
    let live = issue_watch::bilibili::LiveMonitoring::new(
        &f.database,
        Arc::new(issue_watch::bilibili::BilibiliClient::with_endpoint(&f.source_base).unwrap()),
    );
    live.command("/bilibili 6", "alice").await.unwrap();
    f.snapshot.lock().unwrap()["fail"] = json!(true);
    let reply = live
        .command("/unbilibili 7734200", "alice")
        .await
        .unwrap()
        .unwrap();
    assert!(reply.contains("已取消"), "{reply}");
    f.snapshot.lock().unwrap()["fail"] = json!(false);
    live.command("/bilibili 6", "alice").await.unwrap();
    f.snapshot.lock().unwrap()["fail"] = json!(true);
    let reply = live
        .command("/unbilibili 6", "alice")
        .await
        .unwrap()
        .unwrap();
    assert!(reply.contains("已取消"), "{reply}");
}

#[tokio::test]
async fn abandoned_send_claim_is_retried_after_lease_expires_without_process_restart() {
    use std::sync::atomic::{AtomicI64, Ordering};
    let f = Fixture::new().await;
    let now = Arc::new(AtomicI64::new(1000));
    let live = issue_watch::bilibili::LiveMonitoring::new(
        &f.database,
        Arc::new(issue_watch::bilibili::BilibiliClient::with_endpoint(&f.source_base).unwrap()),
    )
    .with_clock({
        let now = now.clone();
        move || now.load(Ordering::SeqCst)
    });
    live.command("/bilibili 6", "alice").await.unwrap();
    f.snapshot.lock().unwrap()["live_status"] = json!(1);
    f.snapshot.lock().unwrap()["live_time"] = json!(1700000000);
    live.check().await.unwrap();
    let gate = Arc::new(GatedSink {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        sent: Mutex::new(vec![]),
    });
    let task = tokio::spawn({
        let live = live.clone();
        let gate = gate.clone();
        async move { live.deliver(gate.as_ref()).await.unwrap() }
    });
    gate.entered.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    now.store(1121, Ordering::SeqCst);
    let sink = Sink::default();
    live.deliver(&sink).await.unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn sqlite_lock_wait_does_not_block_async_heartbeat_work() {
    let f = Fixture::new().await;
    let live = issue_watch::bilibili::LiveMonitoring::new(
        &f.database,
        Arc::new(issue_watch::bilibili::BilibiliClient::with_endpoint(&f.source_base).unwrap()),
    );
    live.command("/bilibili 6", "alice").await.unwrap();
    let lock = rusqlite::Connection::open(&f.database).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let started = std::time::Instant::now();
    let command = tokio::spawn({
        let live = live.clone();
        async move { live.command("/bilibili", "alice").await.unwrap() }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "SQLite busy waits must not run on the async executor"
    );
    lock.execute_batch("ROLLBACK").unwrap();
    assert!(command.await.unwrap().unwrap().contains("7734200"));
}
