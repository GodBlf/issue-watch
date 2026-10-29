//! Shared Bilibili room subscriptions and live-start notifications.
mod client;
mod storage;

use crate::Store;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
pub use client::BilibiliClient;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Serialize)]
pub struct LiveSnapshot {
    pub room_id: u64,
    pub anchor: String,
    pub title: String,
    /// 0 is offline, 1 is live, 2 is replay; other values are unknown.
    pub live_status: i64,
    pub live_time: Option<i64>,
}
impl LiveSnapshot {
    fn session(&self) -> Option<i64> {
        (self.live_status == 1)
            .then_some(self.live_time)
            .flatten()
            .filter(|t| *t > 0)
    }
    pub(crate) fn feedback(&self) -> String {
        format!(
            "{} · {} · 房间 {}\nhttps://live.bilibili.com/{}",
            self.anchor,
            if self.live_status == 1 {
                "当前正在直播"
            } else {
                "当前未确认直播中"
            },
            self.room_id,
            self.room_id
        )
    }
}

#[async_trait]
pub trait LiveSource: Send + Sync {
    async fn snapshot(&self, room: u64) -> Result<LiveSnapshot>;
}

#[derive(Clone)]
pub struct LiveMonitoring {
    database: PathBuf,
    source: Arc<dyn LiveSource>,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    store: Arc<std::sync::Mutex<Option<Store>>>,
}
impl LiveMonitoring {
    pub fn new(database: impl AsRef<Path>, source: Arc<dyn LiveSource>) -> Self {
        Self {
            database: database.as_ref().to_owned(),
            source,
            clock: Arc::new(|| chrono::Utc::now().timestamp()),
            store: Arc::new(std::sync::Mutex::new(None)),
        }
    }
    /// Replace the wall clock at the external time boundary.
    pub fn with_clock(mut self, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }
    /// Recover interrupted sends once at process startup, before starting workers.
    pub async fn recover(&self) -> Result<()> {
        self.database_operation(|store| store.recover_live_deliveries())
            .await
    }
    // SQLite can block on locks; keep connection initialization and every live
    // operation off the async executor, with one reused connection per service.
    async fn database_operation<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let database = self.database.clone();
        let cached = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let mut cached = cached
                .lock()
                .map_err(|_| anyhow::anyhow!("Live database worker lock poisoned"))?;
            if cached.is_none() {
                *cached = Some(Store::open(database)?);
            }
            let store = cached
                .as_ref()
                .context("Live database connection unavailable")?;
            operation(store)
        })
        .await
        .context("Live database task failed")?
    }
    pub fn production(database: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::new(database, Arc::new(BilibiliClient::new()?)))
    }
    pub(crate) async fn subscribe_target(&self, id: i64, input: &str) -> Result<LiveSnapshot> {
        let observed_at = chrono::Utc::now().timestamp_micros();
        let room = room_number(input)?;
        let snapshot = self
            .source
            .snapshot(room)
            .await
            .context("直播间验证失败，请稍后重试")?;
        let saved = snapshot.clone();
        let now = (self.clock)();
        self.database_operation(move |store| {
            store.add_live_subscription(id, &saved, room, now, observed_at)
        })
        .await?;
        Ok(snapshot)
    }
    /// Process private live subscription commands, committing before the caller replies.
    pub async fn command(&self, content: &str, user: &str) -> Result<Option<String>> {
        let words: Vec<_> = content.split_whitespace().collect();
        let Some(command) = words.first().copied() else {
            return Ok(None);
        };
        if !matches!(command, "/bilibili" | "/unbilibili") {
            return Ok(None);
        }
        let identity = user.to_owned();
        let target = self
            .database_operation(move |store| store.live_target(&identity))
            .await?;
        if command == "/bilibili" && words.len() == 1 {
            let rooms = if let Some(id) = target {
                self.subscriptions(id).await?
            } else {
                vec![]
            };
            return Ok(Some(if rooms.is_empty() {
                "未订阅任何直播间".into()
            } else {
                rooms
                    .iter()
                    .map(LiveSnapshot::feedback)
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }));
        }
        if words.len() != 2 {
            return Ok(Some(
                "用法：/bilibili 房间号、/bilibili 或 /unbilibili 房间号".into(),
            ));
        }
        let room = match room_number(words[1]) {
            Ok(room) => room,
            Err(_) => {
                return Ok(Some(
                    "直播订阅失败：请输入有效房间号或 live.bilibili.com 直播间链接".into(),
                ))
            }
        };
        if command == "/unbilibili" && target.is_none() {
            return Ok(Some("未订阅该直播间".into()));
        }
        if command == "/unbilibili" {
            if let Some(id) = target {
                if let Some(canonical) = self
                    .database_operation(move |store| store.known_live_room(id, room))
                    .await?
                {
                    let removed = self.remove(id, canonical).await?;
                    return Ok(Some(cancel_feedback(removed).into()));
                }
            }
        }
        let observed_at = chrono::Utc::now().timestamp_micros();
        let snapshot = match self.source.snapshot(room).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(%error,room,"Bilibili command validation failed");
                return Ok(Some("直播间验证失败，请检查房间号或稍后重试".into()));
            }
        };
        if command == "/unbilibili" {
            let removed = if let Some(id) = target {
                self.remove(id, snapshot.room_id).await?
            } else {
                false
            };
            return Ok(Some(cancel_feedback(removed).into()));
        }
        let saved = snapshot.clone();
        let identity = user.to_owned();
        let now = (self.clock)();
        self.database_operation(move |store| {
            store.add_live_user_subscription(&identity, target, &saved, room, now, observed_at)
        })
        .await?;
        Ok(Some(format!("已订阅直播间\n{}", snapshot.feedback())))
    }
    /// Check each subscribed room once, retaining the last successful state on failure.
    pub async fn check(&self) -> Result<()> {
        let rooms = self
            .database_operation(|store| store.active_live_rooms())
            .await?;
        for room in rooms {
            let observed_at = chrono::Utc::now().timestamp_micros();
            match self.source.snapshot(room).await {
                Ok(snapshot) => {
                    let now = (self.clock)();
                    self.database_operation(move |store| {
                        store.observe_live(&snapshot, now, observed_at)
                    })
                    .await?;
                }
                Err(error) => {
                    tracing::warn!(room,%error,"Bilibili check failed");
                    self.database_operation(move |store| store.live_check_error(room, observed_at))
                        .await?;
                }
            }
        }
        Ok(())
    }
    /// Send pending notifications; each recipient has its own persistent result.
    pub async fn deliver(&self, sink: &dyn crate::qq::MessageSink) -> Result<()> {
        loop {
            let now = (self.clock)();
            let Some(delivery) = self
                .database_operation(move |store| store.claim_live_delivery(now))
                .await?
            else {
                break;
            };
            let mut permit = sink.reserve_delivery().await;
            let id = delivery.id;
            let claim = delivery.claim;
            let now = (self.clock)();
            if !self
                .database_operation(move |store| store.live_delivery_valid(id, claim, now))
                .await?
            {
                continue;
            }
            permit.started();
            let result = sink.send(&delivery.user, &delivery.text).await;
            drop(permit);
            let (state, next, error) = match result {
                Ok(()) => ("sent", None, None),
                Err(crate::qq::SendError::Retryable(error)) => {
                    ("retry", Some((self.clock)() + 30), Some(error))
                }
                Err(error) => ("permanent_failure", None, Some(error.to_string())),
            };
            if let Some(error) = &error {
                tracing::warn!(%error,delivery_id=delivery.id,"Live notification send failed");
            }
            let public_error = error.as_ref().map(|_| {
                if state == "retry" {
                    "QQ 临时发送失败，稍后重试"
                } else {
                    "QQ 永久发送失败，请检查平台权限或服务日志"
                }
            });
            self.database_operation(move |store| {
                store.finish_live_delivery(id, claim, state, next, public_error)
            })
            .await?;
        }
        Ok(())
    }
    pub(crate) async fn diagnostics(&self) -> Result<serde_json::Value> {
        self.database_operation(|store| store.live_diagnostics())
            .await
    }
    /// Run shared room checks independently of GitHub and delivery backlogs.
    pub async fn run_checks(&self) -> Result<()> {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(120));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = self.check().await {
                tracing::warn!(%error,"Live monitoring cycle failed");
            }
        }
    }
    pub(crate) async fn subscriptions(&self, id: i64) -> Result<Vec<LiveSnapshot>> {
        self.database_operation(move |store| store.live_subscriptions(id))
            .await
    }
    pub(crate) async fn remove(&self, id: i64, room: u64) -> Result<bool> {
        self.database_operation(move |store| store.remove_live_subscription(id, room))
            .await
    }
}

fn room_number(input: &str) -> Result<u64> {
    let input = input.trim();
    let number = if input.bytes().all(|c| c.is_ascii_digit()) {
        input.to_owned()
    } else {
        let url = url::Url::parse(input).context("请输入直播房间号或直播间链接")?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str() != Some("live.bilibili.com")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            bail!("请输入 live.bilibili.com 直播间链接");
        }
        url.path().trim_matches('/').to_owned()
    };
    let room = number
        .parse::<u64>()
        .context("请输入有效直播房间号，不支持主播 UID")?;
    anyhow::ensure!(room > 0 && room <= i64::MAX as u64, "直播房间号无效");
    Ok(room)
}

fn cancel_feedback(removed: bool) -> &'static str {
    if removed {
        "已取消直播订阅，待发送通知已取消；已发出的请求可能仍送达"
    } else {
        "未订阅该直播间"
    }
}
