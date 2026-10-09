# 2026-10-09 多人比赛平台验证

## 已完成

- 注册、Argon2id 密码哈希、用户名密码登录、12 小时会话、退出撤销、服务端 HTTP/WS 身份验证。会话和邀请码存储摘要。
- 绑定角色和人工账户席位的一次性邀请；邀请消费、成员和账户授权在 PostgreSQL 同一事务提交。
- 准备、倒计时、服务端开赛、绝对截止、账户冻结、精确收益排名和不可改写的归档结果。复用原撮合、Bot、journal 和 CandleScope 复制源码的看盘框架。
- 创建比赛后冻结旧管理/注资/转账/时钟/手动价格接口；管理员不参与交易，选手只访问自己的账户；观众可见范围在建立比赛时配置。
- 0017 新迁移，已有迁移保持不变；账号/比赛状态使用单活数据库写入通道，开赛和结束过渡可重试。
- 同服务刷新恢复会话，过期/撤销会话退回登录；切换服务地址不携带旧令牌。旧房间平台可继续使用开发身份入口。

## 自动验证

- `cargo test -p exchange-server --lib --target-dir target/candlescope-portal`：186 passed。
- Simulation 前端协议、会话、行情、盘口及入口测试：25 passed。
- 前端 typecheck 和 Vite production build 通过。复制的 CandleScope 仍有原有大 chunk 提示。
- 新增关键测试：密码登录/重复用户名/退出/到期；邀请码并发单次兑换、不可修改绑定角色；过期邀请不授予权限；比赛设置前已入场的交易员保留席位；准备 gate、截止拒单、管理员拒单、私有账户隔离、结果与会话恢复。

## 实际运行

- 新后台：127.0.0.1:57307，账号模式，实际 PostgreSQL `marketforge_competition`，同项目私有 55432 集群。
- 原 57306 服务及其 `marketforge_workbench` 数据库保留，未迁移旧开发成员权限，也未改动外部 CandleScope。
- `scripts/validate_competition_platform.py`：4 个正式注册测试账号、2 位选手、20 个 Bot，一场 10 秒比赛完成；验证真人成交、开赛与截止拒单、冻结管理接口、观赛权限、退出令牌失效。
- 独立 Playwright 浏览器 context：房主通过页面创建并配置房间，生成邀请；两位交易员和观众分别密码登录和兑换邀请；选手准备后进入看盘，房主启动倒计时。
- 浏览器第二场 20 秒比赛：账户 20 市价买入 1，持仓 20→21；账户 30 市价卖出 1，持仓 20→19。三方页面显示相同最终排名：乙 12000，甲 11999。比赛操作与结果使用原 CandleScope 右侧栏。
- 浏览器另测注册、退出、再次密码登录。
- 最终构建刷新后，房主、两位选手和观众仍显示相同的归档排名；没有遗留错误提示。另从服务端撤销选手会话，页面自动返回登录、清空 sessionStorage 会话，并移除私有图表与账户内容。旧 57306 开发入口可检测并登录。
- 对本轮新建测试后台用私有停止标记触发正常排空退出，再启动最终构建；账号、有效会话和两场比赛结果保持。HTTP proof 的 `restart_verified` 为 true；结算价格和结果逐项相同。

## 证据

- `output/candlescope-workbench/competition-http-proof.json`：实际比赛、权益、成交游标、重启结果；无密码、会话令牌或邀请码。
- `output/candlescope-workbench/competition-server-tests.log`、`competition-frontend-tests.log`、`competition-typecheck.log`、`competition-frontend-build.log`。
- `output/candlescope-workbench/competition-browser-join.log`、`competition-browser-signup.log`：浏览器过程，密码/令牌已脱敏。
- `output/playwright/candlescope-workbench/competition-player-20.png`、`competition-player-30.png`、`competition-admin-results.png`。
- `output/playwright/candlescope-workbench/competition-login-final.png`：最终正式账号入口。
- 仅生成的测试账号凭据保留于被 Git 忽略的 `.local/candlescope-runtime`；未把凭据写入源码、公开结果或 URL。

## 边界与运行切换

- 单市场现货/永续场内权益评分已实现；实际浏览器比赛证据为现货，不等于多市场组合评分或互联网发布资格。
- 未完成跨市场计价、赛季系统、密码重置、OAuth、独立观赛延迟、申诉裁判或 room-leased 多节点账号服务。
- 公网仍需 HTTPS、备份/监控、比赛与指标脚本服务隔离和容量验证。本机指标服务属于受信任脚本环境。
- 原后台进程的切换被自动审批拦截，工具仅返回 `blocked by policy`；因此保留原服务，在新端口和新数据库验证。没有重试终止原进程。
- 本轮新服务支持受控停止标记，因此仅重启本轮创建的验证服务，验证正常排空和恢复。

未提交、未推送。
