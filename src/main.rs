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
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();
    let config_path = std::env::var("ISSUE_WATCH_CONFIG").unwrap_or_else(|_| "config.toml".into());
    let config = Config::load(&config_path)?;
    let store = Store::open(&config.file.database_path)?;
    for repository in &config.file.repositories {
        store.ensure_repository(repository, chrono::Utc::now())?;
    }
    let github = GithubClient::new(config.github_token.clone())?;
    let access_token = match std::env::var("QQ_ACCESS_TOKEN") {
        Ok(token) => Some(token),
        Err(_) => match issue_watch::qq::fetch_access_token(
            &reqwest::Client::new(),
            &config.qq_app_id,
            &config.qq_app_secret,
        )
        .await
        {
            Ok(token) => Some(token),
            Err(error) => {
                warn!(%error, "QQ access token unavailable; notifications remain queued");
                None
            }
        },
    };
    let sender = access_token.map(QqHttpSender::new);
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
