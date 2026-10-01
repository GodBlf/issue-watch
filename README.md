# issue-watch

Rust 服务，按配置轮询 GitHub 公开仓库的新建 Issue，并通过 QQ 官方机器人私聊通知。

## 本机运行

1. 复制 `config.example.toml` 为 `config.toml`，填写 `repositories`。
2. 在 `.env.local` 设置 `QQ_APP_ID`、`QQ_APP_SECRET`，可选设置 `GITHUB_TOKEN` 和 `QQ_ACCESS_TOKEN`。
3. 执行 `cargo run`。

默认每 60 秒检查一次，SQLite 数据保存到 `data/issue-watch.sqlite3`。第一次加入监控的仓库只建立当前时间基线，不推送历史 Issue。

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
