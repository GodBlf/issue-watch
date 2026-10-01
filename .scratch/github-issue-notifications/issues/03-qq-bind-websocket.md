# 03: QQ WebSocket 连接、鉴权与 `/bind`

**What to build:** 服务能连接 QQ 官方 Gateway，并通过一次私聊 `/bind` 将发送目标绑定为官方事件提供的 `user_openid`。

**Blocked by:** 01: 服务骨架、TOML 配置与 SQLite 状态

**Status:** claimed

- [x] 服务通过 AppID/AppSecret 获取访问凭证并建立 QQ WebSocket 连接。
- [x] WebSocket 实现 Identify、心跳、ACK、断线重连和会话 Resume 所需行为。
- [x] 私聊事件中的 `/bind` 在尚未绑定时保存发送者 `user_openid`。
- [x] 已存在绑定时不会被后续私聊命令覆盖，并在日志中说明绑定状态。
- [x] 普通 QQ 号不会被当作发送 API 的目标标识。
- [ ] fake Gateway 测试覆盖绑定、重复绑定、心跳、重连和恢复。
