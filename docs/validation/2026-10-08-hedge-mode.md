# 双向持仓本地验证（2026-10-08）

已实现合约级 `PositionMode::Hedge`，默认仍为 `OneWay`。核心、HTTP通用下单动作、Python交易工具及网页交易面板均可指定多空方向。使用方式及边界见 [HEDGE_MODE.md](../HEDGE_MODE.md)。

## 验证结果

- `cargo test --workspace --quiet`：测试运行器报告 446 passed、0 failed（核心269、服务器166、CLI2、适配器3、provider contract6；其余目标及文档测试无用例）。
- `cargo clippy --workspace --all-targets -- -D warnings`：通过。
- `cargo fmt --all -- --check`：通过。
- `git diff --check`：通过。
- `.venv/Scripts/python.exe -m unittest python.tests.test_agent_runtime python.tests.test_agent_orders_policies`：46个用例，44通过、2跳过。
- `marketforge-web` 中 `npm run build`：TypeScript及Vite构建通过。

Rust测试命令的PATH前置仓库 `.venv/Scripts`，使已有进程插件用例调用真实的 `python3.exe`。首次未调整PATH的全量运行有三个旧插件用例因 WindowsApps 的 `python3.exe` 占位程序返回9009而失败；使用仓库环境后全量通过。

## 新增覆盖

`exchange-core/src/hedge_tests.rs` 的18个测试覆盖：

- 同一账户多空独立数量/均价、零净仓总保证金及未实现盈亏。
- 指定腿平仓、不抵消另一腿、不反手；缺方向、超量平仓及开仓只减仓意图拒绝。
- 总持仓限额、多空挂单保证金相加、撤单及改量释放预留。
- 第二条腿不能借减少净敞口绕过保证金；保证金不足时仍可安全减仓。
- 部分maker成交、仓位方向进入成交记录、快照恢复及交易日志重放。
- 正负资金费逐腿支付/接收、现金守恒及全市场只有零净仓锁仓账户时的结算。
- 双腿部分强平及恢复、无多仓深度时先平空、最后一次统一结算强平手续费。
- ADL选择盈利腿并保留另一腿、零净仓账户的坏账ADL及现金/净仓守恒。
- 各腿均价加权和交易费用归属、FOK深度不足不改变仓位。
- 旧JSON默认单向及默认字段省略。
- 房间通用网关和实际房间自动强平：标记价变化导致锁仓总抵押不足时，保留不足状态并进入双腿强平流程。

`exchange-server/src/hedge_tests.rs` 的两个测试覆盖真实Axum路由下单、HTTP私有结算、历史持仓查询及拒绝回执，以及内存journal恢复两条腿和挂单方向。

Python新增测试覆盖交易工具把方向传到有价格保护和显式无价格限制的动作，以及可选持仓策略按双腿总量计限额。

## 验证边界

以上为本地自动化验证。未在本次任务中运行连接真实PostgreSQL的双向历史查询验证，也未执行网页人工交互验收；SQL查询已按已有私有JSON结算记录读取双腿信息，不需要新增数据库列。未重启现有服务、迁移已有房间、提交或推送。

双向模式按新建合约配置确定；平仓沿用非长期挂单的只减仓限制（市价/IOC/FOK）。既有净库存策略需要明确适配两条腿；缺少方向的双向订单由核心拒绝。
