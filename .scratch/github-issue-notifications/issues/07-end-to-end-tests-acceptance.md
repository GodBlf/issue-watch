# 07: 端到端验收与测试套件

**What to build:** 使用者和实现者可以通过自动化测试验证核心监控行为，并按文档完成一次真实 QQ 绑定和私聊发送验收。

**Blocked by:** 05: Issue 通知队列、重试与去重; 06: 本机运行、Docker 与运维配置

**Status:** claimed

- [x] 使用 fake GitHub 和 fake QQ 适配器覆盖发现、分页、基线、Pull Request 排除、去重、补发和失败重试。
- [x] 使用临时 SQLite 验证进程重启后队列和成功状态的保留。
- [x] 覆盖 `/bind` 首次绑定、重复绑定保护、消息渲染、长度限制和 URL 降级。
- [x] 覆盖配置校验、缺少 QQ 凭据、可选 GitHub Token、无效仓库和 Docker 挂载数据目录。
- [x] WebSocket 心跳、重连和 Resume 使用 fake Gateway 或协议测试，不依赖真实 QQ 账号。
- [ ] 提供真实 QQ 手工验收步骤：测试账号私聊 `/bind`、确认 OpenID 绑定、触发测试 Issue 并确认私聊消息。
- [ ] 记录真实账号权限、主动消息设置、配额和 URL 接受情况作为验收结果。
