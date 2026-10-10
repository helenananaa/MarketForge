# 2026-10-10 第二阶段：Agent 工作区和量化 API 验收

本轮仍在 `7246dfb` 的独立 trading-capabilities 工作树实现，保留原检出的并行改动。提交范围仅包含本工作树的能力扩展；没有推送或合并到原检出。第一阶段历史证据保留在 [交易能力验收](2026-10-10-trading-capabilities.md)。

## 自动检查

- Rust workspace：517 项通过，9 项既有性能测试默认忽略；新增 5 项高级交易场景和 1 项私有成交结算过滤测试。高级场景覆盖超额只减仓挂单、FOK 实际可执行深度、追踪止损快照恢复、分批止盈/剩余止损、限价退出撤销和条件入场子单去重。
- Rust clippy 全 workspace/all-targets 严格警告检查和格式检查通过。
- Python agents 测试：122 项，其中 115 通过、7 项外部环境测试跳过；含 11 项工作区/程序 SDK 测试，覆盖暂停/离线/警报、共用预算、账户和工具范围、原生编号、账户策略与条件单互斥、请求 ID 输入核对、未知回执保留和空页游标前进、未知读取同 ID 重连和已知范围错误停止。
- 配置隔离 PostgreSQL 后，6 项 postgres 测试单独通过。首次尝试与正在验收的后台同时持有同一测试数据库，得到 RuntimeLockUnavailable；停止该后台后通过，没有修改锁行为。
- 第一阶段前端 27 测试、typecheck/build 证据继续适用于人类 TP/SL UI；第二阶段没有改前端。高级订单在原生 HTTP/MCP/SDK 中开放，人类页面尚未增加专用控件。

## 真实 MCP、Docker 和 HTTP

通过 `scripts/validate-agent-workspace.py`，官方 MCP SDK stdio 会话枚举 **44 个工具**。初次成功房间 `capability-qa-workspace-1010-c`，最终修订再次在 `capability-qa-workspace-1010-final` 通过，持久 PostgreSQL 后台 127.0.0.1:57315，数据库 55435。CandleScope 分析服务 18096 使用本工作树源码和已有安装运行时。

成功证据 `output/trading-capabilities/capability-qa-workspace-1010-c/workspace-acceptance.json`：

1. 经 MCP 启动 Docker、写入自编 Python、Shell 执行；普通用户 UID65534，volume 只挂 `/work`，没有宿主目录、Docker socket 或凭据注入。
2. 容器程序通过账户 relay 读取观察、交易规则、完整历史、手续费和公开事件；真实 FOK 成交后，相同 ID/参数重试返回相同回执，仓位只有 1；同一 ID 改数量拒绝。
3. 完整历史按每页 2 条扫描，跨空/公开记录分页；精确订单投影可查询普通/7e18 条件子单。查询其他账户历史或 portfolio 为 403。
4. 原生 conditional 注册、触发、查询和取消；子单准确编号 7000000000000000013，取消后退出盘口。
5. Docker bridge 联网，Shell 执行 pip --user 安装 packaging24.2 并成功导入；启动长驻 Python、读取输出、按进程组停止。
6. 暂停选手后，程序新订单在受理前被拒绝。容器 stop/start 后 proof.json 内容保持一致。
7. 共用 CandleScope MA 内置指标和 Pine6 脚本计算成功；MCP chart_export 以原生 ImageContent 导出相同权威 bars 上的指标线，人工检查 PNG 可读。PNG 的指标标签和基础预热图例随后修正；最终脚本再次验证。

此前两次失败也保留 QA 房间/程序结果：第一次测试名称误用 SMA，真实 registry 为 MA；第二次测试误认订单状态 resting，API 投影实际为 open。修正的是验收脚本期望，没有改变引擎结果。

## PostgreSQL + 后台真实恢复

在上述成功房间设置 SL105、退出限价108，当前价跌到105后生成挂着的只减仓退出单 `8000000000000000017`；再注册 Mark130 条件入场。停止后台和 PostgreSQL，然后重新启动：

- 保护状态/挂单/账户字段与恢复前相同。
- 已武装 conditional 保持相同；恢复后标记价130使其生成唯一子单 `7000000000000000020`。
- 普通新订单编号101，未落入系统范围。
- 完整账户历史游标可继续分页读取。

证据为同目录 recovery-before.json / recovery-after.json。本轮没有对这批新扩展重复跨100命令快照边界测试；第一阶段的边界证据和本轮核心快照测试分别保留，不能混称新的实机边界覆盖。

## 实际限制

程序执行能力和 API 已实测，不代表模型会盈利、LLM 图像理解已验证或成熟框架真实模型回合已验收。事件当前是游标轮询，未提供专业低延迟/L2 增量协议。Pine 经 MCP 实测；Pyne 使用仓库 transport fixture 通过同一提供 bars 的 SDK 接口实测，结果保存于最终验收目录 pyne.json。图像导出只画指标线，没有完整复现脚本标注、填充、所有副图或私人布局。

新合约高级订单是交易所原生行为；现货条件单、冰山、原子跨品种多腿和组合 OCO 未提供。正常服务停机先暂停选手再停止工作区；恢复文件/任务记录，不恢复任意 Python 指令位置。需明确恢复选手、workspace 和程序，保存游标/策略状态。

本轮临时后台、分析服务、数据库和已连接工作区在验收结束后停止。证据、数据库、工作区 volume 保留；工具令牌删除。
