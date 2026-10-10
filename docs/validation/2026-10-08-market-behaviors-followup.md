# 增强市场行为补充验证，2026-10-08

本轮补完高速调度、内存、实际数据库恢复和现货＋永续历史样本参考。
所有 live 实验使用任务专属服务、随机端口和独立数据库，没有停止原有服务。

## 高速调度和内存

优先处理完成的决策，将已就绪的策略合并为一次持久化事务；不等待慢策略。
状态／epoch 失效的决策跳过，批次失败回滚，普通策略错误按单个决策隔离重试，
存储错误仍停止写入。回归测试覆盖批次恢复、失败回滚、无重复提交、过期成员和
暂停／恢复边界，原有慢策略、异常、进程超时和时钟推进测试继续通过。

没有训练采样时，原生动作精度验证避免复制完整观察；gateway 继续校验实时资金与
市场状态。内存日志增量维护投影，失败候选不会污染唯一键或投影；仅保留每个房间
的最新恢复快照，但保存所有执行和历史调度日志。历史调度日志存为准确 JSON 字节。
引擎命令／事件／清算历史使用 Arc 写时复制，JSON 和公开切片接口不变。快照采用
流式 JSON 校验，保留恢复使用的整数范围限制；越界账户无法发布快照。

33 bot、25ms 配置、完整原始消息／POV 时间表、默认 6000 仿真毫秒订单时效：

- 1000 步在 149.516 秒完成，沿用 173.08 秒预算，没有提高回归门槛。
- 服务峰值工作集 380,596,224 字节，即 362.965 MiB，低于 512 MiB 门槛。
- 两侧 POV 实际完成 25／25；四个消息交易者收到两条公共消息。
- worker 无错误，最终保存状态观察延迟为 0 仿真毫秒。

回执 `.local/behavior-stream-long/report.json`、`resources.json`、`before.json`。
实际平均吞吐约 6.69 步／秒；25ms 是请求配置，不证明实际达到 40 步／秒。
本验收覆盖一个房间、1000 步；历史执行本身仍增长，不证明多房间或多小时稳定。

中间失败保留，包括高速超时、旧快照保留导致约 500 步已达到约 1.7 GiB、仅限制
快照后仍超过内存门槛，以及后续性能失败。相关目录为 `.local/behavior-long-delivery/`、
`.local/behavior-bounded-long/`、`.local/behavior-packed-long/`、`.local/behavior-arc-long/`。
最终使用流式校验与其他优化组合通过，不能归因于某一个改动的独立性能增益。

## 实际 PostgreSQL 与服务重启

使用本机 PostgreSQL 18.4、任务专属 `.local/behavior-postgres` 集群和每次新建的
唯一数据库。500ms 配置和缩短消息／POV 时间表，在第 30 步暂停，POV 尚未截止，
买入完成 5、卖出完成 8。真正终止专属服务，停止再启动专属 PostgreSQL，并启动
新服务进程连接同一数据库。

重启后完整 scheduler、时钟、33 组账户／品种观察、订单簿、公开最近成交和消息
逐项完全一致。恢复运行到第 60 步，POV 实际完成 25／25 并截止，worker 无错误。
不是单纯序列化恢复或只重启客户端。集群在 finally 中正常停止。

初次重启发现：公开最近成交因 checkpoint 后没有执行历史而变空。现在从恢复的
成交回执补齐，按品种／trade ID 去重，重叠 replay 和新成交衔接都有回归测试。
公开成交不依赖费用完整性；POV 历史完整性规则仍保留。

回执 `.local/behavior-postgres-final/{before,after,resumed,report}.json` 和服务／数据库
日志；完整对比 33 组账户／策略腿。高速 PostgreSQL 完整 260 步在 45 秒内的另一项
尝试未通过，因此本记录只证明上述恢复场景，不证明数据库高速吞吐已通过。

高速长运行和最终数据库重启均使用专属服务二进制 SHA256：
`65ba01fc1af515e6d76aa193a256feb44ed9829ec27ce64bd152ee607ab68d4d`。

## 测试与复现

- Rust workspace：452 passed，0 failed（core 271，server 170，其他 11）。
- Clippy 所有 target，`-D warnings`：通过。
- Python 行为 HTTP／配方：3 passed，47.096 秒，包含原时间表 260 步／25ms 和
  缩短时间表 60 步／500ms；前者保持原订单时效和 45 秒预算。
- Python 历史数据校准：5 passed；覆盖时间单位、精确数量、异常／重复来源、留出
  数据与未来资金费率不参与拟合、缺失收益样本不伪装为零波动。
- fmt、Python 编译、diff 空白检查通过。

Rust PostgreSQL 条件测试在未提供专用环境时可能跳过内部数据库分支，不将上述
Rust 数字当作数据库端到端证明；实际数据库证据来自前述隔离 live 验证。

```powershell
$env:PATH = "$PWD\.venv\Scripts;$env:PATH"
cargo rustc -p exchange-server --bin exchange-server -- -o .local/behavior-stream-server.exe
.venv/Scripts/python.exe scripts/validate_behavior_market_live.py --server .local/behavior-stream-server.exe --steps 1000 --timeout-seconds 173.08 --max-memory-mib 512 --output .local/behavior-stream-long
.venv/Scripts/python.exe scripts/validate_behavior_market_live.py --server .local/behavior-stream-server.exe --postgres-data .local/behavior-postgres --compressed-timeline --steps 30 --interval-ms 500 --output .local/behavior-postgres-final
```

数据库目录需要事先初始化为任务专属、停止的集群，脚本拒绝附着正在运行的目录。
Windows pg_ctl 输出重定向到文件，避免守护进程继承管道导致控制命令等待不退出。

公共历史数据、实测统计、三种子实际撮合残差和未满足的市场相似性指标见
[历史样本校准](../MARKET_REFERENCE_CALIBRATION.md)。真实盘口／撤单／网络延迟、
多日期检验及更广泛的资金费率策略仍没有资格验证。
