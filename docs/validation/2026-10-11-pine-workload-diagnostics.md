# Pine 实际负载诊断（2026-10-11）

本轮增加实际周期/输入分布和诊断构建，定位接入路径耗时；没有改变订单、账户反馈或策略执行时序。普通构建不包含这些计时。短时场景支持按负载判断容量，不能推导任意社区脚本或长历史实盘容量。

## 配方与实际覆盖

- `short` 保留原 10/15/30 秒周期、默认参数，理论平均 66.7 bar 更新/仿真秒。
- `diverse-short` 保留同样周期，参数按固定种子打散；1000 实例共有 692 种输入组合。
- `mixed-horizon` 假设 60% 一分钟、30% 五分钟、10% 一小时，理论平均 11.028 bar 更新/仿真秒。
- `slow-horizon` 假设 50% 五分钟、50% 一小时，理论平均 1.806 bar 更新/仿真秒；本轮仅导出/检查配方，未跑容量测试。
- 参数差异不改变交易所共同收线时点。混合周期测试先将空仿真时钟推进到 3590 秒，再启动实际 bot 和真实成交 feed，跨过共同整点；所有周期组都实际收到收盘 bar。它仅验证短历史下的共同收线，不能当作已运行一小时。

所有实测均为 1000 个 Pine、固定 38 个原生流动性 bot，另外保留 HTTP 探测和有限资金成交账户，使用三份原有增量资格源码。资金/深度初始化缩放，feed 通过真实订单网关产生非空 K 线，未直接注入行情。使用冻结源码和独立诊断 release 产物。

本任务的编译/回归先结束，再串行启动最终三场景。短周期参数打散场景的 compiler/其他 benchmark server 采样均为零。混合场景仍记录到其他任务的 `cargo test --workspace` 活动，因此只保留诊断观察，拒收其容量证据。`compiler_quiet` 包含 cargo 进程活动；它不证明 cargo 在每个采样点实际编译，也不证明整台机器饱和。

短历史不足以预热大部分指标，尤其混合周期组只有很少 bar，可能没有 Pine 交易信号。实际 Pine 订单/成交覆盖单独保存在 JSON 的 `pine_execution_activity`；feed 和原生 bot 的成交不能替代这项证据。

## 串行试跑与证据等级

| 场景 | 证据等级 | 测量秒 | 市场步/秒 | 进程树平均逻辑核 | 峰值 MiB | 最终积压 P95/最大 bar | 宿主检查/worker 请求次数 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| mixed-real-time-final | 受外部测试干扰，仅诊断 | 30 | 0.971 | 0.328 | 297.6 | 0/0 | 35658/2000 |
| diverse-short-real-time-clean | 有效短时证据 | 60 | 0.987 | 0.510 | 411.8 | 0/0 | 65266/5042 |
| mixed-accelerated-final | 受外部测试干扰，仅诊断 | 30 | 30.741 | 3.892 | 704.8 | 1/1 | 220714/12775 |

1x 场景目标为 1 步/秒，25ms tick 场景目标为 40 步/秒；严格目标和原有 95% 判定分别保留在 JSON。初始化成功、无错误、无积压与市场推进目标分别记录。三个场景测量时间、初始时钟和异步成交轨迹不同，不作为固定工作量的加速比。

## 新诊断覆盖与解释

`bench-execution-timing` 构建记录外部 bot 状态匹配/复制、blocking 队列等待、行情构造/补齐、事件指纹检查、实际决策，以及 worker 槽位等待/冷启动、输入差量编码、进程往返和响应解析。每段保留次数、累计墙钟时间、均值、最大值和二次幂纳秒桶的 P95 上界。进程树采样另保留每个 PID 的 CPU/内存；累计阶段包含嵌套和等待，不可相加当成 CPU 总量。

worker 往返包含 Python 解码、broker、Pine 和响应编码，不能称为解释器本体耗时。bot turn 不包含等待全局派发名额和 durable commit，不能称为完整决策 P95。数据包含暖机和冷启动；全量、暖追加与冷重建尚未分别输出独立分位数。

实际检查次数明显多于 worker 调用次数，确认了低频策略仍有宿主检查成本。下一步应先设计带版本的行情/账户事件筛选，在构造完整观察前判定无变化，同时让历史修正、截断、账户/订单反馈、宿主回执和定时恢复事件继续唤醒。随后测长历史的复制/编码成本，再决定共享不可变数据、响应差量或 worker 扩容。

增量仍只覆盖 6 份精确源码；普通社区脚本全历史重放、完整计算 checkpoint 和默认 512/最大 2048 bar 的停止限制仍待解决。它们需要单独验收，不能用低频短时测试替代。

## 验证与复现

诊断构建 release 和 clippy、普通构建 cargo check 均通过；调度测试 23 通过/1 原有忽略；worker 服务预算/重建测试、耗时直方图尾部测试各 1 通过；Python session/transport 回归 9 通过。本轮没有重跑全库解释器或全部 HTTP 成交恢复资格，复用范围仅为原有语义和本轮调度诊断回归。

```powershell
cargo build --release --locked -p exchange-server --features bench-execution-timing
.venv/Scripts/python.exe scripts/benchmark_strategy_runtime.py --server target/release/exchange-server.exe --population pine --pine-profile mixed-horizon --sustained-input --start-step 3590 --require-diagnostics --counts 1000 --seconds 60 --interval-ms 1000 --output .local/pine-mixed-repeat
```

源码/产物摘要、分段数据、qualification 字段、失败回执和原始文件路径见 [JSON](2026-10-11-pine-workload-diagnostics.json)。源码冻结、产物与原始日志保存在 `.local/pine-workload-diagnostics-20261011`。早期编码/测试解释器环境错误和重叠试跑均保留并排除；容量证据必须同时满足无编译活动和无其他 benchmark server，不能把受干扰场景判成通过。本轮未执行 commit/push，也未改动其他任务的暂存区。
