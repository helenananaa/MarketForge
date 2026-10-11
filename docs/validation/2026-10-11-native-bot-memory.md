# 1000 bot 加速组内存归因与 bot 日志容量修复

## 结论

主因是内存日志存储长期保留 bot 状态记录、execution 和查询投影，序列化缓冲的空余容量又放大了开销。六次释放所有者的实验中，日志存储侧占暂停后有效堆内存约 90%–97%。运行时 bot 来源的存活分配约 11–14 MiB，delivery 来源约 0.092 MiB；5–30 秒间没有随日志同步增长。来源标签并不等同于所有者，因此还实际停止工作线程、释放日志记录及其他日志状态，验证归因。

本轮只收紧包含 bot 状态的 `SchedulerDelta` 和 `SchedulerProgress` 留存缓冲：固定工作量有效堆 307.87 → 244.41 MiB（-20.61%），bot 状态日志缓冲 169.47 → 106.01 MiB（-37.44%）。实际 Release 服务峰值中位数 1244.6 → 981.4 MiB（-21.15%），但只有每版本两次、成交轨迹不同，不能作为稳定内存上界。

日志仍随运行时长增长；本轮没有删除历史、改变检查点频率或把空动作观察省略。1000 bot 加速组仍不能认定稳定达到配置的 40 步/秒。

## 范围和来源

沿用独立 worktree `E:/CodexData/worktrees/native-bot-delivery/MarketForge`，分支 `codex/native-bot-delivery`；没有合并其他 worktree，也没有写入原 `E:/projects/MarketForge`。本轮基线 B1 是上轮原生调度与共享观察的候选版本，不是优化前的逐 bot 投递 B0。工作树 HEAD 仍是冻结未合并优化的 `07c98ba761448b9161b97a8d66f22167c60da267`；本轮改动尚未提交、合并或推送。

| 二进制 | SHA-256 |
| --- | --- |
| B1 修复前 Release | `ba05ea7d5b9b85419e76e87390b17ee5d55e43e72591ad145e09ee1cdd21ebb0` |
| B2 修复后 Release | `ed976938ef1249b988b15c0379b4be4d387fb0bd06bab0aaae1f52e83cb9bcd2` |
| 测试专用归因二进制 | `4797ea812def4153d692e1454d2b9ece767637ebd5bea5350040ffecf95c4a46` |

来源收据：`.local/native-bot-memory/before-provenance.json`。生产路径的新增行为在 `exchange-server/src/journal.rs` 的 `InMemoryJournalStore::store_mutation`：序列化完成后、放入历史 Vec 前，对有非空 bot 变更的 delta 或有 bot roster 的完整 progress 调用 `shrink_to_fit()`。不涉及无 bot 状态的 clock delta、其他市场日志或 PostgreSQL 编码。

## 归因方法和所有者释放

`memory-diagnostics` feature 只在库测试中加载分配计数器；正式服务没有此 allocator、环境开关或清空日志的 API。四种分配来源为 delivery、bot、journal、other。记录的是 Rust 请求的存活堆字节，排除 allocator 的内部空闲、线程栈和额外 tracer header；不是 RSS。`other` 包含交换所、调度主线程、Tokio、日志投影等，不能全部视为撮合内存。

1000 bot、冻结 seed 7 recipe、25ms 墙钟步长，运行 30 秒，每 5 秒采样；暂停并等待完成提交后，依次停止并 join 工作线程、释放 mutation Vec、释放剩余测试日志存储、释放 App。破坏性释放仅存在于 ignored 的隔离测试，全部写线程停止后执行，普通运行与恢复测试不会清空日志。

以下均为有效堆 MiB。暂停已经清理部分缓存，工作线程栏仅是暂停之后再释放的部分；不能把该栏当作整个运行时 bot 成本。

| 运行 | 最终模拟步 | 暂停时堆 | 停止线程释放 | 历史 mutation 释放 | 其他日志状态释放 | App 释放 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| profile-before-01 | 1163 | 1047.8 | 4.92 | 922.6 | 79.7 | 40.4 |
| profile-before-02 | 1155 | 1237.1 | 2.16 | 1144.8 | 56.9 | 33.2 |
| 01-original | 1088 | 677.3 | 2.15 | 469.6 | 138.1 | 67.4 |
| 02-tight | 1142 | 717.9 | 1.45 | 616.9 | 62.1 | 37.3 |
| 03-tight | 1125 | 747.9 | 2.83 | 644.0 | 62.4 | 38.4 |
| 04-original | 1053 | 579.6 | 0.75 | 398.8 | 123.6 | 56.3 |

最初两个 B1 实验中，历史 mutation 分别占 922.6 / 1144.8 MiB；其中 bot delta 的原始 JSON 357.7 / 463.4 MiB，缓冲容量却是 702.4 / 910.4 MiB。完整 bot progress 又分别留存约 219.0 / 233.0 MiB 容量。前一轮 1000 bot 高峰约 1–1.7 GiB 的量级可以由这类日志留存解释。

`InMemoryJournalStore.mutations` 保留所有序列化记录；每次空动作决策也会持久化 bot 的 last observation、价格历史及状态。调度 revision 每到 100 的倍数还会保存完整 roster。价格历史在 bot 内有界，但每条历史记录各自保留一份，所以累计日志仍是随提交次数增长的。快速完成更多观察、进入较多空动作状态更新的轨迹，会增加记录频率；实测 reference/tight 的并发轨迹不能当作相同工作量。

上轮固定 benchmark 的约 1.4 GiB 进程峰值还包含了最终完整 recovery/receipt 构造，不能直接归因于 bot 投递。本轮固定堆采样提前到 receipt 构造之前；正式服务采样也在负载窗口内结束。

## 固定工作量对照

同一 Release 归因测试二进制，原始容量/收紧容量按 ABBA 重复两轮，共 8 次；原生投递、正常 gateway、提交顺序、状态与检查点完全相同。测试专用 reference 开关只绕过容量收紧。

1000 bot、64 步、64,000 次决策、20,409 个动作、43,952 次空动作决策、2048 次 blocking task 派发。八次最终市场、账户、订单、完整 bot 状态及 execution receipt 的 SHA-256 全部为 `e16c444ca79b5375c4b3b02e3799dc245d6fae0ac92f6e8f280dde2233da0f6a`；与上轮 B1 receipt 也相同。

| 中位数 | B1 原始容量 | B2 收紧容量 | 变化 |
| --- | ---: | ---: | ---: |
| receipt 构造前有效堆 MiB | 307.87 | 244.41 | -20.61% |
| bot 状态日志容量 MiB | 169.47 | 106.01 | -37.44% |
| 整轮 ms，含正常提交 | 16078.67 | 15903.64 | -1.09% |

JSON 实际长度与记录数相同。delta 中不含 bot 变更的时钟记录有少量余量保留，state checkpoint 也保持原样，所以不是所有记录容量都等于长度。带 allocator 计数器的耗时会受诊断开销影响；原始容量整轮约 14.09–18.54 秒，收紧容量约 15.21–23.04 秒，不能声称吞吐有明确提升。

原始数据：`.local/native-bot-memory/fixed/summary.json`；每次 `.receipt.json`、`.memory.json` 保留在同目录。

## 实际 Release 服务

无 allocator 计数器。全部构建和检查完成后串行 ABBA，1000 bot、冻结 38 bot recipe 扩展、seed 7、有限账户、25ms 墙钟步长，预热 5 秒、测量 30 秒。独立进程、独立端口、内存日志；同时测 HTTP 下单、撤单、读取。CPU 是平均占用的逻辑核；P95 是各次自己的样本 P95。

| 运行 | 步/秒 | CPU 核 | 进程峰值 MiB | 下单/撤单 P95 ms | 最终观察滞后 P95（模拟 ms） | 完成日志操作 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 01-before | 35.44 | 1.092 | 779.5 | 39.89 / 40.59 | 9000 | 4364 |
| 02-after | 36.32 | 1.094 | 696.0 | 37.06 / 43.49 | 10000 | 4702 |
| 03-after | 38.89 | 1.303 | 1266.8 | 33.64 / 31.21 | 4000 | 13029 |
| 04-before | 38.76 | 1.363 | 1709.7 | 30.27 / 36.97 | 1000 | 12555 |

每版本两次的中位数：步频 37.10 → 37.61，CPU 1.227 → 1.198，下单 P95 35.08 → 35.35 ms，撤单 P95 38.78 → 37.35 ms。局部没有观察到明显 CPU/HTTP 退化，但两边只有一次满足脚本的 38 步/秒门槛；不构成长期容量验收。

四次均无 HTTP/服务错误、无 bot 错误，1000 bot 全部有最终观察，没有未来时间观察。观察滞后是暂停后的模拟时间差，不是墙钟延迟；其 P95 中位数修复前 5000、修复后 7000 模拟 ms，这个观测值变差，保留为未排除的时序退化信号。自动完成顺序、成交轨迹与提交量不同，当前证据不能把它确认归因于容量收紧，也不能据此宣称观察时效改善。两版本的日志操作量不同，进程峰值修复前约 779–1710 MiB，修复后约 696–1267 MiB；应以固定工作量的字节差确认修复收益，不能把每个实时峰值差都归因于这一行修复。

原始数据：`.local/native-bot-memory/live/*/bots-1000/`，保留 report、metrics、scheduler、recipe 与采样。机器为 Windows、32 个逻辑处理器；未测试 PostgreSQL 或长期稳定占用。

## 语义和验证

- 每条历史记录仍保留，序列化字节、mutation sequence、checkpoint 频率、恢复协议均不变。新测试逐条比较 wire bytes 和 decode sequence，确认只缩减选中的 bot 状态缓冲。
- TTL、逐次观察的价格/波动率更新、风控退出与 gateway 风控、部分成交/成交反馈、epoch/config fence 和一 bot 一个在途决策均未改动。没有跳过空动作决策。
- core 库测试：315 通过，11 ignored；server 带归因 feature 的 Release 库测试：218 通过，3 ignored。Windows 进程插件测试使用原仓库 `.venv/Scripts` PATH；PostgreSQL 测试没有配置真实数据库，不计为数据库验证。
- `cargo clippy -p exchange-core -p exchange-server --all-targets --features memory-diagnostics -- -D warnings`、`cargo fmt --all -- --check`、`cargo check --workspace --all-targets` 全部通过。

## 复现

在该 worktree 中，使用已冻结的 `.local/native-bot-delivery/fixed-1000.json`（SHA-256 `b8a793856936feda4b2823612eb138e1f4b21021611a49cbb27ca0ec9021f019`）：

```powershell
cargo test -p exchange-server --release --lib --features memory-diagnostics --no-run
python scripts/benchmark_native_bot_memory.py --test-binary target/release/deps/exchange_server-<hash>.exe --recipe .local/native-bot-delivery/fixed-1000.json --output .local/native-bot-memory/fixed-rerun --rounds 2
python scripts/benchmark_native_bot_memory.py --test-binary target/release/deps/exchange_server-<hash>.exe --recipe .local/native-bot-delivery/fixed-1000.json --output .local/native-bot-memory/ownership-rerun --workload live --rounds 1 --seconds 30
```

实际服务的前后 ABBA 命令保存在 `.local/native-bot-memory/run_live.py`，直接使用已保存的 before/after Release 二进制。先完成构建，再单独运行测量，不与编译或其他负载组并行。

## 剩余问题

容量浪费已消除；保留所有 bot 历史状态和 execution/projection 的策略仍让内存随时长增长。本轮不改变这些市场日志与恢复/审计边界，因此没有承诺稳定内存上界。进一步降低长期增长需要单独设计日志归档、压缩或保留策略，并验证历史恢复和查询语义；不能靠省略观察、TTL 检查或成交反馈解决。
