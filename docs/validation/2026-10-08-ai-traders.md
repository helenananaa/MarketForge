# agent.v1 首版验证记录

日期：2026-10-08。基线：`38fa8ca`，本次新增代码在工作区，未提交。

## 已完成

- 安装式 `agent.v1` 插件、兼容 Chat Completions 的模型适配器、独立交易员运行服务。
- 多品种行情/自己账户查询、直接交易、单文件 Python 策略保存/隔离测试/启停、共享额度和策略订单归属。
- SQLite 操作回执、未知交易结果同键恢复、策略输出先落盘再执行、暂停后丢弃迟到响应。
- 模型配置/交易员操作界面、最新事件与 JSON 导出、房间载入按钮、有限资金示例房间准备脚本。

## 验证证据

- `cargo build -p exchange-server` 通过，复用当前 Rust 后端，无 Rust 代码修改。
- `python -m unittest python.tests.test_agent_runtime python.tests.test_agent_live -v`：15 项通过，无跳过。启用 `MARKETFORGE_AGENT_SANDBOX_TEST=1`、`MARKETFORGE_AGENT_LIVE_TEST=1` 和 WSL Docker。
- Docker 镜像在 Ubuntu-22.04 内构建。真实策略代码执行成功；无限循环、写只读文件系统、外部网络访问均被拒绝/终止。
- 真实 HTTP 测试启动独立交易所，启用 Bearer 权限，验证选手不能读取另一选手账户；两个选手分别在现货和永续合约取得仓位，其中一个通过生成 Python 策略提交交易。
- 模型响应故意延迟时，市场时钟仍持续推进。
- 恢复测试包括：提交后响应丢失、提交后 JSON 损坏、服务恢复、策略周期不重新执行代码、父子共享额度、子策略撤单归属、暂停保留未提交周期。
- `npm --prefix marketforge-web run build` 通过；`git diff --check` 通过。
- Playwright 实际操作：载入房间 → 连接运行服务 → 测试模型协议连接 → 创建交易员 → 启动 → 查询交易声明/记录 → 暂停 → 导出 JSON。导出有 23 条事件（5 次模型请求/回复、5 次工具请求/结果及生命周期事件）。
- 本地截图：`output/playwright/ai-trader-setup.png`、`output/playwright/ai-trader-panel.png`；临时 UI 服务和生成记录均为测试数据。
- `scripts/agent_arena.py --room agent-provision-check --traders provision-one provision-two` 经真实 HTTP 建房与授权成功。

## 边界

模型 HTTP 服务使用明确的 scripted fixture；未使用真实供应商密钥，未证明真实 LLM 自主研究/策略质量。
真实生成代码隔离、交易 HTTP 和 Bearer 权限已验证；交易所使用内存 journal，未在本轮重跑 PostgreSQL 故障矩阵或整仓 Rust 测试。
本期没有新增现货/合约价格联动、资金费率、比赛排名、资产自选、观战回放播放器或视频导出。
新增的记录链路可以支持后续视频呈现，但不是完整视频产品验收。
