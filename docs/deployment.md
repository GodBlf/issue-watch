# 自动构建与部署

PR 自动执行格式检查、Rust 测试、部署入口测试和镜像构建验证。更新 `master` 后，检查成功才发布 `ghcr.io/godblf/issue-watch:sha-<提交>`，随后按镜像摘要通过 SSH 部署到服务器。生产工作流和服务器入口都限制并发；服务器跳过已经被新主分支提交取代的发布。

## 首次准备

管理员通过已核实主机密钥的 SSH 连接，将仓库中的 `deploy` 目录复制到服务器临时目录并执行其中的 `install.sh`。此步骤安装 Ubuntu 的 Docker/Compose 软件包、启用 Docker、安装 root 拥有的固定部署脚本和生产 Compose；不停止原服务。

运行环境固定在 `/home/godblf/issue-watch`：配置为 `config/config.toml`，数据库为 `data/issue-watch.sqlite3`，凭据为 `.env.local`。配置的数据库路径应为 `data/issue-watch.sqlite3` 或容器内的 `/app/data/issue-watch.sqlite3`。请保留原凭据文件，Compose 会明确读取它作为容器环境；配置目录和数据目录通过挂载提供。

在已有 `gh` 登录和可信 SSH 连接的管理员机器执行：

```powershell
python deploy/configure-actions.py
```

这会建立专用 Ed25519 密钥，在服务器替换同名部署公钥，验证任意命令和 TCP 转发被拒绝，并通过标准输入写入仓库 secrets `DEPLOY_SSH_KEY` 和 `DEPLOY_KNOWN_HOSTS`。不会打印私钥。密钥只能调用固定部署入口，不能上传脚本、修改配置或打开交互式 shell。部署脚本变更须由管理员重新安装。

首次向 GHCR 发布后，在 GitHub 包设置确认该包可见性为 Public。GitHub 仓库公开不代表容器包公开。服务器入口使用空的 Docker 凭据配置执行匿名拉取；未公开或拉取失败时仍保持原服务运行。设置包可见性可能需要具有包管理权限的账号；工作流令牌可发布，但不保证能修改公开设置。

## 部署入口

受限 SSH 仅接受一个完整请求：

```text
deploy ghcr.io/godblf/issue-watch@sha256:<64位小写十六进制摘要> <40位提交SHA>
```

脚本校验镜像摘要、当前主分支提交及镜像 revision 标签，先拉取镜像，再停当前实例并使用 SQLite backup API 制作一致性备份，验证备份完整性后启动新容器。首次迁移保留旧 systemd 定义和二进制，在新程序启动前禁用旧服务自动启动，避免失败后重启服务器启动不兼容的旧二进制。

成功时保存 `deploy/current.json` 和对应备份位置。容器有自动重启策略；Docker 已配置开机启动。健康入口保持 `http://127.0.0.1:8081/api/status`，不占用 8080，不向公网开放。

验收要求目标容器运行、镜像引用匹配且状态接口成功响应，默认最多等待 120 秒。业务状态为未知或告警不会阻止部署，验收不会主动发送 QQ 消息。部署成功仅表示容器启动验收通过；真实权限、消息配额和通知投递仍通过既有健康网页观察。

管理员查看状态：

```bash
sudo cat /home/godblf/issue-watch/deploy/current.json
sudo docker ps --filter label=com.docker.compose.project=issue-watch
curl -fsS http://127.0.0.1:8081/api/status
systemctl is-enabled docker
systemctl is-enabled issue-watch.service
```

## 失败与人工恢复

拉取或前置验证失败不会停止旧实例。停止后的备份或准备失败，只在确定新程序未启动时恢复旧实例。新程序一旦尝试启动，就保守视为可能迁移了数据库：失败后禁用该容器自动重启并停止，保存 `deploy/recovery-required.json`、数据库备份和受限权限的容器日志，Actions 返回失败。该标记阻止后续自动部署，避免反复启动失败版本。

备份位于 `backups/<UTC时间>-<唯一编号>/`，包含更新前 SQLite 数据库、目标版本及旧版本信息。失败日志可能包含运行细节，仅供服务器管理员查看，不自动上传 Actions。备份暂不自动清理，管理员应关注磁盘空间。

人工恢复须先确认没有原服务或容器继续写入数据库，再保存失败后的数据库及可能存在的 WAL/SHM 文件。根据迁移兼容性决定继续使用当前数据库，还是恢复更新前的备份；恢复旧备份可能丢失更新后的通知投递记录并造成重复通知。

需要恢复数据库时，将备份复制到临时文件，验证完整性后替换数据库；旧 WAL/SHM 应先移到失败数据保存目录，不能与恢复的主文件混用。恢复文件时保留原属主和权限。

恢复容器版本时，从备份中的 `previous.json` 或 `deployment.json` 取得旧镜像引用，管理员设置 `ISSUE_WATCH_IMAGE`、`ISSUE_WATCH_ROOT` 后运行 Compose；恢复原生版本时，应确认其数据库兼容，再启用并启动旧 systemd 服务。两者只能启动一个。

```bash
# 容器恢复示例；先按上述步骤保存及恢复数据，再填写正确的旧镜像摘要。
sudo env ISSUE_WATCH_ROOT=/home/godblf/issue-watch ISSUE_WATCH_IMAGE='<旧镜像摘要引用>' \
  docker compose --project-name issue-watch --project-directory /home/godblf/issue-watch \
  --env-file /home/godblf/issue-watch/.env.local \
  -f /home/godblf/issue-watch/deploy/compose.yml up -d --no-build
```

验收恢复后的服务，更新 `deploy/current.json` 为实际运行版本，再由管理员移走恢复标记以允许下次自动部署。不要仅删除标记就自动重试，也不要在未处理数据库兼容性时直接启动旧版本。

## 本地验证

```bash
cargo fmt --check
cargo test --locked
python3 -m unittest discover -s tests -p test_deploy.py -v
node --test tests/health_dashboard_browser.cjs
bash -n deploy/install.sh
sh -n deploy/ssh-entry
docker build --platform linux/amd64 -t issue-watch:check .
python3 tests/container_smoke.py issue-watch:check
```

部署入口测试在 Linux 运行，用临时 SQLite 和外部命令替身模拟服务管理、Docker、主分支查询和健康 HTTP 请求。镜像测试实际启动生产 Compose，用隔离网络和测试数据验证配置热加载及广播订阅、监控进度和通知投递记录在重启后保留；通过后才发布镜像。测试不会连接 QQ 用户或生产数据库。

Linux 上镜像默认以 root 创建数据库，镜像测试需使用 `sudo python3 tests/container_smoke.py issue-watch:check`，以便写入测试状态并清理临时文件；Actions 已使用同样方式运行。
