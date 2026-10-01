# Issue 监控

本项目为自行部署的使用者监控 GitHub 仓库，并通过 QQ 机器人发送新建 issue 通知。

## Language

**监控仓库（Watched Repository）**：使用者指定的、需要关注新建 issue 的 GitHub 仓库。
_Avoid_: 项目源、监听项目

**新建 Issue（New Issue）**：监控仓库中新提交的问题或需求条目。
_Avoid_: Issue 更新、仓库动态

**筛选规则（Filter Rule）**：使用者用于决定哪些新建 issue 值得通知的条件。
_Avoid_: 智能推荐、修复策略

**Issue 通知（Issue Notification）**：向使用者传达符合筛选规则的新建 issue 的消息。
_Avoid_: 修复任务、PR 通知

**通知目标（Notification Destination）**：接收 issue 通知的 QQ 会话。
_Avoid_: 监控仓库、消息来源
