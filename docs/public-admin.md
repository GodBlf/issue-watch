# 公网管理员入口

访问地址为 `https://172.197.176.142/`，无需域名、额外子路径或 SSH 隧道。应用与数据库仍由原生产 Compose 管理；宿主机 Nginx 终止 TLS，再转发至 `127.0.0.1:8081`。80 端口仅提供 ACME 挑战文件，其他 HTTP 路径返回 404；公网禁止访问内部健康路径和应用端口。

## 升级与初始化

先保留管理员 SSH 通道，备份现有配置并按[部署说明](deployment.md)更新 root 拥有的部署入口和 Compose。新的部署验收使用 `/internal/status`；它不要求浏览器登录，但公网代理明确拒绝整个 `/internal/` 前缀。`/api/status` 与其他管理 API 一样需要会话。升级后尚未设置密码时，管理功能保持关闭，监控和通知继续运行。

在服务器 `/home/godblf/issue-watch/.env.local` 增加一行：

```dotenv
ISSUE_WATCH_ADMIN_ORIGIN=https://172.197.176.142
```

值仅包含协议、IP 和可选端口，不带路径或尾部斜线；部署地址改变时同步调整证书和代理配置。默认值为 `https://127.0.0.1`，不会默默信任任意请求来源。环境变量变更需要重新创建容器；配置热加载不会更新认证 Origin。

可选的 `ISSUE_WATCH_ADMIN_TRUSTED_PROXY` 是应用实际看到的 Nginx **精确 socket 对端 IP**，Docker bridge 下通常是网关地址，不能直接假定为 127.0.0.1。先通过 Docker 网络配置核实。设置后仅信任该对端覆盖后的 `X-Real-IP`，不解析任意客户端转发链。不设置时所有请求按实际对端限流，代理后的多个设备可能共享限额，仍有全局保护。

按照部署说明使用新镜像摘要重建容器，然后在服务器交互式设置初始密码（至少 16 个 UTF-8 字节，最多 1024）：

```bash
sudo env ISSUE_WATCH_ROOT=/home/godblf/issue-watch ISSUE_WATCH_IMAGE='<新镜像摘要引用>' \
  docker compose --project-name issue-watch --project-directory /home/godblf/issue-watch \
  --env-file /home/godblf/issue-watch/.env.local \
  -f /home/godblf/issue-watch/deploy/compose.yml exec issue-watch issue-watch --admin-password
```

命令从终端隐藏读取并确认密码，不将秘密放入参数或 shell 历史。无需 QQ 或 GitHub 凭据即可执行该管理命令。自动化或无 TTY 场景可用 `--admin-password-stdin`，通过受保护的标准输入传入；不要把密码写在命令行、脚本或版本库中。此命令同时适用于初始化和重置，每次成功都会原子撤销全部会话。

## 安装 HTTPS 代理

部署工具面向专用的 Ubuntu/Debian Nginx 实例。先检查 `sudo ss -ltnp` 和现有 Nginx 配置；有其他站点或占用 80/443 的进程时，安装器会停止，由管理员决定如何集成，不覆盖未知服务。常见的发行版默认站点也必须先检查，确认不用后才移除其启用链接。安装系统依赖和独立 Certbot 环境：

```bash
sudo apt-get update
sudo apt-get install -y nginx python3-venv openssl iproute2 curl
sudo python3 -m venv /opt/issue-watch-certbot
sudo /opt/issue-watch-certbot/bin/pip install 'certbot>=5.4,<6'
sudo /opt/issue-watch-certbot/bin/certbot --version
```

IP 证书要求 Certbot 5.4 以上的 webroot 支持，不能直接依赖发行版可能过旧的 Certbot。云安全组及宿主机防火墙允许入站 TCP 80、443，保留 SSH；**不要开放 8081**。HTTP-01 必须从公网到达 80，改成本地其他端口不能替代。安装器不修改云网络规则。

可先只读查看生成配置，然后安装（将邮箱替换为自己的联系邮箱）：

```bash
python3 /usr/local/lib/issue-watch/public-admin.py render --ip 172.197.176.142
sudo python3 /usr/local/lib/issue-watch/public-admin.py install \
  --ip 172.197.176.142 --email admin@example.com
```

安装器检查版本、未知站点、端口、应用 Origin、已初始化的密码以及匿名认证请求确实返回 401。首次安装先启用仅挑战文件的 HTTP 站点，通过 ACME 测试环境验证，再签发生产证书；验证证书 IP、有效期和 Nginx 配置后启用 HTTPS。失败时恢复原代理配置，不开放临时匿名管理。安装器使用 root 拥有的专用配置、证书重载脚本和 systemd 续期服务；`--prefix` 只用于隔离文件系统测试，正常部署使用默认根目录。

## 登录与撤销

浏览器打开 HTTPS 地址，在登录页输入管理员密码。所有管理 API 都验证会话，写请求还需匹配显式配置的 Origin，并携带现有 `X-Issue-Watch-Admin: 1`；该固定请求头本身没有授权能力。认证操作为 `POST /api/auth/login`、`POST /api/auth/logout` 和 `GET /api/auth/session`。登录正文只包含 `password`。

服务端保存 Argon2id 密码哈希和随机会话凭据的 SHA-256 摘要。浏览器使用 `__Host-issue_watch_session` Cookie，带 Secure、HttpOnly、SameSite=Strict、Path=/，不把密码或会话写入 localStorage。每次成功登录从当时起固定有效 365 天，访问不续期，服务重启后保留；浏览器自己清理 Cookie、隐私模式清理或主动撤销仍会提前结束登录。

多个设备可分别登录。页面“退出登录”撤销当前会话，复制出的旧 Cookie 也失效；其他设备保持登录。通过 SSH 重新执行密码设置命令会撤销所有设备，运行中的服务无需重启。会话失效时页面保留草稿并提示重新登录，未确认结果的写操作须重新登录后刷新核实，不会自动重放。

登录限额为每来源每 60 秒最多 5 次、全局每 60 秒最多 20 次（成功尝试也计入），同时最多 2 个密码校验任务；超限返回 429 与 Retry-After，不永久锁死账户。错误密码或未初始化返回 401，认证服务不可读取返回 503，不返回内部错误细节。调整限额需要修改代码并验证其行为。

## 续期与验收

Let's Encrypt IP 证书有效 160 小时，必须持续保留 80 的挑战路径。安装器启用 `issue-watch-certbot.timer`，每小时加随机延迟检查一次，开机补执行错过的检查；Certbot 在到达续期窗口时续期。成功后 hook 将证书和私钥复制到独立的只读发布目录，验证 IP、有效期、密钥配对及 Nginx 配置，再 reload。Nginx 使用 `/var/lib/issue-watch-admin-tls/current`，不直接跟随 Certbot 的 live 链接；安装或续期失败时恢复原证书引用及配置，后续重启也不会加载失败候选。保留上一份证书副本供排查；私钥仅 root 可读。安装器显式指定 Let’s Encrypt 测试和生产 ACME 地址，避免系统默认 CA 配置影响签发。状态和错误可通过 systemd/journal 检查，本期不增加外部告警渠道。

```bash
sudo systemctl status issue-watch-certbot.timer issue-watch-certbot.service
sudo systemctl list-timers issue-watch-certbot.timer
sudo journalctl -u issue-watch-certbot.service
sudo /opt/issue-watch-certbot/bin/certbot renew --cert-name issue-watch-ip --dry-run --run-deploy-hooks
curl -fsS http://127.0.0.1:8081/internal/status
```

从外网执行以下检查，HTTPS 请求**不使用 `-k`**：

- `/login` 证书受信任且页面正常；未登录访问 `/api/admin/config` 返回 401。
- HTTP 根路径、登录及 API 返回 404，只有真实 ACME 挑战文件能读取。
- HTTPS `/internal/status` 返回 404；更换 Host、转发头或编码路径不能绕过。
- 公网不能连接 8081；宿主机内部健康检查正常。
- 浏览器登录后管理正常，关闭后重新打开仍登录；脚本不可读取会话 Cookie。
- 两台设备登录，一台退出不影响另一台；SSH 重置密码后两台均失效。

证书续期 dry-run、外部连通性和浏览器可信链必须在目标部署验证后才能称为已上线。本地测试不会申请生产证书。没有 Docker、Nginx 或公网条件的环境不能替代这些验证。

开发验收可使用独立 Nginx、Chrome 和 Playwright，运行下列隔离烟测。它只绑定回环地址，启动真实管理 HTTP fixture，使用临时证书和测试专用信任验证 TLS IP、Cookie、双设备退出及密码重置；不启动 GitHub/QQ 监控任务，不接触生产 CA。测试先确认回环 8081 空闲，并在结束后停止所有临时进程：

```bash
python3 tests/public_admin_https.py --nginx /absolute/path/to/nginx \
  --chrome /absolute/path/to/chrome --playwright /absolute/path/to/playwright
```

## 故障恢复

续期失败检查 timer、journal、证书目录权限、80 挑战路径和云网络规则；修复后先 dry-run，再执行续期。证书过期、认证数据库不可读或代理配置错误时，保留 SSH 修复，不降级到 HTTP 或匿名管理。重载失败先检查 `nginx -t` 和 systemd 状态。

怀疑凭据泄露时通过 SSH 重置密码，撤销全部会话。恢复旧数据库或旧镜像前先关闭公网代理的管理转发；恢复数据库可能复活旧会话，旧镜像也可能没有鉴权。按既有人工恢复策略保留失败数据和证据，确认运行的是支持鉴权的版本、重置密码并撤销所有会话后，才重新开放公网。不要自动回滚数据库。
