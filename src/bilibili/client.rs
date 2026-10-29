use super::{LiveSnapshot, LiveSource};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;

pub struct BilibiliClient {
    client: reqwest::Client,
    endpoint: String,
}
impl BilibiliClient {
    pub fn new() -> Result<Self> {
        Self::with_endpoint("https://api.live.bilibili.com")
    }
    pub fn with_endpoint(endpoint: &str) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .user_agent("issue-watch/0.1")
                .build()?,
            endpoint: endpoint.trim_end_matches('/').into(),
        })
    }
    async fn get(&self, path: &str, key: &str, value: u64) -> Result<Value> {
        let response: Value = self
            .client
            .get(format!("{}{path}", self.endpoint))
            .query(&[(key, value)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        anyhow::ensure!(
            response["code"].as_i64() == Some(0),
            "Bilibili 房间查询失败"
        );
        response
            .get("data")
            .filter(|v| v.is_object())
            .cloned()
            .context("Bilibili 房间响应无效")
    }
}
#[async_trait]
impl LiveSource for BilibiliClient {
    async fn snapshot(&self, room: u64) -> Result<LiveSnapshot> {
        let init = self.get("/room/v1/Room/room_init", "id", room).await?;
        let room_id = init["room_id"]
            .as_u64()
            .filter(|id| *id > 0 && *id <= i64::MAX as u64)
            .context("Bilibili 房间不存在")?;
        let info = self
            .get("/room/v1/Room/get_info", "room_id", room_id)
            .await?;
        let uid = init["uid"].as_u64().context("Bilibili 主播信息无效")?;
        let master = self.get("/live_user/v1/Master/info", "uid", uid).await?;
        let live_status = init["live_status"]
            .as_i64()
            .context("Bilibili 直播状态无效")?;
        let live_time = init["live_time"].as_i64().filter(|time| *time > 0);
        anyhow::ensure!(
            live_status != 1 || live_time.is_some(),
            "Bilibili 开播时间无效"
        );
        Ok(LiveSnapshot {
            room_id,
            anchor: master["info"]["uname"]
                .as_str()
                .context("Bilibili 主播名称无效")?
                .chars()
                .take(200)
                .collect(),
            title: info["title"]
                .as_str()
                .context("Bilibili 标题无效")?
                .chars()
                .take(1000)
                .collect(),
            live_status,
            live_time,
        })
    }
}
