# CandleScope 共享页面布局验收（2026-10-09）

此前仿真页单独编写了顶部连接表单、三列工作台、横向绘图工具栏和底部订单区，没有满足直接复用 CandleScope 页面布局的要求。现已删除这些布局，所有更改位于 MarketForge 内的源码副本。

## 实现

页面使用原有 MarketPageFrame / MarketTopBarFrame / IntervalSelector / MarketChartWorkspace / MarketRightRailFrame / MarketStatusBar，图表继续使用 SingleChartPanes，左侧使用原 DrawingToolbar。右侧折叠面板承载房间连接与配方、下单与账户、盘口、挂单、逐笔成交、成交分布及市场控制。

盘口数据通过薄适配器进入原 OrderBookDock；保留主动方向颜色、价量、价差及展示聚合。点击价格支持键盘和鼠标，填入右侧限价单。当前房间推送频率由 MarketForge 决定，因此不显示不能实际调整的盘口频率选择器，也不开放缺少数据源的连续深度模式。指标引擎、订单执行和仿真时钟的归属不变。

simulation.css 只负责新增操作表单、数据适配提示和窄屏侧栏展开；不覆盖原绘图工具栏方向、页面网格或原盘口/订单流样式。新增 simulation source 标记只改项目副本中的共享组件类型。

## 验证

- 类型检查、相关 lint、生产构建、架构检查及 30 语言国际化检查通过。
- 仿真、共享页面与盘口相关测试 53/53，其中新增盘口适配测试覆盖模拟时间、空盘口和单边盘口。
- 1440×1000 视口实测：顶部 48 px，周期栏 36 px，绘图栏 40×892，图表 1033×892，右侧栏约 367×892，底部状态栏 24 px。整页滚动高度为 1000，交易控件全部位于原右侧栏内。
- 浏览器原生自定义周期弹窗添加 3m 并切换，房间投影显示 5 根 K 线；1m 显示 14 根。成交分布显示 500 笔，逐笔面板显示 72 条虚拟化记录。页面没有 JavaScript pageerror。
- 桌面与 390×844 窄屏检查包含侧栏收起/展开、限价输入和盘口点价填入；证据保存在 `output/candlescope-workbench/shared-layout-*-proof.log` 及 `output/playwright/candlescope-workbench/shared-layout-*.png`。

本次页面验证使用暂停的验收房间。交易行为沿用已有协议和会话实现，布局验证不修改该房间的时钟或账户。本次没有提交或推送。
