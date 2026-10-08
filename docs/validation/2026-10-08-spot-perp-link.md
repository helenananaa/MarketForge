# 现货与永续绑定验收 — 2026-10-08

实现与使用说明见 [SPOT_PERP_LINK.md](../SPOT_PERP_LINK.md)。本轮实现同交易所、同资产对的显式价格绑定，包括现货指数、永续标记价、保证金与自动强平联动，以及快照和日志恢复。网页可以创建绑定市场、切换品种和按选定品种下单。

## 自动检查

- `cargo test --workspace`：验证时工作区共 370 个测试通过，零失败、零 ignored；209 core、150 server、2 插件库、3 SDK contract、6 provider contract。日志在 `target/price-link-validation/workspace-tests.log`。
- 本轮新增 12 个用例：8 个价格绑定核心用例、1 个做市商用例、3 个 HTTP/日志恢复用例。覆盖配置拒绝、首次价格门槛、独立永续订单簿、行情过期与不可用、成交价回退、价格网格、多合约联动、拒单原子性、快照与确定性重放、自动强平、reduce-only、做市商报价中心及过期撤单、公共 API 回执与日志篡改拒绝。
- `cargo clippy --workspace --all-targets -- -D warnings`：通过；日志在 `target/price-link-validation/clippy.log`。
- `npm.cmd --prefix marketforge-web run build`：TypeScript 与 Vite 构建通过。
- 新增 Python 配方及修改的 arena 脚本通过语法检查；绑定配方支持 dry-run、导出与 HTTP 创建。
- `git diff --check` 通过。本轮新增 Rust 文件及已修改核心文件的独立格式检查通过。最终全工作区格式检查受到其他并行任务尚在编辑的 `simulation_ws` 模块影响；没有为此改动其他任务的代码。

Windows 中系统的 `python3` 别名指向 Store stub，初次测试导致三个已有进程插件用例失败。验证使用任务目录中的 `python3.exe` 转发器，调用仓库 `.venv/Scripts/python.exe` 后全套通过；未修改产品行为或跳过这些用例。

## 真实 HTTP 与浏览器检查

使用独立的内存后端 `http://127.0.0.1:57546` 和 Vite preview `http://127.0.0.1:15177`，保留原有 57305 服务。浏览器通过 Playwright 操作实际网页：

1. 创建 `linked-ui-final` 房间，启用 20 个现货背景交易者及 3 个永续做市商。
2. 切换 `V-BTC-PERP`，显示关联 `V-BTC-SPOT` 的指数、标记价与联动状态。
3. 账户 20 买入并成交 1 张永续，永续持仓变为 1、入场价 104；现货持仓仍为 20。
4. 后续现货行情变化推动永续标记价、未实现盈亏与做市商报价变化。永续成交由独立订单簿产生。

公共行情、账户快照和事件记录保存在 `target/price-link-validation/live-ui-receipt.json`，网页截图在 `output/playwright/spot-perp-linked.png`。截图中图表仍是原有占位图，不能作为真实 K 线验收证据。

两个脚本均通过实际 HTTP 创建检查：

```powershell
python scripts/linked_market.py --base-url http://127.0.0.1:57546 --room linked-script-check
python scripts/agent_arena.py --base-url http://127.0.0.1:57546 --room linked-arena-check --traders link-trader-one
```

浏览器测试发现固定永续种子挂单会阻碍做市商跟随指数重新报价；新绑定配方已移除这些种子，全部永续报价由做市商管理。配置正确后完成品种切换和下单流程；初始默认 URL 指向原有服务时的两个 CORS 错误仍留在浏览器历史中。

## 验收边界

- PostgreSQL 条件测试在没有配置数据库时可以提前返回；本轮没有重新执行 PostgreSQL 故障矩阵。上述恢复证据来自确定性日志与快照用例。
- 本轮全套测试与 Clippy 记录对应运行时的工作区；其他任务正在同仓库继续修改 WebSocket、Pine 和工作台功能，这些后续变更需要各自验收。
- 需要运行新后端并创建新房间，已有房间不会自动迁移。未替换原有运行服务。
- 尚未实现资金费率、跨交易所指数、基差平滑或自动套利。既有挂单在指数失效时没有新增撮合暂停规则，规则只限制新增风险订单与改单。
