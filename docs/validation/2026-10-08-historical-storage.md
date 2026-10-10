# 2026-10-08 历史存储与复盘验证

实现和本地验收通过。当前交付是 PostgreSQL 检查点恢复、完整房间归档、独立恢复、
ClickHouse 批量历史镜像和可复用容量工具。尚不能宣称大量逐笔数据的生产容量通过。

基于 `888b2bb` 的未提交工作区，保留原有 bot 插件改动。本轮没有提交、推送、生产部署，
也没有对业务数据库实施迁移或清理。使用自己创建的独立 PostgreSQL/ClickHouse 容器，
只绑定 loopback；测试结束后移除容器，归档和日志保留在 `target/storage-validation/`。

## 实现范围

- 迁移 15 保存快照与对应命令/变更边界、订单 ID 高水位，恢复只读后缀，并保留最新
  scheduler/bot 和每个训练任务状态。周期快照不再在变更日志复制 actor JSON。
- 启动和租约接管使用 repeatable-read 读取。快照缺失回退完整日志，错误边界拒绝恢复；
  执行缓存为空时从已提交 actor 游标继续写入，订单 ID 不复用。
- K 线查询持久化模拟时间和整数金额，同时间交易按命令/事件序号决定开收盘；
  旧 tick 缺模拟时间时完整重放。最近成交价格独立恢复，ticker 不因后缀为空而丢失。
- 完整事件遍历和指定命令位置的隔离复盘保留完整日志路径。
- Python 标准库/psql 工具导出所有房间表、用户和迁移记录，提供压缩与原始双 SHA-256、
  行数、主键和序号检查。只恢复到已迁移的空独立目标，在事务内插入并修复序列，旧租约过期。
- ClickHouse 保存类型化历史字段及完整 payload，重复导入用归档身份和 FINAL 去重，
  逐行读回校验。当前是关闭房间的批量路径；HTTP K 线仍由 PostgreSQL 提供。

使用方式和后续阶段见 [HISTORICAL_STORAGE.md](../HISTORICAL_STORAGE.md)。

## 容量实测

PostgreSQL 16.14，`postgres:16-alpine`；Rust 1.95.0 的 debug 构建；本机 WSL。
每个房间一个并发 HTTP 客户端，单品种 spot，每轮包含挂卖单、买入成交、挂买单、撤单、
推进时钟。一次轮次有 4 条订单命令和 5 个 HTTP 写请求；P99 包含时钟请求。
各组顺序运行，在空独立数据库启动，WAL 集群期间没有其他测试写入。

| 房间 | 每房间轮次 | 订单命令 | 命令/秒 | 请求 P99 ms | 物理增长 KiB/命令 | WAL KiB/命令 | 重启就绪 ms |
|---|---|---|---|---|---|---|---|
| 1 | 100 | 400 | 60.6 | 28.6 | 9.14 | 9.50 | 199.8 |
| 4 | 100 | 1,600 | 77.2 | 85.5 | 8.49 | 9.69 | 305.7 |
| 16 | 100 | 6,400 | 60.2 | 416.4 | 8.28 | 9.96 | 404.1 |
| 1 | 400 | 1,600 | 37.4 | 104.8 | 16.75 | 18.51 | 235.2 |

原始证据：`target/storage-validation/delivery-{1,4,16,long}/report.json`，各目录还保存
服务日志、指标、重启前后完整重放状态和检查点状态。最终矩阵日志为
`final-capacity-matrix.log`。数据文件采用完整字节数，表中使用 KiB=1024 字节。

四组的重启状态、ticker、K 线均一致，检查点状态与从头重放一致。关闭房间完成最终检查点后，
运行时读取的订单命令均为 0；必要变更分别为 2、8、32、2 条。
16 房间旧路径样本启动就绪为 1,294.9 ms，最终样本为 404.1 ms，
从读取 6,400 条命令降为 0 条。旧路径证据为 `baseline-16/report.json`。

本轮**没有证明写吞吐提升**。改前 1/4/16 房间短样本为 70.5/79.6/70.0 命令/秒，
改后另一轮样本为 71.8/79.1/69.2；最终数据如上表。短样本有明显波动，不应据此估计生产极限。
新增恢复元数据有写入和索引成本，增加房间没有带来近似线性吞吐增长。

历史增长已有直接证据：单房间从 400 增到 1,600 条命令，最大快照 JSON 从
212,256 增到 849,150 字节，单位命令物理增长从 9.14
升到 16.75 KiB。
长样本快照表含索引/TOAST占 71.3% 的总表物理大小。
快照仍重复保存增长的引擎历史，加上每进程共享状态锁和单写通道，是下一阶段的优先优化项。
这些值包含固定页分配、索引和数据库内部占用，不能作为任意负载的固定每条事件成本。

## 验收结果

| 检查 | 结果 | 证据，位于 target/storage-validation/ |
|---|---|---|
| 强制 PostgreSQL Rust 工作区，串行测试 | 331 通过，0 失败 | workspace-postgres-tests-final-accepted.log |
| Python SDK 全量，真实 PostgreSQL | 23 通过，0 跳过 | python-sdk-final.log |
| 归档完整性负例 | 9 通过 | archive-unit-tests-final.log |
| 格式、Clippy 全目标 -D warnings、构建、diff 空白检查 | 通过 | fmt-final.log、clippy-final-accepted.log、build-final-accepted.log，终端 diff 检查 |
| PostgreSQL 重启/单实例锁/备用接管 | 通过 | postgres-smoke-final.log |
| 多实例不同房间写入/租约接管/fencing | 通过 | multi-active-smoke-final.log |
| 4 组容量、完整重放与检查点、市场视图 | 通过 | delivery-*/report.json |
| 20 表/2,527 行/命令 0..399 的归档和恢复 | 通过 | archive-acceptance-final.json |
| 最终二进制四方重放状态对比 | 通过 | final-binary-archive-audit.json |
| 活跃目标恢复被拒绝、空目标保持为空 | 通过 | archive-acceptance-final.log |
| 已有数据目标再次恢复被拒绝 | 通过 | archive-acceptance-final.json |
| 仅有业务用户的目标恢复被拒绝且保持原样 | 通过，兼容迁移自带 local-user | strict-restore-final.log |
| 最终归档工具再次恢复两个归档并重复导入 | 通过 | strict-restore-basic.json、strict-restore-plugin.json |
| ClickHouse 重复导入、每行 payload 校验 | 通过 | archive-acceptance-final.json |
| ClickHouse 成交聚合 | 100 笔、100 手、金额 10,100，一致 | clickhouse-query-final.json |
| 源归档身份在恢复/导入后不变 | 通过，未删除源记录 | archive-acceptance-final.log |
| 真实训练和进程 bot 归档、恢复、重复镜像 | 20 表/67 行/4 条命令，四方重放一致 | plugin-archive-acceptance.json |
| 恢复库启动后的训练/bot HTTP 视图 | 相同，2 笔成交，插件执行 2 次 | plugin-archive-runtime-views.json |

Rust/Python 设置 `MARKETFORGE_TEST_DATABASE_URL` 和 `MARKETFORGE_REQUIRE_POSTGRES_TESTS=1`，
同时设置相同的 `MARKETFORGE_BOT_PLUGIN_DIR`，防止跳过数据库测试或丢失插件恢复配置。
归档原文件为 `archive-current/` 和 `archive-plugin-current/`，包括完整 manifest。

回归期间发现并修复：缺快照时错误过滤旧命令；恢复用的检查点被误当作原始变更，导致
自动撮合后训练元数据被误判为游标倒退；完整检查点留下空执行缓存，后续 scheduler 写入
误从 0 开始；旧 tick 回退聚合未沿用查询捕获的模拟时钟。最终测试及真实插件重启覆盖这些边界。
早期失败和中间容量轮次不作为最终通过证据。

## 完成边界

本地正确性、完整归档恢复和批量镜像已验收。没有压缩引擎快照历史、拆分房间锁/写通道、
实现活跃房间增量 CDC、改 HTTP 历史查询到 ClickHouse，或删除 PostgreSQL 已归档数据。
ClickHouse 镜像尚未减少 PostgreSQL 历史表占用。

迁移 15 对旧命令扫描回填并建索引，大型库执行前要单独评估迁移时间与阻塞。
当前基准没有高深度盘口、每命令多次撮合、多品种/永续混合负载、备份/副本成本，
也没有长时间运行、磁盘满/数据库断连/CDC 崩溃追赶验收。
事件速率与保留期未确定，因此不承诺生产容量。
下一阶段先压缩快照与拆分写入路径，再完成活跃历史归档、查询切换及保留窗口验收。
