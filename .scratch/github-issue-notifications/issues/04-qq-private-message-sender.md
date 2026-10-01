# 04: QQ 私聊 HTTP 发送适配器

**What to build:** 服务可以使用已绑定的 `user_openid` 通过 QQ 官方 HTTP API 发送一条可读的 Issue 通知，并对接口错误进行分类。

**Blocked by:** 03: QQ WebSocket 连接、鉴权与 `/bind`

**Status:** claimed

- [x] 使用持久化的 `user_openid` 调用 QQ 官方私聊发送接口。
- [x] 消息包含监控仓库、Issue 标题、作者、原始创建时间和 Issue 链接。
- [x] 消息长度受限，用户可控文本不会破坏消息格式。
- [x] URL 被拒绝时生成包含仓库和 Issue 编号的纯文本降级消息并记录告警。
- [x] 网络错误、限频/配额错误、目标权限错误和内容错误可被上层区分。
- [ ] fake QQ HTTP 测试覆盖成功、限频、权限错误、URL 拒绝和网络失败。
