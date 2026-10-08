# 历史存储、容量测量和复盘

本轮交付 PostgreSQL 检查点恢复、完整房间归档、ClickHouse 历史查询镜像和容量基准。
撮合提交仍由 PostgreSQL 事务保证；HTTP 历史 K 线仍查询 PostgreSQL。
ClickHouse 当前接受关闭房间的批量归档，不承担账户、租约或幂等判定，也不是持续 CDC。

## 1. 容量基准（closed）

依赖：Python 标准库、`psql`、构建好的服务端、**空的独立数据库**。
每个房间一个并发客户端；每轮挂卖单、买入成交、挂买单、撤单、推进时钟。
工具拒绝已有房间的数据库，只启动和停止自己的进程。

```sh
cargo build -p exchange-server --bins
export MARKETFORGE_DATABASE_URL='postgres://USER:PASSWORD@127.0.0.1:PORT/EMPTY_TEST_DATABASE'
python3 scripts/storage_capacity.py --rooms 1 --cycles 100 \
  --output target/storage-capacity/rooms-1 --isolated-target
```

分别在三个空数据库运行 `--rooms 1`、`--rooms 4`、`--rooms 16`。
不要同时运行：WAL 计数属于整个 PostgreSQL 集群，其他写入会污染结果。
交付 `report.json`、完整重放/检查点状态、服务端日志和指标。
验收：订单均接受，重启前后状态和 ticker/K 线一致，检查点状态与从头重放一致。
测量包括物理占用、索引、WAL、快照 JSON 大小、吞吐、请求延迟和恢复时间。
短时 debug 基准不是生产容量或长期稳定性证据。

## 2. 检查点恢复（closed）

迁移 15 新增 `marketforge_recovery_heads` 和恢复视图。
同一事务保存快照、下一个命令序号、最新房间变更序号和订单 ID 高水位。
周期快照的 actor JSON 只保存到快照表一次，不在变更日志再复制一份；
房间创建时的原始 bootstrap 检查点保留，兼容旧恢复契约。
恢复读取快照之后的命令/变更，并保留检查点之前最新的 scheduler/bot 和每个训练任务状态。
读取使用 repeatable-read 事务，避免混用不同提交时刻的快照、边界和事件。
恢复检查点单独表示，原始变更记录仍校验游标顺序；自动撮合后的训练元数据可以引用
前一个命令游标。快照覆盖全部命令、执行缓存为空时，后续写入从 actor 游标继续。

历史事件接口仍分页读取完整日志；隔离复盘和离线审计使用完整日志。
历史 K 线从持久化 tick 聚合，同一时间的开收盘按
`(market_time_ms, command_seq, event_seq)` 确定。保持整数金额、空时间段不补线。
旧 tick 缺少模拟时间时回退到完整重放，不能用 `created_at` 替代。
最近成交单独恢复，避免没有快照后成交时 ticker/SSE 返回空值。

```sh
# 只读；不执行迁移。完整历史从场景起点重放。
target/debug/journal-audit database ROOM_ID > target/full-replay.json
# 线上启动和租约接管使用的检查点路径。
target/debug/journal-audit runtime ROOM_ID > target/checkpoint-recovery.json
```

报告中的 `state` 必须一致。序号断裂、结果分歧或边界不匹配应拒绝恢复。
快照缺失时回退到完整日志重放，保持快照作为可选优化的原有契约。
快照仍包含增长的引擎历史；本轮没有压缩快照历史、拆分写通道或删除日志。

## 3. 完整归档（closed）

一个 repeatable-read 只读事务导出目标房间的所有 MarketForge 表、所需用户和迁移版本。
包含命令、拒单、订单事件、成交、清算、房间变更、快照、训练/bot、幂等和配额。
未知全局表拒绝导出，必须先明确范围。默认只接受关闭房间。

```sh
python3 scripts/storage_archive.py export CLOSED_ROOM_ID /absolute/archive/path
python3 scripts/storage_archive.py verify /absolute/archive/path
python3 scripts/storage_archive.py recovery /absolute/archive/path \
  | target/debug/journal-audit stdin > target/archive-replay.json
```

交付分表 gzip JSONL 和 manifest：版本、主键、行数、原始/压缩大小、双 SHA-256。
验证命令序号从 0 开始且连续，主键有序唯一，记录属于目标房间，大小/数量/校验一致。
归档保存 bot 配置和状态；插件执行文件由部署环境安装，恢复服务须提供相同版本的插件。
先写临时目录，验证后改名发布；出错清理临时目录，不覆盖已有归档。
`--allow-active` 生成某个时刻的完整快照，不是持续归档。
SHA-256 校验发现损坏，不提供来源认证；归档含账户等私有数据，使用受控存储权限。
**导出与验证不会删除任何源记录。**

## 4. 独立恢复与联合验收（closed）

目标必须为空，并先由当前服务端启动完成迁移，再停止该目标服务端。
源/目标迁移版本、表名和列必须一致。恢复在一个事务内执行，有房间或已有业务用户的
目标拒绝恢复；仅允许迁移 6 自带的 `local-user` 种子用户。归档包含该用户时恢复原始创建时间。
恢复的旧租约立即过期；变更序列生成器同步到最大已有序号。

```sh
export MARKETFORGE_RESTORE_DATABASE_URL='postgres://USER:PASSWORD@127.0.0.1:PORT/EMPTY_RESTORE_DATABASE'
python3 scripts/storage_archive.py --dsn-env MARKETFORGE_RESTORE_DATABASE_URL \
  restore /absolute/archive/path --isolated-target
```

联合验收工具也会执行恢复，为它另准备一个空目标：

```sh
python3 scripts/storage_acceptance.py /absolute/archive/path --isolated-target \
  --output target/storage-acceptance.json
```

验收比较源数据库从头重放、归档从头重放、目标从头重放、目标检查点恢复。
还验证重复恢复被拒绝，源记录未删除。比较保留命令、事件和队列顺序；只规范化
`seen_order_ids` 集合顺序，消除 HashSet 随机序列化顺序。

## 5. ClickHouse 镜像（closed，关闭房间的批量路径）

先准备运营者管理的 ClickHouse 数据库，再配置：

```sh
export MARKETFORGE_CLICKHOUSE_URL='http://127.0.0.1:8123'
export MARKETFORGE_CLICKHOUSE_USER='USER'
export MARKETFORGE_CLICKHOUSE_PASSWORD='PASSWORD'
python3 scripts/storage_clickhouse.py import /absolute/archive/path --database default
python3 scripts/storage_clickhouse.py verify /absolute/archive/path --database default
```

创建 `marketforge_room_archive_rows`，包括房间、归档身份、表、行序号、命令/事件序号、
模拟时间、品种、事件类型、价格、数量和完整 payload。
写入后逐表读回，验证每条 payload 的总 SHA-256、字节数、行数和行顺序。
重试复用不可变归档身份，查询用 `FINAL`，不依赖后台合并已完成。
同一房间已有不同归档身份时拒绝新导入，避免重复统计多个房间版本。
一个房间由一个导入任务负责，当前没有跨进程的版本替换协调。

```sql
-- 只统计一份成交投影，不能同时累计 trades 和 market_ticks。
SELECT room_id, instrument_id, intDiv(market_time_ms, 60000) AS minute,
       count() AS trades, sum(qty) AS volume
FROM marketforge_room_archive_rows FINAL
WHERE table_name = 'marketforge_market_ticks'
GROUP BY room_id, instrument_id, minute
ORDER BY room_id, instrument_id, minute;

SELECT command_seq, event_seq, event_type, payload
FROM marketforge_room_archive_rows FINAL
WHERE room_id = 'ROOM_ID' AND table_name = 'marketforge_order_events'
ORDER BY command_seq, event_seq;
```

联合验收增加 `--clickhouse` 会故意重复导入并验证内容一致。
镜像由运营者凭据访问，本轮没有新增绕过应用权限的 HTTP 历史接口。

## 6. 后续生产验收（planned）

1. 确定事件/命令速率、峰值、保留期、查询/恢复时限；用真实挂单深度、多笔成交、
   训练/bot 混合负载跑 release 和长历史样本。
2. 压缩引擎快照历史，保留撮合/去重/清算计数器；完整重放对比盘口、K 线、最近成交、
   未完成 bot 动作和训练分数通过后，才接受压缩检查点。
3. 保持房间内顺序和 fencing，拆分全局状态锁与每进程单写通道；验证多房间吞吐增长，
   以及提交失败不推进内存状态。
4. 活跃房间实现可恢复的增量归档/CDC、同步游标、完整性清单和查询可见水位；
   验证崩溃、重复、乱序、停机追赶。批量镜像不能替代此阶段。
5. 设计分区与保留窗口。归档、重放、一致性、历史查询路由全部通过后，再清理已归档
   数据，同时保留在线幂等判定所需记录。
6. 完成声明负载的长期运行、磁盘满/数据库断连/归档失败、备份恢复和接管验收。

迁移 15 对旧日志回填订单 ID 高水位，大型生产库需评估迁移扫描和建索引影响。
源历史表仍在 PostgreSQL 中增长；导出不意味着磁盘占用已经下降。

本次实测和验收证据见 [2026-10-08 验证报告](validation/2026-10-08-historical-storage.md)。
