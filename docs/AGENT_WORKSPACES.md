# Agent 持久 Docker 编程工作区

Codex/OpenCode 继续管理模型、规划和上下文。MarketForge 提供容器内编程环境和账户 API，不增加模型循环。MCP 现有 41 个业务工具，加 context、decision_begin、receipt，共 44 个。

## 权限和生命周期

每个选手绑定自己的 Docker container 和 named volume `/work`。允许任意 Python/数据文件、`/bin/sh` 命令、用户级 pip 依赖、互联网和多个后台进程。文件、依赖、任务记录在 stop/start 后保留。

用户为 UID/GID 65534；限制为 2 CPU、2 GiB、256 进程，系统目录只读，`/tmp` 256 MiB，输出最多保存 4 MiB。不挂载宿主文件或 Docker socket，不注入模型、运营或交易所密钥。Docker bridge 联网明确开放，不使用 web_read 的逐 URL/IP 校验。宿主框架的权限保持原设置。

先 context，再 decision_begin；写工具携带当前 decision_id/generation 和稳定 request_id：

1. workspace_start 创建/恢复容器，写入账户 SDK，连接后台 relay。
2. workspace_write(path="strategy.py",text="...") 写入 `/work` 内文件。
3. workspace_exec(command="python -u strategy.py",wait_seconds=1) 返回 job_id。最多等待 15 秒，进程可以继续后台运行；相同请求 ID 不重复启动。
4. workspace_process(job_id="...",offset=0) 读取输出，保存 next_offset；stop=true 终止进程组。新轮询使用新 ID。
5. workspace_stop 停止容器及进程，保留文件，不自动撤销交易所挂单。

workspace_read 是只读。workspace_process 同时支持 stop，因此仍需写工具凭证。Shell 可运行 `python -m pip install --user pandas numpy requests`。Shell 不以 root 运行 apt；系统包/不可变策略镜像构建仍可使用旧 strategy_install。

Docker 不可用会明确失败，不退回宿主执行。Windows 可使用已有 WSL Docker：

```powershell
$env:MARKETFORGE_AGENT_DOCKER_WSL='Ubuntu-22.04'
```

## 程序账户 API

workspace_start 写入 `/work/marketforge_program.py`。程序无需密钥：

```python
from marketforge_program import Client
api=Client()
view=api.observe("V-BTC-PERP")
bars=api.candles("V-BTC-PERP",interval_ms=1000,limit=500)
rules=api.call("market_rules",{"instrument":"V-BTC-PERP"})
receipt=api.trade("V-BTC-PERP","fok","entry-2026-001",
    side="Buy",price_tick=101,qty=1)
# accepted 仅表示受理，仍需检查风险/撮合事件。
for page in api.iter_history("V-BTC-PERP",limit=500):
    for activity in page["activities"]:
        print(activity)
```

api.call 开放 market_read、market_history、market_indicators、chart_export、risk_events、orders、fills、account_history、market_rules、portfolio、ledger、trade、order_cancel_all、policy_status、conditional_orders、indicator_compute、indicator_catalog；context 也可读取。没有运营、模型控制、框架凭证或其他选手访问权。

程序不需要每笔订单再向 LLM 申请 decision lease。真实账户授权、max_order_qty、共享 orders_per_minute、当前可选账户策略继续检查。选手暂停、已登记框架离线或紧急警报等待重新评估时，新 trade/cancel_all 被阻止；已提交交易和原生保护不回滚。程序能操作该账户挂单，旧命名策略自身订单限制保留。

客户端和服务端核对 ID 与输入；未知交易重试保留同一交易所 Idempotency-Key。明确拒绝后修改意图须用新 ID；超时表示结果未知。Shell launch 按 job_id 去重，容器重启后旧任务为 interrupted，不自动再次执行。

## 可恢复的事件消费

```python
from pathlib import Path
from marketforge_program import Client
api=Client()
path=Path("cursor.txt")
cursor=int(path.read_text()) if path.exists() else None
for page in api.watch("V-BTC-PERP",after_command_seq=cursor):
    for activity in page["activities"]:
        print(activity)  # 在这里处理公开/自己的事件。
    # 处理成功后保存，即使 activities 为空也前进。
    if page["next_after_command_seq"] is not None:
        path.write_text(str(page["next_after_command_seq"]))
```

watch 默认每 250 ms 游标轮询，has_more 时立即补页；队列超时和临时网络不确定结果按同一读取 ID 重试。默认从尾页开始，完整回放用 iter_history 或显式游标。读取和成交不构成事务，程序自行保存业务状态、检查序号和处理重放。

服务正常退出先暂停选手，再停止已连接容器；恢复后选手为 paused。重新连接框架/恢复选手后，workspace_start 恢复文件和 relay，程序以新 launch 意图启动，读取持久状态续接。异常退出后遗留进程可能继续计算，没有 relay 不能调用交易 API，重新接通后仍受暂停检查。

## 共用人的指标引擎

按工作台文档运行 setup-candlescope-workbench.ps1 安装运行时并启动分析服务。默认 URL 为 `http://127.0.0.1:18086/api/v1`，运营者可设置 MARKETFORGE_ANALYSIS_URL。

indicator_catalog 返回真实 registry/运行时。按 catalog 名称调用，例如 MA（简单均线），不是 SMA。indicator_compute 将交易所同一批权威 K 线提供给人的引擎，支持 builtin 或 script/Pine/Pyne，securityMode=safe，不推进时钟；结果覆盖提供的窗口。chart_export 的 indicator 参数叠加同一计算结果的线。

## 验收复现

使用独立 PostgreSQL 后台和新 QA 房间：

```powershell
$env:PYTHONPATH='python'
$env:MARKETFORGE_AGENT_DOCKER_WSL='Ubuntu-22.04'
$env:MARKETFORGE_ANALYSIS_URL='http://127.0.0.1:18096/api/v1'
.venv/Scripts/python.exe scripts/validate-agent-workspace.py --base-url http://127.0.0.1:57315 --room capability-qa-workspace-new
```

验证官方 MCP → Docker → scoped API、依赖下载、后台进程、文件恢复、成交去重、账户隔离、原生大订单号和共享内置/Pine 指标图。不调用模型提供商，不代表策略质量已验证；详见 [验收记录](validation/2026-10-10-agent-workspaces.md)。
