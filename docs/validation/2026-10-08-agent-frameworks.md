# 外部 Agent 框架接入验证（2026-10-08）

后续的存活检测、状态面板、真实模型活动中断和回放验证见 [2026-10-08-agent-liveness.md](2026-10-08-agent-liveness.md)。下文保留首次无模型接入验证的范围。

本次实现将默认运行入口改为无模型循环的 `TradingService`，提供账户限定的 MCP 工具和业务唤醒事件。Codex / OpenCode 的模型调用、历史和规划仍由各自框架管理。旧模型循环需显式启用；已有选手可以暂停后保留原账户迁移。

## 已验证

- Python 回归共 69 项：66 项通过，3 项按环境条件跳过；覆盖运行时、警报、外部工具、连接器和策略项目。
- 官方 MCP Python SDK 1.30.0 的真实 stdio 客户端发现 23 个工具并执行调用。
- 本机 Codex CLI 0.144.5 的真实 app-server 发现同样的 23 个 MCP 工具，读取绑定账户，调用虚拟下单工具，并拒绝警报触发后的旧决策新订单。该测试没有发起模型推理。
- 隔离安装的 OpenCode 1.18.35 启动真实本机 serve 进程，连接 MCP，创建并恢复同一个原生 session。该测试没有发起模型推理。
- 控制协议测试覆盖 Codex 活动 turn 的 steer、turn 完成竞态和同一 thread 续接，OpenCode 的 abort 后同一 session 续接，以及批量唤醒、暂停和游标在交付后持久化。
- 业务测试覆盖账户令牌隔离与轮换、决策代际失效、未知订单按原请求 ID 核对、暂停后的结果核对、旧账户迁移和观测期间警报竞态。
- 前端生产构建、Python 编译及 `git diff --check` 通过。

## 验证边界

真实客户端测试证明 MCP 接入和原生会话协议兼容；活动推理被真实市场警报打断、模型自主调整计划、模型认证和持续交易效果尚未做模型端到端验证。原生测试不产生提供商推理费用。

三个需要额外环境的 Docker / 实时交易提供商测试按条件跳过。既有 Rust 订单保护沿用此前工作区回归结果，本次框架接入没有新增 Rust 改动。

测试生成的本机记录位于忽略目录 `.local/validation/`，包括 `agent-frameworks-python.log`、`agent-frameworks-web.log`、`native-codex-mcp.json` 和 `native-opencode-mcp.json`。没有重启既有用户服务、迁移真实选手或修改全局框架配置。

使用方式见 [AGENT_FRAMEWORKS.md](../AGENT_FRAMEWORKS.md)。
