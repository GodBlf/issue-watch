use async_trait::async_trait;
use chrono::{Duration, Utc};
use issue_watch::{
    health::{Health, HealthStatus},
    qq::{MessageSink, SendError},
    qq_health::QqHealth,
    queue::deliver_pending_observed,
    IssueNotification, Store,
};
use std::sync::Mutex;

fn issue(number: u64) -> IssueNotification {
    IssueNotification {
        repository: "o/r".into(),
        number,
        title: format!("issue {number}"),
        author: "author".into(),
        created_at: Utc::now(),
        url: format!("https://github.com/o/r/issues/{number}"),
    }
}

struct Sink {
    calls: Mutex<Vec<String>>,
    fail_once: Mutex<Option<String>>,
}
#[async_trait]
impl MessageSink for Sink {
    async fn send(&self, user: &str, _: &str) -> Result<(), SendError> {
        self.calls.lock().unwrap().push(user.into());
        let mut failing = self.fail_once.lock().unwrap();
        if failing.as_deref() == Some(user) {
            *failing = None;
            return Err(SendError::Retryable("try again".into()));
        }
        Ok(())
    }
    async fn reply(&self, _: &str, _: &str, _: &str) -> Result<(), SendError> {
        unreachable!()
    }
}

#[tokio::test(start_paused = true)]
async fn broadcasts_and_retries_only_the_failed_recipient_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broadcast.sqlite");
    let store = Store::open(&path).unwrap();
    store.bind_user("alice").unwrap();
    store.bind_user("bob").unwrap();
    store.insert_notification(&issue(1)).unwrap();
    let health = Health::new(vec![]);
    let qq = QqHealth::new(health.clone());
    qq.refresh_queue(&store).unwrap();
    assert_eq!(
        health.snapshot().components["qq_binding"].details["subscriber_count"],
        2
    );
    assert_eq!(store.notification_summary().unwrap()["pending"], 2);
    let sink = Sink {
        calls: Mutex::new(vec![]),
        fail_once: Mutex::new(Some("bob".into())),
    };
    assert_eq!(
        deliver_pending_observed(&store, &sink, &qq).await.unwrap(),
        1
    );
    assert_eq!(*sink.calls.lock().unwrap(), ["alice", "bob"]);
    assert_eq!(
        health.snapshot().components["notification_queue"].status,
        HealthStatus::Warning
    );
    assert_eq!(store.notification_summary().unwrap()["retrying"], 1);
    drop(store);
    let store = Store::open(&path).unwrap();
    let pending = store
        .pending_deliveries(Utc::now() + Duration::minutes(1))
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].user_openid, "bob");
    // Make the persisted retry due without a real-time wait.
    store
        .mark_retry(&pending[0], Utc::now() - Duration::seconds(1), "try again")
        .unwrap();
    assert_eq!(
        deliver_pending_observed(&store, &sink, &qq).await.unwrap(),
        1
    );
    assert_eq!(*sink.calls.lock().unwrap(), ["alice", "bob", "bob"]);
    assert_eq!(store.notification_summary().unwrap()["retrying"], 0);
    assert!(store.pending_deliveries(Utc::now()).unwrap().is_empty());
}

#[test]
fn late_joiners_do_not_receive_history_or_another_users_backlog() {
    let store = Store::open_in_memory().unwrap();
    store.insert_notification(&issue(1)).unwrap(); // No subscribers yet.
    assert!(store.bind_user("alice").unwrap());
    assert!(store.pending_deliveries(Utc::now()).unwrap().is_empty());
    store.insert_notification(&issue(2)).unwrap();
    assert!(store.bind_user("bob").unwrap());
    assert!(!store.bind_user("alice").unwrap());
    assert!(!store.insert_notification(&issue(2)).unwrap());
    assert_eq!(store.pending_deliveries(Utc::now()).unwrap().len(), 1);
    store.insert_notification(&issue(3)).unwrap();
    let deliveries = store.pending_deliveries(Utc::now()).unwrap();
    assert_eq!(
        deliveries
            .iter()
            .map(|d| (d.user_openid.as_str(), d.issue.number))
            .collect::<Vec<_>>(),
        [("alice", 2), ("alice", 3), ("bob", 3)]
    );
}

#[test]
fn unbinding_cancels_only_own_work_and_rebinding_does_not_restore_it() {
    let store = Store::open_in_memory().unwrap();
    store.bind_user("alice").unwrap();
    store.bind_user("bob").unwrap();
    store.insert_notification(&issue(1)).unwrap();
    store.insert_notification(&issue(2)).unwrap();
    let snapshot = store.pending_deliveries(Utc::now()).unwrap();
    let alice = &snapshot[0];
    store.mark_permanent_failure(alice, "blocked").unwrap();
    assert!(store.unbind_user("alice").unwrap());
    assert!(!store.unbind_user("alice").unwrap());
    assert!(!store.delivery_is_pending(alice).unwrap());
    // A send that was in flight when unbinding must not resurrect cancelled work.
    store.mark_retry(alice, Utc::now(), "late result").unwrap();
    store.mark_sent(&snapshot[2]).unwrap();
    assert_eq!(store.notification_summary().unwrap()["permanent_failed"], 0);
    assert_eq!(store.notification_summary().unwrap()["pending"], 2);
    store.bind_user("alice").unwrap();
    assert!(store
        .pending_deliveries(Utc::now())
        .unwrap()
        .iter()
        .all(|d| d.user_openid == "bob"));
    store.insert_notification(&issue(3)).unwrap();
    let deliveries = store.pending_deliveries(Utc::now()).unwrap();
    let new_alice = deliveries
        .iter()
        .find(|d| d.user_openid == "alice")
        .unwrap();
    assert_ne!(new_alice.subscription_id, alice.subscription_id);
    assert_eq!(new_alice.issue.number, 3);
}

#[tokio::test(start_paused = true)]
async fn a_recipient_that_leaves_during_a_batch_is_skipped() {
    struct UnbindingSink(std::path::PathBuf, Mutex<Vec<String>>);
    #[async_trait]
    impl MessageSink for UnbindingSink {
        async fn send(&self, user: &str, _: &str) -> Result<(), SendError> {
            self.1.lock().unwrap().push(user.into());
            Store::open(&self.0).unwrap().unbind_user("bob").unwrap();
            Ok(())
        }
        async fn reply(&self, _: &str, _: &str, _: &str) -> Result<(), SendError> {
            unreachable!()
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.sqlite");
    let store = Store::open(&path).unwrap();
    store.bind_user("alice").unwrap();
    store.bind_user("bob").unwrap();
    store.insert_notification(&issue(1)).unwrap();
    let sink = UnbindingSink(path, Mutex::new(vec![]));
    let qq = QqHealth::new(Health::new(vec![]));
    assert_eq!(
        deliver_pending_observed(&store, &sink, &qq).await.unwrap(),
        1
    );
    assert_eq!(*sink.1.lock().unwrap(), ["alice"]);
}

fn legacy_database(path: &std::path::Path, bound: bool) {
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE issue_notifications(repository TEXT NOT NULL,number INTEGER NOT NULL,title TEXT NOT NULL,author TEXT NOT NULL,created_at TEXT NOT NULL,url TEXT NOT NULL,state TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,next_attempt_at TEXT,last_error TEXT,PRIMARY KEY(repository,number));
        INSERT INTO issue_notifications VALUES('o/r',1,'pending','a','2020-01-01T00:00:00Z','url','pending',0,NULL,NULL);
        INSERT INTO issue_notifications VALUES('o/r',2,'sent','a','2020-01-02T00:00:00Z','url','sent',0,NULL,NULL);
        INSERT INTO issue_notifications VALUES('o/r',3,'retry','a','2020-01-03T00:00:00Z','url','retry',4,'2020-01-04T00:00:00Z','timeout');
        INSERT INTO issue_notifications VALUES('o/r',4,'failed','a','2020-01-04T00:00:00Z','url','permanent_failure',2,NULL,'denied');").unwrap();
    if bound {
        db.execute(
            "INSERT INTO settings VALUES('qq_user_openid','original')",
            [],
        )
        .unwrap();
    }
}

#[test]
fn migrates_old_binding_and_delivery_results_once_without_resurrecting_subscriptions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.sqlite");
    legacy_database(&path, true);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.bound_users().unwrap(), ["original"]);
    assert_eq!(
        store
            .pending_deliveries(Utc::now())
            .unwrap()
            .iter()
            .map(|d| d.issue.number)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    let summary = store.notification_summary().unwrap();
    assert_eq!(summary["pending"], 1);
    assert_eq!(summary["retrying"], 1);
    assert_eq!(summary["permanent_failed"], 1);
    assert!(summary["failures"][0]["enqueued_at"].is_null());
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT attempts FROM notification_deliveries WHERE number=3",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        4
    );
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.notification_summary().unwrap(), summary);
    store.bind_user("newcomer").unwrap();
    assert!(store
        .pending_deliveries(Utc::now())
        .unwrap()
        .iter()
        .all(|d| d.user_openid == "original"));
    store.unbind_user("original").unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.bound_users().unwrap(), ["newcomer"]);
    assert!(store.pending_deliveries(Utc::now()).unwrap().is_empty());
    assert_eq!(store.notification_summary().unwrap()["permanent_failed"], 0);
}

#[test]
fn legacy_history_without_a_binding_is_not_given_to_new_subscribers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.sqlite");
    legacy_database(&path, false);
    let store = Store::open(&path).unwrap();
    store.bind_user("newcomer").unwrap();
    assert_eq!(store.recorded_notifications().unwrap().len(), 4);
    assert!(store.pending_deliveries(Utc::now()).unwrap().is_empty());
}

#[test]
fn recipient_creation_failure_rolls_back_issue_and_all_deliveries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.sqlite");
    let store = Store::open(&path).unwrap();
    store.bind_user("alice").unwrap();
    store.bind_user("bob").unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_bob BEFORE INSERT ON notification_deliveries
        WHEN NEW.subscription_id=2 BEGIN SELECT RAISE(ABORT,'simulated failure'); END;",
    )
    .unwrap();
    assert!(store.insert_notification(&issue(1)).is_err());
    assert!(store.recorded_notifications().unwrap().is_empty());
    assert!(store.pending_deliveries(Utc::now()).unwrap().is_empty());
    db.execute_batch("DROP TRIGGER reject_bob").unwrap();
    assert!(store.insert_notification(&issue(1)).unwrap());
    assert_eq!(store.pending_deliveries(Utc::now()).unwrap().len(), 2);
}
