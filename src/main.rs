use anyhow::Result;
use issue_watch::{
    github::{discover_repository, GithubClient},
    qq::QqHttpSender,
    queue::deliver_pending,
    Config, Store,
};
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    eprintln!("Loading local configuration");
    dotenvy::from_filename(".env.local").ok();
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .init();
    let config_path = std::env::var("ISSUE_WATCH_CONFIG").unwrap_or_else(|_| "config.toml".into());
    let config = Config::load(&config_path)?;
    eprintln!("Opening monitoring database");
    let store = Store::open(&config.file.database_path)?;
    for repository in &config.file.repositories {
        store.ensure_repository(repository, chrono::Utc::now())?;
    }
    let github = GithubClient::new(config.github_token.clone())?;
    eprintln!("Connecting to QQ API");
    info!("Requesting QQ access token");
    let access_token = match std::env::var("QQ_ACCESS_TOKEN") {
        Ok(token) => Some(token),
        Err(_) => match issue_watch::qq::fetch_access_token(
            &reqwest::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(20))
                .build()?,
            &config.qq_app_id,
            &config.qq_app_secret,
        )
        .await
        {
            Ok(token) => {
                eprintln!("QQ access token acquired");
                Some(token)
            }
            Err(error) => {
                eprintln!("QQ access token unavailable: {error:#}");
                warn!(%error, "QQ access token unavailable; notifications remain queued");
                None
            }
        },
    };
    let sender = access_token.clone().map(QqHttpSender::new);
    eprintln!("Starting monitoring loop");
    if let Some(token) = access_token.clone() {
        let gateway_url = match std::env::var("QQ_GATEWAY_URL") {
            Ok(url) => url,
            Err(_) => {
                info!("Requesting QQ gateway URL");
                let response = reqwest::Client::builder()
                    .no_proxy()
                    .timeout(std::time::Duration::from_secs(20))
                    .build()?
                    .get("https://api.sgroup.qq.com/gateway")
                    .header("Authorization", format!("QQBot {token}"))
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<serde_json::Value>()
                    .await?;
                response
                    .get("url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("QQ gateway URL missing"))?
                    .to_owned()
            }
        };
        let database_path = config.file.database_path.clone();
        tokio::spawn(async move {
            loop {
                match issue_watch::qq_gateway::run_gateway(&gateway_url, &token, &database_path)
                    .await
                {
                    Ok(()) => warn!("QQ gateway disconnected; reconnecting"),
                    Err(error) => warn!(%error, "QQ gateway failed; reconnecting"),
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }
    info!(
        repositories = config.file.repositories.len(),
        poll_interval_seconds = config.file.poll_interval_seconds,
        "issue-watch started"
    );
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
        config.file.poll_interval_seconds,
    ));
    loop {
        interval.tick().await;
        for repository in &config.file.repositories {
            match discover_repository(&github, &store, repository, chrono::Utc::now()).await {
                Ok(count) => info!(repository, discovered = count, "GitHub poll complete"),
                Err(error) => warn!(repository, %error, "GitHub poll failed"),
            }
        }
        if let Some(sender) = &sender {
            if let Err(error) = deliver_pending(&store, sender).await {
                warn!(%error, "notification delivery failed");
            }
        } else {
            warn!("QQ_ACCESS_TOKEN is not configured; notifications remain queued");
        }
    }
}
