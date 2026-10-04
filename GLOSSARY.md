# Issue 监控

本项目为自行部署的使用者监控 GitHub 仓库及指定 Issue，并通过 QQ 机器人广播新建 Issue 与追踪动态。

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

**QQ 号备注（QQ Number Note）**：管理员为当前广播订阅选填的 QQ 号，用于人工识别接收者，不作为通知投递的身份或地址；随订阅退出而删除。
_Avoid_: 发送目标、绑定凭据

**广播订阅（Broadcast Subscription）**：通知目标加入共享 Issue 通知的关系；所有订阅者关注相同的监控仓库。
_Avoid_: 用户配置、独立监控

**通知投递（Notification Delivery）**：一条 Issue 通知向一个通知目标的发送及其结果。
_Avoid_: Issue 处理状态、广播结果

**Issue 追踪（Issue Tracking）**：持续关注指定 Issue 的新动态，并向所有广播订阅者通知的共享关系；取消追踪或 Issue 关闭时结束。
_Avoid_: 广播订阅、监控仓库、个人关注

**Issue 动态（Issue Activity）**：被追踪 Issue 的新评论、其他 Issue 或 PR 对它的引用、PR 与它的明确关联，以及它的关闭；也包括明确关联 PR 的合并、关闭和重新打开。
_Avoid_: 新建 Issue、仓库全部动态

**追踪命令权限（Tracking Command Permission）**：允许指定 QQ 用户添加或取消共享 Issue 追踪的授权；添加与取消分别授予，与广播订阅无关。
_Avoid_: QQ 管理员角色、广播订阅权限
