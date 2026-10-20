use issue_watch::admin_auth::Auth;
use issue_watch::{
    admin::{serve, Admin},
    health::Health,
};
use std::sync::{
    atomic::{AtomicI64, Ordering},
    Arc,
};

const PASSWORD: &str = "long-test-admin-password";

/// Started only by the isolated HTTPS/browser smoke runner; no monitoring workers.
#[tokio::test]
#[ignore = "Requires the isolated Nginx/browser smoke runner"]
async fn https_browser_fixture() {
    let root = std::path::PathBuf::from(std::env::var("ISSUE_WATCH_BROWSER_FIXTURE_ROOT").unwrap());
    let config = root.join("config.toml");
    let database = root.join("data.sqlite3");
    std::fs::write(
        &config,
        format!("repositories=[\"owner/repo\"]\ndatabase_path={database:?}\n"),
    )
    .unwrap();
    let origin = std::env::var("ISSUE_WATCH_ADMIN_ORIGIN").unwrap();
    let auth = Auth::open(&database, &origin)
        .unwrap()
        .with_trusted_proxy("127.0.0.1".parse().unwrap());
    auth.reset_password(PASSWORD).unwrap();
    let health = Health::new(vec![]);
    let admin = Admin::new(config, health.clone()).unwrap().with_auth(auth);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8081")
        .await
        .unwrap();
    serve(listener, health, admin).await.unwrap();
}
struct Fixture {
    _dir: tempfile::TempDir,
    config: std::path::PathBuf,
    auth: Auth,
    now: Arc<AtomicI64>,
    base: String,
    origin: String,
    client: reqwest::Client,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        Self::with_origin(None, false).await
    }
    async fn with_origin(origin: Option<&str>, trusted: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let database = dir.path().join("data.sqlite3");
        std::fs::write(
            &config,
            format!(
                "repositories = [\"owner/repo\"]\ndatabase_path = {:?}\n",
                database
            ),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let now = Arc::new(AtomicI64::new(1_800_000_000));
        let clock = now.clone();
        let origin = origin.unwrap_or(&base).to_owned();
        let mut auth = Auth::open(&database, &origin)
            .unwrap()
            .with_clock(move || clock.load(Ordering::SeqCst));
        if trusted {
            auth = auth.with_trusted_proxy("127.0.0.1".parse().unwrap());
        }
        auth.reset_password(PASSWORD).unwrap();
        let health = Health::new(vec![]);
        let admin = Admin::new(&config, health.clone())
            .unwrap()
            .with_auth(auth.clone());
        let task = tokio::spawn(serve(listener, health, admin));
        Self {
            _dir: dir,
            config,
            auth,
            now,
            base,
            origin,
            client: reqwest::Client::new(),
            task,
        }
    }
    async fn login(&self, password: &str) -> reqwest::Response {
        self.client
            .post(format!("{}/api/auth/login", self.base))
            .header("origin", &self.origin)
            .header("x-issue-watch-admin", "1")
            .json(&serde_json::json!({"password":password}))
            .send()
            .await
            .unwrap()
    }
    async fn get(&self, path: &str, cookie: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{path}", self.base))
            .header("cookie", cookie)
            .send()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn configured_https_origin_can_manage_but_cross_site_login_and_writes_fail() {
    let f = Fixture::with_origin(Some("https://203.0.113.10"), false).await;
    let response = f.login(PASSWORD).await;
    assert_eq!(response.status(), 200);
    let token = cookie(&response);
    for origin in [
        "http://203.0.113.10",
        "https://203.0.113.10:444",
        "https://evil.example",
    ] {
        assert_eq!(
            f.client
                .post(format!("{}/api/auth/login", f.base))
                .header("origin", origin)
                .header("x-issue-watch-admin", "1")
                .json(&serde_json::json!({"password":PASSWORD}))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            f.client
                .post(format!("{}/api/auth/logout", f.base))
                .header("cookie", &token)
                .header("origin", origin)
                .header("x-issue-watch-admin", "1")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    assert_eq!(
        f.client
            .post(format!("{}/api/admin/subscriptions", f.base))
            .header("cookie", &token)
            .header("origin", &f.origin)
            .header("x-issue-watch-admin", "1")
            .json(&serde_json::json!({"user_openid":"new-user"}))
            .send()
            .await
            .unwrap()
            .status(),
        201
    );
}

#[tokio::test]
async fn spoofed_forwarding_headers_cannot_escape_source_limit_and_limit_recovers() {
    let f = Fixture::new().await;
    for n in 0..6 {
        let response = f
            .client
            .post(format!("{}/api/auth/login", f.base))
            .header("origin", &f.origin)
            .header("x-issue-watch-admin", "1")
            .header("x-real-ip", format!("192.0.2.{}", n + 1))
            .json(&serde_json::json!({"password":"wrong"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), if n < 5 { 401 } else { 429 });
    }
    f.now.fetch_add(60, Ordering::SeqCst);
    assert_eq!(f.login(PASSWORD).await.status(), 200);
}

#[tokio::test]
async fn trusted_proxy_sources_still_share_a_global_login_budget() {
    let f = Fixture::with_origin(None, true).await;
    for n in 0..21 {
        let response = f
            .client
            .post(format!("{}/api/auth/login", f.base))
            .header("origin", &f.origin)
            .header("x-issue-watch-admin", "1")
            .header("x-real-ip", format!("192.0.2.{}", n + 1))
            .json(&serde_json::json!({"password":"wrong"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), if n < 20 { 401 } else { 429 });
        if n == 20 {
            assert_eq!(response.headers()["retry-after"], "60");
        }
    }
}

#[tokio::test]
async fn reset_racing_with_old_password_logins_leaves_no_old_password_session_usable() {
    let f = Fixture::new().await;
    let auth = f.auth.clone();
    let reset =
        tokio::task::spawn_blocking(move || auth.reset_password("a-new-racing-admin-password"));
    let (one, two) = tokio::join!(f.login(PASSWORD), f.login(PASSWORD));
    reset.await.unwrap().unwrap();
    for response in [one, two] {
        if response.status() == 200 {
            assert_eq!(
                f.get("/api/auth/session", &cookie(&response))
                    .await
                    .status(),
                401
            );
        } else {
            assert!([401, 429].contains(&response.status().as_u16()));
        }
    }
    assert_eq!(f.login(PASSWORD).await.status(), 401);
    assert_eq!(f.login("a-new-racing-admin-password").await.status(), 200);
}
fn cookie(response: &reqwest::Response) -> String {
    response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn logout_revokes_only_this_device_and_ssh_reset_revokes_every_device() {
    let f = Fixture::new().await;
    let first = cookie(&f.login(PASSWORD).await);
    let second = cookie(&f.login(PASSWORD).await);
    assert_ne!(first, second);
    assert_eq!(
        f.client
            .post(format!("{}/api/auth/logout", f.base))
            .header("cookie", &first)
            .header("origin", &f.base)
            .header("x-issue-watch-admin", "1")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(f.get("/api/admin/config", &first).await.status(), 401);
    assert_eq!(f.get("/api/admin/config", &second).await.status(), 200);
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_issue-watch"))
        .arg("--admin-password-stdin")
        .env("ISSUE_WATCH_CONFIG", &f.config)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"a-new-long-admin-password\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("a-new-long-admin-password"));
    assert_eq!(f.get("/api/admin/config", &second).await.status(), 401);
    assert_eq!(f.login(PASSWORD).await.status(), 401);
    assert_eq!(f.login("a-new-long-admin-password").await.status(), 200);
}

#[tokio::test]
async fn password_login_creates_a_fixed_one_year_session_that_survives_restart() {
    let mut f = Fixture::new().await;
    assert_eq!(f.login("wrong-password").await.status(), 401);
    let response = f.login(PASSWORD).await;
    assert_eq!(response.status(), 200);
    let header = response.headers()["set-cookie"].to_str().unwrap();
    for attribute in [
        "Secure",
        "HttpOnly",
        "SameSite=Strict",
        "Max-Age=31536000",
        "Path=/",
    ] {
        assert!(header.contains(attribute), "{header}");
    }
    let token = cookie(&response);
    assert_eq!(f.get("/api/admin/config", &token).await.status(), 200);
    f.task.abort();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    f.base = format!("http://{}", listener.local_addr().unwrap());
    let health = Health::new(vec![]);
    let clock = f.now.clone();
    let database = f._dir.path().join("data.sqlite3");
    f.auth = Auth::open(database, &f.base)
        .unwrap()
        .with_clock(move || clock.load(Ordering::SeqCst));
    let admin = Admin::new(&f.config, health.clone())
        .unwrap()
        .with_auth(f.auth.clone());
    f.task = tokio::spawn(serve(listener, health, admin));
    f.now.store(1_831_535_999, Ordering::SeqCst);
    assert_eq!(f.get("/api/admin/config", &token).await.status(), 200);
    f.now.store(1_831_536_000, Ordering::SeqCst);
    assert_eq!(f.get("/api/admin/config", &token).await.status(), 401);
}

#[tokio::test]
async fn management_requires_login_including_reads_and_the_dashboard() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "repositories = [\"owner/repo\"]\ndatabase_path = {:?}\n",
            dir.path().join("data.sqlite3")
        ),
    )
    .unwrap();
    let health = Health::new(vec![]);
    let admin = Admin::new(&config, health.clone()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(serve(listener, health, admin));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    for path in [
        "/api/admin/config",
        "/api/admin/subscriptions",
        "/api/admin/repositories",
        "/api/admin/tracking",
        "/api/admin/permissions",
        "/api/status",
    ] {
        assert_eq!(
            client
                .get(format!("{base}{path}"))
                .header("x-issue-watch-admin", "1")
                .send()
                .await
                .unwrap()
                .status(),
            401,
            "{path}"
        );
    }
    for (method, path) in [
        ("POST", "/api/admin/subscriptions"),
        ("PATCH", "/api/admin/subscriptions/1"),
        ("DELETE", "/api/admin/subscriptions/1"),
        ("PUT", "/api/admin/subscriptions/1/repositories"),
        ("PATCH", "/api/admin/config"),
        ("POST", "/api/admin/tracking"),
        ("DELETE", "/api/admin/tracking/1"),
        ("PUT", "/api/admin/permissions/user"),
    ] {
        assert_eq!(
            client
                .request(
                    method.parse::<reqwest::Method>().unwrap(),
                    format!("{base}{path}")
                )
                .header("x-issue-watch-admin", "1")
                .header(
                    "cookie",
                    format!("__Host-issue_watch_session={}", "0".repeat(64))
                )
                .json(&serde_json::json!({}))
                .send()
                .await
                .unwrap()
                .status(),
            401,
            "{method} {path}"
        );
    }
    assert_eq!(client.get(&base).send().await.unwrap().status(), 303);
    assert_eq!(
        client
            .get(format!("{base}/login"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{base}/internal/status"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    task.abort();
}
