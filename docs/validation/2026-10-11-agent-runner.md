# 2026-10-11 Linux Agent 框架容器验收

本次将 Codex/OpenCode 原生会话与脚本工作区放入每交易员独立容器，增加账户 HTTP 网关，并保留宿主的连接管理、事件唤醒和交易权限检查。没有重启已有业务服务或连接已有市场。

## 结果

| 验证 | 结果 |
|---|---|
| Agent Python 回归 | 146 项：139 通过，7 跳过 |
| Linux 镜像 | WSL Ubuntu-22.04 Docker 实际构建成功，`marketforge-agent:1` |
| Codex 原生会话 | 真正的 app-server 初始化、49 项 MCP 工具发现、原生 MCP context 调用、原生文件写入 `/work` |
| Codex 恢复 | 停止容器后由连接器恢复，同一 thread ID，未调用模型 |
| OpenCode 原生会话 | 真正的容器内 serve、MCP connected，容器重启后同一 session ID |
| 文档中的 CLI | 两个 backend 的默认 Docker `--check` 各执行两次，第二次恢复相同会话 |
| HTTP 脚本订单 | 独立真实交易所里账户 20 买入 1 单位，成交/清算回执与账户持仓一致 |
| 重复订单 | 固定 ID/固定参数得到相同回执；修改参数拒绝，不重复成交 |
| 权限边界 | HTTP 测试确认别的账户、操作员接口、连接控制、程序工具越权被拒绝 |
| 交易拦截 | 暂停、断线、待重新评估的紧急提醒拒绝程序新订单；提醒撤销旧 MCP 决策 |
| 旧容器迁移 | 核对所有权，保留 named volume，旧文件在新容器中仍能读取 |
| 容器约束 | UID/GID 65534、只读根目录、cap-drop ALL、仅自己的 `/work` 卷、没有发布 OpenCode 端口 |

最终镜像 ID：`sha256:042af2e18f99af436a8f1e73b028fb57da3178d4f32ce0fe5686ba84123b34c5`。版本为 Codex CLI 0.162.1、OpenCode 1.19.0；镜像固定 MCP Python SDK 1.30.0、Matplotlib 3.11.2。结构化回执见 [agent-runner.json](2026-10-11-agent-runner.json)。

7 个跳过项是既有的可选宿主 Codex/OpenCode、提供商传输、策略安装/沙箱与回放测试。本次另用独立验收脚本实际验证容器里的两个原生框架、MCP 和交易所，未调用任何模型提供商。

## 复现

```powershell
$env:PYTHONPATH='python;.'
$env:MARKETFORGE_AGENT_DOCKER_WSL='Ubuntu-22.04'
.venv/Scripts/python.exe scripts/build_agent_runner.py
.venv/Scripts/python.exe -m unittest discover -s python/tests -p 'test_agent*.py' -q
.venv/Scripts/python.exe scripts/validate_agent_runner.py --gateway-host 172.23.16.1
```

`172.23.16.1` 是本次 WSL 默认路由中的 Windows 地址，其他环境按 [启动文档](../AGENT_RUNNERS.md) 替换。脚本使用独立交易所进程、临时账户、临时业务服务和独立工作卷，成功退出后清理本次容器/卷，报告不包含令牌或认证文件。

此次证明原生框架、工具/脚本执行环境、账户接口和恢复链路可运行。没有验证真实模型认证、推理行为、交易策略质量或收益；真实推理需显式为交易员配置提供商认证。宿主已有服务没有自动切换到容器网关。
