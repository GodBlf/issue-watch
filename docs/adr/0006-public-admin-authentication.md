---
status: accepted
---

# 公网管理入口使用 HTTPS 与单管理员登录

管理界面目前通过 SSH 隧道访问。为支持浏览器直接访问，已确认对外地址采用 `https://IP:端口/`，不使用域名或额外路径前缀；公网入口使用 HTTPS，不采用明文 HTTP。

使用者已选择单管理员、本地密码登录，不开放注册或接入第三方登录。服务端保存密码哈希并限制登录尝试；浏览器通过带有 Secure、HttpOnly、SameSite 属性的 Cookie 保存会话凭据，不保存密码。会话从登录成功起固定有效一年，访问不会续期，服务更新或重启后仍有效；允许多个设备分别登录，退出只撤销当前会话。

SSH 作为恢复通道：忘记密码或怀疑凭据泄露时，通过 SSH 重置管理员密码，同时撤销全部浏览器会话，不增加邮箱找回系统。

公网使用 TCP 443 提供管理入口，默认地址为 `https://172.197.176.142/`；TCP 80 仅提供 ACME HTTP-01 证书验证路径。HTTPS 代理转发到现有回环服务，8081 不直接对公网开放。采用 Certbot 的 webroot 验证方式申请 Let's Encrypt IP 证书，自动续期后重载代理；证书失效时不降级为明文 HTTP，SSH 恢复通道仍保留。

这些选择将访问控制从 SSH 访问权限迁移到 Web 身份认证，换取直接使用浏览器管理的便利；一年有效期减少重复登录，也延长了会话凭据泄露后的可利用时间。登录必须覆盖全部管理页面和管理 API，现有同源校验不能作为身份认证。

IP 证书有效期为 160 小时，自动续期是公网入口的必要运维依赖。证书验证必须在公网 TCP 80 可达的条件下完成。内部无登录健康检查使用 `/internal/status`，公网代理拒绝整个 `/internal/` 前缀；原 `/api/status` 需要有效会话，部署验收脚本同步迁移。

依据：

- [Let's Encrypt IP 证书正式可用及有效期](https://letsencrypt.org/2026/01/15/6day-and-ip-general-availability)
- [Certbot 5.4 及以上版本使用 webroot 申请 IP 证书](https://letsencrypt.org/2026/03/11/shorter-certs-certbot)
- [ACME 验证方式及端口要求](https://letsencrypt.org/docs/challenge-types/)

使用者已确认规格、工单拆分并授权实施。实现以本地公网鉴权规格及四张工单为验收依据；线上部署及可信证书验收仍需独立验证。
