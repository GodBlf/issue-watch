//! Shared Issue tracking: GitHub snapshots enter here; commands and admin use the same rules.
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Activity {
    pub key: String,
    pub kind: String,
    pub at: DateTime<Utc>,
    pub actor: String,
    pub text: String,
    pub url: String,
    pub related: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IssueSnapshot {
    pub title: String,
    pub url: String,
    pub state: String,
    pub is_pull_request: bool,
    pub activities: Vec<Activity>,
    pub linked_prs: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrackedIssue {
    pub id: i64,
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub started_at: DateTime<Utc>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingPermissions {
    #[serde(default)]
    pub user_openid: String,
    pub can_add: bool,
    pub can_cancel: bool,
}
pub async fn tracking_command(
    store: &mut crate::Store,
    source: &dyn TrackingSource,
    content: &str,
    user: &str,
) -> Result<Option<String>> {
    let parts: Vec<_> = content.split_whitespace().collect();
    let Some(command) = parts.first().copied() else {
        return Ok(None);
    };
    if !matches!(command, "/track" | "/untrack" | "/tracking") {
        return Ok(None);
    }
    let permissions = store.tracking_permissions(user)?;
    let allowed = match command {
        "/track" => permissions.can_add,
        "/untrack" => permissions.can_cancel,
        _ => {
            permissions.can_add
                || permissions.can_cancel
                || store.bound_users()?.iter().any(|id| id == user)
        }
    };
    if !allowed {
        return Ok(Some(
            "权限不足：你没有执行此命令的权限，请联系后台管理人员。".into(),
        ));
    }
    let result: Result<String> = async move {
        if command == "/tracking" {
            anyhow::ensure!(parts.len() <= 2, "用法：/tracking [完整仓库 URL]");
            let repository = parts.get(1).map(|url| repository_url(url)).transpose()?;
            let page = parts
                .get(1)
                .and_then(|s| url::Url::parse(s).ok())
                .and_then(|url| {
                    url.query_pairs()
                        .find(|(key, _)| key == "page")
                        .and_then(|(_, v)| v.parse::<usize>().ok())
                })
                .unwrap_or(1)
                .max(1);
            let issues: Vec<_> = store
                .tracked_issues()?
                .into_iter()
                .filter(|i| {
                    repository
                        .as_ref()
                        .is_none_or(|r| i.repository.eq_ignore_ascii_case(r))
                })
                .collect();
            let pages = issues.len().div_ceil(10).max(1);
            anyhow::ensure!(page <= pages, "页码超出范围");
            let mut text = format!("共享追踪清单 · {} 项 · 第 {page}/{pages} 页", issues.len());
            for issue in issues.iter().skip((page - 1) * 10).take(10) {
                text.push_str(&format!(
                    "\n{} #{} · {}\n{}",
                    issue.repository,
                    issue.number,
                    issue.title.chars().take(40).collect::<String>(),
                    issue.url
                ));
            }
            if pages > 1 {
                text.push_str("\n请按仓库查询；在仓库 URL 后加 ?page=2 可查询下一页。");
            }
            return Ok(text.chars().take(1800).collect());
        }
        anyhow::ensure!(parts.len() == 2, "用法：{command} <完整 Issue URL>");
        let (repository, number) = issue_url(parts[1])?;
        if command == "/untrack" {
            let issue = store
                .tracked_issues()?
                .into_iter()
                .find(|i| i.repository.eq_ignore_ascii_case(&repository) && i.number == number);
            let removed = if let Some(issue) = issue {
                store.cancel_tracking(issue.id)?
            } else {
                false
            };
            return Ok(if removed {
                "已取消共享追踪，所有订阅者将停止收到此 Issue 的动态。"
            } else {
                "该 Issue 未在追踪中"
            }
            .into());
        }
        anyhow::ensure!(
            store.tracking_repository_allowed(&repository)?,
            "只能追踪已配置的监控仓库"
        );
        let now = Utc::now();
        let snapshot = source.snapshot(&repository, number).await?;
        // An administrator may revoke permission while the external request is in flight.
        anyhow::ensure!(
            store.tracking_permissions(user)?.can_add,
            "权限不足：你没有执行此命令的权限，请联系后台管理人员。"
        );
        let (added, issue) = store.add_tracking(&repository, number, &snapshot, now)?;
        Ok(format!(
            "{}\n标题：{}\n状态：{}\n链接：{}",
            if added {
                "已添加共享追踪"
            } else {
                "该 Issue 已在追踪中"
            },
            issue.title.chars().take(120).collect::<String>(),
            snapshot.state,
            issue.url
        ))
    }
    .await;
    Ok(Some(result.unwrap_or_else(|e| e.to_string())))
}
#[async_trait]
pub trait TrackingSource: Send + Sync {
    async fn snapshot(&self, repository: &str, number: u64) -> Result<IssueSnapshot>;
}
pub async fn check_tracked(store: &mut crate::Store, source: &dyn TrackingSource) -> Result<usize> {
    let mut count = 0;
    for issue in store.tracked_issues()? {
        match source.snapshot(&issue.repository, issue.number).await {
            Ok(snapshot) => {
                count += store.apply_tracking_snapshot(&issue, &snapshot, Utc::now())?;
            }
            Err(error) => {
                store.tracking_failed(issue.id, &format!("{error:#}"))?;
            }
        }
    }
    Ok(count)
}
pub(crate) fn render_activity(
    issue: &TrackedIssue,
    snapshot: &IssueSnapshot,
    events: &[Activity],
) -> String {
    let mut display: Vec<&Activity> = vec![];
    for event in events {
        if matches!(event.kind.as_str(), "referenced" | "linked") {
            if let Some(index) = display.iter().position(|other| {
                matches!(other.kind.as_str(), "referenced" | "linked")
                    && other.related.is_some()
                    && other.related == event.related
            }) {
                if event.kind == "linked" {
                    display[index] = event;
                }
                continue;
            }
        }
        display.push(event);
    }
    let mut text = format!(
        "[Issue追踪] {} #{}\n标题: {}\n当前状态: {}\n新增动态: {} 条",
        issue.repository,
        issue.number,
        snapshot.title,
        snapshot.state,
        display.len()
    );
    if events.iter().any(|event| event.kind == "closed") {
        text.push_str("\n追踪已结束：Issue 关闭，重新打开需重新添加。");
    }
    for event in display.iter().take(6) {
        let kind = match event.kind.as_str() {
            "comment" => "新评论",
            "referenced" => "引用",
            "linked" => "关联 PR",
            "closed" => "Issue 关闭",
            "merged" => "PR 合并",
            "pr_closed" => "PR 关闭",
            "pr_reopened" => "PR 重新打开",
            other => other,
        };
        text.push_str(&format!(
            "\n{} · {}\n{}\n链接: {}",
            kind,
            event.actor,
            event.text.chars().take(140).collect::<String>(),
            event.url
        ));
    }
    if events.len() > 6 {
        text.push_str("\n其余动态请查看 Issue。");
    }
    // Always keep the primary URL even when excerpts fill the message budget.
    let footer = format!("\n链接: {}", issue.url);
    let budget = 1700usize.saturating_sub(footer.chars().count());
    text = text.chars().take(budget).collect();
    text.push_str(&footer);
    text
}
pub async fn deliver_tracking<S: crate::qq::MessageSink + ?Sized>(
    store: &crate::Store,
    sink: &S,
) -> Result<usize> {
    deliver_tracking_inner(store, sink, None, Utc::now()).await
}
/// Send deliveries due at the supplied external clock observation.
pub async fn deliver_tracking_due<S: crate::qq::MessageSink + ?Sized>(
    store: &crate::Store,
    sink: &S,
    now: DateTime<Utc>,
) -> Result<usize> {
    deliver_tracking_inner(store, sink, None, now).await
}
pub async fn deliver_tracking_observed<S: crate::qq::MessageSink + ?Sized>(
    store: &crate::Store,
    sink: &S,
    health: &crate::qq_health::QqHealth,
) -> Result<usize> {
    deliver_tracking_inner(store, sink, Some(health), Utc::now()).await
}
async fn deliver_tracking_inner<S: crate::qq::MessageSink + ?Sized>(
    store: &crate::Store,
    sink: &S,
    health: Option<&crate::qq_health::QqHealth>,
    now: DateTime<Utc>,
) -> Result<usize> {
    let mut sent = 0;
    for (index, delivery) in store
        .pending_tracking_deliveries(now)?
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        if !store.tracking_delivery_pending(&delivery)? {
            continue;
        }
        if let Some(health) = health {
            health.progress(true, "sending");
        }
        let result = sink.send(&delivery.user_openid, &delivery.text).await;
        if let Some(health) = health {
            match &result {
                Ok(()) => {
                    health.sent("sent", None);
                    health.authentication(true, None);
                }
                Err(crate::qq::SendError::Permanent(e)) => health.sent("permanent_failed", Some(e)),
                Err(crate::qq::SendError::Authentication(e)) => {
                    health.sent("retrying", Some(e));
                    health.authentication(false, Some(e));
                }
                Err(crate::qq::SendError::Retryable(e)) => health.sent("retrying", Some(e)),
            }
        }
        match result {
            Ok(()) => {
                store.finish_tracking_delivery(&delivery, "sent", None, None)?;
                sent += 1;
            }
            Err(crate::qq::SendError::Retryable(e)) => store.finish_tracking_delivery(
                &delivery,
                "retry",
                Some(now + chrono::Duration::seconds(30)),
                Some(&e),
            )?,
            Err(crate::qq::SendError::Authentication(e)) => store.finish_tracking_delivery(
                &delivery,
                "retry",
                Some(now + chrono::Duration::seconds(30)),
                Some(&e),
            )?,
            Err(crate::qq::SendError::Permanent(e)) => {
                store.finish_tracking_delivery(&delivery, "permanent_failure", None, Some(&e))?
            }
        }
        if let Some(health) = health {
            health.progress(true, "recorded");
            health.refresh_queue(store)?;
        }
    }
    if let Some(health) = health {
        health.progress(false, "idle");
        health.refresh_queue(store)?;
    }
    Ok(sent)
}

pub fn issue_url(input: &str) -> Result<(String, u64)> {
    let url = url::Url::parse(input).context("请输入完整的 GitHub Issue URL")?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none(),
        "请输入 https://github.com/owner/repo/issues/编号"
    );
    let parts: Vec<_> = url
        .path()
        .trim_end_matches('/')
        .split('/')
        .skip(1)
        .collect();
    anyhow::ensure!(
        parts.len() == 4 && parts[2] == "issues",
        "请输入完整的 GitHub Issue URL，不支持 PR 链接"
    );
    let repository = repository_parts(&parts[..2])?;
    let number = parts[3].parse::<u64>().context("Issue 编号无效")?;
    anyhow::ensure!(number > 0 && number <= i64::MAX as u64, "Issue 编号无效");
    Ok((repository, number))
}
fn repository_parts(parts: &[&str]) -> Result<String> {
    anyhow::ensure!(
        parts.len() == 2
            && parts.iter().all(|part| !part.is_empty()
                && *part != "."
                && *part != ".."
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))),
        "仓库 URL 无效"
    );
    Ok(format!("{}/{}", parts[0], parts[1]))
}
pub fn repository_url(input: &str) -> Result<String> {
    let url = url::Url::parse(input).context("请输入完整的 GitHub 仓库 URL")?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none(),
        "仓库 URL 无效"
    );
    repository_parts(
        &url.path()
            .trim_end_matches('/')
            .split('/')
            .skip(1)
            .collect::<Vec<_>>(),
    )
}

pub struct GithubTrackingSource {
    client: reqwest::Client,
    token: Option<String>,
    base: url::Url,
}
impl GithubTrackingSource {
    pub fn new(token: Option<String>) -> Result<Self> {
        Self::with_api_base(token, "https://api.github.com/")
    }
    pub fn with_api_base(token: Option<String>, base: &str) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent("issue-watch/0.1")
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            token,
            base: url::Url::parse(base)?,
        })
    }
    async fn get(&self, path: &str) -> Result<serde_json::Value> {
        let mut request = self
            .client
            .get(self.base.join(path)?)
            .header("Accept", "application/vnd.github+json");
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("该 Issue 不存在");
        }
        response
            .error_for_status()?
            .json()
            .await
            .context("读取 GitHub 数据失败")
    }
    async fn timeline(
        &self,
        repository: &str,
        number: u64,
        pull_request: bool,
    ) -> Result<Vec<serde_json::Value>> {
        let token = self
            .token
            .as_deref()
            .context("Issue 追踪需要配置 GITHUB_TOKEN，以读取 GitHub 动态及关联 PR")?;
        let (owner, name) = repository.split_once('/').context("仓库名称无效")?;
        let query = if pull_request {
            PR_TIMELINE
        } else {
            ISSUE_TIMELINE
        };
        let mut nodes = vec![];
        let mut after: Option<String> = None;
        loop {
            let response:serde_json::Value=self.client.post(self.base.join("graphql")?).bearer_auth(token)
                .json(&serde_json::json!({"query":query,"variables":{"owner":owner,"name":name,"number":number,"after":after}}))
                .send().await?.error_for_status()?.json().await?;
            anyhow::ensure!(
                response.get("errors").is_none(),
                "GitHub 时间线查询失败，请检查 Token 权限或 API 配额"
            );
            let resource = if pull_request { "pullRequest" } else { "issue" };
            let timeline = &response["data"]["repository"][resource]["timelineItems"];
            nodes.extend(
                timeline["nodes"]
                    .as_array()
                    .context("GitHub 时间线不可访问")?
                    .iter()
                    .cloned(),
            );
            if timeline["pageInfo"]["hasNextPage"].as_bool() == Some(false) {
                break;
            }
            let next = timeline["pageInfo"]["endCursor"]
                .as_str()
                .context("GitHub 时间线分页信息无效")?
                .to_owned();
            anyhow::ensure!(after.as_ref() != Some(&next), "GitHub 时间线分页未推进");
            after = Some(next);
        }
        Ok(nodes)
    }
}
const ISSUE_TIMELINE: &str = r#"query($owner:String!,$name:String!,$number:Int!,$after:String){repository(owner:$owner,name:$name){issue(number:$number){timelineItems(first:100,after:$after,itemTypes:[ISSUE_COMMENT,CROSS_REFERENCED_EVENT,CONNECTED_EVENT,DISCONNECTED_EVENT,CLOSED_EVENT]){pageInfo{hasNextPage endCursor} nodes{__typename ... on IssueComment{id createdAt body url author{login}} ... on CrossReferencedEvent{id createdAt actor{login} willCloseTarget source{__typename ... on Issue{title url} ... on PullRequest{title url}}} ... on ConnectedEvent{id createdAt actor{login} source{__typename ... on PullRequest{title url}} subject{__typename ... on PullRequest{title url}}} ... on DisconnectedEvent{id createdAt actor{login} source{__typename ... on PullRequest{title url}} subject{__typename ... on PullRequest{title url}}} ... on ClosedEvent{id createdAt actor{login}}}}}}}"#;
const PR_TIMELINE: &str = r#"query($owner:String!,$name:String!,$number:Int!,$after:String){repository(owner:$owner,name:$name){pullRequest(number:$number){timelineItems(first:100,after:$after,itemTypes:[MERGED_EVENT,CLOSED_EVENT,REOPENED_EVENT]){pageInfo{hasNextPage endCursor} nodes{__typename ... on MergedEvent{id createdAt actor{login}} ... on ClosedEvent{id createdAt actor{login}} ... on ReopenedEvent{id createdAt actor{login}}}}}}}"#;
#[async_trait]
impl TrackingSource for GithubTrackingSource {
    async fn snapshot(&self, repository: &str, number: u64) -> Result<IssueSnapshot> {
        let item = self
            .get(&format!("repos/{repository}/issues/{number}"))
            .await?;
        let mut snapshot = IssueSnapshot {
            title: item["title"].as_str().context("缺少 Issue 标题")?.into(),
            url: format!("https://github.com/{repository}/issues/{number}"),
            state: item["state"].as_str().context("缺少 Issue 状态")?.into(),
            is_pull_request: item.get("pull_request").is_some(),
            activities: vec![],
            linked_prs: vec![],
        };
        if snapshot.is_pull_request {
            return Ok(snapshot);
        }
        let mut linked = std::collections::BTreeSet::new();
        for node in self.timeline(repository, number, false).await? {
            let kind = node["__typename"].as_str().unwrap_or("");
            let at = node["createdAt"]
                .as_str()
                .context("GitHub 动态缺少时间")?
                .parse::<DateTime<Utc>>()?;
            let key = node["id"]
                .as_str()
                .context("GitHub 动态缺少标识")?
                .to_owned();
            let actor = node["actor"]["login"]
                .as_str()
                .or(node["author"]["login"].as_str())
                .unwrap_or("unknown")
                .to_owned();
            let (event_kind, text, url, related) = match kind {
                "IssueComment" => (
                    "comment",
                    node["body"].as_str().unwrap_or("").to_owned(),
                    node["url"].as_str().unwrap_or(&snapshot.url).to_owned(),
                    None,
                ),
                "ClosedEvent" => (
                    "closed",
                    "Issue 已关闭，自动结束追踪".into(),
                    snapshot.url.clone(),
                    None,
                ),
                "CrossReferencedEvent" | "ConnectedEvent" | "DisconnectedEvent" => {
                    let source = if kind == "CrossReferencedEvent"
                        || node["source"]["__typename"] == "PullRequest"
                    {
                        &node["source"]
                    } else {
                        &node["subject"]
                    };
                    let Some(url) = source["url"].as_str() else {
                        continue;
                    };
                    let is_pr = source["__typename"] == "PullRequest";
                    let explicit =
                        is_pr && (kind == "ConnectedEvent" || node["willCloseTarget"] == true);
                    if kind == "DisconnectedEvent" {
                        linked.remove(url);
                        continue;
                    }
                    if explicit {
                        linked.insert(url.to_owned());
                    }
                    (
                        if explicit { "linked" } else { "referenced" },
                        source["title"].as_str().unwrap_or("相关条目").to_owned(),
                        url.to_owned(),
                        Some(url.to_owned()),
                    )
                }
                _ => continue,
            };
            snapshot.activities.push(Activity {
                key,
                kind: event_kind.into(),
                at,
                actor,
                text,
                url,
                related,
            });
        }
        snapshot.linked_prs = linked.into_iter().collect();
        for pr in &snapshot.linked_prs {
            let url = url::Url::parse(pr)?;
            let parts: Vec<_> = url
                .path()
                .trim_end_matches('/')
                .split('/')
                .skip(1)
                .collect();
            anyhow::ensure!(
                url.host_str() == Some("github.com") && parts.len() == 4 && parts[2] == "pull",
                "GitHub 关联 PR URL 无效"
            );
            let repository = repository_parts(&parts[..2])?;
            let number = parts[3].parse::<u64>()?;
            let nodes = self.timeline(&repository, number, true).await?;
            let merged: std::collections::HashSet<_> = nodes
                .iter()
                .filter(|n| n["__typename"] == "MergedEvent")
                .filter_map(|n| n["createdAt"].as_str())
                .collect();
            for node in &nodes {
                let kind = match node["__typename"].as_str() {
                    Some("MergedEvent") => "merged",
                    Some("ReopenedEvent") => "pr_reopened",
                    Some("ClosedEvent")
                        if !merged.contains(node["createdAt"].as_str().unwrap_or("")) =>
                    {
                        "pr_closed"
                    }
                    _ => continue,
                };
                snapshot.activities.push(Activity {
                    key: node["id"].as_str().context("关联 PR 动态缺少标识")?.into(),
                    kind: kind.into(),
                    at: node["createdAt"]
                        .as_str()
                        .context("关联 PR 动态缺少时间")?
                        .parse()?,
                    actor: node["actor"]["login"].as_str().unwrap_or("unknown").into(),
                    text: format!("{repository} #{number}"),
                    url: pr.clone(),
                    related: Some(pr.clone()),
                });
            }
        }
        Ok(snapshot)
    }
}
