//! File-backed edits and reload/application observations for the administrator.
use crate::config::FileConfig;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct ConfigManagement(Arc<Mutex<Inner>>);
struct Inner {
    path: PathBuf,
    source: Vec<u8>,
    version: u64,
    epoch: u128,
    accepted: FileConfig,
    applied: FileConfig,
    error: Option<String>,
}
#[derive(Serialize)]
pub struct ConfigSnapshot {
    pub version: String,
    pub saved: Option<FileConfig>,
    pub accepted: FileConfig,
    pub applied: FileConfig,
    pub stage: &'static str,
    pub error: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigEdit {
    pub version: String,
    pub poll_interval_seconds: Option<u64>,
    pub repositories: Option<Vec<String>>,
}
pub enum SaveError {
    Conflict,
    Invalid(String),
    Io(anyhow::Error),
}
impl From<anyhow::Error> for SaveError {
    fn from(error: anyhow::Error) -> Self {
        Self::Io(error)
    }
}

impl ConfigManagement {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = std::fs::canonicalize(path)?;
        let source = std::fs::read(&path)?;
        let config = FileConfig::parse(std::str::from_utf8(&source)?)?;
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        Ok(Self(Arc::new(Mutex::new(Inner {
            path,
            source,
            version: 1,
            epoch,
            accepted: config.clone(),
            applied: config,
            error: None,
        }))))
    }
    pub fn snapshot(&self) -> ConfigSnapshot {
        self.0.lock().unwrap().snapshot()
    }
    pub fn accepted(&self, config: &FileConfig) {
        let mut inner = self.0.lock().unwrap();
        inner.accepted = config.clone();
        inner.error = None;
    }
    pub fn rejected(&self) {
        self.0.lock().unwrap().error =
            Some("热加载失败，继续使用上一份有效配置；请检查配置文件和服务日志".into());
    }
    pub fn applied(&self, config: &FileConfig) {
        self.0.lock().unwrap().applied = config.clone();
    }
    pub fn save(&self, edit: ConfigEdit) -> Result<ConfigSnapshot, SaveError> {
        let mut inner = self.0.lock().unwrap();
        let source = inner.read().map_err(SaveError::Io)?;
        if inner.version_token() != edit.version {
            return Err(SaveError::Conflict);
        }
        let text = std::str::from_utf8(&source)
            .map_err(|_| SaveError::Invalid("当前配置不是有效 UTF-8，修正文件后重试".into()))?;
        let mut candidate = FileConfig::parse(text)
            .map_err(|_| SaveError::Invalid("当前配置无效，修正文件后重试".into()))?;
        if candidate.database_path != inner.applied.database_path {
            return Err(SaveError::Invalid(
                "数据库路径变更需要重启，请先恢复当前配置".into(),
            ));
        }
        if edit.poll_interval_seconds.is_none() && edit.repositories.is_none() {
            return Err(SaveError::Invalid("未提供要修改的配置".into()));
        }
        if let Some(interval) = edit.poll_interval_seconds {
            candidate.poll_interval_seconds = interval;
        }
        if let Some(repositories) = edit.repositories {
            candidate.repositories = repositories;
        }
        let output = toml::to_string_pretty(&candidate).map_err(anyhow::Error::from)?;
        FileConfig::parse(&output).map_err(|error| SaveError::Invalid(error.to_string()))?;
        let permissions = std::fs::metadata(&inner.path)
            .map_err(anyhow::Error::from)?
            .permissions();
        let mut temporary = tempfile::NamedTempFile::new_in(inner.path.parent().unwrap())
            .map_err(anyhow::Error::from)?;
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(anyhow::Error::from)?;
        temporary
            .write_all(output.as_bytes())
            .map_err(anyhow::Error::from)?;
        temporary
            .as_file()
            .sync_all()
            .map_err(anyhow::Error::from)?;
        // Recheck after preparing the replacement; never silently overwrite a detected external edit.
        if std::fs::read(&inner.path).map_err(anyhow::Error::from)? != source {
            return Err(SaveError::Conflict);
        }
        temporary
            .persist(&inner.path)
            .map_err(|error| SaveError::Io(error.error.into()))?;
        #[cfg(unix)]
        std::fs::File::open(inner.path.parent().unwrap())
            .and_then(|file| file.sync_all())
            .map_err(anyhow::Error::from)?;
        Ok(inner.snapshot())
    }
}
fn equivalent(left: &FileConfig, right: &FileConfig) -> bool {
    left.poll_interval_seconds == right.poll_interval_seconds
        && left.database_path == right.database_path
        && left.repositories.len() == right.repositories.len()
        && left
            .repositories
            .iter()
            .zip(&right.repositories)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}
impl Inner {
    fn version_token(&self) -> String {
        format!("{}-{}", self.epoch, self.version)
    }
    fn read(&mut self) -> Result<Vec<u8>> {
        let source = std::fs::read(&self.path)?;
        if source != self.source {
            self.source = source.clone();
            self.version += 1;
            self.error = None;
        }
        Ok(source)
    }
    fn snapshot(&mut self) -> ConfigSnapshot {
        let saved = self
            .read()
            .and_then(|bytes| Ok(FileConfig::parse(std::str::from_utf8(&bytes)?)?));
        let (saved, error) = match saved {
            Ok(saved) => (Some(saved), self.error.clone()),
            Err(_) => (
                None,
                Some("配置文件无法读取或校验失败，请修正文件后重试".into()),
            ),
        };
        let stage = if error.is_some() {
            "rejected"
        } else if saved
            .as_ref()
            .is_some_and(|saved| equivalent(saved, &self.applied))
        {
            "applied"
        } else if saved
            .as_ref()
            .is_some_and(|saved| equivalent(saved, &self.accepted))
        {
            "accepted"
        } else {
            "saved"
        };
        ConfigSnapshot {
            version: self.version_token(),
            saved,
            accepted: self.accepted.clone(),
            applied: self.applied.clone(),
            stage,
            error,
        }
    }
}
