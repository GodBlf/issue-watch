use chrono::{TimeZone, Utc};
use issue_watch::health::{Health, HealthStatus};
use serde_json::json;

fn observed() -> Health {
    let health = Health::new(vec![]);
    for name in [
        "github",
        "qq_gateway",
        "qq_binding",
        "qq_auth",
        "notification_queue",
    ] {
        health.observe(name, HealthStatus::Normal, json!({}));
    }
    health.observe(
        "qq_delivery",
        HealthStatus::Unknown,
        json!({"result":"unverified"}),
    );
    health
}

#[test]
fn active_business_stalls_but_idle_wait_and_dashboard_reads_do_not_advance_it() {
    let health = observed();
    let start = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
    health.observe(
        "github_progress",
        HealthStatus::Normal,
        json!({"active":true,"stage":"checking_repository","last_progress_at":start}),
    );
    assert_eq!(
        health
            .snapshot_at(start + chrono::Duration::seconds(300))
            .status,
        HealthStatus::Normal
    );
    assert_eq!(
        health
            .snapshot_at(start + chrono::Duration::seconds(301))
            .status,
        HealthStatus::Error
    );
    assert_eq!(
        health
            .snapshot_at(start + chrono::Duration::seconds(302))
            .status,
        HealthStatus::Error
    );
    health.set_poll_interval(600);
    assert_eq!(
        health
            .snapshot_at(start + chrono::Duration::seconds(301))
            .status,
        HealthStatus::Normal
    );
    assert_eq!(
        health
            .snapshot_at(start + chrono::Duration::seconds(1801))
            .status,
        HealthStatus::Error
    );
    health.observe(
        "github_progress",
        HealthStatus::Normal,
        json!({"active":false,"stage":"repository_complete","last_progress_at":start}),
    );
    assert_eq!(
        health.snapshot_at(start + chrono::Duration::days(1)).status,
        HealthStatus::Normal
    );
}

#[test]
fn overall_preserves_known_failures_and_does_not_require_a_test_notification() {
    let health = observed();
    assert_eq!(health.snapshot().status, HealthStatus::Normal);
    health.observe("github", HealthStatus::Warning, json!({"error":"once"}));
    assert_eq!(health.snapshot().status, HealthStatus::Warning);
    health.observe(
        "notification_queue",
        HealthStatus::Error,
        json!({"permanent_failed":1}),
    );
    assert_eq!(health.snapshot().status, HealthStatus::Error);
    health.observe("github", HealthStatus::Normal, json!({}));
    assert_eq!(health.snapshot().status, HealthStatus::Error);
    assert_eq!(Health::new(vec![]).snapshot().status, HealthStatus::Unknown);
}

#[tokio::test]
async fn status_endpoint_remains_available_when_business_is_stalled() {
    let health = observed();
    health.observe("notification_progress", HealthStatus::Normal, json!({"active":true,"stage":"sending","last_progress_at":Utc::now()-chrono::Duration::minutes(6)}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(issue_watch::health::serve(listener, health));
    let response = reqwest::get(format!("http://{address}/api/status"))
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(response["status"], "error");
    assert_eq!(
        response["components"]["progress"]["details"]["stalled"],
        true
    );
    server.abort();
}

#[tokio::test]
async fn pending_repository_check_does_not_block_status_requests() {
    use async_trait::async_trait;
    use issue_watch::{
        github::{GithubIssue, IssueSource},
        github_health::GithubHealth,
        Store,
    };
    struct HangingSource;
    #[async_trait]
    impl IssueSource for HangingSource {
        async fn list_issues(
            &self,
            _: &str,
            _: Option<chrono::DateTime<Utc>>,
            _: u32,
        ) -> anyhow::Result<Vec<GithubIssue>> {
            std::future::pending().await
        }
    }
    let health = observed();
    let monitor = GithubHealth::new(health.clone(), &["owner/repository".into()]);
    let store = Store::open_in_memory().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(issue_watch::health::serve(listener, health));
    let check = monitor.poll_with_clock(&HangingSource, &store, "owner/repository", || {
        Utc::now() - chrono::Duration::minutes(6)
    });
    let read = async {
        reqwest::get(format!("http://{address}/api/status"))
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    };
    let response = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::select! {
            biased;
            _ = check => panic!("hanging source must remain pending"),
            response = read => response,
        }
    })
    .await
    .unwrap();
    assert_eq!(response["status"], "error");
    assert_eq!(
        response["components"]["progress"]["details"]["active"],
        true
    );
    server.abort();
}
