# Issue 监控：设计讨论

## 已确认的第一版范围

- 自行部署使用，监控平台为 GitHub。
- 配置文件列出监控仓库。
- 所有新建 issue 都发送通知；规则匹配留到后续。
- 使用 QQ 官方机器人，以 WebSocket 接收机器人事件，不使用 webhook。
- QQ 凭据保存在本地 `.env.local` 中；不将凭据写入文档。
- 默认检查间隔为 60 秒，允许配置。
- 首次添加仓库从当前时间开始，不推送历史 issue。
- 重启后补发停机期间的新 issue，持久保存处理进度。
- 使用 Rust，支持本机运行和 Docker 部署。
- 自动调用 agent、修复 issue 和发布 PR 留到后续。
- 第一版只使用一个固定 QQ 私聊通知目标，后续再扩展群聊。
- 测试用户通过私聊发送 `/bind`；程序从事件取得 `author.user_openid` 并保存到 SQLite。普通 QQ 号不作为发送 API 的目标参数，也不要求写入环境变量。
- 停机积累的消息逐条排队补发，控制发送速度，消息展示原始创建时间。
- 优先避免漏发；发送失败重试，故障导致发送结果不确定时允许偶尔重复。
- 每个通知目标只运行一个实例；本机开发，服务器长期运行，不做多实例协调。
- 配置使用 TOML，进度和待发送消息使用 SQLite 存储；Docker 挂载数据目录。
- 通知包含仓库、标题、作者、创建时间和链接。

## 尚待确认

- 实际机器人的事件订阅与主动发送权限、运行状态，以及 issue 链接能否发送，需要联调核实。
- GitHub 默认监控公开仓库，身份认证和配额策略的最终设计待确认。

本文记录设计讨论，尚未完成最终确认。

## 最终确认方案

以下补充约定待使用者整体确认后实施：

- GitHub 监控对象为公开仓库；可通过环境变量提供 `GITHUB_TOKEN`，有令牌时使用认证请求，无令牌时遵守匿名配额并在配额不足时延迟检查。
- 仅通知新建 issue，排除 GitHub issue 列表中的 PR，不通知评论、编辑和关闭事件。
- 配置修改通过重启生效；新增仓库建立当前时间基线，已有仓库继续使用持久化进度。
- 首次基线立即持久化，包括尚未绑定 QQ 的情况；绑定前发现的新 issue 排队等待发送。
- GitHub 分页检查与消息排队使用持久化去重记录；发现进度与发送状态分别保存，QQ 发送故障不导致发现的新 issue 被丢弃。
- QQ WebSocket 实现鉴权、心跳、重连与会话恢复；HTTP 消息接口负责主动发送。
- `/bind` 仅在未绑定时接受首次私聊绑定；日志显示绑定状态，使用者在正式运行前确认目标。第一版不允许聊天命令覆盖现有绑定。
- 临时失败使用退避重试；权限、目标和内容错误保留待发送消息并在日志中说明；限频或当天配额不足则延后发送。
- 消息采用纯文本，包含仓库、标题、作者、创建时间、issue 链接；对长度做限制。URL 被拒绝时保留 issue 的仓库及编号作为降级内容并记录告警。
- 本机读取 `.env.local`；Docker 注入环境变量，不将本地凭据复制进镜像。环境变量文件和 SQLite 数据不提交 Git。
- 提供配置示例、README、Dockerfile、Compose 文件，以及针对发现、分页、持久化去重、补发和失败重试的测试。
- 真实 QQ 发送与绑定需要使用者参与一次私聊；模拟测试不作为真实账号权限验证的替代。

## QQ 官方接口核实

官方文档目前支持 WebSocket 事件连接和无用户消息触发的主动通知。消息发送使用 HTTP OpenAPI。

- [WebSocket 方式](https://bot.q.qq.com/wiki/develop/api-v2/dev-prepare/event-emit/websocket.html)
- [消息收发概述](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/overview.html)
- [群消息接口](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_groups_group_openid_messages.post.html)
- [群 @ 机器人事件](https://bot.q.qq.com/wiki/develop/api-v2/autogen/event/group_at_message_create.html)
- [单聊事件](https://bot.q.qq.com/wiki/develop/api-v2/autogen/event/c2c_message_create.html)

QQ 群和单聊目标均使用 OpenID，而非普通 QQ 群号或 QQ 号。单目标主动消息配额目前为每分钟 20 条、每天 1000 条；还需遵守机器人整体配额。接收方可关闭主动消息权限。接口也可能拒绝 URL，实际权限和 issue 链接发送能力尚未联调确认。
