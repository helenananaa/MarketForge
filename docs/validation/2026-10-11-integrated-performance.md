# MarketForge 整合后的新性能测试

2026-10-11，在本机重新编译并串行测量。对照为共同接口依赖提交 `c5e3c0a`，候选为整合提交 `8f99502`。这是本轮实际结果；旧 worktree 报告没有作为新结果复用。

测试覆盖风控、账户视图、房间并发、原生 bot 投递、日志内存、PostgreSQL 固定写入与恢复，以及 1000 bot / 25 ms 的真实异步调度。完整指标、范围、原始文件路径和 SHA256 见同名 JSON。

本轮结论：多 venue 并发吞吐增加约 84%–105%，PG 固定轨迹吞吐约为基线的 4.15 倍；真实 1000 bot 的 Memory 中位数为 33.37→35.55 step/s，PG 为 5.28→26.01 step/s。八次正式实载回执全部有效，但整合版两种后端均未达 40 step/s。单簿撤单路径的尾延迟上升及短窗口测量边界保留在下文。

## 环境与输入

Windows 11 26200，i9-13900HX，24 核/32 逻辑处理器，约 64 GiB RAM，平衡电源计划。OBS、Codex、ChatGPT 等后台程序保留运行；本机不是专用空载测量机。Rust/Cargo 版本、编译日志和各二进制哈希均已保存。

两个版本使用独立 Cargo target；正式 server 为默认 feature 的 Release。原生吞吐使用不带分配诊断的 Release test binary；内存归因另用 `memory-diagnostics` binary，其耗时不代表生产吞吐。首次共用 target 引起的失败构建已保留并弃用，所有被测程序都由隔离 target 重新构建。

原生固定输入为 1000 bot、seed 7、64 step，recipe SHA256 `b8a793856936feda4b2823612eb138e1f4b21021611a49cbb27ca0ec9021f019`。PG/live 使用原 38 bot 配方；live 同一驱动扩展为 38 mixed + 962 noise、seed 7，recipe template SHA256 `3afd8e33c247ab67fe112e8936e9f6d7ac4fa409e0c21bfcc0dc988135c3c205`。真实调度保持单 venue；多 venue 并行度由另一组 HTTP 房间实验衡量。

## 固定风控与视图

12 个风控场景按 ABBA × 2，共 96 次运行；完整 canonical 状态、按顺序的执行/历史回执和业务指标跨版本一致。表中时间为固定工作总耗时中位数，排除预热、恢复 fixture 和结果序列化。

| 风控场景 | 前 ms | 后 ms | 前/后倍数 | 耗时下降 |
|---|---:|---:|---:|---:|
| sparse-1k | 443.000 | 25.520 | 17.36× | 94.24% |
| sparse-10k | 6379.746 | 35.797 | 178.22× | 99.44% |
| dense-1k | 481.027 | 426.610 | 1.13× | 11.31% |
| sparse-8markets | 551.712 | 38.383 | 14.37× | 93.04% |
| dense-8markets | 1271.765 | 1126.432 | 1.13× | 11.43% |
| hedge-sparse | 327.512 | 13.195 | 24.82× | 95.97% |
| same-price | 255.391 | 6.468 | 39.49× | 97.47% |
| orders | 130.076 | 24.738 | 5.26× | 80.98% |
| clock | 74.119 | 4.104 | 18.06× | 94.46% |
| liquidation | 174.524 | 88.536 | 1.97× | 49.27% |
| threshold-sparse | 282.149 | 115.666 | 2.44× | 59.01% |
| threshold-dense | 274.731 | 173.939 | 1.58× | 36.69% |

账户视图：固定 10,000 账户状态、每轮 1000 次、ABBA × 2 共 8 次运行；响应 3,399,114 bytes，完整字节与哈希一致。

| 每 1000 次操作 | 前 ms | 后 ms | 耗时下降 |
|---|---:|---:|---:|
| 构造 | 1520.15 | 1399.24 | 7.95% |
| 序列化 | 5021.84 | 4607.59 | 8.25% |

以上是固定路径测量；风控倍数与无网络的视图耗时不能直接当作整个服务的提升。

## HTTP 房间并发

Spot/Perp × rest_cancel/roundtrip × 单簿/四 venue × 并发 1/8，共 16 配方，每个 ABBA4。首轮 64 次全部有效，393,216 条正式命令、131,072 笔实际成交，零请求错误、零编译重叠。每配方 2048 正式 cycles，每 worker 另有 2 次预热。

| 市场 / 工作量 / 拓扑 / 并发 | 前 req/s | 后 req/s | 吞吐变化 | P95 ms 前→后 | P99 ms 前→后 |
|---|---:|---:|---:|---:|---:|
| Perp / rest_cancel / four_venues / 1 | 4386.2 | 4872.2 | +11.08% | 0.3052→0.2820 | 0.4384→0.4213 |
| Perp / rest_cancel / four_venues / 8 | 7662.7 | 15066.0 | +96.61% | 1.3881→0.8792 | 1.6658→1.4074 |
| Perp / rest_cancel / one_book / 1 | 4114.2 | 4330.5 | +5.26% | 0.3254→0.3076 | 0.4728→0.4379 |
| Perp / rest_cancel / one_book / 8 | 9183.7 | 10035.4 | +9.27% | 1.1566→1.2350 | 1.4183→1.7274 |
| Perp / roundtrip / four_venues / 1 | 3451.7 | 3492.9 | +1.19% | 0.3772→0.3687 | 0.8059→0.6858 |
| Perp / roundtrip / four_venues / 8 | 5542.3 | 10725.9 | +93.53% | 2.7425→2.0220 | 4.4725→3.5743 |
| Perp / roundtrip / one_book / 1 | 3710.4 | 3630.5 | -2.15% | 0.3526→0.3574 | 0.7677→0.7088 |
| Perp / roundtrip / one_book / 8 | 6081.0 | 6650.9 | +9.37% | 2.5260→2.3317 | 4.1576→3.9417 |
| Spot / rest_cancel / four_venues / 1 | 4335.7 | 5129.8 | +18.32% | 0.3068→0.2710 | 0.4278→0.3847 |
| Spot / rest_cancel / four_venues / 8 | 8905.6 | 18230.1 | +104.70% | 1.1884→0.7298 | 1.4387→1.0167 |
| Spot / rest_cancel / one_book / 1 | 4572.4 | 4461.8 | -2.42% | 0.2969→0.3172 | 0.4350→0.4617 |
| Spot / rest_cancel / one_book / 8 | 9562.2 | 11642.5 | +21.76% | 1.0953→0.9951 | 1.3705→1.1963 |
| Spot / roundtrip / four_venues / 1 | 3864.9 | 3794.3 | -1.83% | 0.3384→0.3513 | 0.6204→0.6581 |
| Spot / roundtrip / four_venues / 8 | 6766.7 | 12443.9 | +83.90% | 2.1991→1.6810 | 3.5383→2.8857 |
| Spot / roundtrip / one_book / 1 | 3808.9 | 4115.6 | +8.05% | 0.3434→0.3155 | 0.5890→0.5866 |
| Spot / roundtrip / one_book / 8 | 7631.7 | 8751.5 | +14.67% | 1.9257→1.7868 | 3.0196→2.8979 |

表中吞吐为正式 accepted command rate；两版同跑 book 读探针，读探针不计入该吞吐。P95/P99 为每轮 nearest-rank 分位数，再取运行中位数，不混池请求。资产守恒、释放资产预留、订单/命令身份和最终空簿全部验证；撤单路径还验证逐账户恢复。成交实验不声称每个账户或 Perp position/checkpoint 字节等价。四 venue 同时改变锁分布及状态/历史分布，是闭环 HTTP 工作量，不能单独解释为纯锁消融或最大到达率容量。

四 venue、并发 8 的四组吞吐增加 83.90%–104.70%，前后运行范围均不重叠；服务 CPU 并行度增加，进程峰值内存中位数约增加 3.26–4.88 MiB。单客户端三组吞吐下降 1.83%–2.42%，前后范围有重叠，结果保留。

Perp / rest_cancel / one_book / c8 追加同输入 ABBA4，另有 16,384 条正式命令、零错误。首轮吞吐 +9.27%、P95 +6.78%、P99 +21.79%；追加吞吐 +10.79%、P95 +2.69%、P99 +1.11%。吞吐收益重复出现，大幅尾部回退未重复；两轮 P95/P99 中位数均上升，首轮 P99 升幅明显。首轮和追加独立列出；没有用追加结果覆盖首轮。后台活动与异常同时出现，不能建立因果归因。

## 原生 bot 与日志内存

原生投递按 owned/native ABBA × 2 共 8 次，比较同一个整合后的 plain Release binary。每次 64,000 决策、20,409 动作、43,952 空动作，完整状态/执行回执一致；native 派发数由 64,000 降到 2,048。

| 固定工作中位数 | owned | native | 下降 |
|---|---:|---:|---:|
| 完整工作耗时 ms | 11549.59 | 9814.73 | 15.02% |
| 投递耗时 ms | 1346.49 | 728.08 | 45.93% |

elapsed/delivery 排除最终完整回执生成；进程 CPU/RSS 峰值包含回执生成。完整工作耗时的前后范围重叠；投递耗时的前后范围本轮不重叠。此处是中位数观察及同 binary 投递模式对照，不是 c5→完整整合版端到端归因。

内存诊断另跑 raw/compressed ABBA × 1，共 4 次，全部相同工作量/回执。raw 已为紧凑 payload，不使用旧 spare-capacity 对照。

| 回执生成前的 requested heap / 日志 | tight raw | compressed |
|---|---:|---:|
| 存活有效堆 MiB | 243.742 | 143.326 |
| 峰值有效堆 MiB | 265.803 | 170.419 |
| mutations | 1090 | 1090 |
| logical JSON bytes | 111,701,192 | 111,701,192 |
| stored payload bytes | 111,701,192 | 6,406,297 |

requested live 堆下降 41.20%，requested peak 下降 35.89%；payload 为 logical bytes 的 5.735%。这些值排除 allocator slack、线程栈和 tracer header，且测于回执生成前；它们不是生产 RSS，也不证明长期内存有界。分配诊断的耗时不用于生产吞吐结论。

## PostgreSQL 固定轨迹与恢复

本任务专用 PostgreSQL 端口 55440，每版每轮独立新数据库，ABBA4；fsync、synchronous_commit、full_page_writes 全部 on，预加载 pg_stat_statements。每轮 35 个预热请求，1340 个正式请求、320 笔正式成交；写入 profile 关闭，投影 drain 在计时窗外。

| 正式工作中位数 | 前 | 后 |
|---|---:|---:|
| 总耗时 s | 13.232 | 3.197 |
| req/s | 101.27 | 420.66 |
| 服务 CPU s | 4.336 | 1.898 |
| 进程 lifetime peak RSS MiB | 32.225 | 21.635 |
| HTTP overall p50_ms | 4.961 | 1.941 |
| HTTP overall p95_ms | 45.232 | 5.179 |
| HTTP overall p99_ms | 66.791 | 7.081 |

每轮分位数先计算 nearest rank，再取前后各两轮的中位数；不是把所有请求混池计算的分位数。1,340 条正式请求含下单、成交、撤单、改价、风控拒绝、转账和 clock。吞吐 +315.38%、耗时 −75.84%；CPU 是同一正式窗，RSS peak 包含进程先前启动/预热。

可归因 SQL WAL 从 15,752,082 降到 14,283,190 bytes（−9.33%）；这是 pg_stat_statements 查询窗口 delta 之和，包含完成投影排空，统计窗不等同于 drain 前结束的 HTTP/CPU 正式计时窗。它不能当作整个 PostgreSQL WAL 文件增长或纯 fsync 时间。

四轮 HTTP 响应摘要、最终外部观测（scheduler、clock、全部 38 个配方账户 observe）和七表业务行严格一致，表为 orders/order_events/trades/market_ticks/account_ledger/position_snapshots/transfers，仅排除 created_at/updated_at。候选投影错误计数为零；含预热的查询表计数每轮相同，trades=328、ledger=656、transfers=42。每轮用同版 binary 恢复两次：首次与固定 final 对齐，再强杀已排空投影的 idle 服务后恢复，上述观测/表摘要一致。这不等于内部 MarketState 或 checkpoint 字节等价。PG 进程未在恢复实验中崩溃或重启，未验证在途写入强杀。旧 binary 没有读取新 BYTEA/compressed checkpoint。

## 真实 1000 bot / 25 ms 异步调度

Memory 和 PostgreSQL 各 ABBA4，每轮预热 5 秒、正式窗请求 30 秒；全量 scheduler 观察探针每 2 秒，另有 HTTP place/cancel/view 和 clock 探针。相同观察成本保留在两版测量中，资源守卫均为 3072 MiB。实际窗口/资源、完整成功与失败检查、CPU/内存、观察时钟 bracket 及投影队列见以下原始聚合。

资格严格检查 literal 40 step/s，与旧的 95% 配置速率门槛分开；还检查请求 P95、bot 错误、观察覆盖/未来观察、journal/scheduler 错误和 PG 耐久性/投影清空。缺失 counter 为 unavailable，不能当作零。live 为异步轨迹，不要求前后市场 hash 相等。simulation lag 以模拟时钟计量，不是墙钟响应延迟。

八轮正式回执全部有效，无测试错误、bot 错误或未来观察，最终全部 1000 bot 均已观测。八轮均未达到 38 或 40 step/s；整合版下单/撤单 P95 均通过 250 ms 门槛，PG 基线另有下单/撤单 P95 超标。PG 候选两轮的采样投影队列峰值为 174/56，最终 barrier 后均清零，投影错误 delta 为零。进程峰值均低于 3072 MiB 守卫，但全部短窗 private memory 增量仍为正，未证明长期内存有界。

# Live ABBA retest

Eight planned 1000-bot ABBA cases; bounded asynchronous synthetic load with full scheduler probes in the measured window. Short windows do not establish long-run stability, bounded memory or production capacity.

All report-producing runs contribute, including invalid, failed-check and unqualified reports; no best-run selection. Median/min/max of per-run statistics are not pooled latency percentiles or SLOs. Null means unavailable, never zero. Each wrapper attempt is preserved separately.

Formal selection: {"memory": "memory-repaired", "postgres": "postgres-pgsafe"}. Explicit directory selection by the caller, never based on speed or qualification. Other attempt groups remain in a separate complete appendix with their raw measurements and wrapper failures.

Each backend group must be internally consistent. Cross-backend driver SHA equality is not required: PostgreSQL repairs adapt credential parsing/explicit child connection environment and nullable observation initialization. Nonnull observation fields/percentiles, clock brackets, live workload algorithm, configuration, windows and qualification targets are unchanged. Every earlier group uses its captured original hashes and matching saved original files when available, never repaired current script paths.

State: complete raw report set. Formal reports: 8 / 8 planned; formal wrapper failures: 0. Every planned case appears below.

| Run group | Run | step/s | Gap to 40 | Place P95/P99 ms | CPU cores | Peak MiB | Valid / qualified |
| --- | --- | ---: | ---: | --- | ---: | ---: | --- |
| memory-repaired | 01-baseline | 33.550 | 6.450 | 42.950/68.885 | 1.524 | 758.559 | yes / no |
| memory-repaired | 02-candidate | 37.526 | 2.474 | 44.778/57.887 | 1.347 | 260.691 | yes / no |
| memory-repaired | 03-candidate | 33.564 | 6.436 | 45.966/71.793 | 1.070 | 400.770 | yes / no |
| memory-repaired | 04-baseline | 33.184 | 6.816 | 50.132/91.310 | 1.475 | 716.926 | yes / no |
| postgres-pgsafe | 01-baseline | 4.792 | 35.208 | 553.261/651.665 | 0.396 | 230.477 | yes / no |
| postgres-pgsafe | 02-candidate | 26.505 | 13.495 | 156.153/255.224 | 0.842 | 194.602 | yes / no |
| postgres-pgsafe | 03-candidate | 25.520 | 14.480 | 129.859/192.635 | 0.821 | 177.566 | yes / no |
| postgres-pgsafe | 04-baseline | 5.764 | 34.236 | 525.770/690.946 | 0.422 | 224.008 | yes / no |

Group statistics use every available raw report, including invalid runs. They describe this short window; qualification is reported per run.

| Backend / variant | Reports / valid / qualified | step/s median [min,max] | Place P95 median [min,max] | Cancel P95 median [min,max] |
| --- | --- | --- | --- | --- |
| memory-repaired / baseline | 2 / 2 / 0 | 33.367 [33.184,33.550] | 46.541 [42.950,50.132] | 51.920 [43.819,60.021] |
| memory-repaired / candidate | 2 / 2 / 0 | 35.545 [33.564,37.526] | 45.372 [44.778,45.966] | 42.029 [37.626,46.433] |
| postgres-pgsafe / baseline | 2 / 2 / 0 | 5.278 [4.792,5.764] | 539.515 [525.770,553.261] | 353.722 [268.354,439.089] |
| postgres-pgsafe / candidate | 2 / 2 / 0 | 26.013 [25.520,26.505] | 143.006 [129.859,156.153] | 161.565 [141.225,181.905] |

| Run group | Captured wrapper SHA | Captured runner SHA | Internal provenance |
| --- | --- | --- | --- |
| memory-repaired | 120b56b52b7c | bd7dbe18f52f | consistent complete four-report group |
| postgres-pgsafe | a4d783a96455 | dee4b1badb37 | consistent complete four-report group |

Preserved earlier attempts are shown below. They remain outside the explicitly selected formal group because that group was rerun after a documented harness repair. A slower report is retained in this appendix; selection does not depend on its rate. The JSON includes separate all-report statistics for each earlier group.

| Earlier group | Run | Raw step/s | Gap to 40 | Valid / qualified | Wrapper failure |
| --- | --- | ---: | ---: | --- | --- |
| memory | 01-baseline | 33.554 | 6.446 | no / no | Enhanced runner failed with exit -1; its report is retained and remaining cases continue. |
| memory | 02-candidate | — | — | no / no | Clean driver revision changed |
| memory | 03-candidate | — | — | no / no | Clean driver revision changed |
| memory | 04-baseline | — | — | no / no | Clean driver revision changed |
| postgres | 01-baseline | — | — | no / no | Private SQL failed (exit 2): psql: error: connection to server at "localhost" (::1), port 5432 failed: fe_sendauth: no password supplied   |
| postgres | 02-candidate | — | — | no / no | Private SQL failed (exit 2): psql: error: connection to server at "localhost" (::1), port 5432 failed: fe_sendauth: no password supplied   |
| postgres | 03-candidate | — | — | no / no | Private SQL failed (exit 2): psql: error: connection to server at "localhost" (::1), port 5432 failed: fe_sendauth: no password supplied   |
| postgres | 04-baseline | — | — | no / no | Private SQL failed (exit 2): psql: error: connection to server at "localhost" (::1), port 5432 failed: fe_sendauth: no password supplied   |
| postgres-repaired | 01-baseline | 7.983 | 32.017 | no / no | — |
| postgres-repaired | 02-candidate | 28.746 | 11.254 | yes / no | — |
| postgres-repaired | 03-candidate | 28.162 | 11.838 | yes / no | — |
| postgres-repaired | 04-baseline | 5.317 | 34.683 | no / no | — |

Across formal and preserved groups: 13 raw reports, 20 wrapper attempts, 8 recorded wrapper failures.

The companion JSON includes all P99 statistics, private/current memory, simulation freshness, queue samples, unavailable counters, actual errors, source hashes and every wrapper failure. Live asynchronous receipts may differ between versions. Exact cross-version receipt equality belongs to the fixed-work suite.


## 收据与工作区

初次 live Memory 调度 wrapper 的局部 LASTEXITCODE 遮蔽了真实 native exit：实际首轮数据保留，后续三轮未启动。第一次 PG 连接预检把 URI 放入 PGDATABASE，libpq 没有展开该 URI，四轮均未开始负载。连接修复后的 PG 四轮里，两个基线均在预热阶段遇到 bot 尚未初始化、kind_state=null 的合法状态，旧观察辅助函数把它当字典而报错；计时窗口仍完整，但不把有采样错误的轮次当作正式有效对照。这三组初次尝试全部保留于附录，包括两次无采样错误的候选结果。

退出码改为 Process.ExitCode 并做对子脚本/参数引用的独立 smoke；PG 连接改为明确 host/port/user/password/database 子进程环境，并核对真实 task PG 端口；任务观察器将尚未初始化的合法空状态计为缺失观察，保持非空观察算法、模拟时钟 bracket、计时和市场负载不变，另做纯数据 null/malformed smoke。正式 Memory/PG 分别重新跑完整 ABBA，输出 memory-repaired/postgres-pgsafe。每个正式组内部的 wrapper、driver、业务代码和配方均冻结；不同 backend 组的测试适配源码各自存档，未依据速度筛选轮次。

全部测量使用冻结的干净 source。主目录原有 21 个 Pine/策略/Python/研究及文档 WIP 的状态和哈希保持不变。测试后主目录 Release 缓存重新构建并核对来源，排除初始共用 target 留下的基线程序；源码内容未因缓存恢复改变。只停止本任务端口 55440 的 PG，数据库、日志、原始回执及失败证据保留；其他运行中的 PG 未停止。测试阶段没有生产源码修复、提交或 push。

同名 JSON 记录最终源码/输入/原始 WIP 核验与各证据文件 SHA256。所有受限资格和回退均保留；固定路径收益、完整工作等价、短窗口真实吞吐分别解释。
