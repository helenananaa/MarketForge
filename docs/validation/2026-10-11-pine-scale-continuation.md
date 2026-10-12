# Pine 大规模接入修复与复测（2026-10-11）

本轮落地了 worker 亲和性、增量 session 容量、按新增 bar 构造 pass、宿主输入差量传输，以及外部任务名额取得顺序和持续派发。测试验证了状态与真实成交恢复的一致性。容量必须按以下实测负载判断，不能用实例启动成功或解释器支持脚本数量推算。

## 已实现与边界

- Pine 默认仍为 4 个常驻 worker。完整 participant 身份固定到槽位，忙碌时在既有 deadline 内等待；失败或进程替换时重建，不迁移暖会话。
- 每 worker session 上限从 64 提至 512，编译缓存仍为 64。Pine worker 回收阈值从默认 10000 调为 500000 次请求；阈值可配置，仍有上限。大规模长期内存和回收尚未验收。
- 暖 session 只构造新增 external pass，保存紧凑 provider/evaluation transcript；全量重建与原有冻结前缀校验保留。没有把复杂历史 pass 全部复制并缓存成 O(H²) 前缀。
- 差量请求最多 512 份先前宿主输入、256 个 patch；数组追加必须精确匹配旧长度，错误 base 拒绝。缓存不采用 worker 输出；拒绝提交后的宿主旧状态可以精确重建。完整展开大小仍受协议上限约束，首次、淘汰后或差量不合算时发全量。
- 调度先取得全局 16 个外部任务名额，再复制实际派发实例的状态。待处理队列只保存身份和索引，任务完成和 poll 会继续补名额；下一 clock 使用最新候选，epoch、暂停、停止和单实例在途限制保留。原生批处理不占外部名额，插件析构仍在房间锁外。
- 增量路径仍只覆盖 `incremental-qualified.json` 的 6 个精确源码。其他脚本和 legacy 继续完整重放；私有 runtime 仍为 `0.3.1+mf1`，没有将上游新兼容能力接入。

外部 session 仅导出 append/cursor，没有完整计算状态序列化与恢复接口。因此仍保留默认 512、最大 2048 根非空收盘 bar 的明确停止行为；没有截断历史后重新初始化变量，也没有放宽上限掩盖接续缺口。响应和 durable state 仍是完整 transcript；共享不可变行情、完整计算 checkpoint 和响应增量仍需后续实现。

## 正确性验收

- 冻结候选源码 `cargo test --workspace --locked -- --test-threads=1`：594 通过、13 忽略、0 失败；独立 PostgreSQL 测试数据库强制启用。忽略项为原有受控 benchmark 等，不计作通过。
- `cargo clippy --workspace --all-targets --locked -- -D warnings` 通过。
- Python 私有 runtime 的 broker/session/transport：23 项通过。覆盖暖追加、历史改写拒绝、未提交 session 重建、淘汰、宿主旧状态回滚、传输输入隔离和精确追加边界。
- 当前 release 的真实 HTTP/账户/订单/PostgreSQL 恢复：13 项通过；包括三种策略真实部分成交和退出、原生条件订单、跨市场请求、合约和重启不重发。
- 固定三种脚本的 59 步真实交易：完整账户、订单、成交、book 和 durable state 与全量重放逐项相等，摘要 `48c26aa5e50e9a8417e8ce7de464eb4cff0a511f65288bb686e0d3a15024ecfa`。
- 新的 48 外部 bot 回归要求在第二个 10000ms tick 前完成首轮，每 bot 只计算一次；通过。同时保留慢 bot 隔离、epoch/停止后的过期结果拒绝和 writer-lane 回归。

固定计算 ABBA 为 6 脚本 × 128 bar = 768 决策，冻结合成账户反馈。完整重放两轮 5.699/5.528 秒，候选增量两轮 3.405/3.449 秒，耗时减少 38.95%；四轮完整响应摘要相同。它比较候选增量与候选全量重放，不是本轮接入改动的端到端收益，也不是真实异步成交吞吐。

## 持续输入的短时阶梯

每档是实际 100/500/1000 个 Pine 实例，另有固定 38 个原生流动性 bot、一个独立 HTTP 探测账户和两组有限资金成交账户。三种 spot 脚本使用 10/15/30 秒仿真周期、独立账户。初始 maker 深度和资金按总参与者数缩放，不在交易中补钱。成交账户通过真实网关提交限价单，保留 TradePrinted/结算回执，不直接注入 K 线。每轮 5 秒暖机、30 秒测量、25ms wall tick 推进 1000ms simulation，目标为 40 步/秒。

| Pine 实例 | 市场步/秒 | 下单 P95 ms | 进程树峰值 MiB | 积压 P95/最大 bar | 落后实例 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 100 | 32.68 | 113.2 | 554.0 | 2/2 | 55 |
| 500 | 19.24 | 246.0 | 942.9 | 10/16 | 429 |
| 1000 | 14.78 | 362.3 | 1242.7 | 13/27 | 940 |

本轮 40 步/秒目标：未通过，不能承诺千级加速容量。初始化成功和没有报错与性能通过分开记录。

下单 P95 是独立 HTTP 探测账户的请求延迟，不是 Pine 决策 P95。积压是暂停后的 durable 快照相对于实际非空收盘 bar 的差值，不是稳态 SLO。每条报告同时保留实际闭合 bar 数、最新 bar 时间、真实 feed 成交、全部实例初始化和错误。市场推进加快可能产生更多实际工作，不能只比较积压数量。

## 同配方 1000 Pine ABBA

| 顺序 | 实现 | 步/秒 | 下单 P95 ms | 进程树峰值 MiB | 积压 P95 bar |
| --- | --- | ---: | ---: | ---: | ---: |
| A1 | before | 7.53 | 278.0 | 626.4 | 5 |
| B1 | candidate | 14.45 | 293.9 | 1176.7 | 12 |
| B2 | candidate | 15.36 | 329.6 | 1274.0 | 11 |
| A2 | before | 7.00 | 229.5 | 646.2 | 4 |

两轮平均 before 7.26、candidate 14.90 步/秒。它们采用相同配方和 harness，实际异步市场轨迹不同；仅两个样本，不能外推任意策略或长时容量。内存增加需与吞吐和积压一起评估。

## 按真实时间推进的补充场景

额外测试 wall tick=1000ms，仍推进 1000ms simulation，1000 Pine、持续真实成交、5 秒暖机和 120 秒测量。它与上述 40 倍加速压力不是同一验收档位。

| Pine 实例 | 市场步/秒 | 下单 P95 ms | 进程树峰值 MiB | 积压 P95/最大 bar | 落后实例 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1000 | 0.99 | 64.7 | 546.0 | 0/0 | 0 |

实际推进为 0.994138 步/秒，按 harness 原有 `configured_step_rate` 的 95% 阈值判定为 True；严格 `literal_step_rate` 判定为 False，两者分别记录。没有放宽原阈值。全部 1000 实例初始化、最近非空输入、无 bot/feed 错误等字段另见 JSON。

该短时场景的成功也不能作为复杂社区脚本、长时内存、历史接续或 PostgreSQL 千 bot 容量证明。

## 保留的负结果与证据范围

- 最初 pilot 使用旧集成 binary，且与编译重叠，不能作为冻结对照。
- phase1 ABBA 遇到另一任务编译，保留全部结果但不据此声称稳定收益。
- phase2 未持续成交时，1000 Pine 最新非空 bar 明显落后于市场时间，“零积压”是 quiet tail；该组不能作持续容量结论。放大初始 maker 深度仍未消除 quiet tail，因此加入真实 gateway feed，并新增 recent-nonempty-input 判定。
- funding-only 的 4/16 worker 各单次测试，市场输入轨迹不同，16 worker 未改善市场推进，保留 4 worker 默认；没有把单次结果当作 worker 数量最优证明。
- phase2 持续 ABBA：before 8.51/8.99、candidate 15.57/17.89 步/秒。候选积压 P95 20/17 bar，订单 P95 363.97/241.83ms，40 步目标未通过，促使补充任务完成后派发。
- 首次共用 Cargo target 错将候选判作 Fresh，产物与 before 摘要相同，已在任何对照前拒收并改为独立 target；错误产物与日志保留。PostgreSQL 首次 host 参数拼接错误已改成分离参数。并发任务的新 market_analysis 编译参数问题只修了一处，并纳入两个冻结版本共同基线。

源码冻结到独立目录，before/candidate 分别使用独立 Cargo target；测试和编译在压测前结束，最终压测 compiler monitor 记录单独保留。共享工作区的其他改动和其他任务提交保留，本轮没有 commit/push。

候选 release SHA256：`0946c59aac2d0c274fd576e2f80af1cfc495252a0751df0200bde82b0f813458`。
before release SHA256：`bd569ccb310c72588b83d656487933fcd9d631cf97b049cd30bafc0ab7dbb4ae`。
harness SHA256：`8a8f2a041e2713e970ebd4ed1a64cc047a933e0d48879cfceae3942835a7c998`。
runtime 源 commit `8838ed004c7475ce2877189bcd0e43a23bcf365f`，DLL SHA256 `8ef5900e5bfeadfbced7edef772e8b0a00188215448d4417f869b29d09c9c427`；本轮没有修改上游源码或重新发布 runtime。

机器、逐档指标、阶段负结果、源码/产物摘要和全部原始回执路径见 [JSON](2026-10-11-pine-scale-continuation.json)。原始证据保存在 `E:\projects\MarketForge\.local\pine-scale-continuation-20261011`，包含冻结源码、编译/测试日志、HTTP 交易一致性回执和各档完整 scheduler/recipe/server/resource/input-feed 文件。当前没有全量社区库的大规模性能复测、Pine 决策 wall P95、长期内存/回收和 PostgreSQL 大规模容量资格。

复测命令：

```powershell
.venv/Scripts/python.exe scripts/benchmark_strategy_runtime.py --server <独立release产物> --plugin-dir bot-plugins --population pine --sustained-input --counts 100 500 1000 --seconds 30 --output .local/pine-repeat
```
