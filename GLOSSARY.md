# Issue 监控

本项目为自行部署的使用者监控 GitHub 仓库、指定 Issue 及 Bilibili 直播间，并通过 QQ 机器人按订阅发送新建 Issue、追踪动态与开播通知。

## Language

**监控仓库（Watched Repository）**：使用者指定的、需要关注新建 issue 的 GitHub 仓库。
_Avoid_: 项目源、监听项目

**监控周期（Monitoring Period）**：一个仓库从开始监控到移除之间的一段连续监控关系；重新加入开启新的监控周期。
_Avoid_: 恢复旧监控、抓取进度

**新建 Issue（New Issue）**：监控仓库中新提交的问题或需求条目。
_Avoid_: Issue 更新、仓库动态

**筛选规则（Filter Rule）**：使用者用于决定哪些新建 issue 值得通知的条件。
_Avoid_: 智能推荐、修复策略

**Issue 通知（Issue Notification）**：向使用者传达符合筛选规则的新建 issue 的消息。
_Avoid_: 修复任务、PR 通知

**通知目标（Notification Destination）**：接收 Issue 通知、Issue 追踪动态或开播通知的 QQ 私聊会话，也是配置仓库订阅与直播订阅的对象；可以暂时没有任何订阅。
_Avoid_: 监控仓库、消息来源

**QQ 号备注（QQ Number Note）**：管理员为通知目标选填的 QQ 号，用于人工识别接收者，不作为通知投递的身份或地址；随通知目标删除而删除。
_Avoid_: 发送目标、绑定凭据

**仓库订阅（Repository Subscription）**：通知目标选择接收某个监控仓库的新建 Issue 通知及 Issue 追踪动态的关系；不同通知目标可以选择不同仓库。
_Avoid_: 广播订阅、独立抓取

**通知投递（Notification Delivery）**：一条通知向一个通知目标的发送及其结果。
_Avoid_: Issue 处理状态、广播结果

**Issue 追踪（Issue Tracking）**：持续关注指定 Issue 新动态的共享关系，动态仅通知订阅该仓库的通知目标；取消追踪或 Issue 关闭时结束。
_Avoid_: 仓库订阅、监控仓库、个人关注

**Issue 动态（Issue Activity）**：被追踪 Issue 的新评论、其他 Issue 或 PR 对它的引用、PR 与它的明确关联，以及它的关闭；也包括明确关联 PR 的合并、关闭和重新打开。
_Avoid_: 新建 Issue、仓库全部动态

**追踪命令权限（Tracking Command Permission）**：允许指定 QQ 用户添加或取消共享 Issue 追踪的授权；添加与取消分别授予，与仓库订阅无关。
_Avoid_: QQ 管理员角色、仓库订阅权限

**直播订阅（Live Subscription）**：通知目标选择接收某个 Bilibili 直播间开播通知的关系；一个通知目标可以订阅多个直播间，同一直播间可以有多个通知目标订阅。
_Avoid_: 房间绑定、主播绑定

**开播通知（Live Start Notification）**：向订阅某个 Bilibili 直播间的通知目标传达该直播间开始直播的消息。
_Avoid_: 直播动态、下播通知

**直播场次（Live Session）**：某个 Bilibili 直播间一次由平台开播时间标识的直播；平台将断流重连记录为新的开播时间时，视为新场次。
_Avoid_: 节目、直播标题
