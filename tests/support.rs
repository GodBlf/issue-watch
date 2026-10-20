use issue_watch::{
    admin::{serve, Admin},
    admin_auth::Auth,
    health::Health,
};

pub async fn authenticated_server(
    listener: tokio::net::TcpListener,
    health: Health,
    admin: Admin,
    database: &std::path::Path,
) -> (
    reqwest::Client,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let base = format!("http://{}", listener.local_addr().unwrap());
    let auth = Auth::open(database, &base).unwrap();
    auth.reset_password("test-admin-long-password").unwrap();
    let task = tokio::spawn(serve(listener, health, admin.with_auth(auth)));
    let response = reqwest::Client::new()
        .post(format!("{base}/api/auth/login"))
        .header("origin", &base)
        .header("x-issue-watch-admin", "1")
        .json(&serde_json::json!({"password":"test-admin-long-password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    // Explicitly replay the Secure cookie through the HTTP test transport. Production
    // browser storage and delivery are verified separately through actual HTTPS.
    let token = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::COOKIE, token.parse().unwrap());
    (
        reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap(),
        task,
    )
}
