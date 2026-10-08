# CandleScope 仿真工作台

MarketForge 负责房间、撮合、订单、账户、费用、风控、强平、AI 参与者、仿真时钟和持久化。
CandleScope 的 `/simulation.html` 通过 WebSocket 接收状态，通过 HTTP 提交交易，复用图表、绘图、导出、主题与工作区导航。
这个页面不启动 CandleScope Paper broker，也不需要 CandleScope Python 后端。

## Windows 启动

两边源码均需包含本次接入改动。从 MarketForge 根目录运行：

```powershell
.\scripts\start-candlescope-workbench.ps1
```

默认启动专属 PostgreSQL，并将 MarketForge 绑定到 **57306**。需要已安装 PostgreSQL 可执行文件；
不在 PATH 时指定 `-PostgresBin D:\SQL\bin`。也可使用 `-DatabaseUrl` 或环境变量
`MARKETFORGE_DATABASE_URL` 指向现有数据库。显式 `-Memory` 才允许内存模式。
PostgreSQL 数据位于 `.local/candlescope-runtime/postgres`，仅监听 127.0.0.1:55432；
密码随机生成，使用当前 Windows 用户的 DPAPI 保存，数据与凭据均在 Git 忽略目录中。

可通过 `-CandleScopeRoot` 指定其他 CandleScope 检出目录。`-SkipBuild` 仅用于已有对应构建产物时；
后端专属构建位于 `target/candlescope-workbench/debug/`。
脚本在空闲端口启动隐藏的后台进程，打印 PID 和页面地址；遇到已有服务时保留该进程。
日志位于 `output/candlescope-workbench/`。不自动创建房间、不自动下单。

手动运行先配置 `MARKETFORGE_DATABASE_URL` 和 `MARKETFORGE_BIND_ADDR=127.0.0.1:57306`，
执行 `cargo run -p exchange-server`，然后在 CandleScope `frontend` 目录
执行 `npm ci --ignore-scripts`、`npm run build`、`npm run preview`。
打开 `http://127.0.0.1:15173/simulation.html`。

默认后端地址为 `http://127.0.0.1:57306`。上轮 57305 的内存测试服务未停止，旧房间未迁移到新库；
需要查看时在页面填写旧地址。页面明确显示后端存储模式。
默认 CORS 包含 CandleScope 的 IPv4/localhost 15173
来源；其他来源通过 `MARKETFORGE_CORS_ORIGINS` 显式配置，保留部署现有来源。
远程部署需要 HTTPS 和 Bearer 令牌。令牌只存在当前页面内存中，不写入 URL 或本地存储。
本地 `x-user-id` 与 Bearer 二选一，权限由 MarketForge 校验。

## 操作

1. 填写服务地址、房间和账户，连接已有房间；或点击“创建背景市场”。
2. 创建操作从 `GET /scenarios/background-market` 获取 MarketForge 自己的配方，只改房间名称，
   再通过原有 `POST /rooms` 创建。默认账户 20，20 个有限资金背景参与者自动运行。
3. 市价/限价买卖通过原有交易入口提交。挂单来自参与者观测接口，撤单由后端检查所有权。
4. 盘口点击可填入限价。账户展示余额、持仓、冻结资金和后端提供的保证金指标。
5. 暂停、继续和暂停后的单步推进均请求后端控制接口。有调度器时推进完整参与者步骤；
   后端明确返回“没有调度器”时，改为推进一次后端时钟。其他错误不降级。读行情不推动仿真时间。

## 数据与生命周期

- `GET /rooms/{room}/observe` (`strategy.v1`) 一次返回当前账户、挂单、盘口、最近 32 笔成交和仿真时间；
  页面不依赖管理员全量账户接口。
- K 线读取原有 `/candles` (`http.v1`)，只展示成交生成的蜡烛，空区间不补价格。
- WebSocket 首条消息在连接内提交凭据，URL 不含令牌。后端每 250 ms 检查变化，推送经过账户授权的
  完整观测与最近 500 个周期 K 线，5 s 发送空闲心跳。数据库读取不持有撮合锁；
  读取期间若账户、盘口、时钟或命令游标变化，丢弃混合样本并重新读取。
- 前端验证协议、房间、账户、周期、序号与快照时钟。12 s 无消息判定超时；断线先禁用交易，
  再指数退避重连，每次重连从完整快照开始。401/403 不反复尝试 WS。
- WS 正常时 30 s 做一次 HTTP 核对；不可用时退回串行、可取消的 750 ms HTTP 读取。
  请求限时 10 s。切换房间、账户、品种或周期后，旧会话不能覆盖新会话，旧 HTTP 响应也不能
  覆盖更新的 WS 快照。订单确认后读取权威状态；不做本地账本预测。
- 写操作有独立幂等键且同一页面至多一个请求在途。失败不自动重试写入，提示先核对账户和订单。
- 回执的 `accepted` 是 actor 指令确认；页面同时检查 `RiskRejected`、`OrderRejected` 等后端事件，
  避免将“指令执行了但订单被风控拒绝”显示为下单成功。
- JSON 的大整数先无损保留为十进制字符串。绘图 tick/lot 超过安全整数时拒绝渲染；不会舍入金额。
- 图表横轴是仿真耗时。固定 epoch 只用于图表坐标，既不改撮合时间，也不作为真实市场日期。
- 初版加载最近 500 个周期的成交 K 线，未接更早历史分页、多图联动或指标计算。
  最近成交区是快照中的有界近期成交，不宣称完整逐笔归档。
- 未配置 PostgreSQL 时，后端为内存模式，退出后房间丢失；页面刷新可重新连接仍在运行的房间。
- PostgreSQL 模式在交易成功前提交日志，恢复账户、挂单、冻结资金、时钟、幂等结果和参与者状态。
  托管服务启动时恢复已保存的 Auto 调度器；暂停房间保持暂停，停止的机器人保持停止。
  发现未完成的手动动作时拒绝自动启动，避免跳过恢复步骤。
- 这是独立后端接入，未打包为插件，也未生成 Electron 发布安装包。

## 验证

后端：`cargo test -p exchange-server --lib candlescope_` 与
`cargo test -p exchange-server --lib simulation_ws::tests`。
前端：类型、架构、国际化、ESLint、生产构建，以及 `src/features/simulation/__tests__` 的协议/会话测试。
浏览器闭环还需验证真实创建、撮合、账户更新、撤单、拒单和市场控制；构建成功不替代这一步。

本次已完成的真实闭环与检查范围见 [2026-10-08 验证记录](validation/2026-10-08-candlescope-workbench.md)。

持久化/实时追加验收见 [验证记录](validation/2026-10-08-candlescope-durable-ws.md)。

## 备份

```powershell
.\scripts\backup-candlescope-workbench.ps1
```

生成 PostgreSQL custom archive 到 `.local/candlescope-runtime/backups/`，校验归档可读，不覆盖同名文件。
可指定 `-BackupPath` 放到另一块磁盘。该步骤是手动备份；同盘备份不能替代异机备份。

`verify-candlescope-backup.ps1` 将备份恢复到新建的验证库并启动临时服务，
用 `-RoomId`、`-BaselineDirectory`、`-BackupPath` 指定测试房间、对应的四份
`persistent-before-{observe,clock,orders,candles}.json` 和同时间点归档。
默认参数对应本次验收样本。它保留验证库和证据，不覆盖运行中的数据库。
