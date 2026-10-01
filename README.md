# issue-watch

Rust 服务，按配置轮询 GitHub 公开仓库的新建 Issue，并通过 QQ 官方机器人私聊通知。

## 本机运行

1. 复制 `config.example.toml` 为 `config.toml`，填写 `repositories`。
2. 在 `.env.local` 设置 `QQ_APP_ID`、`QQ_APP_SECRET`，可选设置 `GITHUB_TOKEN` 和 `QQ_ACCESS_TOKEN`。
3. 执行 `cargo run`。

默认每 60 秒检查一次，SQLite 数据保存到 `data/issue-watch.sqlite3`。第一次加入监控的仓库只建立当前时间基线，不推送历史 Issue。

## 配置热加载

运行时直接修改并保存 `config.toml`，无需重启。服务每 2 秒检查文件内容，支持编辑器原子替换保存方式：

```toml
poll_interval_seconds = 60
database_path = "data/issue-watch.sqlite3"
repositories = ["GodBlf/mycode-rust", "owner/another-repo"]
```

`repositories` 和 `poll_interval_seconds` 可热加载（间隔范围 1–86400 秒）。新增仓库在配置被接受时建立当前时间基线；移除仓库停止后续轮询，已有待发送通知继续投递。移除后重新加入的仓库恢复原进度，会补发停用期间的新 Issue。QQ 绑定和已有仓库进度不会重置。

无效 TOML、未知字段、空仓库列表、重复/无效仓库名、无效间隔或文件暂时不可读时，保留最后一次有效配置并记录错误。修正文件后自动恢复加载。数据库路径和环境变量凭据变更需要重启；改变数据库路径的配置会整份拒绝。

配置检查独立于 GitHub 请求运行，监控任务在当前轮询/投递完成后使用新设置；已经开始的请求不会取消。配置更新后立即开始下一轮检查，之后使用新的间隔。

Docker 建议挂载配置所在的目录，并将 `ISSUE_WATCH_CONFIG` 指向该目录中的文件。当前 Compose 的单文件挂载可能无法看到宿主机编辑器以原子替换方式保存的文件；使用单文件挂载时需原地写入文件。

## Docker

复制配置示例并填写仓库，然后通过环境变量提供凭据：

```powershell
Copy-Item config.example.toml config.toml
docker compose up -d --build
```

Compose 将 `./data` 挂载为持久化目录；不要把 `.env.local` 复制进镜像。

## QQ 绑定

启动服务后，用目标 QQ 私聊机器人发送 `/bind`。服务保存事件中的 `user_openid`，普通 QQ 号不能直接作为官方发送 API 的目标。第一版每个部署只支持一个私聊目标。

`QQ_ACCESS_TOKEN` 可用于直接启动发送适配器；生产环境应由官方 AppID/AppSecret 流程获取并注入短期 token。WebSocket Gateway 的事件连接由 QQ 适配器负责，真实账号权限和主动消息配额需要手工验收。

## 测试

```powershell
cargo fmt --check
cargo test
```
