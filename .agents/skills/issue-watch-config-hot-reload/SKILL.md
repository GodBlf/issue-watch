---
name: issue-watch-config-hot-reload
description: Configure a specified GitHub repository for issue-watch hot reload on a specified SSH server. Use when a user asks to add, remove, or update monitored repositories in a running issue-watch deployment.
---

# Issue-watch 配置热加载

为指定服务器上的 issue-watch 修改监控仓库配置，并通过运行时热加载生效。

## 必要输入

先从用户消息中提取：

- **服务器地址**：SSH 目标，至少包含主机；优先使用用户提供的完整 `user@host[:port]`。
- **仓库地址**：GitHub 仓库 URL 或 `owner/repository`。

如果缺少服务器地址，先询问用户具体的 SSH 地址（例如 `godblf@172.197.176.142`，以及非默认端口）。如果缺少仓库地址，先询问 GitHub 仓库 URL 或 `owner/repository`。两个输入齐全前不要执行远程写操作；不要猜测或复用其他任务的地址。

## 执行步骤

1. 阅读当前项目的 `AGENTS.md`，并在需要探索配置路径或部署约定时阅读 `docs/agents/domain.md`、`GLOSSARY.md` 和 `docs/deployment.md`。使用项目定义的“监控仓库”术语。
2. 先通过 SSH 只读检查连接、当前配置和运行状态。生产部署的默认配置路径是 `/home/<ssh-user>/issue-watch/config/config.toml`；如果项目文档、`ISSUE_WATCH_CONFIG` 或运行进程显示其他路径，以实际路径为准。确认配置文件可由 SSH 用户写入，并确认当前服务正在运行。
3. 将用户仓库规范化为 `owner/repository`，保留 GitHub 大小写；拒绝缺少 owner/name、包含路径穿越、重复或明显不是 GitHub 仓库的输入。保留现有监控仓库、轮询间隔和数据库路径，只对用户要求的仓库做变更。
4. 写入前在本地内存中完整生成候选 TOML，并确保原配置仍能解析。远程写入使用配置目录内的临时文件，然后设置与原文件相同的私有权限并用 `mv` 原子替换；不要截断正在使用的配置文件，也不要把密钥写入配置或命令输出。
5. 依赖 issue-watch 的热加载机制生效，通常等待至少 3 秒；不要为仓库配置变更重启容器或服务。默认生产健康接口是服务器回环地址 `http://127.0.0.1:8081/api/status`，本地 Compose 常用 `8080`，以部署文档或运行状态为准。
6. 验证健康接口中的 `github.details.repositories` 已包含目标仓库，并确认整体状态和目标仓库状态正常。若健康接口不可用，验证配置文件已替换后明确报告“已写入但未完成运行时验证”，不要把写入误报为热加载成功。

## 完成标准

只有同时满足以下条件才报告热加载完成：目标仓库出现在远程配置中，原子替换成功，且运行中的健康状态已显示目标仓库。报告服务器、配置路径、目标仓库和验证结果；不要输出凭据或完整环境文件内容。

如果 SSH、权限、TOML 校验或健康验证失败，保留原配置（若尚未替换），说明失败环节和可执行的下一步。远程写入后验证失败时，明确区分“配置已写入”和“运行时尚未确认”。
