# 公网管理入口部署验收（2026-10-20）

目标：`https://172.197.176.142/`。管理员只输入密码，无用户名字段；密码由管理员通过标准输入设置，未记录在本文、命令参数或版本库。

已部署应用提交：`8aee37f3eeba1aa636af6154b1809fb8d75e2f75`。

不可变镜像：`ghcr.io/godblf/issue-watch@sha256:a1f097de885fa40b39b7386bda19d8874503079285b964773a612adfc95c48b8`。

最终 [CI、镜像与部署运行](https://github.com/GodBlf/issue-watch/actions/runs/39373740348) 全部成功；服务器运行版本与摘要已核对，内部健康检查 200。该次数据库备份位于服务器 `/home/godblf/issue-watch/backups/20261020T051812Z-8b8caa8d/`。

## 部署决定

服务器为 Ubuntu 24.04，现有 Caddy 2.6.2 已在 80/443 为 `xxh.malaysiawest.cloudapp.azure.com` 转发至本机 8080。用户确认保留该服务，使用 Caddy 新增 IP 管理入口，修订 ADR 0006；不启动竞争端口的 Nginx。

管理应用继续由生产 Compose 运行，宿主机仅发布 `127.0.0.1:8081`。外部 Origin 为 HTTPS IP，可信代理精确地址为应用 Docker 网络网关 `172.18.0.1`。新版部署脚本、Compose 与独立证书安装器已安装，原部署配置和环境文件保存在服务器 root 限制访问的 `deploy/pre-public-admin-f5fbd8e/` 中。自动部署另行保存每次数据库一致性备份。

Caddy 主配置保留原站点、权限和属主，增加独立 IP 配置 import；`default_sni` 为不发送 SNI 的 IP 浏览器选择证书，原域名显式 SNI 仍使用原证书。新私钥副本为 root:caddy 0640，发布目录 root:caddy 0750；原 Certbot 私钥权限不变。

## 验收结果

- 使用 Certbot 5.8.0，先通过 Let's Encrypt staging，再签发 production shortlived IP 证书。颁发者为 Let's Encrypt YE2，SAN 包含目标 IP，证书有效期 160 小时。验收使用系统可信链，未关闭证书校验。
- 外网 HTTPS：登录页 200，匿名管理配置及状态 API 401；内部健康路径及编码变体 404。HTTP 根路径、登录页和管理 API 均 404；真实 ACME HTTP-01 挑战通过。
- 真实 Chrome：登录、认证 API、Secure/HttpOnly/SameSite=Strict Cookie、365 天固定到期、脚本无法读取 Cookie、关闭浏览器再打开保持登录通过。多设备会话、当前设备退出隔离、跨站退出 403 通过；测试会话退出后撤销。
- 在最终 Compose 镜像更新前登录，更新完成后使用同一浏览器会话读取认证状态、刷新管理页面及退出均通过，确认真实应用更新不丢失登录。
- 宿主机 `ss` 及 Docker NAT 配置确认 8081 仅绑定回环。外网对 8081 的 HTTP 探测 10 秒内无响应；客户端网络可提前完成 TCP 建连，因此不把单独的 connect 成功视作应用可达，也不声称验证过云平台控制面的规则。应用 HTTP 数据未从此端口获取。
- 宿主机内部健康检查 200。既有服务本地访问 200，外网显式使用其原域名 SNI 验证可信 TLS 与 HTTP 200；未改动其 upstream。
- 完整 `certbot renew --dry-run --no-random-sleep-on-renew --run-deploy-hooks --deploy-hook /usr/local/lib/issue-watch/reload-admin-certificate` 成功。该调用显式指定 hook，避免只验证挑战而漏测重载。systemd 续期服务实际启动成功，首次证书尚未到期时正常跳过续期；小时级 timer 已启用且 active。
- 实机 Caddy ExecReload 已确认包含 `--force`，确保配置路径不变时仍重新读取证书。部署脚本 SHA256 与仓库文件一致。

## 回归与评审

GitHub Actions 执行 Rust 1.90 全套测试、页面测试、部署测试、真实生产镜像构建及 Compose smoke 后，才发布镜像并使用不可变摘要部署。

本地额外使用官方 SHA512 校验过的 Caddy 2.6.2 与 Chrome 完成隔离 HTTPS 测试，覆盖 IP 无 SNI、可信测试证书、Cookie、HTTP/内部路径隔离、多设备、退出与运行时密码重置。实机临时目录的 11 项部署 CLI 回归通过，包括原 Caddyfile 权限/属主保留及失败恢复；未操作生产配置或数据库作为测试夹具。

Standards：发现并修复 Caddyfile 原权限/属主未保留的问题，复评剩余 0 项。Spec：部署适配及无 SNI 修复复评剩余 0 项。原证书回滚修复继续保留。首次公网无 SNI 握手失败已修复，并已由真实外网 TLS 与 Chrome 再验收。

线上不进行暴力尝试或故意破坏证书。限流、故障恢复及并发密码重置由隔离 HTTP/部署回归覆盖；真实自然到期后的无人值守续期尚未等待一个完整证书周期，已使用 staging dry-run 和实际重载验证链路。
