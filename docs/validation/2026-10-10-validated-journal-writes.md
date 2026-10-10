# 日志校验复用与借用编码

本轮接续账户顺序合并，优化日志写入中的重复遍历和复制。工作区开始时干净，前驱为已存在提交 `7246dfbb42d35d12f0a95a1ef601072a13ec0a15`；本轮仅改 `exchange-server/src/journal.rs`，没有提交或推送。

## 实现与语义边界

- `validate_pending_mutation` 返回私有 `ValidatedPendingMutation`，绑定已经校验的 mutation 的不可变借用。内存存储和 PostgreSQL 插入函数要求该凭据；调用者仍先校验命令 cursor、恢复 JSON 数值范围、mutation 内容，再执行记录、转账、snapshot、quota 与事务检查。
- 内存后端用借用的 `JournalMutationRef` 编码原有五字段 envelope，省掉完整 mutation 克隆和第二次相同校验。成功编码后才推进 mutation 序号和发布字节。
- PostgreSQL 普通和 fenced append 复用入口校验，在原事务内执行原写入和 fence 检查。`run_postgres` 同步执行于专用日志线程，可以直接借用 coordinator 已拥有的输入，省掉 mutation、执行数组、转账数组、snapshot 和 lease claim 的额外复制。
- bootstrap 写入仍经过校验；没有公开未校验写入 API。不可变借用保证从验证到写入期间数据不变。

不改变日志字段、schema、恢复格式、事务、队列容量、风险校验或 tick 目标。snapshot 的独立校验和其他 JSON payload 构造仍保留；本轮没有消除整个日志路径的所有重复工作。

## 固定工作量

Release 测试二进制分别追加 200 条相同 mutation。每配置前/后/后/前/前/后/后/前，两个独立测量块，共 64 次，逐字节比较全部最终日志。测试专用参照恢复前驱的第二次校验和 owned mutation 克隆编码，仅用于内存后端。

1000 个完整模板由 38 类 fixture 模板循环构造，稀疏 delta 取 64 个运行状态。这个测试测日志序列化成本，不代表 1000 个原生策略在服务中执行。

| mutation | 块 | 原中位数 ms | 新中位数 ms | 耗时下降 |
|---|---|---:|---:|---:|
| 64 状态 scheduler delta | C | 4.797 | 2.754 | 42.60% |
| 64 状态 scheduler delta | D | 5.173 | 3.119 | 39.71% |
| 1000 模板 scheduler progress | C | 542.452 | 188.233 | 65.30% |
| 1000 模板 scheduler progress | D | 534.128 | 189.659 | 64.49% |
| 无转账 clock | C | 0.099 | 0.082 | 17.26% |
| 无转账 clock | D | 0.088 | 0.069 | 21.59% |
| 小型 state checkpoint | C | 2.575 | 1.499 | 41.79% |
| 小型 state checkpoint | D | 2.672 | 1.205 | 54.91% |

小于 0.1 ms 的 clock 批次不宜用百分比外推。完整 scheduler progress 通常用于初始化/配置等路径，实时运行已经主要使用稀疏 delta，不能将 65% 解释为服务整体收益。该基准没有测 PostgreSQL 网络或事务延迟。

证据 `.local/bot-journal-validation/paired-c.log`、`paired-d.log`、`paired-summary.json`。`paired-a/b` 是编译窗口中的初步运行，保留但不计入上述结果；C/D 在全部编译结束后执行。

## 验证

- 全工作区 Rust 501 通过、10 ignored、0 失败；新增 ignored 性能测试在 Release 单独执行并通过。
- 严格 Clippy、格式与差异空白检查通过。
- 新回归对比 scheduler delta、完整 scheduler progress、clock、checkpoint、status 的编码字节；覆盖解码、cursor 溢出优先于 room 错配、JSON 极值优先于 room 错配、损坏 delta、snapshot 错配和失败后日志/序号不发布。原有 idempotency、quota、fence、projection 与恢复测试保持通过。
- 首轮全量测试有 3 个 Python process 插件测试失败，退出 9009，测试进程使用了 WindowsApps 的 `python3` 别名。把仓库 `.venv/Scripts` 加入该测试进程 PATH 后，全量通过；没有改变源码、测试预算或插件超时，首次失败日志保留在 `workspace-tests.log`，通过日志为 `workspace-tests-python-path.log`。

前驱 Release SHA256 `8fb0d2c159a41ea79c2814c25bfd5a7657b9bf902ff05fba64b9450d33994104`；新版 `74bf8dcf6b5c9ba18a7adafd5466e92b9c7f76a6db736d3855f12206eb2089bf`。冻结二进制及原始回执位于 `.local/bot-journal-validation/`。

## 1000 bot 服务负载

同 recipe、seed 7、1000 活跃 bot、2048 MiB 内存护栏；每轮预热 3 秒、采样 20 秒。加速目标 40 step/s（25 ms 墙钟间隔），实时目标 1 step/s（1000 ms）。每个模拟 step 为 1000 模拟毫秒。加速块 A 前/后，块 B 后/前，各进程使用独立随机 loopback 端口；没有并发运行本任务的编译、基准或其他服务测试。

| 模式/块 | 版本 | step/s | 读取 P95 ms | 下单 P95 ms | 撤单 P95 ms | 峰值 MiB | 观察落后 step 中位数/最大 |
|---|---|---:|---:|---:|---:|---:|---:|
| 加速 A | 前驱 | 30.964 | 56.25 | 56.19 | 57.35 | 460.81 | 17 / 24 |
| 加速 A | 新版 | 35.611 | 43.21 | 52.66 | 59.69 | 710.95 | 2 / 3 |
| 加速 B | 前驱 | 33.260 | 47.15 | 49.97 | 51.06 | 471.05 | 15 / 20 |
| 加速 B | 新版 | 33.815 | 43.41 | 37.89 | 42.11 | 455.04 | 14 / 18 |
| 实时 | 前驱 | 0.963 | 40.49 | 40.03 | 42.99 | 204.26 | 0 / 1 |
| 实时 | 新版 | 0.963 | 32.32 | 44.17 | 37.02 | 192.62 | 0 / 1 |

六轮客户端/worker/bot 错误为空，均观察到 1000 活跃 bot；没有触发内存护栏。两组加速吞吐分别提高 15.01% 与 1.67%，仍未达到 40 step/s 目标，也未达到目标 95%（38 step/s）的准入条件。A 新版消耗约 2.03 个逻辑核心，而其他加速轮约 1.18–1.24；A 的观察更新更及时、峰值内存也显著增加。不能把 A 的全部收益归因于日志优化，或声称新增持久缓存导致内存差异（本轮无新缓存）。

日志阶段累计计时 A 为 2.203→2.364 秒，B 为 2.231→1.739 秒；apply A 为 16.736→12.337 秒、B 为 16.711→16.941 秒。服务中的异步决策顺序、有限资金轨迹、已完成工作量和宿主机负载会改变，累计阶段计时可以嵌套，不能直接相加或当作固定工作量。实时模式读取/撤单延迟改善，下单延迟变差，不能声称所有接口都加速。

相比前一轮同一前驱二进制约 16 step/s，本轮前驱自身也达到约 31–33 step/s，说明跨轮宿主机/异步负载差异很大；性能判断使用本轮的相邻前后对照，不将跨轮差异计入改动收益。固定工作量证明了日志阶段收益，服务整体的稳定增益仍未确定。原始回执在 `{before,after}-1000-{fast-a,fast-b,realtime}/bots-1000/`，汇总 `comparison.json`。

## 生产二进制与真实数据库对照

冻结生产二进制执行相同 335 个固定 API 操作，实际成交 80 次、风险拒绝 5 次、手动时钟推进至 280 step；逐操作响应、完整最终账户/盘口/消息/时钟/调度快照一致。该对照配置 38 bot，自动 worker 关闭，因此不作为原生策略自动运行一致性的证据。回执 `fixed-trading-comparison/`。

另在独立随机端口的 PostgreSQL 集群和新建数据库中，强制 `MARKETFORGE_REQUIRE_POSTGRES_TESTS=1` 执行原有 PostgreSQL 集成测试，1 通过、0 失败。真实验证普通写入/恢复、房间范围恢复、有效和失效 writer fence、执行/转账投影，以及 exclusive runtime lock；不是未配置数据库时的跳过结果。回执 `postgres-integration/`。测试结束后停止本任务启动的集群，不操作用户数据库。

新版生产二进制另完成原生策略验证：

- 38 bot、100 ms、压缩时间线：30 step 暂停后数据库与服务均重启，账户/盘口/消息/时钟/调度快照精确一致，恢复至 60 step，POV 截止完成量 11/18；全部事件送达，无 bot 错误。该短测试不代表原始完整时间线或数据库吞吐达标。回执 `postgres-boundary-recovery/`。
- 33 bot、25 ms、原始完整时间线：1000 step 用时 25.000 秒，POV 买卖均 25/25，全部事件送达，无 bot 错误。最终观察落后 1000 模拟毫秒（1 step），进程峰值约 103.63 MiB，低于 512 MiB 护栏；未延长订单 TTL。回执 `original-1000/`。

上述均为本地合成模拟，未做真实市场校准。77 份源码/manifest 摘要、新版 Release 与全部资格回执已核对，见 `final-verification.json`。本任务测试服务均已退出，独立 PostgreSQL 集群已停止。

## 剩余热点

后续优先关注 writer 中 action apply、预留金额协调和保证金同步，以及原生策略观察的构造和决策提交成本。继续优化应保留全组风险检查与精确失败回滚，并测等价固定工作量；不能仅靠本次异步负载最优轮推断吞吐已经解决。PostgreSQL 事务吞吐和持续长时间运行尚未在本轮量化。
