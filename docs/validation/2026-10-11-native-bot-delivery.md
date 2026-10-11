# 原生 bot 调度与观察投递对照

后续的 1000 bot 加速组内存归因、bot 状态日志容量修复与 B1/B2 对照见 `2026-10-11-native-bot-memory.md`；下文保留原始 B0/B1 测量结果。

## 范围和基线

工作树：`E:/CodexData/worktrees/native-bot-delivery/MarketForge`，分支 `codex/native-bot-delivery`。基线提交 `07c98ba761448b9161b97a8d66f22167c60da267` 冻结原工作目录当时的全部未提交源码；本轮实现与该提交比较。原 `E:/projects/MarketForge` 未被本轮修改，其他 worktree 未合并。

基线服务二进制来自此前同一份源码的 Release 构建。119 个 core/server/scenario 源文件与冻结基线逐项相同；来源及哈希保存在 `.local/native-bot-delivery/baseline-provenance.json`。本轮不改变订单撮合、账户协调、日志协议、Pine 回放或进程插件协议。

## 实现与语义边界

- 原生工厂显式选择批次调度，每个 blocking task 最多计算 32 个 bot。外部工厂默认关闭，每个插件仍独立执行。每个 bot 仍最多一个在途决策，批内每个 bot 单独捕获错误和 panic。
- 时钟任务冻结同一份房间状态后派发工作；观察构造和元数据读取移到 blocking worker。相同冻结 wave、相同 instrument 的公共观察通过 `Arc` 和 `OnceLock` 复用。账户、订单、保护、风险、自己的成交与费用仍单独查询。
- 10 种 market bot 通过借用视图读取公共行情，避免逐 bot 深拷贝订单簿、公开成交和事件。旧内置策略及插件保留完整 `ParticipantObservation`，序列化格式不变。
- 没有按 `next_decision_ms` 跳过调用。每次投递仍先执行原来的价格/波动率更新、TTL、风险退出与成交进度，再判断策略是否到期。交易动作和完整 JSON 状态仍照常产生。
- 完成结果按最多 64 个 bot 分批提交；缓存结果与到期时钟轮流获得优先权。停止、暂停、配置/epoch 拦截、精确提交状态匹配、普通 gateway 的实时风控与成交反馈均保留。
- 公共观察的缓存只活在该冻结 wave 内。历史索引/蜡烛仍按 worker batch 生成，没有引入跨 wave 历史缓存或更改截断规则。

## 正确性验证

`cargo test -p exchange-core --lib`：315 通过、11 个诊断基准忽略。`cargo test -p exchange-server --lib`：217 通过、2 个诊断基准忽略。`cargo clippy -p exchange-core -p exchange-server --all-targets -- -D warnings` 通过。

新增对照覆盖：15 个内置 bot 的 32 轮真实 gateway 交易，新旧观察、动作、完整状态逐步相同；现有 TTL、风险退出、部分成交、POV、对冲等策略断言同时检查新旧投递；并发观察验证私有账户隔离、4096 笔成交边界、冻结观察与新 wave 的新成交；原生批次中的错误/panic 隔离、200 bot 的 64 条提交上限、结果缓冲与时钟公平性。

进程插件测试初次因 Windows 的 `python3` alias 返回 9009 而失败；使用原仓库 `.venv/Scripts` 的 PATH 重跑后全部通过。没有为通过测试修改插件实现。PostgreSQL 集成测试未配置数据库，本报告的恢复和负载证据使用内存日志。

## 固定工作量

Release 同一测试二进制中的 owned 参照还原旧的逐 bot 观察构造和任务派发，native 模式调用实际生产批次派发函数。两者都经过同一普通 gateway 与日志提交；固定顺序只用于让实际工作和结果可以严格比较。

1000 bot，seed 7，64 个市场时钟步；顺序为旧/新/新/旧，重复两轮共 8 次。每次均为 64,000 次决策、20,409 个动作、43,952 次空动作决策，最终市场、账户、订单、bot 状态和全部 execution receipt 的 SHA-256 均为 `e16c444ca79b5375c4b3b02e3799dc245d6fae0ac92f6e8f280dde2233da0f6a`。只排序快照中的无序 `seen_order_ids` 集合，不删除状态字段。

| 中位数 | 旧投递 | 原生批次 | 变化 |
| --- | ---: | ---: | ---: |
| 观察准备、派发、计算与收集 | 918.86 ms | 469.08 ms | -48.95% |
| 整轮（包括正常提交） | 11,826.78 ms | 11,323.93 ms | -4.25% |
| blocking task 派发次数 | 64,000 | 2,048 | -96.8% |

整轮旧路径范围 10.97–13.61 秒，新路径 10.17–13.65 秒，存在波动与重叠；不能把局部约 49% 的下降解释为整机吞吐提升 49%。公共投递以外的撮合、账户协调、状态快照和日志提交仍占多数成本。最终 receipt 序列化排除在上述计时之外；脚本附带的进程 CPU/峰值内存采样包含它，不用于声称 bot 分配量改善。

原始报告：`.local/native-bot-delivery/fixed-1000/summary.json`，每次完整 receipt 保存在同目录。

复现：

```powershell
cargo test -p exchange-server --release --lib fixed_native_delivery_workload --no-run
python scripts/benchmark_native_delivery.py --test-binary target/release/deps/exchange_server-<hash>.exe --recipe .local/native-bot-delivery/fixed-1000.json --output .local/native-bot-delivery/fixed-rerun --steps 64 --rounds 2
```

## 实时服务

独立端口、有限账户、冻结 recipe、seed 7、内存日志；每组按旧/新/新/旧运行。500 与 1000 bot 使用 25ms 墙钟步长，1000 bot 另测 1000ms 步长。全部构建结束后串行运行，同时保留 HTTP 读写探针；这是同 recipe 的异步负载，完成顺序与交易轨迹可能不同，不是固定动作数量对照。

下表是每个版本两次运行指标的中位数；HTTP 栏是各次 P95 的中位数，不是合并样本的 P95。CPU 单位为平均占用的逻辑核，内存为整个服务进程的峰值 MiB。

| 配置 | 版本 | 步/秒 | CPU 核 | 下单/撤单 P95 ms | 峰值 MiB | 最终观察滞后 P95（模拟 ms） |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 500 bot，25ms | 旧 | 39.78 | 2.28 | 22.50 / 34.18 | 885 | 2,000 |
| 500 bot，25ms | 新 | 39.97 | 0.73 | 24.27 / 29.55 | 960 | 500 |
| 1000 bot，25ms | 旧 | 37.60 | 1.56 | 35.86 / 43.17 | 867 | 10,000 |
| 1000 bot，25ms | 新 | 37.70 | 1.23 | 41.91 / 42.56 | 1,265 | 8,500 |
| 1000 bot，1000ms | 旧 | 0.958 | 0.364 | 63.55 / 53.68 | 199 | 500 |
| 1000 bot，1000ms | 新 | 0.961 | 0.180 | 32.34 / 30.71 | 197 | 500 |

12 次运行全部无 HTTP/服务错误、无 bot 错误，所有 bot 都有最终观察，没有未来时间观察。500 bot 加速组两版本都满足该脚本的配置步频门槛；1000 bot 加速组两版本均只有一次满足，不能声明稳定 40 步/秒。正常 1× 组两版本均满足门槛。观察滞后来自暂停后的最终快照，不能当作持续运行时的墙钟延迟 SLO。

实时组的交易和工作量随异步完成顺序变化。1000 bot 加速组，旧的两个峰值为 709/1025 MiB，新的为 1732/798 MiB；下单 P95 旧为 37.21/34.51 ms，新为 28.97/54.84 ms，区间明显重叠。CPU 中位数下降与单次 HTTP P95 改善不能据此推导稳定吞吐提高。

内存权衡需保留：1000 bot 加速组新版本的峰值中位数增加约 46%，500 bot 加速组增加约 8%。同时累计日志操作中位数分别从 5,443.5 增至 9,564、从 6,629.5 增至 9,015；这支持“更频繁交付/提交带来更多内存日志保留”的解释，但不是内存分配归因或泄漏排查。相同固定工作量的进程峰值仅相差约 -0.75%，正常 1× 组接近。日志保留、逐次 bot 状态快照与长期容量仍未解决，本轮没有靠少执行 TTL/风控/成交进度来压低成本。

数值、全部单次运行、错误、观察滞后及二进制哈希汇总见 [2026-10-11-native-bot-delivery.json](2026-10-11-native-bot-delivery.json)；原始报告与日志位于 `.local/native-bot-delivery/live/`。基线服务 SHA-256 为 `87d2517b4f97205ccdbcb611901fded829cbc1144ae669ec38d1bb8a33037d4e`，新服务为 `ba05ea7d5b9b85419e76e87390b17ee5d55e43e72591ad145e09ee1cdd21ebb0`。短时间有限资金合成负载不代表长期容量或数据库写入性能。
