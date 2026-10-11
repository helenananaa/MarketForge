# Journal 等待、内存守卫与撤单尾延迟（2026-10-11）

已在同一 worktree 完成两阶段优化及同负载复测。最终候选降低了同步 journal 成本和内存保留量，幂等与恢复检查通过；尾延迟仍有波动及部分小幅回退，不能称全负载性能已收尾。所有结果均为本机有限窗口，没有提交或部署。

## 最终同负载结果

沿用原 run_study.py、recipe、seed=7、keepalive SDK，正式测量关闭 profiling。基线是上一轮 BYTEA JSON/prepared statement 版本（before.exe），最终候选包含本轮所有改动（after.exe）。表中两轮项取每轮 P95/指标的中位数，不是合并请求后重新算 P95。

| 负载 | 下单 P95 ms 前→后 | 撤单 P95 ms 前→后 | scheduler journal / wall 前→后 | 峰值 MiB 前→后 |
|---|---:|---:|---:|---:|
| postgres 500 bot / 60s | 37.71→35.65 | 9.11→5.69 | 51.07%→46.80% | 157.9→129.5 |
| postgres 1000 bot / 60s | 38.58→26.34 | 19.47→20.16 | 10.77%→10.16% | 212.9→193.3 |
| memory 500 bot / 30s | 8.31→6.77 | 8.03→4.23 | 10.19%→10.66% | 1162.0→257.3 |
| memory 500 bot / 60s | 7.79→10.17 | 3.70→11.16 | 10.40%→9.90% | 2092.5→458.4 |
| postgres 38 bot / 60s | 12.90→13.24 | 2.47→3.07 | 0.87%→0.71% | 28.4→26.9 |

固定 PostgreSQL 轨迹总耗时 2.5087→2.3090s，下降 7.96%。每轮 4 个预热块、160 个测量块，1340 请求、320 成交。PostgreSQL 前→后→后→前，各两轮；内存各一轮。六轮响应摘要和完整最终状态摘要一致，四轮 PostgreSQL 七张查询表业务摘要一致（仅剔除 created_at/updated_at 数据库墙钟列）。

500 bot PostgreSQL journal/wall 下降约 8.4%，1000 bot 下降约 5.7%。500 bot 下单收益较小，轮间波动大；1000 bot 撤单约 +0.69 ms，38 bot 撤单约 +0.61 ms，均作为回退保留。内存 30 秒双方完整成功窗口的撤单 8.03→4.23 ms；旧版内存 60 秒在 59.54 秒触发原有 2 GiB 守卫，新版完整 60.02 秒通过。该失败窗口不可当作完整成功的 60 秒尾延迟验收，新版该轮撤单 P95=11.16 ms 仍需关注。

各连续轮预热 5 秒。500/1000 PostgreSQL 各版两轮/60秒；500 内存各版两轮/30秒另加各版一轮/60秒；38 PostgreSQL 各版一轮/60秒。最终保留 22 轮，编译干扰排除 0 轮，保留轮次编译样本 0。启动前等待无常见编译进程，测量期间按秒记录构建进程和主机 CPU；出现干扰则排除并重测，没有停止其他任务。投影 drain 在窗口外，结束时 backlog=0。自动并发决策顺序不是 seed-only 确定，固定轨迹负责严格业务等价，连续轮负责性能。样本较少，背景负载与采样相位可能影响结果，不外推生产 SLA。

journal 比例是 scheduler await（编码、入队、SQL、提交和唤醒），不含外部下单自身 journal；不能当作纯磁盘 fsync。已有阶段计数不完全覆盖 publish/install/序列化间隙。PostgreSQL 18.4 的 fsync、synchronous_commit、full_page_writes 均开启。

## 定位与实现

内存诊断的旧版在 57.60 秒触发守卫，峰值 2106.63 MiB。16,600 条 mutation 的原始 JSON 总长 1,100,851,043 字节，Vec 容量却为 1,980,116,480 字节，多约 879 MB；execution vector 槽位约 31 MB。主要来源是完整 mutation 历史的内容和多余容量。新版诊断样本 15,800 条记录的原始总长 1,037,316,135 字节，实际存储 69,426,459 字节；自动轨迹不同，只用于容量归因，RSS 对照以表格为准。

1. 内存 journal 改为紧凑 Box slice，至少 1 KiB 且节省空间时用安全 LZ4 block 编解码，保留原始长度；小/不可压缩记录保留 raw 字节。完整 envelope、每条 mutation、sequence 和 replay 顺序不变，没有裁剪历史或提高守卫。无限增长的完整历史仍可能达到内存上限。
2. 没有 execution、transfer、snapshot、control-idempotency 和 status 变更的单行 mutation 使用一个 SQL statement 的隐式同步事务，减少 BEGIN/COMMIT 两次往返。驱动消费 ReadyForQuery 后才成功确认。多效果写入保留原显式原子事务。fenced CTE 在同一 statement 中校验并锁定有效 lease，再插入；等待锁时发生接管会拒绝旧 token。所有原校验保持提交前。
3. execution SQL 借用 coordinator 输入，投影数组直接构造，去掉重复 clone/to_value 分配。网关再验证、调度器和观察版本规则不变。
4. 大 PostgreSQL 快照在应用侧对完全相同的 checked JSON 做无损 LZ4 压缩，再传输到 actor_bytes；仅选择更小编码。16 字节零前缀版本 envelope 带原始长度。旧 raw JSON BYTEA、旧 JSONB 均可读取，解压后使用原 Value/actor、legacy-actor 和 recovery-head 校验。未知版本、截断、错误长度或解压错误均拒绝恢复。解码长度保留原 PostgreSQL 字段上限，见 [PostgreSQL 18 limits](https://www.postgresql.org/docs/18/limits.html)。无需新迁移或重写历史行。

快照完整历史、频率、事务边界、同步持久性设置及幂等/quota/身份/fencing 约束保持。查询表沿用持久队列后台批量投影和有限集合 read-after-write barrier。旧二进制不能读取新压缩快照；旧新 writer 混用和回退旧二进制不支持，升级需停旧 writer。此轮没有部署。

## 撤单长尾路径

最终 PG 1000 bot 诊断中，旧版一笔 53.97 ms 撤单，room_wait=50.55 ms、journal=1.37 ms；新版一笔 86.79 ms，room_wait=83.83 ms、journal=1.79 ms。另有新版 41.42 ms 撤单自身 journal=40.64 ms，关联同步快照。内存后端一笔 54.24 ms 撤单中 room_wait=52.52 ms、journal=0.149 ms；另一笔 19.13 ms 中 journal=11.80 ms、room_wait=6.03 ms。数据库提交成本下降和内存保留量下降，不能保证房间通道等待同步下降。

自动 bot ready 批次上限仍为 64，与外部请求共用 FIFO 房间执行通道，候选构建和同步提交占用该通道；同一通道上的长操作会让后续下单/撤单等待。同步完整快照的校验、编码和提交也是慢请求来源。本轮没有改变调度或快照节拍。诊断确认等待路径；前后分位数差异中，提交、bot 批次/快照相位和其他 CPU 成本的各自因果贡献尚未全部量化。

全量 profiling 的 PG 快照操作中位数 27.212→20.464 ms，新版 stored/json 比率中位数约 15.07%。这些诊断含预热/启动并有日志开销，只用于定位，不混入正式性能分位数。两轮 PG 和一轮内存追加诊断无错误或编译样本。

## 阶段1回退保留

阶段1已做单行快速提交、复制削减和内存无损压缩，但尚未做应用侧快照压缩。当时主矩阵 22 轮，加 4 轮 1000 bot 无 profiling 重复后，四轮/版本下单 P95 中位数 15.167→26.412 ms，撤单 21.909→18.576 ms，journal/wall 10.861%→11.919%，明确没有通过全负载尾延迟验收。

随后关闭快速提交做独立对照：1000 bot 关闭/启用的下单 P95 19.707/20.319 ms，启用原始值 26.990、13.647 ms，关闭 21.467、17.947 ms。该组未证明关闭快速提交能稳定消除回退。500 bot 单轮关闭/启用下单 21.348/53.797 ms、撤单 10.880/6.101 ms，也保留。构建对照时只将 eligibility 强制 false，源码已恢复并核对哈希；没有按更有利的新轮次替换先前回退。阶段1 32 轮性能记录及其 2 轮诊断完整保留在结果 JSON 和原始目录，最终候选的 22 轮单独列出。两阶段共 54 轮性能记录，均无采到的编译进程；已有阶段1慢样本的约 10 ms 同步 commit 等待没有因少两次往返而完全消失。

## 正确性与故障恢复

workspace 550 通过、12 忽略、0 失败；strict Clippy 全 workspace/all targets 通过。私有 PostgreSQL 9 个选定用例通过（8 个真实 PG + 1 个相关用例），覆盖单行提交可见性、SQL 错误无残留且连接可复用、lease 等待中接管拒绝旧写入、压缩/raw/JSONB 三格式完整恢复等价、幂等、quota、控制恢复和投影 late commit。内存 codec 覆盖混合 raw/压缩完整历史和序列；快照 codec 覆盖精确字节、UTF8、小/不可压缩输入及损坏/版本/长度拒绝。

最终二进制外部测试阻塞 projector，写入两个 instrument 的成交、推进时钟、暂停，硬停服务并重启 PG。完整状态一致，4 次相同幂等键重试无重复；解除阻塞 backlog 6→0，orders=6、trades=2、order_events=16、ledger=4、positions=4、transfers=0。

1000 bot 实际压测数据库的快照原始 JSON 12,841,990 字节，存储/传输 1,898,948 字节。恢复时钟和调度器与保存的暂停点一致，再次硬停服务、重启数据库后全部账户观测一致；state SHA256=33cc6435a959a302742e4cf8b228ad2b8920730f5b89920073aba8a438c90718。

首次阶段2测试未补 Python PATH，4 个插件 fixture 以 9009 退出；修正测试环境后全 workspace 通过，失败日志保留。既有 PG control-idempotency fixture 清理时仍有后台 Tokio timer-shutdown panic 日志，断言和测试退出通过，没有宣称修复。

## 可复查产物与边界

该 worktree 为 detached HEAD 71b58e9f9de3a9aede2521af9729d844d8913ace，所有变更未提交。相对前版的本轮编译输入共 7 个，哈希及补丁见 `.local/pg-journal-study/implementation-manifest.json`、`implementation.patch`；其他既有编译输入哈希未变化。前版 SHA256=ecc2a88af26038e665c4df89b3cd776039b118a40e806697db0f8d4aa2e1abd8；阶段1候选 d0197db8762bfc6cebdd88d1e8e348ca1b1261b7a9bea5cd50a18525df38de2e；最终候选 48dd4d03302457b8fde71f1ea6d79d0ce6ad394685758218273069d505ca4b03。

同目录 `2026-10-11-journal-waits-and-memory-results.json` 包含每轮原始值、两阶段收据、诊断、哈希和验收边界；原始基线/失败/关闭快速提交二进制和日志位于 `.local/pg-journal-study/`。fmt/diff 检查通过，所有本轮私有 PG cluster 已停止。

剩余：500 bot scheduler journal 仍占约 47% wall；房间通道长尾、1000/38 bot 撤单小幅回退、内存 60 秒撤单尾部仍未全面关闭。当前是明确的存储/提交成本优化和路径定位，没有生产 SLA 或无限运行内存保证。

每轮最终矩阵原始 P95：

| 负载/版本 | 下单 P95 ms | 撤单 P95 ms |
|---|---|---|
| postgres 500 / 60s / before | 25.934, 49.478 | 6.144, 12.084 |
| postgres 500 / 60s / after | 48.978, 22.314 | 4.974, 6.408 |
| postgres 1000 / 60s / before | 52.559, 24.599 | 20.051, 18.880 |
| postgres 1000 / 60s / after | 20.320, 32.358 | 21.277, 19.035 |
| memory 500 / 30s / before | 7.959, 8.670 | 11.382, 4.683 |
| memory 500 / 30s / after | 9.911, 3.620 | 5.147, 3.321 |
| memory 500 / 60s / before | 7.789 | 3.700 |
| memory 500 / 60s / after | 10.172 | 11.155 |
| postgres 38 / 60s / before | 12.898 | 2.468 |
| postgres 38 / 60s / after | 13.236 | 3.073 |
