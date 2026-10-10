# 2026-10-10 交易能力第一阶段验收

本记录保留第一阶段历史结果；后续扩展见 [第二阶段验收](2026-10-10-agent-workspaces.md)。

来源：`7246dfb` 的独立工作树 `trading-capabilities`。原检出存在其他并行改动，本轮没有覆盖、提交或推送它们。

## 自动检查

- `cargo test --workspace --quiet`：511 项通过，9 项既有性能基准按默认忽略。含 11 项原生仓位保护测试：无效开仓原子拒绝、TP/SL、部分退出锁定重试、挂单撤销、保护替换/取消、快照/日志重放、HTTP 幂等/权限、Last 多笔成交跨越、双向空头独立保护、数量/名义金额分批、时钟续退、强平后的旧保护清理。
- `cargo clippy --workspace --all-targets -- -D warnings` 与 Rust 格式检查通过。
- Python 的 runtime / alerts / orders_policies / projects / external / monitoring / connectors / market_data：108 项，103 项通过，5 项其他外部环境测试跳过。本机 Windows 默认没有 `python3`，Rust 进程插件测试使用测试虚拟环境内的 `python3.exe` 启动别名；没有修改生产插件命令或转移其执行到宿主。
- CandleScope 仿真测试：27 项通过；新增风险大整数/账户隔离和保护请求校验。
- 前端 typecheck 和 production build 通过。构建仍有既有大 chunk 警告，不影响此次通过结论。
- 保留文件原有换行格式，`git diff` 空白检查显式允许 CRLF 行尾。

## 真实运行

隔离 PostgreSQL 位于本工作树 `.local/capability-qa/postgres`，监听 127.0.0.1:55435；后台 57315，前端 15183。测试不使用生产数据库或模型提供商。

1. `scripts/validate-trading-capabilities.py` 通过真实 HTTP 和官方 MCP stdio session 验证 31 个工具、行情历史、原生 PNG 图片、开仓保护、取消保护、风险事件和强平唤醒。外部交易身份读取其他账户风险历史得到 403。
2. 同一脚本加 `--strategy`，在现有 WSL `Ubuntu-22.04` Docker 镜像 `marketforge-strategy:1` 中运行生成的 Python。容器以非 root 运行，读取 `analysis_data.candles`、指标和私有风险字段，返回 bracket action，经过现有策略服务提交真实虚拟交易所。观察到仓位 1、保护 armed、策略 state.done=true；随后停止策略并清理该测试仓位。
3. PostgreSQL 重启：账户、风险、保护、盘口、挂单、私有事件一致。随后对实际保护触发生成的系统退出单再次重启，查出了恢复编号校验遗漏并修复：保护退出单与强平单共同排除在 API 订单游标之外，但仅可信系统记录可以使用该编号范围。
4. 另在 `capability-qa-strategy` 持久化原生退出及 105 个命令，跨越实际快照边界；数据库恢复头 `snapshot_command_seq=126`、`next_order_id=28`。再次重启后，仓位/风险/保护/盘口/挂单/时间、完整 K 线及私有事件一致，新普通订单得到 ID 28，未混入系统编号。
5. Playwright 真实页面：账户 20 的保护从 TP110/SL50 更新为 TP115/SL95，服务器观察确认；取消后保护列表为空；通过页面再开 1 单位附保护，仓位由 5 到 6；标记价94触发原生止损，6单位退出完毕。切换账户40能看到 margin_call / liquidatable / 强平费用4 / 剩余坏账0 的提醒。

本地证据位于 Git 忽略的 `output/trading-capabilities/` 和 `output/playwright/`：包含原生 MCP PNG、运行 JSON、真实策略状态、重启前后结果与页面截图。工具令牌在脚本结束时删除。临时服务在验收后停止，数据库与证据保留。

## 边界

- 原生保护仅适用于合约，含单向/双向仓位。本轮未增加现货条件单、追踪止损、分批止盈计划或跨账户组合保护。
- 市价保护退出受交易所风控、市场状态和流动性约束，触发价不是成交价保证。原生触发不依赖 LLM 轮询。
- 基础 MCP 指标和 PNG 使用公共 K 线独立计算/渲染，不导出人的私有绘图或任意 CandleScope/Pine 指标；没有验证模型对图像的解读质量或盈利能力。
- 自动风险提醒是持久事件轮询加原有会话唤醒通道。真实会话连接器的模型回合响应未在本轮调用；单独 stdio MCP 没有主动通知能力。
- 完整历史 K 线和风险页在恢复测试中一致；即时 observation 的近期公开成交列表可能只包含恢复尾部，并不用于替代完整历史页。
- 工作树的 PostgreSQL 和浏览器验证不等同于生产包发布或合并到原检出。
