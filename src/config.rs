use anyhow::{Context, Result};
use serde::Deserialize;
use std::{env, fs, path::Path};

#[derive(Debug, Clone, Deserialize)]
pub struct FileConfig {
    #[serde(default = "default_poll_interval")]
    pub poll_interval_seconds: u64,
    pub repositories: Vec<String>,
    #[serde(default = "default_database_path")]
    pub database_path: String,
}

fn default_poll_interval() -> u64 {
    60
}
fn default_database_path() -> String {
    "data/issue-watch.sqlite3".to_string()
}

#[derive(Debug, Clone)]
pub struct Config {
    pub file: FileConfig,
    pub qq_app_id: String,
    pub qq_app_secret: String,
    pub github_token: Option<String>,
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content =
            fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
        let file: FileConfig = toml::from_str(&content).context("parse TOML config")?;
        validate_repositories(&file.repositories)?;
        let qq_app_id = env::var("QQ_APP_ID")
            .or_else(|_| env::var("QQ_BOT_APP_ID"))
            .context("QQ_APP_ID or QQ_BOT_APP_ID is required")?;
        let qq_app_secret = env::var("QQ_APP_SECRET")
            .or_else(|_| env::var("QQ_BOT_CLIENT_SECRET"))
            .context("QQ_APP_SECRET or QQ_BOT_CLIENT_SECRET is required")?;
        Ok(Self {
            file,
            qq_app_id,
            qq_app_secret,
            github_token: env::var("GITHUB_TOKEN").ok(),
        })
    }
}

fn validate_repositories(repositories: &[String]) -> Result<()> {
    if repositories.is_empty() {
        anyhow::bail!("at least one repository is required")
    }
    let mut seen = std::collections::HashSet::new();
    for repo in repositories {
        let parts: Vec<_> = repo.split('/').collect();
        if parts.len() != 2 || parts.iter().any(|part| part.trim().is_empty()) {
            anyhow::bail!("repository must use owner/name format: {repo}")
        }
        if !seen.insert(repo) {
            anyhow::bail!("duplicate repository: {repo}")
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn validates_repository_shape() {
        assert!(validate_repositories(&["owner/repo".into()]).is_ok());
        assert!(validate_repositories(&["bad".into()]).is_err());
        assert!(validate_repositories(&["owner/repo".into(), "owner/repo".into()]).is_err());
    }

    #[test]
    fn parses_defaults() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "repositories = [\"owner/repo\"]").unwrap();
        let parsed: FileConfig =
            toml::from_str(&std::fs::read_to_string(file.path()).unwrap()).unwrap();
        assert_eq!(parsed.poll_interval_seconds, 60);
        assert_eq!(parsed.database_path, "data/issue-watch.sqlite3");
    }
}
