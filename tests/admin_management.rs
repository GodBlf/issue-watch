use issue_watch::{
    admin::{serve, Admin},
    health::Health,
    Store,
};
use serde_json::{json, Value};

struct Fixture {
    _dir: tempfile::TempDir,
    base: String,
    client: reqwest::Client,
    database: std::path::PathBuf,
    management: issue_watch::config_management::ConfigManagement,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

#[tokio::test]
async fn notes_can_be_cleared_and_old_subscription_ids_cannot_remove_rejoined_users() {
    let f = Fixture::new().await;
    f.add("123456").await;
    let id = f.subscriptions().await[0]["id"].as_i64().unwrap();
    let path = format!("{}/api/admin/subscriptions/{id}", f.base);
    assert_eq!(
        f.client
            .patch(&path)
            .header("origin", &f.base)
            .header("x-issue-watch-admin", "1")
            .json(&json!({"qq_number_note":""}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(f.subscriptions().await[0]["qq_number_note"], Value::Null);
    let store = Store::open(&f.database).unwrap();
    store.bind_user("other").unwrap();
    store
        .insert_notification(&issue_watch::IssueNotification {
            repository: "owner/repo".into(),
            number: 1,
            title: "new".into(),
            author: "a".into(),
            created_at: chrono::Utc::now(),
            url: "https://github.com/owner/repo/issues/1".into(),
        })
        .unwrap();
    assert_eq!(
        f.client
            .delete(&path)
            .header("origin", &f.base)
            .header("x-issue-watch-admin", "1")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        store
            .pending_deliveries(chrono::Utc::now())
            .unwrap()
            .iter()
            .map(|delivery| delivery.user_openid.as_str())
            .collect::<Vec<_>>(),
        ["other"]
    );
    store.bind_user("openid-a").unwrap();
    assert_eq!(
        f.client
            .delete(&path)
            .header("origin", &f.base)
            .header("x-issue-watch-admin", "1")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let rows = f.subscriptions().await;
    let rejoined = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["user_openid"] == "openid-a")
        .unwrap();
    assert_ne!(rejoined["id"], id);
    assert_eq!(rejoined["qq_number_note"], Value::Null);
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("test.sqlite3");
        let config = dir.path().join("config.toml");
        std::fs::write(&config, format!("repositories = [\"owner/repo\"]\npoll_interval_seconds = 60\ndatabase_path = {:?}\n", database.to_str().unwrap().replace('\\', "/"))).unwrap();
        let health = Health::new(vec![]);
        let admin = Admin::new(&config, health.clone()).unwrap();
        let management = admin.config.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(serve(listener, health, admin));
        Self {
            _dir: dir,
            base,
            client: reqwest::Client::new(),
            database,
            management,
            task,
        }
    }
    async fn subscriptions(&self) -> Value {
        self.client
            .get(format!("{}/api/admin/subscriptions", self.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn config(&self) -> Value {
        self.client
            .get(format!("{}/api/admin/config", self.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn edit(&self, body: Value) -> reqwest::Response {
        self.client
            .patch(format!("{}/api/admin/config", self.base))
            .header("origin", &self.base)
            .header("x-issue-watch-admin", "1")
            .json(&body)
            .send()
            .await
            .unwrap()
    }
    async fn add(&self, note: &str) -> reqwest::Response {
        self.client
            .post(format!("{}/api/admin/subscriptions", self.base))
            .header("origin", &self.base)
            .header("x-issue-watch-admin", "1")
            .json(&json!({"user_openid":"openid-a", "qq_number_note":note}))
            .send()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn repository_edits_follow_the_real_watcher_and_preserve_progress_and_queued_deliveries() {
    let f = Fixture::new().await;
    let path = f._dir.path().join("config.toml");
    let store = Store::open(&f.database).unwrap();
    let baseline = chrono::Utc::now() - chrono::Duration::hours(1);
    store.ensure_repository("owner/repo", baseline).unwrap();
    store.set_cursor("owner/repo", baseline).unwrap();
    store.bind_user("existing").unwrap();
    store
        .insert_notification(&issue_watch::IssueNotification {
            repository: "owner/repo".into(),
            number: 1,
            title: "new".into(),
            author: "a".into(),
            created_at: chrono::Utc::now(),
            url: "https://github.com/owner/repo/issues/1".into(),
        })
        .unwrap();
    let active = issue_watch::config::FileConfig::load(&path).unwrap();
    let (tx, mut rx) = tokio::sync::watch::channel(active.clone());
    let watcher = tokio::spawn(issue_watch::reload::watch_config_managed(
        issue_watch::reload::ConfigReloader::new(&path, active),
        Store::open(&f.database).unwrap(),
        tx,
        Some(f.management.clone()),
    ));
    let version = f.config().await["version"].clone();
    assert_eq!(
        f.edit(json!({"version":version,"repositories":["owner/new"]}))
            .await
            .status(),
        200
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), rx.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.config().await["stage"], "accepted");
    let updated = rx.borrow_and_update().clone();
    f.management.applied(&updated);
    assert_eq!(f.config().await["stage"], "applied");
    assert_eq!(updated.repositories, ["owner/new"]);
    assert!(
        store
            .ensure_repository("owner/new", baseline)
            .unwrap()
            .baseline
            .unwrap()
            > baseline
    );
    assert_eq!(
        store.pending_deliveries(chrono::Utc::now()).unwrap().len(),
        1
    );
    let version = f.config().await["version"].clone();
    assert_eq!(
        f.edit(json!({"version":version,"repositories":["owner/repo"]}))
            .await
            .status(),
        200
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), rx.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(store.get_cursor("owner/repo").unwrap(), Some(baseline));
    let version = f.config().await["version"].clone();
    assert_eq!(
        f.edit(json!({"version":version,"repositories":[]}))
            .await
            .status(),
        400
    );
    assert_eq!(
        f.edit(json!({"version":version,"repositories":["owner/repo","OWNER/REPO"]}))
            .await
            .status(),
        400
    );
    assert_eq!(
        f.edit(json!({"version":version,"repositories":["invalid"]}))
            .await
            .status(),
        400
    );
    watcher.abort();
}

#[tokio::test]
async fn external_invalid_or_missing_files_are_visible_without_being_overwritten() {
    let f = Fixture::new().await;
    let path = f._dir.path().join("config.toml");
    let version = f.config().await["version"].clone();
    std::fs::write(&path, "broken = [").unwrap();
    assert_eq!(
        f.edit(json!({"version":version,"poll_interval_seconds":2}))
            .await
            .status(),
        409
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken = [");
    let snapshot = f.config().await;
    assert_eq!(snapshot["stage"], "rejected");
    assert_eq!(snapshot["applied"]["poll_interval_seconds"], 60);
    assert_eq!(
        f.edit(json!({"version":snapshot["version"],"poll_interval_seconds":2}))
            .await
            .status(),
        400
    );
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        f.edit(json!({"version":snapshot["version"],"poll_interval_seconds":2}))
            .await
            .status(),
        500
    );
    assert!(!path.exists());
}

#[tokio::test]
async fn management_mutations_reject_missing_wrong_origin_and_cross_site_headers() {
    let f = Fixture::new().await;
    let path = format!("{}/api/admin/subscriptions", f.base);
    for (origin, marker, site) in [
        (None, Some("1"), None),
        (Some("null"), Some("1"), None),
        (Some(f.base.as_str()), None, None),
        (Some(f.base.as_str()), Some("1"), Some("cross-site")),
        (Some("http://localhost:1"), Some("1"), None),
    ] {
        let mut request = f
            .client
            .post(&path)
            .json(&json!({"user_openid":"rejected"}));
        if let Some(value) = origin {
            request = request.header("origin", value);
        }
        if let Some(value) = marker {
            request = request.header("x-issue-watch-admin", value);
        }
        if let Some(value) = site {
            request = request.header("sec-fetch-site", value);
        }
        assert_eq!(request.send().await.unwrap().status(), 403);
    }
    assert_eq!(f.subscriptions().await, json!([]));
}

#[tokio::test]
async fn startup_edits_do_not_redirect_management_to_another_database_or_fake_application() {
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    let original_database = directory.path().join("original.sqlite3");
    let redirected_database = directory.path().join("redirected.sqlite3");
    let original = issue_watch::config::FileConfig {
        tracking_interval_seconds: 300,
        poll_interval_seconds: 60,
        repositories: vec!["owner/repo".into()],
        database_path: original_database.to_str().unwrap().into(),
    };
    let redirected = issue_watch::config::FileConfig {
        poll_interval_seconds: 120,
        database_path: redirected_database.to_str().unwrap().into(),
        ..original.clone()
    };
    std::fs::write(&config_path, toml::to_string(&redirected).unwrap()).unwrap();
    let health = Health::new(vec![]);
    let admin = Admin::with_active(&config_path, &original, health.clone()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(serve(listener, health, admin));
    let client = reqwest::Client::new();
    let snapshot: Value = client
        .get(format!("{base}/api/admin/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snapshot["stage"], "saved");
    assert_eq!(snapshot["applied"]["poll_interval_seconds"], 60);
    assert_eq!(
        client
            .post(format!("{base}/api/admin/subscriptions"))
            .header("origin", &base)
            .header("x-issue-watch-admin", "1")
            .json(&json!({"user_openid":"original-target"}))
            .send()
            .await
            .unwrap()
            .status(),
        201
    );
    assert_eq!(
        Store::open(&original_database)
            .unwrap()
            .bound_users()
            .unwrap(),
        ["original-target"]
    );
    assert!(!redirected_database.exists());
    task.abort();
}

#[tokio::test]
async fn polling_changes_are_saved_but_not_reported_applied_and_stale_writes_are_rejected() {
    let f = Fixture::new().await;
    let before = f.config().await;
    assert_eq!(before["stage"], "applied");
    let response = f
        .client
        .patch(format!("{}/api/admin/config", f.base))
        .header("origin", &f.base)
        .header("x-issue-watch-admin", "1")
        .json(&json!({"version":before["version"],"poll_interval_seconds":120}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let saved: Value = response.json().await.unwrap();
    assert_eq!(saved["saved"]["poll_interval_seconds"], 120);
    assert_eq!(saved["applied"]["poll_interval_seconds"], 60);
    assert_eq!(saved["stage"], "saved");
    assert_eq!(
        issue_watch::config::FileConfig::load(f._dir.path().join("config.toml"))
            .unwrap()
            .poll_interval_seconds,
        120
    );
    assert_eq!(
        f.client
            .patch(format!("{}/api/admin/config", f.base))
            .header("origin", &f.base)
            .header("x-issue-watch-admin", "1")
            .json(&json!({"version":before["version"],"poll_interval_seconds":20}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let current = f.config().await;
    assert_eq!(
        f.client
            .patch(format!("{}/api/admin/config", f.base))
            .header("origin", &f.base)
            .header("x-issue-watch-admin", "1")
            .json(&json!({"version":current["version"],"poll_interval_seconds":0}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(f.config().await["saved"]["poll_interval_seconds"], 120);
}

#[tokio::test]
async fn administrator_adds_a_persistent_subscription_without_overwriting_a_duplicate_note() {
    let f = Fixture::new().await;
    assert_eq!(f.subscriptions().await, json!([]));
    assert_eq!(f.add("123456").await.status(), 201);
    assert_eq!(f.add("another note").await.status(), 200);
    let rows = f.subscriptions().await;
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["user_openid"], "openid-a");
    assert_eq!(rows[0]["qq_number_note"], "123456");
    assert_eq!(
        Store::open(&f.database).unwrap().subscriptions().unwrap()[0]
            .qq_number_note
            .as_deref(),
        Some("123456")
    );
    assert_eq!(
        f.client
            .post(format!("{}/api/admin/subscriptions", f.base))
            .header("origin", "https://evil.example")
            .header("x-issue-watch-admin", "1")
            .json(&json!({"user_openid":"evil"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
}
