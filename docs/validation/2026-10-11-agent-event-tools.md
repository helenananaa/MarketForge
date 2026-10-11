# 2026-10-11 LLM 市场事件接口验证

本次补齐 `market_events`、`event_alert_set`，以及资金费/指数状态指标。提交前导出仅包含本次 MCP/API 改动的暂存快照，在隔离目录验证；Pine/bot 调度等其他未提交改动未包含在快照中，也没有重启已有服务。

## 已验证

- Rust `cargo test -p exchange-server --lib --quiet`：251 passed，2 ignored（性能基准），0 failed。首次直接运行有 7 个已有 Python bot 进程测试因 Windows `python3` 解析到不可用解释器而失败（exit 9009 / worker closed stdout）。最终运行仅在该命令 PATH 前置 `.local/agents/event-qa/python3` 的验证专用虚拟环境入口，未改插件配置或全局 PATH。
- Python `.venv/Scripts/python.exe -m unittest discover -s python/tests -p 'test_agent*.py' -q`：138 tests，7 skipped，0 failures/errors。包括本次 8 个事件接口测试；跳过项需要显式开启模型/Docker 等环境。
- `cargo build -p exchange-server --bin exchange-server` 成功。
- `git -c core.whitespace=cr-at-eol diff --check` 通过，保留原文件 CRLF 约定。

新增 Rust HTTP 测试覆盖实际部分强平的金额/方向过滤、隐私字段隔离、无权限用户拒绝、空页扫描、日志恢复、有符号资金费率、模拟时间窗口、价格更新历史、非法参数和未来游标。Python 覆盖规则/参数校验、稳定请求回执、单次提醒、证据上限、冷却积压、normal/urgent 分级、取消后丢弃在途读取、重启持久化、失败不推进游标、MCP schema 与 HTTP 凭证隔离、SDK 翻页。

## 真实进程和 stdio MCP 联调

```powershell
$env:PYTHONPATH='python;.'
.venv/Scripts/python.exe scripts/validate_agent_event_tools.py --report docs/validation/2026-10-11-agent-event-tools.json
```

执行独立临时 Rust 交易所、临时 Python 业务服务和官方 SDK 的 stdio MCP 子进程；运行真实后台提醒线程，不调用模型，不连接已有市场。机器可读证据：[2026-10-11-agent-event-tools.json](2026-10-11-agent-event-tools.json)。

- liquidation：70 × 4 的实际部分强平成交，`min_notional=280` / `side=Sell`；历史记录与提醒证据一致，generation 变为 1，旧凭证写入被拒绝。
- funding：t=2000 ms 的 -10000 ppm 实际结算，公共 `total_transfer="100"`；`max_rate_ppm=-10000` 规则触发，历史记录与提醒证据一致，旧凭证写入被拒绝。
- 两个 stdio MCP 会话均发现 49 个工具，公共历史不含 account_id。

## 验证边界

实际进程联调使用隔离的内存日志，持久恢复另由 Rust journal recovery 和 Python SQLite 重启测试覆盖。未配置 PostgreSQL 进行本次真实数据库联调，未调用真实 LLM 验证其自主选择新工具，也未操作用户正在运行的部署。主动唤醒框架沿用现有会话连接器；新提醒的业务事件和决策失效已验证。

金额阈值按原生 quote 单位的单笔实际成交计算；不表示整次强平累计值或美元金额。每条规则每轮扫描最多 500 条命令，冷却和积压可能增加提醒延迟，证据截断后完整历史仍可查询。
