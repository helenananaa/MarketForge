# 双向持仓

永续合约核心支持同一账户、同一合约同时持有多仓和空仓。模式按合约配置，在创建房间时确定：

```json
"clearing": {
  "maker_fee_ppm": 0,
  "taker_fee_ppm": 0,
  "leverage": 10,
  "position_mode": "Hedge"
}
```

省略 `position_mode` 时使用 `OneWay`，保持已有净持仓行为。现有房间不会自动改成双向模式；没有运行中切换模式或旧仓迁移接口。

## 下单

双向模式必须提供 `position_side`，不能用默认的 `Both` 猜测用户要操作哪条腿。

| position_side | side | 行为 |
| --- | --- | --- |
| Long | Buy | 开多 / 加多 |
| Long | Sell | 平多 |
| Short | Sell | 开空 / 加空 |
| Short | Buy | 平空 |

普通核心 `NewOrder` 使用上述字段。HTTP `/rooms/{room_id}/orders` 的通用 `PlaceProtected` 和 `PlaceUnboundedMarket` 动作也接受 `position_side`。例如指定最高买价开多：

```json
{
  "participant_id": "human",
  "account_id": 20,
  "instrument_id": "V-BTC-PERP",
  "action": {
    "PlaceProtected": {
      "side": "Buy",
      "position_side": "Long",
      "qty": 5,
      "order_type": "ImmediateOrCancel",
      "price_tick": 100,
      "reduce_only": false
    }
  }
}
```

把方向改成 `Sell + Long` 即平多；`reduce_only: true` 可明确表达只减仓意图。双向模式下平仓不会反手，超过该腿现有数量会拒绝。开仓方向配合 `reduce_only: true` 也会拒绝。

开仓支持限价、PostOnly、市价、IOC/FOK及现有价格/时间保护。平仓沿用现有只减仓限制，支持市价、IOC/FOK，**暂不支持长期挂着的限价/PostOnly平仓单**；价格受限的平仓请使用 IOC/FOK。

Python Agent 的 `trade` 工具同样接受 `position_side="Long"/"Short"`，原有价格保护、期限及重试语义继续生效。网页载入双向房间后显示多仓/空仓选择、开多/平多/开空/平空，以及两条腿的数量和开仓均价；价格受限的平仓自动使用 IOC。

单向永续和现货只接受 `Both`，省略方向与原来相同。旧的简化 `PlaceLimit`/`PlaceMarket` 等动作继续表达 `Both`；双向调用者应使用上述两个通用动作。

## 账户和风控

账户快照包含 `hedge_positions.long` 与 `hedge_positions.short`，各自保留非负数量、开仓均价、已实现盈亏、交易手续费及累计资金费收付。账户级金额继续汇总；强平层手续费仍记在账户级 `fees_paid`。

- `position_qty` 是 `long.qty - short.qty` 的净敞口，不能用于判断双向账户是否空仓。
- 双向模式的旧标量 `avg_entry_price_tick` 为零；必须读取对应腿的均价。
- 未实现盈亏分别计算后相加，保证金按多空总持仓保守收取，没有锁仓保证金折扣。
- `max_abs_position_qty` 在双向模式限制总持仓及待成交开仓量，不允许用净仓接近零绕过限额。
- 多空挂单保证金累加；撤单、改量、部分成交都会更新预留。
- 资金费分别处理支付腿与接收腿，再汇总到账户，沿用守恒舍入规则。净敞口为零仍参与资金费结算。
- 强平按腿减仓；深度不足时保存两条腿的剩余量并继续重试。多仓无买盘时可先处理有卖盘的空仓。两条腿都平掉后才执行累计强平手续费、保险基金、ADL及损失分摊。
- ADL 选择盈利的对应腿，保留同一账户的另一条腿；分配事件带有 `position_side`。

订单簿、成交、引擎快照、日志恢复、HTTP私有结算及历史持仓查询均保留双向信息。旧 JSON 缺少新字段时仍按单向读取；单向数据序列化省略新增默认字段。

既有依赖净库存的策略不能直接用于双向合约。策略需要读取两条腿，并明确指定订单方向；核心会拒绝缺少方向的双向下单。
