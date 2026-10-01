use issue_watch::health::{serve, Health};

#[tokio::test]
async fn status_is_unknown_and_read_only_before_business_observations() {
    let health = Health::new(Vec::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(serve(listener, health));
    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://{address}/api/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    let status: serde_json::Value = response.json().await.unwrap();
    assert_eq!(status["status"], "unknown");
    assert!(status["started_at"].is_string());
    assert!(status["updated_at"].is_string());
    assert!(status["uptime_seconds"].is_u64());
    assert_eq!(
        client
            .post(format!("http://{address}/api/status"))
            .send()
            .await
            .unwrap()
            .status(),
        405
    );
    let page = client
        .get(format!("http://{address}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("健康状态"));
    task.abort();
}

#[tokio::test]
async fn business_observations_are_visible_without_exposing_credentials() {
    use issue_watch::health::HealthStatus;
    let health = Health::new(vec!["super-secret".into()]);
    health.observe("github", HealthStatus::Warning, serde_json::json!({
        "error": "failed super-secret; Authorization: Bearer hidden-token", "token": "another-secret", "repository": "owner/name"
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(serve(listener, health.clone()));
    let response = reqwest::get(format!("http://{address}/api/status"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!response.contains("super-secret"));
    assert!(!response.contains("hidden-token"));
    assert!(!response.contains("another-secret"));
    let snapshot: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(snapshot["components"]["github"]["status"], "warning");
    assert_eq!(
        snapshot["components"]["github"]["details"]["repository"],
        "owner/name"
    );
    health.observe(
        "github",
        HealthStatus::Normal,
        serde_json::json!({"repository": "owner/name"}),
    );
    assert_eq!(
        health.snapshot().components["github"].status,
        HealthStatus::Normal
    );
    task.abort();
}
