# 工作区提交前复核（2026-10-09）

本次整理当前 MarketForge 工作区：市场行为和微观结构、双向持仓及订单保护、风险与调度增量优化、Agent 框架与策略工具、账号和比赛平台，以及项目内 CandleScope 源码副本和分析工作台。保留各功能原有验证文档；本记录仅描述提交前重新执行的检查。

## 本次检查

- `cargo test --workspace`：488 passed，4 个受控性能比较测试 ignored。格式和 Clippy 修复后另跑服务端 186 个测试，全部通过。
- `cargo fmt --all -- --check` 和 `cargo clippy --workspace --all-targets -- -D warnings` 通过。
- Python unittest：158 个测试中 141 passed、17 skipped。使用仓库虚拟环境，将其 Scripts 放入 PATH，设置 `MARKETFORGE_REQUIRE_PINE_TESTS=1`，并通过 `MARKETFORGE_TEST_SERVER` 指向 `target/commit-validation/exchange-server.exe`。
- MarketForge 网页 TypeScript 和生产构建通过。
- CandleScope 项目副本 typecheck、生产构建通过；仿真与共享页面测试 35/35，分析服务组合测试 1/1。构建仍有大 chunk 提示，分析测试有依赖弃用提示。
- PowerShell 脚本语法检查通过。暂存区 whitespace 检查按 Windows CRLF 使用 `core.whitespace=blank-at-eol,blank-at-eof,space-before-tab,cr-at-eol`，通过。

## 整理与边界

修正比赛模块格式和四处 Clippy 嵌套条件提示，清理项目源码副本中 11 个文件的行尾空格或末尾空行。来源清单的哈希继续表示初始导入，不重新定义上游快照。

首次 Python HTTP 检查误用旧 debug 服务二进制，因旧程序无法读取新增的 `market_data` 字段失败；构建独立测试二进制后完整重跑通过，没有替换已有服务。

本次没有配置 `MARKETFORGE_TEST_DATABASE_URL`，未重新运行 PostgreSQL 集成；Rust 中相应测试可能提前返回，不能把通过数量视为数据库验证。Python 跳过项包含数据库和可选外部运行环境测试。本次没有重新认证真实模型提供商、Docker Agent 流程、长期负载或浏览器端到端比赛；这些能力的历史记录见各专题文档。

依赖、构建产物、运行日志、数据库和本机凭据不进入提交。提交仅在本地创建，不推送远端。
