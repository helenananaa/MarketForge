# 2026-10-08 交易 bot 插件化验证

状态：插件接入、交易、状态恢复和批量评估验收通过。基于 `888b2bb` 的工作区实现，尚未提交。

## 实现范围

- `exchange-core/src/bots.rs`：宿主无关注册表、工厂、参数描述和校验、`bot.v1` 请求/响应、可恢复状态和宿主动作策略接口。
- 内置五种 bot 注册适配；调度器移除具体 bot 类型分支。兼容已有模板 JSON，新增统一 `Plugin` 配置。
- `exchange-server/src/bot_plugins.rs`：启动时加载本地可信包目录；独立进程决策、有限输入输出、超时、版本/动作数量校验、错误处理和 Unix 进程组回收。
- `GET /bots`、房间 admin `GET /rooms/{id}/bots`；沿用实例启动/状态/停止接口。初始配置先持久化，再执行首个步骤；新增实例保留原有相同配置实例的状态。
- 调度执行接入训练方向与容量约束，逐动作记录盘口和成交证据；批量插件评估使用手动仿真步骤，关闭默认买入策略。
- 前端从描述生成 bot 选择/参数表单，支持多个实例和启停、错误展示、保存列表重新载入；CLI 插件列表和 Python SDK 管理方法。
- `bot-plugins/buy-remaining`：仅 Python 标准库的可运行示例。
- 接入说明：`docs/BOT_PLUGINS.md`；协议说明同步到 API/runtime contract。

## 检查结果

| 检查 | 结果 | 证据 |
|---|---|---|
| `cargo fmt --all -- --check` | 通过 | 终端执行结果 |
| `git diff --check` | 通过 | 终端执行结果 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 | `target/bot-clippy.log` |
| 强制 PostgreSQL Rust 工作区测试，`--test-threads=1` | 330 通过，0 失败 | `target/bot-workspace-postgres-tests.log` |
| 服务端/CLI 构建 | 通过 | `target/bot-build.log` |
| 强制 PostgreSQL Python 全量测试 | 23 通过，0 跳过 | `target/bot-python-postgres-tests.log` |
| 前端 TypeScript/Vite 构建 | 通过 | `target/bot-web-build.log` |
| Chromium 实际操作 | 通过，0 浏览器异常 | `target/bot-browser-test.log` |

PostgreSQL 使用本次单独创建的 `postgres:16-alpine` 容器，数据库 `marketforge_bot_validation`，仅绑定 loopback 57539；验证后停止并自动移除。设置 `MARKETFORGE_TEST_DATABASE_URL` 和 `MARKETFORGE_REQUIRE_POSTGRES_TESTS=1` 防止静默跳过，并设置插件目录，保证同一数据库中的插件恢复配置可被后续测试载入。

首次默认并行运行时，一个恢复测试在读取同一测试数据库的其他房间时，观察到 status journal 与 rooms 表不同步（另一个测试正在修改该房间）。最终使用隔离数据库和串行工作区测试，完整通过。初次 Python 合并回归未给旧测试进程设置插件目录，恢复已保存的插件房间时被正确拒绝为 `unknown bot`；设置相同安装目录后完整通过。这两次失败未作为通过证据。

## 新增验收覆盖

1. 五种内置 bot 接受统一配置、旧枚举和状态仍可反序列化。
2. 参数缺失、类型、范围、choices、未知字段；未知/重复 ID、插件与状态身份/版本不一致。
3. 示例独立进程真实交易；持仓与策略状态正确延续。
4. 决策保存后、动作提交前/后恢复，不再次决策、不重复提交。
5. 非 JSON、缺字段、多 JSON、非零退出、输出超限、协议不匹配和动作超限均拒绝；超时终止进程，策略状态不变。
6. HTTP 初始配置在首次步骤之前可读取；非法配置不创建房间/训练任务。
7. 真实 PostgreSQL 服务端重启，状态、分数和幂等步骤保持一致。
8. 两个种子并发批量评估，分数仅由配置的插件产生；未显式填写 seed 时仍派生 child seed，版本/参数影响实验 digest。
9. 启停、增加实例保持已有状态；前端选择插件、填写参数、两个实例启动/停止、页面刷新后载入列表。
10. 训练账户 bot 不允许卖出或超出剩余买入容量。

## 边界

- 新增独立进程插件无需改核心或重新编译，但安装目录在服务端启动时读取，需要重启加载。
- 每次决策启动一次进程；策略数据须显式保存到返回状态。当前不是常驻模型服务或热升级方案，执行步骤仍串行。
- 自动 worker 在服务端重启后需要显式启动；保存的配置与状态已恢复，手动步骤可以直接继续。
- 进程插件是运营者安装的可信代码；进程隔离不是系统权限沙箱。Unix 提供进程组回收，其他平台只终止直接子进程。
- 跨服务端进程恢复需要持久化 journal。现有 JSON 配置兼容；Rust `into_participant` 返回值变为 `Result`，插件使用宿主注册表。
- 原有 README 工作区修改未更改。未提交、推送或部署。
