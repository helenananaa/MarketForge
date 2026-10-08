# 合并提交验证 — 2026-10-08

本记录覆盖 MarketForge 当前工作区合并后的现货/永续价格联动、周期资金费、Pine
背景策略、CandleScope 工作台启动/备份工具、认证 WebSocket 与 Auto worker 恢复。
此前各功能的验证记录保留其当时的测试数量和边界；本记录补充最终合并回归结果。

## 当前源码回归

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace --target-dir target/pine-validation` | 392 项通过，0 失败、0 ignored，doctest 检查通过 |
| 当前 server 独立构建 | 通过，产物位于 `target/pine-validation/debug/` |
| Python `unittest discover -s python/tests -v` | 63 项：60 通过、3 跳过、0 失败，62.569 秒 |
| `npm.cmd --prefix marketforge-web run build` | TypeScript 与 Vite 生产构建通过 |
| 四个工作台 PowerShell 脚本 | 语法解析通过 |
| Python SDK、scripts 与 Pine 插件 | `compileall` 通过 |
| 暂存区检查 | `git diff --cached --check` 通过；验证期间源码 SHA-256 未变 |

Rust 测试分布为 core 223、server 158、插件库 2、SDK contract 3、provider contract 6。
Rust 和 Python 均设置 `MARKETFORGE_REQUIRE_POSTGRES_TESTS=1`，使用项目 PostgreSQL
上的新建独立测试数据库，未使用工作台运行库。Python 设置
`MARKETFORGE_REQUIRE_PINE_TESTS=1`，使用仓库 Python 3.12.7 与已安装的
`pine-compat-runtime==0.3.1`，通过 `MARKETFORGE_TEST_SERVER` 指向本轮独立构建。

Python 三项跳过为未启用的真实 agent/provider、项目依赖安装和 Docker sandbox
验收；Pine 和 PostgreSQL 用例没有跳过。本轮实际 HTTP 回归通过三种 Pine 策略的
入场、部分成交和平仓，26 个机器人混合运行，以及部分成交后的 PostgreSQL 重启
恢复和步骤幂等检查。WebSocket 认证、Origin、权限撤销和 worker 恢复用例也包含
在本轮 server 回归内。

本地日志为 `target/commit-validation/{rust-tests,build,python-tests}.log`。
数据库名称及源码哈希回执保存在同一 ignored 目录。既有运行服务未停止或替换。

## 提交与验证边界

- 提交包括实现、配置、脚本和各阶段验证文档。本地数据库、凭据、构建产物、日志、
  截图及运行工具保留在 ignored 目录，不进入提交。
- 本轮未重新操作 CandleScope 浏览器，也未提交另一工作区中的 CandleScope 前端。
  工作台浏览器与备份恢复证据仍以此前两份 CandleScope 验证记录为准。
- 本轮未推送、执行远程 CI、生成 Electron 发布包或完成长期负载资格验证。
