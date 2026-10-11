# MarketForge 性能 worktree 整合

日期：2026-10-11。起点 `main=f951b65`。本次为本地源码整合与回归，性能报告保留各自原始基线、源码哈希和测量边界。

## 合入范围

| 方向 | 原始来源 | 整合方式 |
| --- | --- | --- |
| 撮合事务、历史和同步 HTTP SQL | `f951b65` | 保留现有实现，未用 PostgreSQL worktree 的过时 engine/example 覆盖 |
| 共享 bot 观察、账户历史、多市场执行与成交反馈接口 | 主目录原有 Rust 工作副本，`c5e3c0a` | 固定为后续优化的共同依赖；Pine 插件、Python、研究及相关文档工作保留未提交 |
| 增量保证金与强平 | `codex/incremental-margin-risk:83ba558` | 只摘实际优化提交，排除冻结旧工作区的 `f3ab562` |
| 按 venue 准备普通 HTTP 订单 | `codex/room-lock-load:dafd6d6` | 合入候选准备、房间串行提交与原有回退路径 |
| 原生 bot 调度与观察投递 | `codex/native-bot-delivery:07c98ba` 后的工作副本 | 提取 21 个实际改动文件，排除冻结基线本身 |
| PostgreSQL 主日志、查询投影、压缩存储 | `postgres-write-profile` 工作副本 | 仅提取 18 个实际 PostgreSQL 文件差量，保留同步耐久性、幂等与恢复校验 |
| 前端与高级交易 | `5552a77`、`741c657` | 两者已在原 main 中，不重复合入 |

来源性能工作树的文件和 HEAD 未修改、未删除。主目录的 16 个共享 Rust 文件固定为共同依赖后，另外 21 个 Pine、策略、研究及文档 WIP 保持原字节且未提交。提取补丁及前后文件哈希保存在本任务 `.local/worktree-integration-20261011`。文本换行规范化为 LF；历史报告数值及其源码哈希保持原含义。

## 集成处理

- Cargo features 合并为同一张表，保留 `risk-benchmark`、`bench-execution-timing`、`memory-diagnostics` 和相关 example 限制。
- native 与 PostgreSQL 的 journal 存储冲突采用 PostgreSQL 的 `StoredMutation`，完整记录使用紧凑原始 JSON 或无损 LZ4；没有删除或合并历史。
- 风险统计分别保留逻辑 JSON 字节、实际存储字节和记录数。内存归因在测试中保留单独统计的类型标签，采样时无需解压整个历史。
- 内存对照工具改为 `MF_BOT_JOURNAL_STORAGE=raw/compressed`，两者均为紧凑存储。旧 `MF_BOT_JOURNAL_CAPACITY` 对照显式拒绝，历史“空余容量”结果不能套用到本次压缩对照。
- 普通 venue 下单快路与原房间回退路径都保留写入阶段诊断；普通构建默认不开启 profiling。
- 风控固定工作量基准所用 owned decision 入口同时对测试和 `risk-benchmark` 可见，修复原生投递调整后的特性组合编译问题。

## 验证

最终验证结果及收据哈希见同名 JSON。所有 PostgreSQL 测试使用任务独立数据库，并设置 `MARKETFORGE_REQUIRE_POSTGRES_TESTS=1`，不允许静默跳过。

- `cargo fmt --all -- --check` 通过。
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` 通过。
- `cargo test --workspace --all-features`：570 通过、0 失败、14 忽略，保留默认并行运行。
- 默认 `cargo check --workspace --all-targets`、`cargo build -p exchange-server` 通过。
- 当前主目录 Python/Pine 回归使用整合后的默认服务器二进制：203 项中 196 通过、7 个可选测试跳过。真实 PostgreSQL、Pine HTTP 和重启恢复测试已运行。可选跳过对应本机 Codex/OpenCode、真实供应商、两项 replay-exchange、agent-project 与 Docker sandbox 接入。
- 固定 96 bot × 4 步的 owned/native 和 raw/compressed 两组 ABBA 对照，共 8 个案例，每个 384 次 decision、124 个 action、296 次空 decision；全部业务回执哈希一致。测试二进制、配方及收据哈希保留在 JSON。

新增组合回归 `venue_perp_sparse_risk_merge_and_restore_match_serial_execution` 覆盖两个独立永续 venue、128 基础账户、稀疏仓位、同 venue 跨商品保证金、反序候选提交、部分及最终成交、挂撤改单、变价/同价风控更新和冷恢复。每次与持续串行执行及恢复后的串行执行比较完整状态和执行历史。

首轮真实数据库并行回归的失败日志保留；最终报告记录对应原因与修正，不以串行运行或放松资金/恢复不变量绕过失败。

投影测试曾将不完整的手工执行摘要提交到其他测试也会恢复的数据库，还修改共享查询表约束。现在使用真实 actor 回执和独立 schema，保留重启、晚提交、事务回滚及严格全量恢复断言。真实 SQL 错误仍直接返回。保证金组合测试则精确区分挂单预留和持仓标记价补足：无挂单时先验证补足 2 单位，再实际降价验证释放为 0。

原生调度及 journal 表示组合另跑固定 96 bot、4 步的一致性对照。该探针使用 Debug 测试二进制并启用分配归因，只验证业务状态/动作回执一致；其耗时不作为性能结论。

Python 首轮全套中，纯 Python fake HTTP 鉴权测试出现 Windows 10053；原样单独复测及最终全套均通过，未修改 Python 服务或弱化鉴权断言，传输错误根因尚未实验证明。第二轮复用数据库时，batch fixture 未配置插件目录，而先前插件/Pine 测试已持久化对应 agent，触发既有的 unknown-bot 恢复拒绝。验证进程补齐 `MARKETFORGE_BOT_PLUGIN_DIR` 后继续使用同一数据库，全套通过；没有清库、跳过持久化房间或放宽恢复校验。两轮失败收据均保留。

## 实际边界

本次完成各方向源码集成与组合正确性回归，未重新执行完整历史性能矩阵。各报告的加速倍数不能相加；1000 bot、40 step/s 容量、并发接口尾延迟和长期内存上界仍未获得整体资格。

native packet、分区 HTTP 订单和阻塞 PostgreSQL projector 三者同时活动的全面负载/恢复矩阵仍是覆盖空隙。现有恢复、投影、epoch、分区和组合风控回归分别验证这些边界。

PostgreSQL migration 18/19 及新 `actor_bytes` 持久化检查点须使用兼容的新 writer。既有 JSONB/原始 JSON 快照可恢复；新检查点无论采用原始 JSON 字节还是 LZ4，都可能令 `actor_json=NULL`，旧 reader 无法直接恢复，不能用旧二进制直接降级。该限制涉及数据库检查点编码，HTTP/WS JSON 结构保持兼容。此次没有部署或推送。
