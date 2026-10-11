# 窗口统计与组合提醒验收

验证使用本次编译的独立 Rust 交易服务器、临时 Python 业务服务、绑定选手令牌，以及官方 MCP Python 客户端通过 stdio 实际调用。没有调用模型，没有改动已有运行房间。原始回执见 [JSON](2026-10-11-agent-window-tools.json)。

- 多头爆仓先成交 4 lot × 70 = 280：30 秒统计为 280，价格条件成立，但累计条件不成立，没有提醒。随后成交 6 lot × 70 = 420：累计 700，超过 600，且标记价 70 < 80，真实 MCP 注册的组合提醒触发；旧决策的后续注册被拒绝。
- 订单流返回三笔实际成交、总成交额 1700、两笔爆仓成交额 700、Delta/CVD 为零、两个价格成交组，证明爆仓成交没有重复计入全部成交。截断扫描明确返回 `complete=false`。
- 仿真推进 31 秒后，30 秒窗口爆仓累计归零；查询历史窗口仍返回 700，历史盘口为 null。
- 负资金费结算窗口返回一份实际成功回执、费率 -10000 ppm、转移额 100，通过原生 SDK API 读取。

Rust 测试还检查读取权限、错误参数、精确 VWAP、日志恢复、包含窗口边界、固定日志 head、未知时间戳与扫描截断。Python 测试覆盖精确宽整数/有理数比较、指标预热、组合 any/all、持续条件、回差、冷却、网络失败、取消竞态、查询复用、重启恢复与 SDK 路径编码。

提交前仅包含本次 MCP/API 改动的隔离快照回归结果：Rust 交易服务器 251 项通过、2 项忽略；Python Agent 套件运行 138 项，131 项通过、7 项跳过。真实 MCP 事件回归 2 项和窗口验收 3 项均通过。

此回执证明上述受控交易和服务链路；未证明模型自主决策表现、生产 PostgreSQL 负载或高频长期提醒容量。组合条件仍受采样间隔和跨读取时间一致性约束。

复现：

```powershell
cargo build -p exchange-server --bin exchange-server
$env:PYTHONPATH='python'
.venv/Scripts/python.exe scripts/validate_agent_window_tools.py --report docs/validation/2026-10-11-agent-window-tools.json
```
