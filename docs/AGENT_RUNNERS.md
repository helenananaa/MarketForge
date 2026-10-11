# Linux 容器里的交易员框架

连接器默认使用 `--environment docker`：一个交易员对应一个 Linux 容器和持久 `/work` 卷，Codex/OpenCode、原生文件/终端工具、MCP 适配器、Python SDK 和后台脚本共用这个目录。宿主的连接器通过 Docker stdio 管理 Codex app-server，通过容器内 HTTP 管理 OpenCode；提醒、心跳、模型消息许可、订单授权和额度仍由容器外的业务服务管理。

## 构建与启动

镜像包含 Python 3.12、Node 22、Codex CLI 0.162.1、OpenCode 1.19.0、MCP Python SDK 1.30.0、Matplotlib 3.11.2、Git 和 curl。构建脚本只复制 Python 包与 Dockerfile，不把仓库其他文件、`.local`、个人认证或交易所数据发送给 Docker。

```powershell
$env:PYTHONPATH='python'
# Windows Docker Desktop 未运行时，可选择已有 WSL Docker；服务和连接器要一致。
$env:MARKETFORGE_AGENT_DOCKER_WSL='Ubuntu-22.04'
.venv/Scripts/python.exe scripts/build_agent_runner.py
```

业务服务另开一个只接受账户工具令牌的监听入口。操作员接口始终监听 `127.0.0.1:57306`；容器入口不提供操作员、连接所有权或模型控制接口。Docker Desktop/Linux 可以使用 `host.docker.internal`。Windows + 独立 WSL Docker 应使用 WSL 默认路由中的 Windows 地址，例如本次验收的 `172.23.16.1`；地址可能随 WSL 重启变化。

```powershell
.venv/Scripts/python.exe -m marketforge.agents `
  --container-gateway-bind 0.0.0.0 --container-gateway-port 57307 `
  --container-gateway-url http://172.23.16.1:57307
```

Linux/Docker Desktop 的 URL 换成 `http://host.docker.internal:57307`。如只允许 Docker 网段，可将 bind 换成宿主的相应私网接口。MCP 仍是 stdio；它和程序 SDK 在容器内调用这个 HTTP 网关。网关 URL 必须是显式配置的私网地址，禁用重定向和代理环境变量。

按 [框架接入](AGENT_FRAMEWORKS.md) 创建外部交易员、分配账户、生成工具令牌。在连接器的终端中设置 `MARKETFORGE_TOOL_TOKEN` 后启动：

```powershell
.venv/Scripts/python.exe -m marketforge.agents.connectors `
  --backend codex --trader trader-1 `
  --state-file .local/agents/connections/trader-1-codex-docker.json --check

# 检查通过后去掉 --check，开始交付目标/行情消息。
.venv/Scripts/python.exe -m marketforge.agents.connectors `
  --backend codex --trader trader-1 `
  --state-file .local/agents/connections/trader-1-codex-docker.json
```

OpenCode 使用 `--backend opencode` 和独立状态文件。连接器自动启动容器内 `opencode serve`，只监听容器的 `127.0.0.1:4096`，不发布宿主端口。`--model` 对 Codex 是模型名，对 OpenCode 是 `provider/model`。

`--check` 创建/恢复真实原生会话，确认原生 MCP 已连接并报告工具数量/连接状态，不发送推理消息。Codex 在首次初始化时写入一条中性的历史记录，避免空会话尚未落盘而无法恢复。模型认证可用性仍需实际提供商调用验证。

## 模型认证

容器默认没有宿主模型认证，也不会挂载个人 `.codex` 或 OpenCode 配置目录。可在首次启动连接器时显式选择认证来源：

```powershell
# 已在当前终端设置对应的提供商变量；只复制所选变量到该交易员。
.venv/Scripts/python.exe -m marketforge.agents.connectors `
  --backend codex --trader trader-1 `
  --state-file .local/agents/connections/trader-1-codex-docker.json `
  --model-env OPENAI_API_KEY
```

也可传 `--model-auth-file <框架专用 auth.json>`，只复制该文件，Codex 保存到 `/work/.codex/auth.json`，OpenCode 保存到 `/work/.local/share/opencode/auth.json`。重连时保留已有认证；显式传入认证会更新它。凭据不进入镜像、命令行参数或验收报告。容器里的同一交易员程序可以读取自己的模型认证，因此应使用按交易员分配的提供商凭据。

容器以 UID/GID 65534 运行，根文件系统只读，2 CPU / 2 GiB / 256 进程，只挂载自己的 `/work` named volume，不挂载宿主目录或 Docker socket。Codex 在容器内使用 `danger-full-access`，OpenCode 使用 `permission=allow`，让其原生 Shell/文件工具能在该容器中自主工作；外层 Docker 负责执行边界。容器允许互联网和用户级依赖安装。

## 程序 HTTP API

连接器写入 `/work/.marketforge/api.json` 与该账户工具令牌，`marketforge_program.Client()` 自动读取配置。调用方式保持一致：

```python
from marketforge_program import Client
api = Client()
data = api.order_flow('V-BTC-PERP', window_ms=60_000)
receipt = api.trade('V-BTC-PERP', 'fok', 'entry-001',
                    side='Buy', price_tick=101, qty=1)
```

HTTP 端点为 `POST /tools/{trader}/program`，Bearer 使用该交易员工具令牌，请求形状：

```json
{"id":"entry-001","name":"trade","arguments":{"instrument":"V-BTC-PERP","action":"fok","side":"Buy","price_tick":101,"qty":1}}
```

程序共享账户授权、订单预算和可选账户风控，不需要逐笔申请 LLM 决策凭证。暂停、已登记连接器离线、紧急提醒尚未重新评估时禁止新交易。固定 ID/固定参数仍按持久回执去重；HTTP 超时/5xx 表示结果未知，重试原 ID。没有操作员、连接管理或模型控制权限。MCP 继续通过 `/call` 使用原生决策凭证和传输 epoch。旧脚本容器保留文件 relay 兼容方式。

服务的模型预算统计连接器交付的消息和框架报告的用量；程序自行调用提供商不经过这套模型消息预算，账户订单限制仍由业务服务执行。

## 恢复与升级

连接器状态文件留在宿主，原生会话/认证/脚本/依赖在账户卷内。断线继续恢复相同 thread/session，提醒仍从持久游标交付。正常服务退出先暂停账户并停止已连接容器。异常退出后，遗留脚本的新订单会受到业务暂停或心跳过期检查。

宿主旧框架兼容模式必须显式指定 `--environment host`。旧宿主状态文件不能直接当作 Docker 状态文件使用；原生会话路径不同，应为 Docker 创建新状态文件，账户/业务回执保留。

已有脚本容器或镜像升级需要显式迁移：先暂停交易员、退出连接器，使用运营令牌 `POST /traders/{id}/runner` 提交 `{"action":"migrate"}`。接口核对容器所有权，移除旧容器并保留账户卷；再次连接使用配置的 `--runner-image`。不自动重放旧后台任务，读取状态后以新的启动意图恢复。镜像重建不会静默继续使用旧容器。

## 验收

```powershell
$env:MARKETFORGE_AGENT_DOCKER_WSL='Ubuntu-22.04'
.venv/Scripts/python.exe scripts/validate_agent_runner.py --gateway-host 172.23.16.1
```

使用独立交易所进程、临时账户与独立容器，退出时只清理本次创建的容器/卷。验证真实 Codex app-server、原生 MCP、原生文件工具、stdio MCP、HTTP 程序成交/去重、提醒撤销旧决策、OpenCode MCP 和容器重启恢复。该验收不调用模型，不证明模型策略或提供商认证可用；结果见 [验收记录](validation/2026-10-11-agent-runner.md)。

协议依据：[Codex app-server](https://learn.chatgpt.com/docs/app-server)、[Codex 容器执行边界](https://learn.chatgpt.com/docs/agent-approvals-security)、[OpenCode server](https://opencode.ai/docs/server/)。
