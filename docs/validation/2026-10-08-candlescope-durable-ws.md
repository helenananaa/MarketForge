# CandleScope 持久化与实时通信验收（2026-10-08）

本轮接入采用独立 MarketForge 服务。撮合、账户、风控与交易确认仍由 MarketForge 负责；
CandleScope 接收经过账户授权的完整状态，通过原有 HTTP 幂等交易入口提交写操作。
初版浏览器交易闭环见 [上轮记录](2026-10-08-candlescope-workbench.md)。

## 当前运行方式

- 页面：`http://127.0.0.1:15173/simulation.html`。
- 持久化后端：`http://127.0.0.1:57306`，PostgreSQL 18.4 数据库 `marketforge_workbench`。
- 项目 PostgreSQL 仅监听回环地址 55432，数据位于 `.local/candlescope-runtime/postgres`。
  随机密码使用当前 Windows 用户 DPAPI 保存，不进入源码、日志或页面 URL。
- 原 57305 内存测试服务及其房间保留；旧内存房间没有自动迁移到数据库。
- 已验证的当前服务产物位于 `target/candlescope-workbench/debug/`。
  当前演示背景房间 `cs-persistent-20261008` 已暂停，避免无人操作时继续推进。
- 启动、备份与恢复验证命令见 [工作台说明](../CANDLESCOPE_WORKBENCH.md)。

## 已实现的行为

1. 默认启动数据库模式，交易日志提交后才确认写入。托管服务启动恢复 Auto 调度器，
   保留房间暂停状态和机器人启用状态；检测到未完成手动动作则拒绝自动启动。
2. WS 首条消息认证，校验 Origin、账户权限、房间与周期；令牌不放 URL。
   250 ms 检查状态变化，空闲 5 s 心跳。完整观测与 K 线时钟一致；数据库读期间状态改变则重采样。
3. 前端检查序号、所有权、时钟与大整数精度。断线禁用交易，退避重连，12 s 无消息判定超时；
   401/403 停止 WS 重连。HTTP 回退正常可用时可恢复交易，失败写入不自动重试。
4. WS 正常期间每 30 s HTTP 核对，持续快照不会推迟核对；旧 HTTP 结果不能覆盖新的 WS 状态。
5. 手动 custom-format 数据库备份，拒绝覆盖已有文件；恢复验证使用新数据库，不替换运行库。

## 真实服务与浏览器证据

| 场景 | 结果 |
| --- | --- |
| 外部 HTTP 客户端市价买入 1 lot | 未手动刷新，WS 更新账户：现金 10000 → 9898，持仓 20 → 21 |
| 前端限价买入 1 tick × 2 lots | 生成挂单 10019，冻结现金 2 |
| 强制结束所属持久化服务 | 前端显示连接失败并禁用下单 |
| 重启服务 | observe、clock、orders、candles 四份原始 JSON 与停机前逐字节一致；页面自动重连 |
| 重复原有幂等交易请求 | 返回原 command_seq=2，余额与订单没有重复改变 |
| 恢复 Auto 背景房间 | 20 个参与者保留；暂停状态保留；点击继续后正常推进，无需重新启动机器人 |
| pg_dump → 新验证库 → 临时服务 | 同一四份 API 原始响应逐字节一致；临时服务已停止，验证库保留 |
| 浏览器 WebSocket 构造失败 | 自动进入 HTTP 回退；恢复 WebSocket 后自动恢复实时通信 |
| 恢复后前端撤单 | 挂单 10019 消失，冻结现金归零；现金 9898、持仓 21 |
| 1600×1000 / 390×844 页面 | 已查看截图；窄屏 document/app 均宽 390，无横向溢出 |

验收账本房间为 `cs-persistent-ledger-20261008`，使用无自动参与者配方以避免核对时行情漂移。
故障演练产生的连接拒绝与重连日志是预期证据，未宣称浏览器零错误。

本地证据位于 `output/candlescope-workbench/`：

- `persistent-before-{observe,clock,orders,candles}.json` / `persistent-after-*.json`。
- `restart-hash-proof.json`：四份停机前后响应 SHA-256 一致。
- `backup-restore-proof.json`：归档 `workbench-20261008-152557-706.dump`，
  恢复库 `marketforge_restore_734caf3dae`，四项响应全部匹配。
- 图像：`output/playwright/candlescope-workbench/durable-ws-{desktop,mobile}.png`。

归档是撤单之前的样本，不能拿撤单后的运行库状态与该归档混为同一验收时间点。

## 自动检查与边界

- 本轮新增后端真实 WS/服务测试 **5/5** 通过：认证、Origin、快照、权限撤销、恢复与手动动作保护。
- 前端协议、WS、会话及图表源生命周期测试 **21/21** 通过，含持续推送下的周期核对。
- 前端类型、ESLint、架构、插件平台、国际化和生产构建检查通过。
- 四个 PowerShell 脚本语法解析通过；初始化/复用数据库、启动、实际备份与实际恢复均已运行。
- 配置真实 PostgreSQL 并要求数据库测试的全仓运行，**375 个单元/集成测试通过**；
  同次命令的最终 doctest 编译被并行写入的资金费率改动打断。
  最新全仓 clippy 同样被资金费率新增字段与分支尚未补齐阻塞，不能记为全仓检查通过。
  当前运行服务使用该组并行改动写入前、本轮已构建并验证的产物。
- 日志：`ws-backend-tests.log`、`ws-frontend-tests.log`、`persistent-workspace-tests.log`、
  `ws-backend-clippy.log` 与 `ws-frontend-*.log`。

本轮实测交易账本为 Spot。没有新增验证永续合约、资金费率或强平完整链路。
WS 采用有界完整快照（最近 500 个 K 线周期），未做大规模连接负载或长期稳定性资格验证。
早期历史分页、多图联动、指标计算、Electron 发布包、异机备份与高可用部署仍未完成。
这是本地可运行工作台和故障恢复验收，不是完整生产部署资格证明。
