# GitHub Issue 监控与 QQ 私聊通知

Triage: ready-for-agent

## Problem Statement

使用者需要自行部署一个服务，持续监控配置文件中列出的多个 GitHub 公开仓库。当仓库出现新建 Issue 时，服务应在可接受的延迟内通过 QQ 官方机器人发送私聊通知。服务需要跨重启保存监控进度和待发送消息，使停机期间发现的新建 Issue 能在恢复后补发，同时避免正常轮询重复通知。

第一版只解决 Issue 监控和通知。Issue 筛选规则、自动调用 Codex 或其他 agent、自动修复、自动发布 PR、QQ 群聊以及多通知目标都不在本 spec 内。

## Solution

实现一个 Rust 服务，使用 TOML 配置监控仓库和轮询间隔，使用 SQLite 保存监控基线、已发现的新建 Issue、通知投递状态及 QQ 私聊绑定信息。

服务按默认 60 秒的可配置间隔轮询 GitHub API，读取每个监控仓库的新 Issue，排除 GitHub 返回的 Pull Request。第一次加入监控的仓库建立当前时间基线，不通知已有历史 Issue；已有仓库依据 SQLite 中的进度继续检查。服务停止期间产生的新 Issue 在恢复后加入待发送队列，并按创建时间逐条投递。

QQ 官方机器人通过 WebSocket 建立事件连接，完成鉴权、心跳、重连和会话恢复；通知发送通过官方 HTTP OpenAPI。使用者向机器人发送 `/bind` 后，服务从私聊事件取得 `user_openid` 并保存为唯一的固定通知目标。普通 QQ 号不作为发送 API 的目标参数，也不写入配置或环境变量。

投递采用可重试队列和退避策略，优先避免漏发。临时网络错误、限频和配额不足会延后重试；目标无权限、内容不允许等永久性错误保留记录并告警。发送结果不确定时允许故障场景下偶尔重复。通知为纯文本，包含监控仓库、Issue 标题、作者、创建时间和链接；若链接被接口拒绝，降级为包含仓库和 Issue 编号的文本并记录告警。

部署提供本机运行方式、Dockerfile 和 Docker Compose 示例。QQ AppID、AppSecret 及可选 GitHub Token 从环境变量读取；`.env.local` 只用于本机，Docker 通过环境注入。SQLite 数据目录通过卷持久化，凭据和运行数据不得提交 Git。

## User Stories

1. As a self-hosting user, I want to list multiple GitHub repositories in one TOML configuration, so that one service can monitor all of them.
2. As a self-hosting user, I want to configure the polling interval, so that I can balance notification latency and API usage.
3. As a self-hosting user, I want the default polling interval to be 60 seconds, so that the service works with sensible defaults.
4. As a self-hosting user, I want every newly created Issue to be notified in the first version, so that no filtering configuration is required before I can validate monitoring.
5. As a self-hosting user, I want Pull Requests excluded from Issue notifications, so that the notification stream contains actual Issues only.
6. As a self-hosting user, I want a newly added monitoring repository to start from the current time, so that adding a repository does not flood me with historical notifications.
7. As a self-hosting user, I want the service to continue from the persisted progress of an existing monitoring repository, so that a restart does not resend the same Issues during normal operation.
8. As a self-hosting user, I want Issues created while the service is stopped to be discovered after restart, so that downtime does not create a monitoring gap.
9. As a self-hosting user, I want queued Issue notifications sent one at a time in creation order, so that a long downtime produces an understandable sequence of messages.
10. As a self-hosting user, I want the original Issue creation time included in each message, so that a delayed notification is not mistaken for a newly created Issue.
11. As a self-hosting user, I want discovered Issues persisted independently from delivery state, so that a QQ failure does not discard a discovered Issue.
12. As a self-hosting user, I want temporary delivery failures retried with backoff, so that transient outages recover without manual intervention.
13. As a self-hosting user, I want rate limits and daily quotas to delay delivery rather than lose messages, so that QQ limits do not create silent gaps.
14. As a self-hosting user, I want uncertain delivery outcomes to favor retrying, so that the system minimizes missed notifications even if a rare duplicate occurs.
15. As a self-hosting user, I want to bind one QQ private-chat target by sending `/bind`, so that I do not have to enter a normal QQ number in configuration.
16. As a self-hosting user, I want the service to store the QQ `user_openid` obtained from the bind event, so that subsequent notifications use the identifier required by the official API.
17. As a self-hosting user, I want binding accepted only when no target has been configured, so that an unexpected chat command cannot redirect notifications.
18. As a self-hosting user, I want the current binding state visible in logs, so that I can confirm whether the service is ready to send.
19. As a self-hosting user, I want WebSocket reconnect and session recovery, so that a temporary QQ connection interruption does not require a restart.
20. As a self-hosting user, I want QQ credentials read from environment variables, so that secrets stay out of TOML and source control.
21. As a self-hosting user, I want an optional GitHub token supported through an environment variable, so that authenticated API quota is available without changing the repository list.
22. As a self-hosting user, I want unauthenticated GitHub quota exhaustion to delay checks and log the reason, so that the service fails visibly instead of silently dropping repositories.
23. As a self-hosting user, I want notification text to include the repository, title, author, creation time and Issue link, so that a message is actionable without opening another dashboard first.
24. As a self-hosting user, I want URL rejection from QQ handled with a text fallback, so that I still receive the Issue identity when links are not accepted.
25. As a self-hosting user, I want to run the service directly on my machine, so that I can develop and test it locally.
26. As a self-hosting user, I want a Docker image and Compose example with a mounted SQLite data directory, so that I can deploy it on a server without installing Rust.
27. As a self-hosting user, I want configuration changes to take effect after restart, so that runtime configuration remains predictable in the first version.
28. As a self-hosting user, I want logs to distinguish discovery, queueing, delivery, retry, binding and permanent errors, so that operational diagnosis is possible.
29. As a self-hosting user, I want normal polling and restart behavior to deduplicate notifications, so that repeated service starts do not produce repeated messages.
30. As a self-hosting user, I want the service to keep one notification target per deployment, so that the first version has clear ownership and no multi-instance coordination problem.

## Implementation Decisions

- **Language and runtime**: Rust service with asynchronous I/O suitable for periodic HTTP polling and a long-lived WebSocket connection.
- **Configuration**: TOML contains the polling interval, GitHub repository identifiers, and data path or equivalent runtime settings. Configuration is loaded at startup; changes take effect after restart.
- **Secrets**: QQ AppID and AppSecret are environment variables. `GITHUB_TOKEN` is optional. `.env.local` is a local development convention and must be ignored by Git; Docker receives secrets through environment injection.
- **GitHub boundary**: A GitHub client fetches repository Issues with pagination and enough metadata to identify Pull Requests. The discovery boundary emits only new Issue records after filtering Pull Requests.
- **Baseline semantics**: A newly configured monitoring repository records a current-time baseline before normal discovery. Existing repositories resume from persisted progress. The baseline must survive a process crash once committed.
- **Persistence**: SQLite is the source of truth for monitoring repositories, discovery cursors or timestamps, discovered Issue identity, notification state, retry metadata, and the single bound QQ `user_openid`.
- **Idempotency**: Issue identity is unique per repository and Issue number or GitHub canonical identifier. Discovery and queue insertion are idempotent. Delivery state is separate from discovery state.
- **Queue state**: A discovered Issue is pending until successfully delivered. Retryable failures retain the item with a next-attempt time and retry count. Permanent failures retain the item and an error state for operator visibility.
- **Notification target**: One fixed private-chat target is supported. `/bind` is accepted only when no target exists; the service extracts `author.user_openid` from a private-chat WebSocket event and persists it. The supplied test QQ number is not used by the API.
- **QQ transport**: WebSocket handles gateway URL acquisition, Identify, heartbeat, ACK, reconnect and Resume. HTTP OpenAPI sends proactive private-chat messages using the stored OpenID. The implementation must respect official per-target and global rate limits.
- **Message format**: Plain text with repository, title, author, original creation time and canonical Issue URL. Message length is bounded. URL rejection has a fallback containing repository and Issue number.
- **Retry policy**: Network failures, gateway interruptions, rate limiting and temporary service errors are retryable with bounded exponential backoff. Invalid credentials, missing target permission and rejected content are recorded as permanent or operator-action-required errors while retaining the notification record.
- **Concurrency seam**: The highest useful test seam is the orchestration boundary that accepts a GitHub discovery source and a notification sink, persists state between them, and exposes externally visible queue and delivery behavior. Real GitHub and QQ clients are adapters behind this boundary.
- **Deployment**: Provide a reproducible Docker build, Compose example, persistent data volume, health or readiness indication through logs or a documented command, and local development instructions.

## Testing Decisions

- Tests must assert externally observable behavior at the orchestration boundary: which Issue notifications are queued, which are delivered, which remain pending, and what state survives a restart. They should not assert internal function calls or library implementation details.
- Use fake GitHub and QQ adapters for deterministic tests. The fake GitHub source must cover pagination, new and historical Issues, Pull Requests represented in the Issue endpoint, and API failures. The fake QQ sink must cover success, retryable failure, permanent failure, rate limiting and uncertain outcomes.
- Test that a newly added monitoring repository establishes a baseline without queuing historical Issues.
- Test that a previously known repository discovers Issues created after the persisted progress and does not enqueue duplicates across repeated polls.
- Test that Pull Requests are excluded even when returned by GitHub's combined Issues endpoint.
- Test pagination across multiple API pages and continuation after a partial fetch failure.
- Test that discovered Issues remain persisted when notification delivery is unavailable.
- Test retry backoff state, rate-limit deferral, eventual success, and retention of permanent failures.
- Test restart recovery using a real temporary SQLite database: queued notifications must remain available and successful notifications must not be requeued.
- Test `/bind` acceptance for the first private-chat event, rejection or no-op behavior after a target is already bound, and persistence of the OpenID.
- Test message rendering, bounded length, creation-time display, URL fallback, and escaping of user-controlled Issue text.
- Test WebSocket reconnect and session recovery logic with a fake gateway or protocol harness; do not require the real QQ service for unit or integration tests.
- Test configuration loading, missing required QQ credentials, optional GitHub token behavior, invalid repository entries, and Docker startup with a mounted data directory where practical.
- A manual acceptance check is required for real QQ binding and proactive private-chat delivery because account permissions, user settings, quota, and URL acceptance cannot be fully simulated.
- No existing codebase test prior art is available; this is a new repository, so the fake-adapter orchestration seam is the initial testing convention.

## Out of Scope

- Keyword, label, author, repository-specific, or AI-based Filter Rule behavior.
- Monitoring comments, edits, labels, assignments, closures, reopening, or other Issue updates.
- Automatic issue triage, Codex or other agent invocation, code changes, testing fixes, branch creation, fork management, or Pull Request publication.
- QQ groups, QQ channels, multiple QQ targets, normal QQ-number lookup, or multi-instance coordination.
- Other IM channels such as Feishu, Telegram, WeCom, email or generic webhooks.
- GitLab, Gitee or other source-control platforms.
- A web dashboard, user accounts, multi-tenant hosting, billing or hosted service.
- Hot configuration reload, distributed locking, high availability or horizontal scaling.
- Guaranteed exactly-once delivery; the chosen failure policy permits occasional duplicates when the send result is uncertain.
- Secret storage or rotation beyond environment-variable injection.

## Further Notes

- The target QQ number `2158331612` is only a human test reference. The official private-message API requires the `user_openid` obtained from a bind event, so the number must not be added to `.env.local` as a delivery identifier.
- Official QQ documentation indicates that WebSocket is used for gateway events and HTTP OpenAPI is used for proactive messages. The implementation must still verify the actual bot's event permissions, private-message permission, account status, quota and URL acceptance during manual acceptance.
- The first implementation should keep the domain vocabulary from `GLOSSARY.md`: 监控仓库, 新建 Issue, 筛选规则, Issue 通知, and 通知目标.
- This spec is ready for an implementation agent and should be split into implementation tickets only after the implementation owner confirms the execution order.
