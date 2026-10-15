use async_trait::async_trait;
use chrono::{Duration, Utc};
use issue_watch::tracking::{
    check_tracked, deliver_tracking, Activity, IssueSnapshot, TrackingSource,
};
use issue_watch::{
    admin::{serve, Admin},
    health::Health,
    qq::{MessageSink, SendError},
    queue::deliver_pending,
    IssueNotification, Store,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    base: String,
    client: reqwest::Client,
    source: Arc<Source>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite");
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "repositories=['o/a','o/b']\ndatabase_path={:?}",
                path.to_str().unwrap()
            ),
        )
        .unwrap();
        let health = Health::new(vec![]);
        let source = Arc::new(Source(Mutex::new(vec![])));
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
    async fn rows(&self) -> Vec<Value> {
        self.client
            .get(format!("{}/api/admin/subscriptions", self.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn add(&self, user: &str) -> i64 {
        assert!(self
            .write(
                reqwest::Method::POST,
                "/api/admin/subscriptions",
                json!({"user_openid":user})
            )
            .await
            .status()
            .is_success());
        self.rows()
            .await
            .iter()
            .find(|r| r["user_openid"] == user)
            .unwrap()["id"]
            .as_i64()
            .unwrap()
    }
    async fn select(&self, id: i64, repos: &[&str]) -> reqwest::Response {
        self.write(
            reqwest::Method::PUT,
            &format!("/api/admin/subscriptions/{id}/repositories"),
            json!({"repositories":repos}),
        )
        .await
    }
}
#[derive(Default)]
struct Sink(Mutex<Vec<(String, String)>>);
#[async_trait]
impl MessageSink for Sink {
    async fn send(&self, user: &str, text: &str) -> Result<(), SendError> {
        self.0.lock().unwrap().push((user.into(), text.into()));
        Ok(())
    }
    async fn reply(&self, _: &str, _: &str, _: &str) -> Result<(), SendError> {
        unreachable!()
    }
}
fn issue(repo: &str, n: u64) -> IssueNotification {
    IssueNotification {
        repository: repo.into(),
        number: n,
        title: "new".into(),
        author: "a".into(),
        created_at: Utc::now() - Duration::minutes(1),
        url: format!("https://github.com/{repo}/issues/{n}"),
    }
}
#[tokio::test(start_paused = true)]
async fn admin_selects_repositories_and_only_selected_targets_receive_new_issues() {
    let f = Fixture::new().await;
    let alice = f.add("alice").await;
    let bob = f.add("bob").await;
    let both = f.add("both").await;
    f.add("empty").await;
    let store = Store::open(&f.path).unwrap();
    store.insert_notification(&issue("o/a", 1)).unwrap();
    assert!(f.select(alice, &["O/A", "o/a"]).await.status().is_success());
    assert!(f.select(bob, &["o/b"]).await.status().is_success());
    assert!(f.select(both, &["o/a", "o/b"]).await.status().is_success());
    store.insert_notification(&issue("o/a", 2)).unwrap();
    store.insert_notification(&issue("o/b", 3)).unwrap();
    let sink = Sink::default();
    deliver_pending(&store, &sink).await.unwrap();
    let received: Vec<_> = sink
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|(u, t)| (u.clone(), t.contains("o/a")))
        .collect();
    assert_eq!(
        received,
        [
            ("alice".into(), true),
            ("both".into(), true),
            ("bob".into(), false),
            ("both".into(), false)
        ]
    );
    assert_eq!(
        f.rows().await.iter().find(|r| r["id"] == alice).unwrap()["repositories"],
        json!(["o/a"])
    );
}

struct Source(Mutex<Vec<Activity>>);
#[async_trait]
impl TrackingSource for Source {
    async fn snapshot(&self, repo: &str, n: u64) -> anyhow::Result<IssueSnapshot> {
        Ok(IssueSnapshot {
            title: "tracked".into(),
            url: format!("https://github.com/{repo}/issues/{n}"),
            state: "open".into(),
            is_pull_request: false,
            activities: self.0.lock().unwrap().clone(),
            linked_prs: vec![],
            partial_error: None,
        })
    }
}
impl Source {
    fn event(&self, key: &str, kind: &str) {
        self.0.lock().unwrap().push(Activity {
            key: key.into(),
            kind: kind.into(),
            at: Utc::now() + Duration::seconds(1),
            actor: "dev".into(),
            text: key.into(),
            url: "https://github.com/o/a/issues/1".into(),
            related: None,
        });
    }
}

#[tokio::test(start_paused = true)]
async fn tracking_uses_same_selection_and_clearing_does_not_end_shared_tracking() {
    let f = Fixture::new().await;
    let alice = f.add("alice").await;
    let bob = f.add("bob").await;
    f.select(alice, &["o/a"]).await.error_for_status().unwrap();
    f.select(bob, &["o/b"]).await.error_for_status().unwrap();
    for repo in ["o/a", "o/b"] {
        f.write(
            reqwest::Method::POST,
            "/api/admin/tracking",
            json!({"url":format!("https://github.com/{repo}/issues/1")}),
        )
        .await
        .error_for_status()
        .unwrap();
    }
    let mut store = Store::open(&f.path).unwrap();
    for (key, kind) in [
        ("comment", "comment"),
        ("reference", "reference"),
        ("pr", "linked_pr"),
        ("merge", "merged"),
    ] {
        f.source.event(key, kind);
    }
    check_tracked(&mut store, f.source.as_ref()).await.unwrap();
    f.select(alice, &[]).await.error_for_status().unwrap();
    f.select(alice, &["o/a"]).await.error_for_status().unwrap();
    let sink = Sink::default();
    deliver_tracking(&store, &sink).await.unwrap();
    assert_eq!(
        sink.0
            .lock()
            .unwrap()
            .iter()
            .map(|(u, _)| u.clone())
            .collect::<Vec<_>>(),
        ["bob"]
    );
    assert_eq!(store.tracked_issues().unwrap().len(), 2);
    f.source.event("closed", "closed");
    check_tracked(&mut store, f.source.as_ref()).await.unwrap();
    deliver_tracking(&store, &sink).await.unwrap();
    assert_eq!(
        sink.0
            .lock()
            .unwrap()
            .iter()
            .map(|(u, _)| u.clone())
            .collect::<Vec<_>>(),
        ["bob", "alice", "bob"]
    );
    assert!(store.tracked_issues().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn invalid_selection_is_atomic_and_global_removal_clears_without_restoring() {
    let f = Fixture::new().await;
    let alice = f.add("alice").await;
    f.select(alice, &["o/a", "o/b"])
        .await
        .error_for_status()
        .unwrap();
    assert_eq!(
        f.select(alice, &["o/b", "not-configured/repo"])
            .await
            .status(),
        400
    );
    assert_eq!(f.rows().await[0]["repositories"], json!(["o/a", "o/b"]));
    assert_eq!(f.select(99999, &[]).await.status(), 404);
    let unauthorized = f
        .client
        .put(format!(
            "{}/api/admin/subscriptions/{alice}/repositories",
            f.base
        ))
        .json(&json!({"repositories":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 403);
    let store = Store::open(&f.path).unwrap();
    store.insert_notification(&issue("o/a", 1)).unwrap();
    store.insert_notification(&issue("o/b", 2)).unwrap();
    store
        .reconcile_repositories(&["o/b".into()], Utc::now())
        .unwrap();
    assert_eq!(f.rows().await[0]["repositories"], json!(["o/b"]));
    assert_eq!(f.select(alice, &["o/a"]).await.status(), 400);
    store
        .reconcile_repositories(&["O/B".into(), "o/a".into()], Utc::now())
        .unwrap();
    assert_eq!(f.rows().await[0]["repositories"], json!(["o/b"]));
    let sink = Sink::default();
    deliver_pending(&store, &sink).await.unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    assert!(sink.0.lock().unwrap()[0].1.contains("o/b"));
    drop(store);
    let reopened = Store::open(&f.path).unwrap();
    reopened
        .start_monitoring(&["o/a".into(), "o/b".into()], Utc::now())
        .unwrap();
    assert_eq!(f.rows().await[0]["repositories"], json!(["o/b"]));
}

#[tokio::test(start_paused = true)]
async fn migration_preserves_old_recipients_once_and_does_not_replay_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite");
    // A literal pre-repository-subscription database, including an old single-target binding.
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        INSERT INTO settings VALUES('qq_user_openid','legacy');
        CREATE TABLE monitored_repositories(name TEXT PRIMARY KEY,baseline TEXT,cursor TEXT);
        INSERT INTO monitored_repositories VALUES('o/a','2020-01-01T00:00:00Z',NULL),('removed/repo','2020-01-01T00:00:00Z',NULL);
        CREATE TABLE issue_notifications(repository TEXT NOT NULL,number INTEGER NOT NULL,title TEXT NOT NULL,author TEXT NOT NULL,created_at TEXT NOT NULL,url TEXT NOT NULL,state TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,next_attempt_at TEXT,last_error TEXT,PRIMARY KEY(repository,number));
        INSERT INTO issue_notifications(repository,number,title,author,created_at,url,state) VALUES
        ('o/a',1,'pending','dev','2026-01-01T00:00:00Z','https://github.com/o/a/issues/1','pending'),
        ('o/a',2,'sent','dev','2026-01-02T00:00:00Z','https://github.com/o/a/issues/2','sent'),
        ('removed/repo',3,'removed','dev','2026-01-03T00:00:00Z','https://github.com/removed/repo/issues/3','pending');").unwrap();
    drop(db);
    let store = Store::open(&path).unwrap();
    // New identities appearing after schema expansion are not old recipients.
    store.bind_user("new").unwrap();
    store
        .start_monitoring(&["o/a".into(), "o/b".into()], Utc::now())
        .unwrap();
    let rows = store.subscriptions().unwrap();
    assert_eq!(rows[0].repositories, ["o/a", "o/b"]);
    assert!(rows[1].repositories.is_empty());
    let sink = Sink::default();
    deliver_pending(&store, &sink).await.unwrap();
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    assert_eq!(sink.0.lock().unwrap()[0].0, "legacy");
    store
        .replace_subscription_repositories(rows[0].id, &[])
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    store
        .start_monitoring(&["o/a".into(), "o/b".into(), "o/c".into()], Utc::now())
        .unwrap();
    assert!(store
        .subscriptions()
        .unwrap()
        .iter()
        .all(|s| s.repositories.is_empty()));
    assert_eq!(store.recorded_notifications().unwrap().len(), 3);
    assert_eq!(deliver_pending(&store, &sink).await.unwrap(), 0);
}

struct CancelDuringSend {
    base: String,
    client: reqwest::Client,
    id: i64,
    fail: bool,
    calls: Mutex<Vec<String>>,
}
#[async_trait]
impl MessageSink for CancelDuringSend {
    async fn send(&self, user: &str, _: &str) -> Result<(), SendError> {
        self.calls.lock().unwrap().push(user.into());
        if user == "alice" {
            // The send has begun. Mutate through the real admin boundary on another connection.
            for repos in [vec![], vec!["o/a"]] {
                self.client
                    .put(format!(
                        "{}/api/admin/subscriptions/{}/repositories",
                        self.base, self.id
                    ))
                    .header("origin", &self.base)
                    .header("x-issue-watch-admin", "1")
                    .json(&json!({"repositories":repos}))
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap();
            }
            if self.fail {
                return Err(SendError::Retryable("in-flight request failed".into()));
            }
        }
        Ok(())
    }
    async fn reply(&self, _: &str, _: &str, _: &str) -> Result<(), SendError> {
        unreachable!()
    }
}
async fn exercise_cancel_inflight(tracking: bool, fail: bool) {
    let f = Fixture::new().await;
    let alice = f.add("alice").await;
    let bob = f.add("bob").await;
    f.select(alice, &["o/a"]).await.error_for_status().unwrap();
    f.select(bob, &["o/a"]).await.error_for_status().unwrap();
    let mut store = Store::open(&f.path).unwrap();
    if tracking {
        f.write(
            reqwest::Method::POST,
            "/api/admin/tracking",
            json!({"url":"https://github.com/o/a/issues/1"}),
        )
        .await
        .error_for_status()
        .unwrap();
        f.source.event("one", "comment");
        check_tracked(&mut store, f.source.as_ref()).await.unwrap();
        f.source.event("two", "reference");
        check_tracked(&mut store, f.source.as_ref()).await.unwrap();
    } else {
        store.insert_notification(&issue("o/a", 1)).unwrap();
        store.insert_notification(&issue("o/a", 2)).unwrap();
    }
    let sink = CancelDuringSend {
        base: f.base.clone(),
        client: f.client.clone(),
        id: alice,
        fail,
        calls: Mutex::new(vec![]),
    };
    if tracking {
        deliver_tracking(&store, &sink).await.unwrap();
    } else {
        deliver_pending(&store, &sink).await.unwrap();
    }
    assert_eq!(*sink.calls.lock().unwrap(), ["alice", "bob", "bob"]);
    let summary = store.notification_summary().unwrap();
    assert_eq!(summary["retrying"], 0);
    assert_eq!(summary["pending"], 0);
    assert_eq!(summary["sent"], if fail { 2 } else { 3 });
    drop(store);
    let store = Store::open(&f.path).unwrap();
    store
        .start_monitoring(&["o/a".into(), "o/b".into()], Utc::now())
        .unwrap();
    assert!(store
        .pending_deliveries(Utc::now() + Duration::minutes(1))
        .unwrap()
        .is_empty());
    issue_watch::tracking::deliver_tracking_due(&store, &sink, Utc::now() + Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(*sink.calls.lock().unwrap(), ["alice", "bob", "bob"]);
}
#[tokio::test(start_paused = true)]
async fn cancelling_then_readding_during_new_issue_send_preserves_success_and_suppresses_failure_retry(
) {
    exercise_cancel_inflight(false, false).await;
    exercise_cancel_inflight(false, true).await;
}
#[tokio::test(start_paused = true)]
async fn cancelling_then_readding_during_tracking_send_preserves_success_and_suppresses_failure_retry(
) {
    exercise_cancel_inflight(true, false).await;
    exercise_cancel_inflight(true, true).await;
}
