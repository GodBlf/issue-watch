use async_trait::async_trait;
use chrono::{DateTime, Utc};
use issue_watch::{
    admin::{serve, Admin},
    health::Health,
    tracking::{Activity, IssueSnapshot, TrackingSource},
    Store,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

struct Source(Mutex<IssueSnapshot>, std::sync::atomic::AtomicBool);
#[async_trait]
impl TrackingSource for Source {
    async fn snapshot(&self, _: &str, _: u64) -> anyhow::Result<IssueSnapshot> {
        anyhow::ensure!(
            !self.1.load(std::sync::atomic::Ordering::SeqCst),
            "GitHub 暂时不可用"
        );
        Ok(self.0.lock().unwrap().clone())
    }
}
fn snapshot() -> IssueSnapshot {
    IssueSnapshot {
        title: "修复崩溃".into(),
        url: "https://github.com/owner/repo/issues/12".into(),
        state: "open".into(),
        is_pull_request: false,
        activities: vec![],
        linked_prs: vec![],
        partial_error: None,
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    base: String,
    client: reqwest::Client,
    source: Arc<Source>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite");
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "repositories=['owner/repo','owner/other']\ndatabase_path={:?}",
                path.to_str().unwrap().replace('\\', "/")
            ),
        )
        .unwrap();
        let health = Health::new(vec![]);
        let source = Arc::new(Source(
            Mutex::new(snapshot()),
            std::sync::atomic::AtomicBool::new(false),
        ));
        let admin = Admin::new(&config, health.clone())
            .unwrap()
            .with_tracking_source(source.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(serve(listener, health, admin));
        Self {
            _dir: dir,
            path,
            base,
            client: reqwest::Client::new(),
            source,
            task,
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
    async fn list(&self) -> Value {
        self.client
            .get(format!("{}/api/admin/tracking", self.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn add(&self) -> Value {
        let r = self
            .write(
                reqwest::Method::POST,
                "/api/admin/tracking",
                json!({"url":"https://github.com/owner/repo/issues/12"}),
            )
            .await;
        assert_eq!(r.status(), 201);
        r.json().await.unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[derive(Default)]
struct Sink(Mutex<Vec<(String, String)>>);
#[async_trait]
impl issue_watch::qq::MessageSink for Sink {
    async fn send(&self, user: &str, text: &str) -> Result<(), issue_watch::qq::SendError> {
        self.0.lock().unwrap().push((user.into(), text.into()));
        Ok(())
    }
    async fn reply(
        &self,
        user: &str,
        text: &str,
        _: &str,
    ) -> Result<(), issue_watch::qq::SendError> {
        self.send(user, text).await
    }
}
fn activity(key: &str, kind: &str, at: DateTime<Utc>) -> Activity {
    Activity {
        key: key.into(),
        kind: kind.into(),
        at,
        actor: "alice".into(),
        text: "修复已提交".into(),
        url: "https://github.com/owner/repo/issues/12#issuecomment-1".into(),
        related: None,
    }
}
#[tokio::test]
async fn administrator_manages_one_shared_tracking_and_readding_has_a_fresh_identity() {
    let f = Fixture::new().await;
    let first = f.add().await;
    assert_eq!(f.list().await[0]["title"], "修复崩溃");
    assert_eq!(
        f.write(
            reqwest::Method::POST,
            "/api/admin/tracking",
            json!({"url":"https://github.com/owner/repo/issues/12"})
        )
        .await
        .status(),
        200
    );
    let id = first["tracking"]["id"].as_i64().unwrap();
    assert_eq!(
        f.write(
            reqwest::Method::DELETE,
            &format!("/api/admin/tracking/{id}"),
            json!({})
        )
        .await
        .status(),
        200
    );
    assert!(f.list().await.as_array().unwrap().is_empty());
    assert_ne!(f.add().await["tracking"]["id"], id);
    let reloaded = Store::open(&f.path).unwrap();
    assert_eq!(reloaded.tracked_issues().unwrap().len(), 1);
}
#[tokio::test]
async fn comments_broadcast_once_to_discovery_recipients_and_manual_cancel_drops_pending() {
    let f = Fixture::new().await;
    let added = f.add().await;
    let mut store = Store::open(&f.path).unwrap();
    store.bind_user("alice").unwrap();
    let at = Utc::now() + chrono::Duration::seconds(1);
    f.source.0.lock().unwrap().activities = vec![
        activity("old", "comment", at - chrono::Duration::days(1)),
        activity("new", "comment", at),
    ];
    issue_watch::tracking::check_tracked(&mut store, f.source.as_ref())
        .await
        .unwrap();
    store.bind_user("bob").unwrap();
    let sink = Sink::default();
    issue_watch::tracking::deliver_tracking(&store, &sink)
        .await
        .unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    assert_eq!(sink.0.lock().unwrap()[0].0, "alice");
    assert!(sink.0.lock().unwrap()[0].1.contains("修复已提交"));
    issue_watch::tracking::check_tracked(&mut store, f.source.as_ref())
        .await
        .unwrap();
    issue_watch::tracking::deliver_tracking(&store, &sink)
        .await
        .unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    f.source.0.lock().unwrap().activities.push(activity(
        "later",
        "comment",
        at + chrono::Duration::seconds(1),
    ));
    issue_watch::tracking::check_tracked(&mut store, f.source.as_ref())
        .await
        .unwrap();
    f.write(
        reqwest::Method::DELETE,
        &format!("/api/admin/tracking/{}", added["tracking"]["id"]),
        json!({}),
    )
    .await;
    issue_watch::tracking::deliver_tracking(&store, &sink)
        .await
        .unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn tracking_interval_is_independent_defaults_to_five_minutes_and_validates_saves() {
    let f = Fixture::new().await;
    let config: Value = f
        .client
        .get(format!("{}/api/admin/config", f.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(config["saved"]["tracking_interval_seconds"], 300);
    let invalid = f
        .write(
            reqwest::Method::PATCH,
            "/api/admin/config",
            json!({"version":config["version"],"tracking_interval_seconds":0}),
        )
        .await;
    assert_eq!(invalid.status(), 400);
    let saved: Value = f
        .write(
            reqwest::Method::PATCH,
            "/api/admin/config",
            json!({"version":config["version"],"tracking_interval_seconds":90}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(saved["saved"]["tracking_interval_seconds"], 90);
    assert_eq!(saved["saved"]["poll_interval_seconds"], 60);
}
#[tokio::test]
async fn starting_second_uses_event_baseline_and_new_close_ends_tracking() {
    let f = Fixture::new().await;
    let at = Utc::now();
    f.source.0.lock().unwrap().activities = vec![activity("history", "comment", at)];
    f.add().await;
    let mut store = Store::open(&f.path).unwrap();
    store.bind_user("alice").unwrap();
    let start = store.tracked_issues().unwrap()[0].started_at;
    let second = DateTime::from_timestamp(start.timestamp(), 0).unwrap();
    f.source
        .0
        .lock()
        .unwrap()
        .activities
        .push(activity("new-close", "closed", second));
    issue_watch::tracking::check_tracked(&mut store, f.source.as_ref())
        .await
        .unwrap();
    assert!(f.list().await.as_array().unwrap().is_empty());
    let sink = Sink::default();
    issue_watch::tracking::deliver_tracking(&store, &sink)
        .await
        .unwrap();
    let messages = sink.0.lock().unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].1.contains("Issue 关闭"));
    assert!(!messages[0].1.contains("新评论"));
}
#[tokio::test]
async fn close_then_reopen_ends_tracking_but_final_notification_survives_restart() {
    let f = Fixture::new().await;
    f.add().await;
    let mut store = Store::open(&f.path).unwrap();
    store.bind_user("alice").unwrap();
    let at = Utc::now() + chrono::Duration::seconds(1);
    f.source.0.lock().unwrap().activities = vec![activity("close", "closed", at)];
    issue_watch::tracking::check_tracked(&mut store, f.source.as_ref())
        .await
        .unwrap();
    assert!(f.list().await.as_array().unwrap().is_empty());
    drop(store);
    let store = Store::open(&f.path).unwrap();
    let sink = Sink::default();
    issue_watch::tracking::deliver_tracking(&store, &sink)
        .await
        .unwrap();
    assert!(sink.0.lock().unwrap()[0].1.contains("Issue 关闭"));
    assert!(sink.0.lock().unwrap()[0].1.contains("open"));
    f.source.0.lock().unwrap().activities.clear();
    f.add().await;
    assert_eq!(f.list().await.as_array().unwrap().len(), 1);
}
#[tokio::test]
async fn backend_grants_independent_command_permissions_and_revocation_keeps_shared_tracking() {
    let f = Fixture::new().await;
    let mut store = Store::open(&f.path).unwrap();
    let command = "/track https://github.com/owner/repo/issues/12";
    let denied =
        issue_watch::tracking::tracking_command(&mut store, f.source.as_ref(), command, "alice")
            .await
            .unwrap()
            .unwrap();
    assert!(denied.starts_with("权限不足"));
    let grant = f
        .write(
            reqwest::Method::PUT,
            "/api/admin/permissions/alice",
            json!({"can_add":true,"can_cancel":false}),
        )
        .await;
    assert_eq!(grant.status(), 200);
    let added =
        issue_watch::tracking::tracking_command(&mut store, f.source.as_ref(), command, "alice")
            .await
            .unwrap()
            .unwrap();
    assert!(added.contains("已添加"));
    assert!(issue_watch::tracking::tracking_command(
        &mut store,
        f.source.as_ref(),
        "/untrack https://github.com/owner/repo/issues/12",
        "alice"
    )
    .await
    .unwrap()
    .unwrap()
    .starts_with("权限不足"));
    assert!(issue_watch::tracking::tracking_command(
        &mut store,
        f.source.as_ref(),
        "/tracking",
        "alice"
    )
    .await
    .unwrap()
    .unwrap()
    .contains("修复崩溃"));
    store.bind_user("alice").unwrap();
    store.unbind_user("alice").unwrap();
    assert!(store.tracking_permissions("alice").unwrap().can_add);
    f.write(
        reqwest::Method::PUT,
        "/api/admin/permissions/alice",
        json!({"can_add":false,"can_cancel":false}),
    )
    .await;
    assert!(issue_watch::tracking::tracking_command(
        &mut store,
        f.source.as_ref(),
        "/tracking",
        "alice"
    )
    .await
    .unwrap()
    .unwrap()
    .starts_with("权限不足"));
    assert_eq!(f.list().await.as_array().unwrap().len(), 1);
}
#[tokio::test]
async fn accepted_repository_removal_cancels_tracking_but_keeps_new_issue_deliveries() {
    let f = Fixture::new().await;
    f.add().await;
    let mut store = Store::open(&f.path).unwrap();
    store.bind_user("alice").unwrap();
    f.source.0.lock().unwrap().activities = vec![activity(
        "pending",
        "comment",
        Utc::now() + chrono::Duration::seconds(1),
    )];
    issue_watch::tracking::check_tracked(&mut store, f.source.as_ref())
        .await
        .unwrap();
    store
        .insert_notification(&issue_watch::IssueNotification {
            repository: "owner/repo".into(),
            number: 77,
            title: "new issue".into(),
            author: "a".into(),
            created_at: Utc::now(),
            url: "https://github.com/owner/repo/issues/77".into(),
        })
        .unwrap();
    let config = f._dir.path().join("config.toml");
    let active = issue_watch::config::FileConfig::load(&config).unwrap();
    let mut reload = issue_watch::reload::ConfigReloader::new(&config, active);
    std::fs::write(
        &config,
        format!(
            "repositories=['owner/other']\ndatabase_path={:?}",
            f.path.to_str().unwrap().replace('\\', "/")
        ),
    )
    .unwrap();
    assert!(reload.check(&store, Utc::now()).unwrap());
    assert!(f.list().await.as_array().unwrap().is_empty());
    let sink = Sink::default();
    issue_watch::tracking::deliver_tracking(&store, &sink)
        .await
        .unwrap();
    assert!(sink.0.lock().unwrap().is_empty());
    assert_eq!(store.pending_deliveries(Utc::now()).unwrap().len(), 1);
}
#[tokio::test(start_paused = true)]
async fn independent_tracking_schedule_applies_changes_before_ready_ticks_without_resetting_polling(
) {
    use issue_watch::reload::{MonitoringEvent, MonitoringSchedule};
    let config = issue_watch::config::FileConfig::parse(
        "repositories=['owner/repo']\npoll_interval_seconds=60\ntracking_interval_seconds=300",
    )
    .unwrap();
    let mut schedule = MonitoringSchedule::new(&config);
    let (updates, mut receiver) = tokio::sync::watch::channel(config.clone());
    assert!(matches!(
        schedule.next(&mut receiver).await.unwrap(),
        MonitoringEvent::Poll
    ));
    assert!(matches!(
        schedule.next(&mut receiver).await.unwrap(),
        MonitoringEvent::Tracking
    ));
    let mut changed = config.clone();
    changed.tracking_interval_seconds = 10;
    updates.send(changed.clone()).unwrap();
    assert!(matches!(
        schedule.next(&mut receiver).await.unwrap(),
        MonitoringEvent::Configuration(_)
    ));
    schedule.apply(&changed);
    tokio::time::advance(std::time::Duration::from_secs(10)).await;
    assert!(matches!(
        schedule.next(&mut receiver).await.unwrap(),
        MonitoringEvent::Tracking
    ));
    tokio::time::advance(std::time::Duration::from_secs(50)).await;
    assert!(matches!(
        schedule.next(&mut receiver).await.unwrap(),
        MonitoringEvent::Poll
    ));
}
#[tokio::test]
async fn final_batch_excludes_activity_after_the_first_close() {
    let f = Fixture::new().await;
    f.add().await;
    let mut store = Store::open(&f.path).unwrap();
    store.bind_user("alice").unwrap();
    let at = Utc::now() + chrono::Duration::seconds(1);
    let mut late = activity("late", "comment", at + chrono::Duration::seconds(1));
    late.text = "不应该投递的关闭后评论".into();
    f.source.0.lock().unwrap().activities = vec![activity("close", "closed", at), late];
    issue_watch::tracking::check_tracked(&mut store, f.source.as_ref())
        .await
        .unwrap();
    let sink = Sink::default();
    issue_watch::tracking::deliver_tracking(&store, &sink)
        .await
        .unwrap();
    assert!(!sink.0.lock().unwrap()[0].1.contains("不应该投递"));
}
#[tokio::test]
#[ignore = "Interactive local browser fixture; no external GitHub or QQ traffic"]
async fn admin_browser_preview() {
    let f = Fixture::new().await;
    f.add().await;
    let store = Store::open(&f.path).unwrap();
    store.bind_user("preview-user").unwrap();
    store
        .set_tracking_permissions("preview-user", true, false)
        .unwrap();
    println!("ADMIN_PREVIEW_URL={}", f.base);
    std::future::pending::<()>().await;
}
#[tokio::test]
async fn failed_check_keeps_tracking_and_recovers_missed_activity_after_restart() {
    let f = Fixture::new().await;
    f.add().await;
    let mut store = Store::open(&f.path).unwrap();
    store.bind_user("alice").unwrap();
    f.source.1.store(true, std::sync::atomic::Ordering::SeqCst);
    issue_watch::tracking::check_tracked(&mut store, f.source.as_ref())
        .await
        .unwrap();
    assert!(f.list().await[0]["error"]
        .as_str()
        .unwrap()
        .contains("暂时不可用"));
    assert!(f.list().await[0]["last_success_at"].is_null());
    drop(store);
    let mut store = Store::open(&f.path).unwrap();
    f.source.1.store(false, std::sync::atomic::Ordering::SeqCst);
    f.source.0.lock().unwrap().activities = vec![activity(
        "missed",
        "comment",
        Utc::now() + chrono::Duration::seconds(1),
    )];
    issue_watch::tracking::check_tracked(&mut store, f.source.as_ref())
        .await
        .unwrap();
    let sink = Sink::default();
    issue_watch::tracking::deliver_tracking(&store, &sink)
        .await
        .unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    assert!(f.list().await[0]["error"].is_null());
}
#[tokio::test(start_paused = true)]
async fn partial_delivery_retries_only_failed_recipients_after_restart() {
    struct RetrySink {
        messages: Mutex<Vec<String>>,
        fail: std::sync::atomic::AtomicBool,
    }
    #[async_trait]
    impl issue_watch::qq::MessageSink for RetrySink {
        async fn send(&self, user: &str, _: &str) -> Result<(), issue_watch::qq::SendError> {
            self.messages.lock().unwrap().push(user.into());
            if user == "bob" && self.fail.swap(false, std::sync::atomic::Ordering::SeqCst) {
                Err(issue_watch::qq::SendError::Retryable("temporary".into()))
            } else {
                Ok(())
            }
        }
        async fn reply(
            &self,
            user: &str,
            text: &str,
            _: &str,
        ) -> Result<(), issue_watch::qq::SendError> {
            self.send(user, text).await
        }
    }
    let f = Fixture::new().await;
    f.add().await;
    let mut store = Store::open(&f.path).unwrap();
    store.bind_user("alice").unwrap();
    store.bind_user("bob").unwrap();
    let now = Utc::now();
    f.source.0.lock().unwrap().activities = vec![activity(
        "retry",
        "comment",
        now + chrono::Duration::seconds(1),
    )];
    issue_watch::tracking::check_tracked(&mut store, f.source.as_ref())
        .await
        .unwrap();
    let sink = RetrySink {
        messages: Mutex::new(vec![]),
        fail: std::sync::atomic::AtomicBool::new(true),
    };
    issue_watch::tracking::deliver_tracking_due(&store, &sink, now)
        .await
        .unwrap();
    assert_eq!(store.notification_summary().unwrap()["retrying"], 1);
    drop(store);
    let store = Store::open(&f.path).unwrap();
    issue_watch::tracking::deliver_tracking_due(&store, &sink, now + chrono::Duration::seconds(31))
        .await
        .unwrap();
    assert_eq!(*sink.messages.lock().unwrap(), ["alice", "bob", "bob"]);
    assert_eq!(store.notification_summary().unwrap()["retrying"], 0);
}
