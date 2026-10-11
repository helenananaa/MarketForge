# PostgreSQL 下单尾延迟与同步写库复测（2026-10-11）

本轮消除了同负载复测中的下单尾延迟回退。相对上一版异步投影，500 bot 下单 P95 下降 92.8%，1000 bot 下降 96.0%；固定轨迹总耗时下降 66.8%。主日志、幂等和快照仍同步提交。该结论覆盖本机 PostgreSQL 18.4、下述负载与测试范围，尚未部署或提交。

## 定位与实现

分段诊断确认，慢下单主要等候房间执行通道；一个约 538 ms 的请求，约 533 ms 是房间等待，其自身 journal 约 2.18 ms。同步全量快照会占用执行通道数百毫秒，PostgreSQL JSONB 构建及 TOAST 压缩是其中的大开销。诊断只记录超过阈值的慢操作，包含预热及启动，不能当作整体请求分位数。重复投影数字验证耗时较小，保留原验证。

本轮使用三个改动：

- 新快照把同一份完整、整数边界已校验的 UTF-8 JSON 写为 BYTEA，避免同步构建 PostgreSQL JSONB 树。旧 JSONB 快照仍可恢复，迁移不重写历史行；正常快照 upsert 可替换同一 checkpoint key 的编码。恢复继续走原 Value/actor 解码和游标校验。
- mutation JSONB 参数直接使用原 recovery formatter 编码的 JSON 字节，避免中间完整 Value 树。
- 每连接缓存最多 128 条 prepared statement；有界清理缓存。支持 LZ4 的 PostgreSQL 为未来快照/mutation 使用 LZ4 TOAST，不支持时保留原压缩方法。

保持同一同步事务中的 canonical journal、幂等键、quota、writer fencing、订单/成交身份约束、快照和 recovery head；事务成功后才安装并发布状态。未改变快照频率、完整历史、恢复顺序和 fsync 等持久性设置。七张查询表继续按上一版的持久队列后台批量更新，读取保留有限集合的 read-after-write barrier。

迁移 19 新增 actor_bytes，数据库约束 actor_json/actor_bytes 恰好一个非空。物理快照格式有变化，旧二进制不能读取新 BYTEA 行；旧新写入进程混用和回退旧二进制不受支持。升级应停旧写入者再启新写入者。本轮只使用私有测试数据库。

实现说明：[ASYNC_QUERY_PROJECTIONS.md](E:/CodexData/worktrees/postgres-write-profile/MarketForge/docs/ASYNC_QUERY_PROJECTIONS.md)。本轮相对上一版的独立补丁和 9 个编译输入哈希保存在 `.local/pg-tail-study/implementation.patch`、`implementation-manifest.json`，没有把其他既有修改计为本轮工作。

## 测量方法

使用原 `.local/pg-write-study/run_study.py` 负载、recipe、seed=7 和同一 keepalive SDK。三个冻结 release 二进制分别为原始同步查询表版、上一版异步投影和本轮优化版。正式测量关闭 write profiling；不同版本逐个运行，期间不编译。

固定轨迹每轮 4 个预热块、160 个测量块，共 1340 个请求、320 次成交；按 上一版→优化版→原始版→原始版→优化版→上一版 交错运行，每版两轮。六轮响应摘要、最终完整状态摘要及七张查询表业务行摘要全部一致（仅剔除 created_at/updated_at 数据库墙钟列）。

PostgreSQL 连续负载每轮预热 5 秒、测量 60 秒，外部下单/撤单和视图探针约每 0.5 秒一次。500 bot/25 ms、1000 bot/1000 ms 各版本两轮交错运行；另测 38 bot/1000 ms PostgreSQL 各版一轮，发现异常后在全新私有集群按 优化版→上一版→上一版→优化版 再测两轮。内存后端原计划500 bot/25 ms/60秒，上一版在59.7秒触发原有2 GiB守卫，计划完整矩阵没有全部通过；保留失败，不提高守卫，改为双方相同30秒窗口各一轮补充对照。目标分别是 40 和 1 step/s。自动模式并发决策的 journal 顺序并非 seed-only 确定；固定轨迹负责严格业务等价，连续负载负责性能对照。

表中两轮数据是每轮指标的中位数（例如每轮 P95 的中位数），不是将请求合并后再算 P95；每轮原始值列于后文。样本较少，不给置信区间或生产 SLA。负载沿用原配方，资金有限、38 个模板 bot 加噪声扩容，不能当作交易所平衡市场容量证明。

PostgreSQL 18.4，fsync=on、synchronous_commit=on、full_page_writes=on，启用 pg_stat_statements。journal 指标是 scheduler await（入队、编码、SQL、提交与唤醒）耗时，排除外部下单 journal，不能解释为纯磁盘 fsync 时间；SQL 指标也不包括客户端编码和提交全部开销。

每轮开测前等待无编译窗口，测量进程全程以 1 秒间隔记录 cargo/rustc/link 等编译进程与主机 CPU。5 个受编译干扰的轮次独立保留并剔除；最终 26 轮保留数据的编译样本数为 0。用户暂停其他编译后完成连续负载。主机仍非专用隔离机，保留窗口 CPU 平均 8.8%、最高 49.4%。这是同环境的新一轮对照，不能把绝对时间直接与前一日报告比较。

## 固定轨迹

| 版本 | 总耗时 s | 成交请求 P50 ms | 成交请求 P95 ms | 推时钟 P95 ms | 划转 P95 ms | 快照 SQL 平均 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 原始同步查询表版 | 8.166 | 5.037 | 6.987 | 36.148 | 35.968 | 3.084 |
| 上一版异步投影 | 8.608 | 3.421 | 5.996 | 35.541 | 34.383 | 3.345 |
| 本轮优化版 | 2.858 | 1.860 | 3.108 | 6.069 | 11.061 | 0.528 |

固定轨迹累计可归因 SQL WAL 字节中位数：上一版 16,637,935，本轮 16,760,738，约增加 0.74%。这是相同业务轨迹的成本对照。LZ4 微探针存储体积曾增加约 34%，但该探针仍用 JSONB，不能替代最终 BYTEA 格式的整体 WAL 结果。连续负载 WAL/execution 受进度及快照历史体积影响，尤其500 bot优化版跑得更远，不是相同历史体积对照；原始值保留在 JSON 中。

## 连续负载

| 后端 / bot / 间隔 | 版本 | 轮数 | step/s | 下单 P95 ms | 撤单 P95 ms | 视图 P95 ms | journal/墙钟 | 峰值内存 MiB |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| postgres / 500 / 25 ms | 原始同步查询表版 | 2 | 18.964 | 346.42 | 33.98 | 2.25 | 84.7% | 374.6 |
| postgres / 500 / 25 ms | 上一版异步投影 | 2 | 23.664 | 584.54 | 19.17 | 2.33 | 70.0% | 471.2 |
| postgres / 500 / 25 ms | 本轮优化版 | 2 | 36.620 | 41.94 | 12.18 | 1.82 | 46.7% | 274.6 |
| postgres / 1000 / 1000 ms | 原始同步查询表版 | 2 | 1.000 | 270.82 | 210.22 | 4.95 | 62.8% | 393.4 |
| postgres / 1000 / 1000 ms | 上一版异步投影 | 2 | 1.000 | 402.14 | 22.26 | 4.90 | 22.8% | 430.0 |
| postgres / 1000 / 1000 ms | 本轮优化版 | 2 | 1.000 | 15.93 | 17.49 | 5.48 | 9.7% | 223.1 |
| postgres / 38 / 1000 ms | 上一版异步投影 | 1 | 1.000 | 14.31 | 3.98 | 1.94 | 0.8% | 33.1 |
| postgres / 38 / 1000 ms | 本轮优化版 | 1 | 1.000 | 70.46 | 14.78 | 1.91 | 1.6% | 28.4 |
| memory / 500 / 25 ms | 上一版异步投影 | 1 | 38.659 | 20.83 | 10.23 | 3.40 | 13.3% | 889.1 |
| memory / 500 / 25 ms | 本轮优化版 | 1 | 39.423 | 14.37 | 22.00 | 3.28 | 12.8% | 1043.7 |

500 bot：本轮快照 SQL 均值 28.64 ms，上一版 110.90 ms；journal 墙钟占比从 70.0% 降到 46.7%。本轮未完全达到40 step/s，journal 仍是已有阶段计时中的最大项；剩余同步持久化成本仍有进一步优化空间，不能说数据库开销已经消失。

1000 bot：本轮快照 SQL 均值 12.84 ms，上一版 89.19 ms；journal 墙钟占比从 22.8% 降到 9.7%。本轮 scheduler apply 约 7.63 s，journal 约 5.81 s，apply 超过 journal。视图 P95 约 4.90→5.48 ms 的小幅回退保留报告，未为改善下单结果删除此项。

所有保留的 PostgreSQL 及30秒内存对照客户探针错误、bot 错误为零；60秒内存基线失败单列，不宣称通过。优化版 PostgreSQL 后台投影错误为零，500/1000 bot 队列采样最大分别为 50/45；各轮测量后 read barrier 完成，待处理数均为0。采样最大不是精确峰值，测量外 drain 没计入请求延迟或吞吐量。

38 bot 初始单轮上一版→优化版的下单P95为14.31→70.46 ms，撤单3.98→14.78 ms；快照SQL却从3.07降到0.87 ms。同窗口client backend WAL write_time累计约557→1948 ms，walwriter约93→587 ms。该集群此前连续运行大负载，后台checkpoint/autovacuum及共享磁盘活动可能影响低负载，暂不把原因确定为代码。原轮次保留。全新私有集群反序复测结果：

| 版本 | 轮数 | step/s | 下单P95 ms | 撤单P95 ms | 视图P95 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| 上一版异步投影 | 2 | 1.000 | 14.90 | 4.65 | 1.93 |
| 本轮优化版 | 2 | 1.000 | 13.91 | 4.64 | 1.65 |

新集群复测与初始单轮单独汇总，不挑删异常数据；低负载噪声及长时间内存增长仍需与主要下单尾延迟改善分开评价。

30秒内存附测仅一轮：下单P95约20.83→14.37 ms，撤单约10.23→22.00 ms。撤单尾延迟存在回退，且60秒基线触发内存守卫，不能宣称内存后端全面改善或长期内存有界。

每轮连续负载原始摘要：

| 轮次 | step/s | 下单样本数 | 下单 P95 ms | 主机 CPU 均值 |
| --- | ---: | ---: | ---: | ---: |
| live-0-current-500-postgres-try5 | 17.629 | 79 | 746.95 | 14.1% |
| live-1-after-500-postgres-try1 | 37.625 | 116 | 24.23 | 12.1% |
| live-2-original-500-postgres-try1 | 17.266 | 99 | 397.87 | 7.9% |
| live-3-original-500-postgres-try1 | 20.661 | 104 | 294.97 | 8.1% |
| live-4-after-500-postgres-try1 | 35.615 | 114 | 59.65 | 11.2% |
| live-5-current-500-postgres-try1 | 29.699 | 101 | 422.13 | 11.2% |
| live-6-original-1000-postgres-try1 | 1.000 | 96 | 397.55 | 7.1% |
| live-7-after-1000-postgres-try1 | 1.000 | 116 | 14.29 | 6.2% |
| live-8-current-1000-postgres-try1 | 1.000 | 108 | 331.01 | 5.9% |
| live-9-current-1000-postgres-try1 | 1.000 | 100 | 473.28 | 6.2% |
| live-10-after-1000-postgres-try1 | 1.000 | 115 | 17.57 | 5.6% |
| live-11-original-1000-postgres-try1 | 1.000 | 100 | 144.10 | 6.8% |
| live-12-current-38-postgres-try1 | 1.000 | 116 | 14.31 | 5.1% |
| live-13-after-38-postgres-try1 | 1.000 | 115 | 70.46 | 4.7% |
| followup-0-current-500-memory-try1 | 38.659 | 58 | 20.83 | 17.5% |
| followup-1-after-500-memory-try1 | 39.423 | 58 | 14.37 | 18.4% |
| followup-2-after-38-postgres-try1 | 1.000 | 115 | 14.38 | 6.8% |
| followup-3-current-38-postgres-try1 | 1.000 | 115 | 14.81 | 6.2% |
| followup-4-current-38-postgres-try1 | 1.000 | 114 | 14.99 | 6.4% |
| followup-5-after-38-postgres-try1 | 1.000 | 115 | 13.45 | 6.1% |

## 幂等、恢复和质量验证

- `cargo test --locked --workspace`：544 passed、12 ignored、0 failed。Windows Python PATH 使用仓库 .venv；此前环境失败日志保留，最终完整重跑通过。
- `cargo clippy --locked --workspace --all-targets -- -D warnings`、format 和 git diff whitespace 检查通过。
- 真实 PostgreSQL 选定集成验证：8 passed，其中7项真实数据库测试及1项相关路径测试。覆盖快照/尾日志/历史、控制幂等、阻塞投影时 canonical 恢复、配额、身份冲突回滚、投影失败重试、晚提交队列、缓存事务回滚与淘汰。新增编码边界测试验证 JSONB wire prefix、精确整数/转义及越界拒绝。BYTEA 新格式及历史 JSONB 恢复一致。
- 最终 release 进程强杀、数据库重启、持续阻塞投影：完整业务状态一致，4次同幂等键重试返回相同结果且无新增执行；积压 6→0，订单/事件/成交/账本/持仓投影结果一致。
- 最终1000 bot压测数据库再次恢复：最大完整 JSON 快照 14,955,407 字节；恢复的时钟和 scheduler 与压测暂停后的保存结果一致。强杀服务并重启数据库后，1000账户观察及完整恢复状态再次一致。这个检查不等同于对连续负载全部账户建立了崩溃前独立账本 oracle；严格前后完整业务等价另由固定轨迹和小场景强杀验证覆盖。

真实 PostgreSQL 的既有 control-idempotency fixture 在 teardown 出现一次 Tokio timer shutdown 的后台线程 panic，断言与测试进程仍通过；该日志未隐藏。独立最终 release 强杀/恢复验证通过。12项忽略测试未宣称验证。本轮没有生产部署、多机故障、长期稳定性或混合版本滚动升级资格证明。

## 证据与复现

完整结果：[2026-10-11-postgres-tail-results.json](E:/CodexData/worktrees/postgres-write-profile/MarketForge/docs/validation/2026-10-11-postgres-tail-results.json)。工作目录 `E:/CodexData/worktrees/postgres-write-profile/MarketForge`，基础 HEAD `71b58e9f9de3a9aede2521af9729d844d8913ace`，包含隔离时复制的既有未提交更改；本轮9文件输入另有哈希清单，未修改其他任务工作区。

冻结二进制与 SHA256：

```
original 2dad39e126bcb4cf5ff651a0362cd9751acd52774f50217c19e528f731745bb7
current  80882840c89a8b66452da88ae780a049e8deca4a87890143ec4bdefe5d4a0f69
after    ecc2a88af26038e665c4df89b3cd776039b118a40e806697db0f8d4aa2e1abd8
```

本轮复现入口：`.local/pg-tail-study/paired_tail.py`；安静窗口监测 `.local/pg-tail-study/quiet_host.py`；负载函数 `.local/pg-write-study/run_study.py`。入口包含恢复已完成结果逻辑；全新复测须改为新的专用输出目录，保留旧证据，勿覆盖运行中的私有集群。编译输入及二进制冻结后再运行负载。

原始数据全部保留在 `.local/pg-tail-study/`：`paired/report.json`、各轮 `samples.json`/`report.json`/`server.log`、`paired/excluded.json`、`paired/host-monitor.json`、`diagnostic-profile-summary.json`、`workspace-wire-tests-final.log`、`postgres-wire-tests.log`、`clippy-wire.log`、`crash-recovery-receipt.json`、`large-recovery-receipt.json`。早期受编译干扰导致的 harness failure 及内存守卫失败也保留。最终 report 的 passed=true 表示保留的目标/补充轮次通过，postgres_target_passed=true；planned_full_suite_passed=false及ancillary_failure明确原60秒内存对照没有通过。

本轮测试服务和私有 PostgreSQL 集群均已停止，数据与诊断证据保留。未停止其他项目进程，未提交、推送或部署。
