# 01: 服务骨架、TOML 配置与 SQLite 状态

**What to build:** 使用者可以启动 Rust 服务，读取 TOML 中的监控仓库和轮询间隔，并在 SQLite 中保存后续监控所需的基础状态。

**Blocked by:** None (can start immediately)

**Status:** resolved

- [x] 服务默认轮询间隔为 60 秒，并支持 TOML 配置覆盖。
- [x] 配置支持多个 GitHub 公开仓库，并拒绝无效或重复仓库项。
- [x] QQ AppID、AppSecret 和可选 GitHub Token 从环境变量读取，不写入 TOML。
- [x] SQLite 初始化成功后保存监控仓库、基线/进度、Issue 通知状态和 QQ 绑定状态所需的数据。
- [x] 服务在缺少必需 QQ 凭据或配置无效时给出明确错误并退出。
- [x] 使用临时 SQLite 数据库的测试能验证初始化和重启后的状态保留。
