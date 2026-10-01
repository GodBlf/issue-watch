# 02: GitHub 新建 Issue 发现与基线

**What to build:** 使用者配置的监控仓库会被周期性检查；服务只发现新建 Issue，排除 Pull Request，并正确处理首次添加和已有仓库的进度。

**Blocked by:** 01: 服务骨架、TOML 配置与 SQLite 状态

**Status:** resolved

- [x] 服务按配置间隔检查每个监控仓库，并支持 GitHub 分页。
- [x] GitHub Issue API 返回的 Pull Request 不会进入新建 Issue 结果。
- [x] 新加入的监控仓库建立当前时间基线，不推送基线之前的历史 Issue。
- [x] 已存在的监控仓库从 SQLite 持久化进度继续发现停机期间创建的新 Issue。
- [x] Issue 使用仓库和 Issue 编号或等价规范标识去重，重复轮询不会产生重复发现记录。
- [x] 无 GitHub Token 时可以运行并记录配额/限流；有 Token 时使用认证请求。
- [x] fake GitHub 测试覆盖分页、历史数据、Pull Request 排除、停机恢复和 API 错误。
