# 06: 本机运行、Docker 与运维配置

**What to build:** 使用者可以在本机或服务器 Docker 环境中启动服务，并持久化 SQLite 数据、注入凭据和查看足够的运行状态。

**Blocked by:** 05: Issue 通知队列、重试与去重

**Status:** resolved

- [x] 提供本机运行说明，包含 TOML、环境变量和 SQLite 数据目录配置。
- [x] 提供可重复构建的 Dockerfile。
- [x] 提供 Docker Compose 示例，将 SQLite 数据目录挂载为持久卷。
- [x] Docker 不复制 `.env.local` 或其他本地凭据进镜像，通过环境变量注入 QQ 凭据和可选 GitHub Token。
- [x] 日志能区分配置、GitHub 发现、队列、QQ 绑定、发送、重试和永久错误。
- [x] 提供可操作的启动/就绪状态说明，并记录未绑定 QQ 目标时的明确状态。
- [x] 文档说明如何修改配置并重启，以及如何备份 SQLite 数据。
