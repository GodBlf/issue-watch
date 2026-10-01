use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use issue_watch::{
    github::{GithubClient, GithubIssue, IssueSource},
    github_health::GithubHealth,
    health::{Health, HealthStatus},
    Store,
};

struct Empty;
struct Failing;
#[async_trait]
impl IssueSource for Failing {
    async fn list_issues(
        &self,
        _: &str,
        _: Option<DateTime<Utc>>,
        _: u32,
    ) -> Result<Vec<GithubIssue>> {
        anyhow::bail!("GitHub unavailable credential-value")
    }
}
#[async_trait]
impl IssueSource for Empty {
    async fn list_issues(
        &self,
        _: &str,
        _: Option<DateTime<Utc>>,
        _: u32,
    ) -> Result<Vec<GithubIssue>> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn failures_escalate_per_repository_preserve_success_and_recover() {
    let health = Health::new(vec!["credential-value".into()]);
    let monitor = GithubHealth::new(health.clone(), &["owner/bad".into(), "owner/good".into()]);
    let store = Store::open_in_memory().unwrap();
    monitor
        .poll_with_clock(&Empty, &store, "owner/bad", || at(3))
        .await
        .unwrap();
    for (hour, expected) in [(4, "warning"), (5, "warning"), (6, "error")] {
        assert!(monitor
            .poll_with_clock(&Failing, &store, "owner/bad", || at(hour))
            .await
            .is_err());
        monitor
            .poll_with_clock(&Empty, &store, "owner/good", || at(hour))
            .await
            .unwrap();
        let failed = row(&health, "owner/bad");
        assert_eq!(failed["status"], expected);
        assert_eq!(failed["last_success_at"], "2026-10-01T03:00:00Z");
        assert_eq!(failed["consecutive_failures"], hour - 3);
        assert!(!serde_json::to_string(&health.snapshot())
            .unwrap()
            .contains("credential-value"));
    }
    assert_eq!(
        health.snapshot().components["github"].status,
        HealthStatus::Error
    );
    monitor
        .poll_with_clock(&Empty, &store, "owner/bad", || at(7))
        .await
        .unwrap();
    let restored = row(&health, "owner/bad");
    assert_eq!(restored["status"], "normal");
    assert_eq!(restored["consecutive_failures"], 0);
    assert!(restored["error"].is_null());
    assert_eq!(restored["last_success_at"], "2026-10-01T07:00:00Z");
}
fn at(hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, hour, 0, 0).unwrap()
}
fn row(health: &Health, repository: &str) -> serde_json::Value {
    health.snapshot().components["github"].details["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["repository"] == repository)
        .unwrap()
        .clone()
}

#[tokio::test]
async fn no_new_issues_is_success_and_check_time_does_not_use_persisted_cursor() {
    let health = Health::new(vec![]);
    let monitor = GithubHealth::new(health.clone(), &["owner/repo".into()]);
    assert_eq!(row(&health, "owner/repo")["status"], "unknown");
    let store = Store::open_in_memory().unwrap();
    store.ensure_repository("owner/repo", at(1)).unwrap();
    store.set_cursor("owner/repo", at(2)).unwrap();
    assert_eq!(
        monitor
            .poll_with_clock(&Empty, &store, "owner/repo", || at(8))
            .await
            .unwrap(),
        0
    );
    let state = row(&health, "owner/repo");
    assert_eq!(state["status"], "normal");
    assert_eq!(state["last_attempt_at"], "2026-10-01T08:00:00Z");
    assert_eq!(state["last_success_at"], "2026-10-01T08:00:00Z");
    assert_eq!(
        health.snapshot().components["github"].status,
        HealthStatus::Normal
    );
    let restarted = Health::new(vec![]);
    GithubHealth::new(restarted.clone(), &["owner/repo".into()]);
    assert_eq!(row(&restarted, "owner/repo")["status"], "unknown");
}

#[tokio::test]
async fn applied_configuration_removes_failed_repositories_and_adds_unknown_ones() {
    let health = Health::new(vec![]);
    let monitor = GithubHealth::new(
        health.clone(),
        &["owner/removed".into(), "owner/kept".into()],
    );
    let store = Store::open_in_memory().unwrap();
    for _ in 0..3 {
        let _ = monitor.poll(&Failing, &store, "owner/removed").await;
    }
    monitor
        .poll_with_clock(&Empty, &store, "owner/kept", || at(8))
        .await
        .unwrap();
    monitor.sync_repositories(&["owner/kept".into(), "owner/new".into()]);
    let snapshot = health.snapshot();
    assert_eq!(snapshot.components["github"].status, HealthStatus::Unknown);
    assert_eq!(
        snapshot.components["github"].details["repositories"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        row(&health, "owner/kept")["last_success_at"],
        "2026-10-01T08:00:00Z"
    );
    assert_eq!(row(&health, "owner/new")["status"], "unknown");
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("owner/removed"));
}

#[tokio::test]
async fn pagination_advances_progress_before_the_repository_check_finishes() {
    use std::sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    };
    struct Pages {
        hour: Arc<AtomicU32>,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    #[async_trait]
    impl IssueSource for Pages {
        async fn list_issues(
            &self,
            _: &str,
            _: Option<DateTime<Utc>>,
            page: u32,
        ) -> Result<Vec<GithubIssue>> {
            if page == 1 {
                self.hour.store(9, Ordering::SeqCst);
                Ok(vec![GithubIssue {
                    number: 1,
                    title: "old".into(),
                    author: "u".into(),
                    created_at: at(1),
                    url: "https://github.com/o/r/issues/1".into(),
                    is_pull_request: false,
                }])
            } else {
                self.entered.notify_one();
                self.release.notified().await;
                self.hour.store(10, Ordering::SeqCst);
                Ok(vec![])
            }
        }
    }
    let health = Health::new(vec![]);
    let monitor = GithubHealth::new(health.clone(), &["owner/repo".into()]);
    let store = Store::open_in_memory().unwrap();
    let hour = Arc::new(AtomicU32::new(8));
    let source = Pages {
        hour: hour.clone(),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    };
    let mut poll = Box::pin(monitor.poll_with_clock(&source, &store, "owner/repo", || {
        at(hour.load(Ordering::SeqCst))
    }));
    tokio::select! { _ = source.entered.notified() => {}, _ = &mut poll => panic!("check must await next page") }
    let progress = health.snapshot().components["github_progress"]
        .details
        .clone();
    assert_eq!(progress["active"], true);
    assert_eq!(progress["last_progress_at"], "2026-10-01T09:00:00Z");
    assert!(row(&health, "owner/repo")["last_success_at"].is_null());
    source.release.notify_one();
    poll.await.unwrap();
    let progress = health.snapshot().components["github_progress"]
        .details
        .clone();
    assert_eq!(progress["active"], false);
    assert_eq!(progress["last_progress_at"], "2026-10-01T10:00:00Z");
}

#[tokio::test]
async fn http_body_timeout_marks_repository_failure_and_later_checks_continue() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 2048];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n[").await.unwrap();
        ready_tx.send(()).unwrap();
        // Hold the body open: headers succeeded, JSON decoding must still time out.
        std::future::pending::<()>().await;
    });
    let source = GithubClient::with_api_base(None, &format!("http://{address}/")).unwrap();
    let health = Health::new(vec![]);
    let monitor = GithubHealth::new(health.clone(), &["owner/hung".into(), "owner/next".into()]);
    let store = Store::open_in_memory().unwrap();
    let mut poll = Box::pin(monitor.poll(&source, &store, "owner/hung"));
    tokio::select! { _ = ready_rx => {}, result = &mut poll => panic!("request ended early: {result:?}") }
    // Ensure the partial body has entered the client before advancing the external clock.
    for _ in 0..10 {
        tokio::select! { _ = tokio::task::yield_now() => {}, result = &mut poll => panic!("request ended early: {result:?}") }
    }
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    let error = tokio::time::timeout(std::time::Duration::from_secs(1), poll)
        .await
        .expect("GitHub request must finish after its 30 second deadline")
        .unwrap_err();
    assert!(format!("{error:#}").contains("timed out"));
    assert_eq!(row(&health, "owner/hung")["status"], "warning");
    monitor.poll(&Empty, &store, "owner/next").await.unwrap();
    assert_eq!(row(&health, "owner/next")["status"], "normal");
    server.abort();
}
