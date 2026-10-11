# 2026-10-11 事件输出补齐验证

实现范围：公共逐笔成交、绝对数量盘口增量、公开强平成交；私有成交结算、资金费、保证金数值及状态、ADL、社会化损失、钱包总额/冻结/可用额和划转状态。接口说明见 [EVENT_OUTPUT.md](../EVENT_OUTPUT.md)，摘要证据见 [event-output.json](2026-10-11-event-output.json)。

## 验证边界

基线 `8f99502b7d01dea86d8e9ec14b1cb989db34f673`。验证时主工作区同时存在其他 bot runtime / 策略改动，因此另建独立检出，仅应用本次事件输出补丁；没有复制这些并行改动。

独立检出与原目录的本次独占代码、文档和脚本逐文件一致；`lib.rs` 使用仅包含本次改动的补丁，排除并行添加的 bot 模块。补丁 SHA-256 和运行摘要已保存到 JSON。

## 自动测试

`cargo test --workspace --all-targets`：**582 通过、0 失败、13 默认跳过**。默认跳过项为既有基准/诊断测试，不作为性能结论。设置 `MARKETFORGE_REQUIRE_POSTGRES_TESTS=1` 和独立数据库 URL，实际执行 PostgreSQL 用例。

新增事件专项 **11/11 通过**，覆盖：

- 多笔撮合的完整成交明细；挂单、成交、改单和撤单增量重建实际盘口。
- 私有成交及订单冻结额；对手账户、订单和结算信息隔离。
- 仅保证金事件、ADL 接收者无普通成交时的通知。
- 健康状态下标记价格变化导致的权益、未实现盈亏、保证金通知。
- 真实部分强平，先成交 4 手、再成交 6 手，最终产生 ADL。
- 资金费现金流、钱包变化、多市场快照及恢复后载荷一致。
- 延迟划转 Pending/Completed、独立钱包游标、分页和恢复。
- 超过 1,024 缓存、跨 500 条日志分页补读；未来游标拒绝。
- 排队私有快照在撤权后阻断。
- 大于 u64 的资金值无损序列化，以及钱包/保证金回执篡改检测。
- PostgreSQL 执行回执、盘口增量和钱包游标恢复。

`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 和 `git diff --check` 在独立检出全部通过。

## 真实 HTTP / SSE

使用独立 PostgreSQL 数据库，在 `127.0.0.1:57549` 启动服务。运行 `scripts/validate_event_output.py --phase live`，正常关停后重新启动进程，再运行 `--phase replay`。

实测通过：完整成交、盘口重建、账户隔离、钱包划转生命周期、健康保证金变化、部分强平、ADL、资金费、多市场快照。重启后 **5 条订阅、12 个执行载荷、2 个钱包回执** 与之前逐项一致；钱包 SSE 也可以从游标 0 补读。验证服务和本次独立 PostgreSQL 集群已正常关停。

原始证据保留在 `.local/event-output-validation/`：`http-events.json`、`isolated-tests.log`、`isolated-clippy.log`、`event-output.patch` 和文件哈希；数据库凭据仅在本机 DPAPI 文件中，未纳入报告。

## 限制

主目录的整套尝试受并行 bot 改动和 Windows Python 占位程序影响，出现过 bot 测试、编译及 Clippy 失败；这些结果不作为本次补丁独立验证的结论。独立验证使用任务内真实 Python 可执行文件，未修改原目录的 bot 实现。

旧日志可能没有新增的深度、钱包或保证金回执；深度缺失显式输出 `book_delta_available: false`。取得新快照后再继续应用增量。资金费/强平/ADL 使用现有核心模型，本次验证不构成与某一家交易所模型或消息格式完全一致的资格证明，也没有重新进行吞吐量基准。
