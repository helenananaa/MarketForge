# CandleScope 分析能力接入验收（2026-10-09）

本记录描述迁移前的功能验收。用户指出原仓库修改越界后，已将源码和运行环境迁到 MarketForge，
并撤回 CandleScope 中的接入改动。当前边界与独立运行验证见 [源码迁移记录](2026-10-09-candlescope-source-boundary.md)。

## 分工与实现

MarketForge 继续负责交易、房间、账户、背景参与者、成交数据、时钟与 PostgreSQL。
CandleScope 复用原有指标服务、内置指标、Pyne/Pine 运行时、指标侧栏、编辑器、主图/副图及订单流面板。
没有在 Rust 后端复制指标引擎。

- 工作台可加载完整背景市场配方并编辑 JSON，再创建房间；插件及内置参与者的房间身份均随新名称重绑。
- 固定整秒周期支持 1s 至 31d，包含自定义 3m、2h、1w；不解释为日历月。
- K 线增加可选主动买入数量/成交额，兼容旧 JSON；Delta/CVD 从真实主动方向得到。
- WS 支持上述固定周期。HTTP 历史使用排他游标和最多 2,000 的显式 limit，前端每页 500、总窗口 10,000。
- 修复 PostgreSQL 聚合按文本 bucket 排序的问题，改为按原始 numeric bucket 排序，避免长房间快照乱序。
- 仿真图表提供 CandleScope 所需的数据归属元信息，指标结果可真实渲染，不只是计算接口返回成功。
- Monaco 编辑器及 worker 从本地生产包加载，固定 monaco-editor 0.56.0，免除仿真页面的外部 CDN 依赖。
- 启动脚本增加 CandleScope Python 指标服务及 API 代理，并在启动后检查预设接口；复用运行中的服务不停止其他进程。

当时改动分布于 MarketForge 和 CandleScope 两个检出目录；该布局已被后续项目内源码副本替代。未进行 commit/push。

## 静态与自动化验证

- `cargo test -p exchange-core candles::tests`：3/3。
- `cargo test -p exchange-server --lib`：171/171。
  第一次有三项 Python 进程测试因 PATH 缺少 Python 而返回 9009；加入本项目 `.venv/Scripts` 后全量重跑通过。
  部分 PostgreSQL 测试未配置专属测试环境时会自行短路，因此下列真实 PostgreSQL 验收单独记录。
- `cargo clippy -p exchange-core -p exchange-server --all-targets -- -D warnings`、`cargo fmt --all -- --check`：通过。
- CandleScope 仿真协议、HTTP、WS、分析适配测试：20/20，包括 0 号成交、旧字段缺失、混合主动方向、自定义周期及排他历史游标。
- 原有窗格归属、订单流投影、成交分布回归测试：13/13。
- 浏览器/Node TypeScript、架构检查、ESLint、国际化检查及生产构建：通过。
  国际化覆盖 30 locales、4,781 catalog keys；构建保留大型 vendor-editor chunk 的体积提示。

## 真实 PostgreSQL 与指标一致性

环境：MarketForge `127.0.0.1:57306`、CandleScope Python `18080`、生产 preview `15173`。
样本为本次创建并暂停的 `mf-analysis-20261009`，仿真时钟 801,000ms，527 根 1 秒成交 K 线。

只读验收脚本：`scripts/validate_candlescope_analysis.py`。
证据：`output/candlescope-workbench/integration-analysis-proof.json`。

| 固定周期 | K 线数 |
|---|---:|
| 1s | 527 |
| 3s | 265 |
| 1m | 14 |
| 3m | 5 |
| 5m | 3 |
| 15m、1h、2h、4h、1d、1w | 各 1 |

所有周期的数量、成交额、成交笔数、主动买入数量和主动买入成交额与同一份 1s 数据聚合完全相符。
K 线严格按时间递增；排他历史游标返回预期的最后 7 根较早 K 线。
内置 MA、Pyne SMA、Pine SMA 均输出 525 点，最后时间 1704067999、最后值 110，与源数据手算一致。
计算与历史读取前后房间时钟相同。

## 浏览器操作闭环

使用隔离 Chrome 与实际生产页面，不以 mock 数据代替撮合。

- 连接暂停房间后，1s 历史从 309 根加载到全部 527 根。
- 1m/5m/15m/1h/4h/1d/1s 按钮和自定义 3m 均连接成功；3m 显示 5 根。
- 逐笔面板显示真实成交（虚拟列表当前渲染 72 行）；成交分布聚合最近 500 笔，其中主动买 255 笔、主动卖 245 笔。
  成交额显示 tick·lot，时间显示仿真耗时。
- 在本地 Monaco 内编辑 Pyne SMA(3)，运行到图表并保存；当前加载窗口输出 307 点。
  人工查看截图确认 SMA、自定义均线、MACD 副图和 Delta 柱状图实际绘制。
  CVD 使用原有连续末尾窗口规则；1s 样本中间有无成交空档，最新有效连续后缀只有两根，不展示跨空档累计值。
  1m 窗口的连续 CVD 曲线及主动买/卖、Delta 柱均已截图确认。
- 390×844 视口下文档宽度为 390，无横向溢出；桌面/移动截图已检查。
- 从配方编辑器创建 `mf-analysis-trade-20261009`，20 个背景参与者。
  市价买入 1 lot，持仓 20→21；随后限价 1 tick 买入并撤销订单 `12944`，挂单消失；最后暂停房间。
- 启动脚本在运行中的三项服务上执行 `-SkipBuild` 成功，返回正确页面、PostgreSQL 存储与指标端口。
- 最终生产包重新加载、连接和周期切换通过，浏览器 `pageerror` 数为 0。

本地证据文件（Git 忽略）包括 `integration-browser-proof.log`、`integration-trade-proof.log`、
`integration-render-proof.log` 及 `output/playwright/candlescope-workbench/analysis-{desktop,mobile}.png`。

## 明确边界

本次完成当前单图仿真工作台的分析接入，不能等同于 CandleScope 全部产品能力已迁移。
跨品种/跨周期 `request.security` 提供器、多图联动、日历月周期、完整逐笔历史/全场 Footprint 和盘口热力图尚未接入。
脚本使用安全模式，运行语言依赖 CandleScope 已安装的 runtime；不提供任意 Python 主机扩展执行。
房间 JSON 配置用于创建阶段，已有房间的全部运营/机器人管理不等同于可在此编辑器热修改。
未生成 Electron 安装包，也未进行远程部署或长期负载认证。
