# 公网管理认证实施评审

日期：2026-10-20。基线由使用者确认：`1da642f17f8a5f5310eea1bc8851b56825510b8b`。

首次评审范围：`git diff 1da642f17f8a5f5310eea1bc8851b56825510b8b...c43dffa`。实施提交：`c43dffa Add HTTPS admin authentication and persistent sessions`。随后针对发现的问题修复并由原两路评审独立复核工作区增量；修复随本记录一并提交。

## Standards

依据仓库 `CODING_STANDARDS.md`、AGENTS 约定、领域文档及 code-review 技能的代码异味基线进行独立评审。首次评审与修复复评均无 actionable findings。

修复复评结论：独立证书副本、原子链接切换、异常恢复与互斥锁职责清晰；失败向调用方传播，清理失败有日志。CLI 回归覆盖 Certbot 替换证书后安装失败，以及无效续期候选不改变已发布证书。

## Spec

依据本目录 spec.md 及四张实施票据进行独立评审。

首次发现 1 个 P2：Certbot 可在安装验证前切换 live 证书，而异常恢复只还原 Nginx 配置，违反“错误证书或配置不能替换正常配置”。

已修复并复评确认：Nginx 使用独立证书发布目录；验证证书 IP、有效期及私钥配对后原子切换；安装或续期失败先恢复旧证书引用，再恢复 reload。安装与续期共享锁；安装失败同时恢复原续期 hook/unit；显式区分测试与生产 ACME 地址。新增回归先确认原缺陷失败，再修复为通过。复评未发现新增规格问题。

Standards：0 项，最严重项无。Spec：首次 1 项 P2，已修复；剩余 0 项，最严重未解决项无。

## 验证证据与边界

- Rust 1.90.0：fmt、check --locked、全套 test --locked 通过；交互 fixture 按约定忽略，由 HTTPS 验收独立调用。扩展无认证入口矩阵后针对性复测通过。
- 前端 Node 测试：13 项通过。
- 部署 Python 测试：既有部署 14 项、公网入口 9 项，共 23 项通过；部署 shell 语法和 git diff --check 通过。
- 真实 Nginx 1.30.6 与真实 Chrome：本地测试信任链/IP 校验、HTTP 挑战隔离、编码内部路径隔离、Secure/HttpOnly Cookie 实际保存及携带、页面重载、多设备、退出、运行时密码重置与重新登录通过。更改证书发布路径后再次通过。
- 未安装 Docker：镜像构建/Compose smoke 未执行。
- 未连接生产服务器、未推送、未申请生产证书、未更改线上代理/端口/防火墙。实际 ACME staging/生产签发、自动续期及外网 80/443/8081 隔离仍待部署验收；CLI 替身测试及本地可信测试证书不替代这些检查。

使用方法与线上验收步骤见 [部署文档](../../docs/public-admin.md)。第四张票据中的线上检查保留未勾选。
