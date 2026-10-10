# CandleScope 源码与运行边界纠正（2026-10-09）

此前直接修改外部 CandleScope 检出目录超出了 MarketForge 接入范围。现已将所需源码复制进本项目，撤回外部目录中的接入改动，后续修改、构建和运行使用本项目副本。

## 源码归属

- `vendor/candlescope` 包含前端、后端分析代码及 SDK/插件源码，基于上游提交 `1a2a0189ceb45e188eecab2a4927e07a3f5d20c6`。
- `UPSTREAM.json` 记录 3,180 个导入源码文件的来源与初始摘要；本地接入代码在此副本继续维护。
- 保留上游 GPLv3 许可证和包内声明。没有复制外部环境、数据库、缓存或私有配置。
- 外部检出目录恢复了本次接入涉及的 35 个已跟踪文件，并移走 16 个接入新增文件。操作前的补丁与文件备份保留在 `output/candlescope-workbench/source-boundary-backup`。
- 撤回时检测到其他任务正在修改 CandleScope 后端；这些无关改动保持原样。外部前端接入 diff 已为空，不声称整个外部仓库干净。

## 运行归属

启动脚本固定使用 `vendor/candlescope`，不再接受外部 CandleScope 源码目录。Python 环境、插件安装、下载缓存、指标目录与数据均使用 MarketForge 的 `.local/candlescope-runtime`；Node 依赖在项目副本内安装。

项目副本通过 `app.marketforge_analysis` 组合上游指标路由与原有 Pyne/Pine 运行时，只提供分析服务。MarketForge 继续负责房间、账户、撮合、成交与仿真时钟。分析 API 使用 18086 端口，前端使用 15173，MarketForge 使用 57306；分析服务的健康检查返回本项目源码路径，启动器拒绝复用来自外部目录的分析服务和前端进程。

准备和启动命令见 [工作台说明](../CANDLESCOPE_WORKBENCH.md)。

## 迁移后验证

- 项目副本前端类型检查、架构检查、国际化检查、生产构建和相关 lint 通过；仿真及共享面板测试 33/33。
- 分析服务组合测试 1/1：传入 K 线可计算 SMA，服务没有挂载行情和房间路由。
- 独立安装并启动 Pyne/Pine 插件，健康检查显示两个运行时 ready、无失败。
- 对暂停的 `mf-analysis-20261009` 房间验证 527 根原始 K 线、11 个固定周期聚合、排他历史分页及主动成交量/成交额。内置 SMA、Pyne SMA、Pine SMA 各返回 525 个一致结果，末值 110；分析前后仿真时钟均为 801000 ms。
- 浏览器验证历史由 309 根扩展至 527 根，固定周期及自定义 3m 可切换，成交分布显示 500 笔成交，逐笔面板显示真实成交，自定义 Pyne 编辑器返回 307 个指标点并在图表显示；390 px 手机视口无横向溢出。

机器验证记录位于 `output/candlescope-workbench/boundary-source-proof.json`、`boundary-analysis-proof.json`、`boundary-browser-proof.log`，截图位于 `output/playwright/candlescope-workbench/boundary-analysis-*.png`。迁移前验收见 [原功能记录](2026-10-09-candlescope-analysis.md)，其中外部源码和旧服务布局已被本记录替代。本次未进行 commit/push。
