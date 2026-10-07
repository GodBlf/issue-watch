use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use issue_watch::{
    config::FileConfig,
    github::{discover_repository, GithubIssue, IssueSource},
    reload::ConfigReloader,
    Store,
};

fn day(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, day, 0, 0, 0).unwrap()
}
struct Issues;
#[async_trait]
impl IssueSource for Issues {
    async fn list_issues(
        &self,
        _: &str,
        _: Option<DateTime<Utc>>,
        page: u32,
    ) -> anyhow::Result<Vec<GithubIssue>> {
        Ok(if page == 1 {
            [5, 8]
                .into_iter()
                .map(|d| GithubIssue {
                    number: d as u64,
                    title: format!("issue {d}"),
                    author: "alice".into(),
                    created_at: day(d),
                    url: format!("https://github.com/owner/a/issues/{d}"),
                    is_pull_request: false,
                })
                .collect()
        } else {
            vec![]
        })
    }
}

#[tokio::test]
async fn readding_starts_today_and_cancels_old_deliveries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "repositories = ['owner/a', 'owner/b']").unwrap();
    let active = FileConfig::load(&path).unwrap();
    let mut reload = ConfigReloader::new(&path, active.clone());
    let store = Store::open_in_memory().unwrap();
    store
        .ensure_repositories(&active.repositories, day(1))
        .unwrap();
    store.bind_user("alice").unwrap();
    store.bind_user("bob").unwrap();
    assert_eq!(
        discover_repository(&Issues, &store, "owner/a", day(2))
            .await
            .unwrap(),
        2
    );
    std::fs::write(&path, "repositories = ['owner/b']").unwrap();
    reload.check(&store, day(3)).unwrap();
    assert!(
        store.pending_deliveries(day(9)).unwrap().is_empty(),
        "removed repository still has queued deliveries"
    );
    std::fs::write(&path, "repositories = ['owner/a', 'owner/b']").unwrap();
    reload.check(&store, day(7)).unwrap();
    // Previously discovered identities remain historical, even after readding.
    assert_eq!(
        discover_repository(&Issues, &store, "owner/a", day(9))
            .await
            .unwrap(),
        0
    );
    assert!(store.pending_deliveries(day(9)).unwrap().is_empty());
    assert_eq!(store.get_cursor("owner/b").unwrap(), None);
    assert_eq!(store.bound_users().unwrap(), ["alice", "bob"]);
}

#[tokio::test]
async fn readding_excludes_issues_created_during_removal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "repositories = ['owner/a', 'owner/b']").unwrap();
    let active = FileConfig::load(&path).unwrap();
    let store = Store::open_in_memory().unwrap();
    store
        .ensure_repositories(&active.repositories, day(1))
        .unwrap();
    store.set_cursor("owner/a", day(3)).unwrap();
    store.bind_user("alice").unwrap();
    let mut reload = ConfigReloader::new(&path, active);
    std::fs::write(&path, "repositories = ['owner/b']").unwrap();
    reload.check(&store, day(3)).unwrap();
    std::fs::write(&path, "repositories = ['owner/a', 'owner/b']").unwrap();
    reload.check(&store, day(7)).unwrap();
    assert_eq!(
        discover_repository(&Issues, &store, "owner/a", day(9))
            .await
            .unwrap(),
        1
    );
    assert_eq!(store.pending_deliveries(day(9)).unwrap()[0].issue.number, 8);
}

struct RemoveDuringSend {
    database: std::path::PathBuf,
    fail: bool,
    calls: std::sync::Mutex<usize>,
}
#[async_trait]
impl issue_watch::qq::MessageSink for RemoveDuringSend {
    async fn reply(
        &self,
        user: &str,
        text: &str,
        _: &str,
    ) -> Result<(), issue_watch::qq::SendError> {
        self.send(user, text).await
    }
    async fn send(&self, _: &str, _: &str) -> Result<(), issue_watch::qq::SendError> {
        *self.calls.lock().unwrap() += 1;
        let store = Store::open(&self.database).unwrap();
        store
            .reconcile_repositories(&["owner/b".into()], day(7))
            .unwrap();
        if self.fail {
            Err(issue_watch::qq::SendError::Retryable("network".into()))
        } else {
            Ok(())
        }
    }
}

#[tokio::test(start_paused = true)]
async fn removal_during_send_cancels_batch_and_records_inflight_success() {
    for fail in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("state.sqlite");
        let store = Store::open(&database).unwrap();
        store
            .reconcile_repositories(&["owner/a".into(), "owner/b".into()], day(1))
            .unwrap();
        store.bind_user("alice").unwrap();
        discover_repository(&Issues, &store, "owner/a", day(2))
            .await
            .unwrap();
        let sink = RemoveDuringSend {
            database,
            fail,
            calls: std::sync::Mutex::new(0),
        };
        let started = tokio::time::Instant::now();
        assert_eq!(
            issue_watch::queue::deliver_pending(&store, &sink)
                .await
                .unwrap(),
            usize::from(!fail)
        );
        assert_eq!(*sink.calls.lock().unwrap(), 1);
        assert_eq!(
            tokio::time::Instant::now() - started,
            std::time::Duration::ZERO,
            "cancelled batch still incurred pacing"
        );
        assert!(store.pending_deliveries(day(9)).unwrap().is_empty());
        assert_eq!(store.notification_summary().unwrap()["retrying"], 0);
        assert_eq!(
            store.notification_summary().unwrap()["sent"],
            usize::from(!fail)
        );
        store
            .reconcile_repositories(&["owner/a".into(), "owner/b".into()], day(9))
            .unwrap();
        assert!(
            store.pending_deliveries(day(10)).unwrap().is_empty(),
            "inflight failure revived after readding"
        );
    }
}

struct ReaddDuringRequest {
    database: std::path::PathBuf,
}
#[async_trait]
impl IssueSource for ReaddDuringRequest {
    async fn list_issues(
        &self,
        repository: &str,
        since: Option<DateTime<Utc>>,
        page: u32,
    ) -> anyhow::Result<Vec<GithubIssue>> {
        let store = Store::open(&self.database)?;
        store.reconcile_repositories(&["owner/b".into()], day(3))?;
        store.reconcile_repositories(&["owner/a".into(), "owner/b".into()], day(7))?;
        Issues.list_issues(repository, since, page).await
    }
}
#[tokio::test]
async fn old_response_cannot_enter_readded_period_or_advance_its_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("state.sqlite");
    let store = Store::open(&database).unwrap();
    store
        .reconcile_repositories(&["owner/a".into(), "owner/b".into()], day(1))
        .unwrap();
    store.bind_user("alice").unwrap();
    assert_eq!(
        discover_repository(&ReaddDuringRequest { database }, &store, "owner/a", day(2))
            .await
            .unwrap(),
        0
    );
    assert!(store.recorded_notifications().unwrap().is_empty());
    assert_eq!(store.get_cursor("owner/a").unwrap(), None);
    assert_eq!(
        discover_repository(&Issues, &store, "owner/a", day(9))
            .await
            .unwrap(),
        1
    );
}

fn tracking_snapshot() -> issue_watch::tracking::IssueSnapshot {
    issue_watch::tracking::IssueSnapshot {
        title: "tracked".into(),
        url: "https://github.com/owner/a/issues/1".into(),
        state: "open".into(),
        is_pull_request: false,
        activities: vec![],
        linked_prs: vec![],
        partial_error: None,
    }
}
struct TrackingUpdate {
    database: std::path::PathBuf,
    readd: bool,
}
#[async_trait]
impl issue_watch::tracking::TrackingSource for TrackingUpdate {
    async fn snapshot(
        &self,
        _: &str,
        _: u64,
    ) -> anyhow::Result<issue_watch::tracking::IssueSnapshot> {
        if self.readd {
            let store = Store::open(&self.database)?;
            store.reconcile_repositories(&["owner/b".into()], day(3))?;
            store.reconcile_repositories(&["owner/a".into(), "owner/b".into()], day(7))?;
        }
        let mut snapshot = tracking_snapshot();
        snapshot.activities.push(issue_watch::tracking::Activity {
            key: "comment".into(),
            kind: "comment".into(),
            at: day(8),
            actor: "alice".into(),
            text: "new comment".into(),
            url: snapshot.url.clone(),
            related: None,
        });
        Ok(snapshot)
    }
}
#[tokio::test(start_paused = true)]
async fn tracking_removal_cancels_old_requests_and_inflight_failure_without_reviving() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("state.sqlite");
    let mut store = Store::open(&database).unwrap();
    let names = vec!["owner/a".into(), "owner/b".into()];
    store.reconcile_repositories(&names, day(1)).unwrap();
    store.bind_user("alice").unwrap();
    store
        .add_tracking("owner/a", 1, &tracking_snapshot(), day(1))
        .unwrap();
    assert_eq!(
        issue_watch::tracking::check_tracked(
            &mut store,
            &TrackingUpdate {
                database: database.clone(),
                readd: true
            }
        )
        .await
        .unwrap(),
        0
    );
    assert!(store.tracked_issues().unwrap().is_empty());
    let sink = RemoveDuringSend {
        database: database.clone(),
        fail: true,
        calls: std::sync::Mutex::new(0),
    };
    assert_eq!(
        issue_watch::tracking::deliver_tracking(&store, &sink)
            .await
            .unwrap(),
        0
    );
    assert_eq!(*sink.calls.lock().unwrap(), 0);
    store
        .add_tracking("owner/a", 1, &tracking_snapshot(), day(7))
        .unwrap();
    issue_watch::tracking::check_tracked(
        &mut store,
        &TrackingUpdate {
            database,
            readd: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        issue_watch::tracking::deliver_tracking(&store, &sink)
            .await
            .unwrap(),
        0
    );
    assert_eq!(*sink.calls.lock().unwrap(), 1);
    store.reconcile_repositories(&names, day(9)).unwrap();
    assert!(store.tracked_issues().unwrap().is_empty());
    assert_eq!(store.notification_summary().unwrap()["retrying"], 0);
    assert_eq!(
        issue_watch::tracking::deliver_tracking(&store, &sink)
            .await
            .unwrap(),
        0
    );
}

struct NeverCompletes;
#[async_trait]
impl issue_watch::qq::MessageSink for NeverCompletes {
    async fn reply(
        &self,
        user: &str,
        text: &str,
        _: &str,
    ) -> Result<(), issue_watch::qq::SendError> {
        self.send(user, text).await
    }
    async fn send(&self, _: &str, _: &str) -> Result<(), issue_watch::qq::SendError> {
        std::future::pending().await
    }
}
#[tokio::test(start_paused = true)]
async fn startup_recovers_interrupted_claims_but_opening_a_connection_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("state.sqlite");
    let store = Store::open(&database).unwrap();
    let names = vec!["owner/a".into(), "owner/b".into()];
    store.start_monitoring(&names, day(1)).unwrap();
    store.bind_user("alice").unwrap();
    discover_repository(&Issues, &store, "owner/a", day(2))
        .await
        .unwrap();
    assert!(tokio::time::timeout(
        std::time::Duration::from_secs(1),
        issue_watch::queue::deliver_pending(&store, &NeverCompletes)
    )
    .await
    .is_err());
    assert_eq!(store.notification_summary().unwrap()["sending"], 1);
    let reopened = Store::open(&database).unwrap();
    assert_eq!(reopened.notification_summary().unwrap()["sending"], 1);
    reopened.start_monitoring(&names, day(9)).unwrap();
    assert_eq!(reopened.pending_deliveries(day(10)).unwrap().len(), 2);
    // A stopped service can be restarted with a repository removed in the final config.
    reopened
        .start_monitoring(&["owner/b".into()], day(10))
        .unwrap();
    assert!(reopened.pending_deliveries(day(10)).unwrap().is_empty());
}

#[test]
fn upgrading_legacy_repository_state_preserves_configured_progress() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("state.sqlite");
    let legacy = rusqlite::Connection::open(&database).unwrap();
    legacy.execute_batch("CREATE TABLE monitored_repositories(name TEXT PRIMARY KEY,baseline TEXT,cursor TEXT);
        INSERT INTO monitored_repositories VALUES('owner/a','2026-10-01T00:00:00+00:00','2026-10-03T00:00:00+00:00');
        INSERT INTO monitored_repositories VALUES('owner/b','2026-10-01T00:00:00+00:00',NULL);").unwrap();
    drop(legacy);
    let store = Store::open(&database).unwrap();
    store.start_monitoring(&["OWNER/A".into()], day(7)).unwrap();
    assert_eq!(store.get_cursor("owner/a").unwrap(), Some(day(3)));
    let repo = store.ensure_repository("owner/a", day(9)).unwrap();
    assert_eq!(repo.baseline, Some(day(1)));
    assert!(repo.active);
    assert!(!store.ensure_repository("owner/b", day(9)).unwrap().active);
    store.start_monitoring(&["owner/a".into()], day(9)).unwrap();
    assert_eq!(store.ensure_repository("owner/a", day(9)).unwrap(), repo);
}

#[tokio::test]
async fn rejected_repository_change_rolls_back_tracking_and_notification_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("state.sqlite");
    let mut store = Store::open(&database).unwrap();
    store
        .reconcile_repositories(&["owner/a".into(), "owner/b".into()], day(1))
        .unwrap();
    store.bind_user("alice").unwrap();
    store
        .add_tracking("owner/a", 1, &tracking_snapshot(), day(1))
        .unwrap();
    issue_watch::tracking::check_tracked(
        &mut store,
        &TrackingUpdate {
            database: database.clone(),
            readd: false,
        },
    )
    .await
    .unwrap();
    discover_repository(&Issues, &store, "owner/a", day(2))
        .await
        .unwrap();
    let before = store.notification_summary().unwrap();
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute_batch("CREATE TRIGGER reject_removal BEFORE UPDATE OF active ON monitored_repositories WHEN NEW.active=0 BEGIN SELECT RAISE(ABORT,'simulated failure'); END;").unwrap();
    assert!(store
        .reconcile_repositories(&["owner/b".into()], day(7))
        .is_err());
    assert_eq!(store.notification_summary().unwrap(), before);
    assert_eq!(store.tracked_issues().unwrap().len(), 1);
    assert!(store.tracking_repository_allowed("owner/a").unwrap());
}

#[tokio::test]
async fn accepted_removal_updates_health_before_main_consumes_config() {
    use issue_watch::{github_health::GithubHealth, health::Health};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "repositories=['owner/a','owner/b']").unwrap();
    let active = FileConfig::load(&path).unwrap();
    let health = Health::new(vec![]);
    let store = Store::open_in_memory()
        .unwrap()
        .with_health(health.clone())
        .unwrap();
    store
        .reconcile_repositories(&active.repositories, day(1))
        .unwrap();
    store.bind_user("alice").unwrap();
    discover_repository(&Issues, &store, "owner/a", day(2))
        .await
        .unwrap();
    assert_eq!(
        health.snapshot().components["notification_queue"].details["pending"],
        2
    );
    let monitor = std::sync::Arc::new(GithubHealth::new(health.clone(), &active.repositories));
    let management =
        issue_watch::config_management::ConfigManagement::new(&path, active.clone()).unwrap();
    let (tx, rx) = tokio::sync::watch::channel(active.clone());
    std::fs::write(&path, "repositories=['owner/b']").unwrap();
    let task = tokio::spawn(issue_watch::reload::watch_config_observed(
        ConfigReloader::new(&path, active),
        store,
        tx,
        Some(management.clone()),
        Some(monitor),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if management.snapshot().stage == "accepted" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let snapshot = health.snapshot();
    assert_eq!(
        snapshot.components["notification_queue"].details["pending"],
        0
    );
    assert_eq!(
        snapshot.components["github"].details["repositories"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        snapshot.components["github"].details["repositories"][0]["repository"],
        "owner/b"
    );
    assert_eq!(
        management.snapshot().applied.repositories,
        ["owner/a", "owner/b"]
    );
    drop(rx);
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn large_cancelled_batch_does_not_delay_other_repository_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("state.sqlite");
    let store = Store::open(&database).unwrap();
    store
        .reconcile_repositories(&["owner/a".into(), "owner/b".into()], day(1))
        .unwrap();
    store.bind_user("alice").unwrap();
    for number in 1..=100 {
        store
            .insert_notification(&issue_watch::IssueNotification {
                repository: "owner/a".into(),
                number,
                title: "queued".into(),
                author: "alice".into(),
                created_at: day(5),
                url: "https://example.com".into(),
            })
            .unwrap();
    }
    store
        .insert_notification(&issue_watch::IssueNotification {
            repository: "owner/b".into(),
            number: 1,
            title: "other".into(),
            author: "alice".into(),
            created_at: day(8),
            url: "https://example.com".into(),
        })
        .unwrap();
    let sink = RemoveDuringSend {
        database,
        fail: false,
        calls: std::sync::Mutex::new(0),
    };
    let started = tokio::time::Instant::now();
    assert_eq!(
        issue_watch::queue::deliver_pending(&store, &sink)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        tokio::time::Instant::now() - started,
        std::time::Duration::from_secs(3)
    );
    assert_eq!(store.notification_summary().unwrap()["cancelled"], 99);
    assert_eq!(store.notification_summary().unwrap()["sent"], 2);
}

#[tokio::test]
async fn tracking_add_request_cannot_cross_repository_periods() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("state.sqlite");
    let mut store = Store::open(&database).unwrap();
    store
        .reconcile_repositories(&["owner/a".into(), "owner/b".into()], day(1))
        .unwrap();
    store
        .set_tracking_permissions("alice", true, false)
        .unwrap();
    let response = issue_watch::tracking::tracking_command(
        &mut store,
        &TrackingUpdate {
            database,
            readd: true,
        },
        "/track https://github.com/owner/a/issues/1",
        "alice",
    )
    .await
    .unwrap()
    .unwrap();
    assert!(response.contains("监控周期已改变"), "{response}");
    assert!(store.tracked_issues().unwrap().is_empty());
}
