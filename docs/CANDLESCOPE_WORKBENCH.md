# CandleScope 仿真工作台

MarketForge 负责房间、撮合、订单、账户、费用、风控、强平、AI 参与者、仿真时钟和持久化。
本项目 `vendor/candlescope` 内复制的源码提供 `/simulation.html`，通过 WebSocket 接收状态，通过 HTTP 提交交易，复用图表、绘图、导出、主题、指标编辑器和订单流面板。
本项目内的 CandleScope Python 分析服务负责内置及 Pyne/Pine 指标计算和脚本目录；MarketForge 提供权威仿真数据。
这个页面不启动 CandleScope Paper broker。分析请求只读取房间，不推进时钟，不修改账户。
启动、构建和修改均在 MarketForge 内完成，不需要 CandleScope 原检出目录。来源版本、文件哈希与许可见 `vendor/candlescope/UPSTREAM.json` 和 `LICENSE`。

## 页面与操作位置

仿真页面直接使用 CandleScope 原有的 `MarketPageFrame`、`MarketTopBarFrame`、`IntervalSelector`、
`MarketChartWorkspace`、`MarketRightRailFrame` 和 `MarketStatusBar`。左侧保留原绘图工具栏，中央图表占满剩余空间，
右侧采用原活动栏与可独立展开、调整宽高的折叠面板。没有另外的三列工作台和底部挂单区。

- `/simulation.html` 先显示身份入口与房间大厅：登录 → 创建并配置 / 选择已加入房间 → 确认服务端角色 → 加入看盘。
- 创建页设置房间名称、Bot 启动方式、调度间隔和完整场景配置。关闭自动启动时，Bot 模板会持久化为停止状态，随后可在管理页启用。
- 独立房间管理页负责成员角色、账户分配、Bot 参数和启停、市场暂停/继续/单步；图表顶部只提供返回大厅与管理入口。
- 交易员右侧只显示获分配账户的资产、下单与委托；盘口价格点击可以填写限价。
- 房主/管理员可查看全部账户并进入管理页。观众拥有全部账户、当前委托和 Bot 参数的只读视图，没有下单与管理入口。
- 周期、指标、绘图、导出和主题仍留在 CandleScope 看盘页，它们属于个人图表设置。指标和绘图的持久化键包括服务端用户 ID，同一浏览器切换身份不会复用另一用户的图表内容。
- 盘口使用原 `OrderBookDock`，逐笔和成交分布使用原 `TradeFlowDock`；市场数据来自当前仿真房间。
- 周期使用原周期工具栏与自定义弹窗，支持固定时长的周期；日历月不能用于仿真时钟。
- 窄屏使用同一侧栏，收起后完整查看图表，点击活动栏图标再次展开操作面板。

页面布局与浏览器验证见 [共享页面验收](validation/2026-10-09-candlescope-shared-layout.md)。

## Windows 启动

从 MarketForge 根目录首次安装依赖，再启动：

```powershell
.\scripts\setup-candlescope-workbench.ps1 -PythonExecutable <Python-3.12-executable>
.\scripts\start-candlescope-workbench.ps1
```

默认启动专属 PostgreSQL，并将 账号版 MarketForge 绑定到 **57307**（`marketforge_competition` 数据库）。需要已安装 PostgreSQL 可执行文件；
不在 PATH 时指定 `-PostgresBin D:\SQL\bin`。也可使用 `-DatabaseUrl` 或环境变量
`MARKETFORGE_DATABASE_URL` 指向现有数据库。显式 `-Memory` 才允许内存模式。
PostgreSQL 数据位于 `.local/candlescope-runtime/postgres`，仅监听 127.0.0.1:55432；
密码随机生成，使用当前 Windows 用户的 DPAPI 保存，数据与凭据均在 Git 忽略目录中。

源码目录固定为本项目的 `vendor/candlescope`；不接受指向原仓库的 `-CandleScopeRoot`。`-SkipBuild` 仅用于已有对应构建产物时；
后端专属构建位于 `target/candlescope-workbench/debug/`。
脚本在空闲端口启动隐藏的后台进程，打印 PID 和页面地址；遇到已有服务时保留该进程。
日志位于 `output/candlescope-workbench/`。不自动创建房间、不自动下单。

脚本同时启动复制源码中的 Python 指标服务（默认 **18086**，与原桌面应用的 18080 分开），使用本项目
`.local/candlescope-runtime/analysis-env` 环境；也可指定 `-PythonExecutable` 和 `-IndicatorPort`。
setup 安装 backend/SDK 依赖，并按复制源码中的版本锁和 SHA-256 校验安装 Pyne/Pine runtime。
分析数据库、插件注册表和下载缓存都保存在本项目 `.local/candlescope-runtime` 下，与原项目隔离。
分析服务使用 LOCAL_OFFLINE 模式，只计算传入的仿真 K 线。
前端通过 Vite `/api` 代理调用该服务；仅复用来自本项目源码目录的服务，端口被其他进程占用时报告错误。

手动运行先配置 `MARKETFORGE_DATABASE_URL` 和 `MARKETFORGE_BIND_ADDR=127.0.0.1:57307` 与 `MARKETFORGE_AUTH_MODE=accounts`，
执行 `cargo run -p exchange-server`，然后在本项目 `vendor/candlescope/frontend` 目录
执行 `npm ci --ignore-scripts`、`npm run build`、`npm run preview`。
分析服务建议通过上述启动脚本运行，以保证数据目录与插件注册表隔离；不能从原仓库启动来代替。
preview 默认代理到 18086；其他指标端口需设置 `VITE_API_PROXY_TARGET`。
打开 `http://127.0.0.1:15173/simulation.html`。

默认账号后台地址为 `http://127.0.0.1:57307`。57306 的开发后台与历史房间继续保留，不自动转移其成员身份。上轮 57305 的内存测试服务未停止，旧房间未迁移到新库；
需要查看时在页面填写旧地址。页面明确显示后端存储模式。
默认 CORS 包含 CandleScope 的 IPv4/localhost 15173
来源；其他来源通过 `MARKETFORGE_CORS_ORIGINS` 显式配置，保留部署现有来源。
远程部署需要 HTTPS 和 Bearer 令牌。连接与令牌保存在当前标签页的 `sessionStorage` 以支持刷新；退出登录会清除，令牌不写入 URL 或 `localStorage`。
本地 `x-user-id` 与 Bearer 二选一，权限由 MarketForge 校验。

## 操作

1. 默认账号模式支持注册、用户名密码登录和退出；角色由邀请码或房间管理员授予。`-AuthMode local-development` 可显式运行原开发身份模式，使用 57306 与 `marketforge_workbench` 数据库。OAuth 与密码重置尚未实现。
2. 房间大厅只列出该用户已获授权的房间。创建房间后，创建者是房主；管理员在管理页添加成员并分配交易账户。用户不能通过加入按钮自行选择或提升角色。
3. 创建配方来自 `GET /scenarios/background-market`，品种、费用、账户资金、Bot 策略等可在创建页的完整 JSON 配置中调整，由服务端校验。已创建市场的合约与初始资金通过新建房间变更，不伪装成可以热修改。
4. 加入确认页显示服务端返回的身份、可见和可交易账户。图表中的账户选择器只包含这些账户；没有账户的成员使用公共行情观测，不创建虚拟资金账户。
5. 房间管理页通过原有接口暂停、继续和单步推进市场、启停和配置 Bot。所有写操作仍受后端管理员/账户归属约束，读行情不推动仿真时间。
6. 周期按钮和自定义输入支持整秒固定周期，从 1s 到 31d，例如 3m、2h、1w；不支持日历月 `1M`。
7. “指标”打开 CandleScope 原有侧栏，可添加内置指标、编辑并保存 Pyne/Pine 脚本，复用叠加线、副图与绘图输出。
   计算范围为当前已加载 K 线；跨品种/跨周期 `request.security` 数据提供器尚未接入。
8. “加载更早 K 线”或向左拖动加载历史，每页最多 500 根，当前窗口最多 10,000 根。
   “逐笔成交”及“成交分布”复用 CandleScope 面板；Delta/CVD 使用后端成交主动买卖方向，不从涨跌猜测。

## 数据与生命周期

- `GET /identity` 确認已验证用户与认证模式；`GET/POST /rooms/{room}/session` 返回房间角色、能力、可见/可交易账户和市场列表，POST 用于加入前再次校验，不授予新角色。
- `GET /rooms/{room}/workbench` 返回经过权限筛选的当前账户与委托；交易员只收到自己的账户，管理员/观众收到全局视图。观众 Bot 模板中的凭据字段会隐藏，私有 Bot 运行状态不向观众发布。
- `GET /rooms/{room}/members` 仅管理员可调用；现有成员与账户分配写接口继续使用原授权机制。
- `GET /rooms/{room}/observe` (`strategy.v1`) 返回选择账户的观测。观众可以只读其他账户；`account_id=0` 专用于公共观测，会递归清除账户、私有委托与 Bot 私有数据。下单仍使用原来的独立账户权限校验。
- 前端每两秒重新读取服务端权限，角色或账户范围变化会重新挂载图表会话；HTTP/WS 返回 401/403 时立即清除旧的账户观测，成员访问撤销后返回大厅。
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
- 实时快照覆盖最近 500 个周期，历史分页使用排他的 `before_open_time_ms`，不会重复游标蜡烛。
  指标按房间/品种/周期保存设置，切换上下文会清除旧计算结果。
- 逐笔与成交分布读取最近 500 笔成交，成交额单位为 tick·lot，时间为仿真耗时；不是完整逐笔归档。
  CVD 按 CandleScope 连续窗口语义计算，缺失买方字段时禁用，不将未知数据补零。
  多图联动、日历月周期和跨数据源脚本查询尚未接入。
- 未配置 PostgreSQL 时，后端为内存模式，退出后房间丢失；页面刷新可重新连接仍在运行的房间。
- PostgreSQL 模式在交易成功前提交日志，恢复账户、挂单、冻结资金、时钟、幂等结果和参与者状态。
  托管服务启动时恢复已保存的 Auto 调度器；暂停房间保持暂停，停止的机器人保持停止。
  发现未完成的手动动作时拒绝自动启动，避免跳过恢复步骤。
- 这是独立后端接入，未打包为插件，也未生成 Electron 发布安装包。

## 验证

后端：`cargo test -p exchange-server --lib`；定向验证使用 `room_portal::tests` 和 `simulation_ws::tests`。
前端：`npm run typecheck`、`npm run build` 和 `src/features/simulation/__tests__`、共享页面测试。
浏览器闭环还需验证真实创建、撮合、账户更新、撤单、拒单和市场控制；构建成功不替代这一步。

本次已完成的真实闭环与检查范围见 [2026-10-08 验证记录](validation/2026-10-08-candlescope-workbench.md)。

身份入口、房间流程、角色隔离、权限撤销和当前浏览器检查见 [房间平台验收](validation/2026-10-09-room-portal.md)。

持久化/实时追加验收见 [验证记录](validation/2026-10-08-candlescope-durable-ws.md)。

指标、订单流、自定义周期和历史接入见 [2026-10-09 验证记录](validation/2026-10-09-candlescope-analysis.md)。
对已暂停的测试房间可重复运行只读一致性验收：

```powershell
.\.venv\Scripts\python.exe scripts\validate_candlescope_analysis.py --room mf-analysis-20261009 --output output\candlescope-workbench\integration-analysis-proof.json
```

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


## 多人比赛平台（competition.v1）

默认入口为 `http://127.0.0.1:15173/simulation.html?server=http%3A%2F%2F127.0.0.1%3A57307`。
`server` 只选择服务地址，不承载密码、令牌或邀请码；同一个后台的会话在当前标签页恢复，切换后台不会向新服务发送旧令牌。

1. 注册/登录后创建房间，完成市场、账户与 Bot 配置。默认背景市场有 20 个 Bot 和 20/30 两个等额人工账户。
2. 暂停市场，在房间管理页设置参赛账户、赛程、倒计时和观众可见范围，锁定规则并开放报名。
3. 管理员生成绑定角色和席位的一次性邀请码。选手登录自己的账号兑换邀请、确认身份并准备，再进入 CandleScope 看盘页。配置比赛前已加入且唯一拥有席位的交易员也会进入名单。
4. 全部席位加入且准备后，管理员启动倒计时。服务端到时恢复市场和 Bot，截止后封闭房间、冻结账户并归档排名。右侧比赛面板同步准备状态、剩余时间与最终结果。

账号密码使用 Argon2id PHC 哈希。随机会话令牌和邀请码只保存 SHA-256 摘要；会话有效 12 小时，邀请码有效 24 小时且只能兑换一次。
退出登录删除服务端会话，HTTP 与 WebSocket 按当前会话重新鉴权。Argon2 运算有并发上限，失败尝试受用户名和实际连接 IP 的速率限制。
单机可创建多个账号分别参赛，不使用客户端传入的 `x-user-id` 认定登录身份；注册用户名不能继承旧 `local-user` 的房间权限。

PostgreSQL 新迁移 0017 保存版本化用户、会话、邀请与比赛记录，并为用户名建立唯一约束。
邀请码消费、成员加入及席位归属在同一事务提交；平台修订号防止丢失更新。
使用现有单活 journal writer，重启加载账号和有效会话，恢复倒计时/开赛/结算过渡及最终结果。比赛绝对截止时间保留，不因重启延长。
`MARKETFORGE_SHUTDOWN_FILE` 可指定尚不存在的本机绝对路径；创建该文件触发正常排空和退出，用于受控后台管理与重启验证，不暴露 HTTP 停机接口。

当前评分 `equity-return.v1` 支持单市场现货或永续的场内账户，要求参赛账户资金、持仓、费用和保证金初始状态一致且没有挂单或场外资产。
现货按收盘双边盘口中价估值，无双边盘口时使用最后成交价，再回退开赛参考价；永续使用清算引擎标记价权益。手续费已计入账户余额。
使用精确整数收益排序，同收益并列；初始/最终权益、收益、评分版本、结算价格和成交游标持久化，完成后禁止重算或继续交易。
挂单在封闭房间内保留供审计，冻结资产仍计入总权益；不把未成交订单当成成交。管理员负责组织，不能操作参赛账户；比赛建立后成员、Bot、注资、转账和市场时钟不能通过旧接口改写。
观众默认只看公开行情，管理员可在开赛配置中允许观众查看全账户；赛后开放全账户和结果。选手始终只读写自己的账户。

接口：`GET /auth/config`、`POST /auth/register|login|logout`、`GET /auth/me`；
`POST /rooms/{room}/invitations`、`POST /invitations/redeem`；
`GET|POST /rooms/{room}/competition`、`POST /rooms/{room}/competition/ready|start|abort`。
`GET /rooms/{room}/workbench` 附带角色适配的比赛概览。

这一版提供实际多人比赛闭环。账号/比赛服务暂不支持 room-leased 多节点运行；公共互联网发布仍需 HTTPS、指标服务隔离、备份、监控和并发容量验证。
跨市场资产组合评分、赛季系统、邮箱验证/OAuth/密码重置、独立观赛延迟与申诉裁判不是本次已完成能力。
本机指标服务仍是受信任的个人脚本环境，不能未经隔离直接面向不可信参赛者开放执行能力。
