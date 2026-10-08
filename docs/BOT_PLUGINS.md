# 交易 bot 插件接入（bot.v1）

MarketForge 的调度器通过 `BotRegistry` 创建 bot，不再直接区分噪声、定投、网格等具体实现。内置 bot 和独立进程插件使用统一的 `Plugin` 配置；已有 `NoiseTrader`、`DcaTrader`、`GridTrader`、`ContinuousMarketMaker`、`CancelAtStep` JSON 配置仍可读取、运行和恢复。

## 1. 启用插件目录

```bash
export MARKETFORGE_BOT_PLUGIN_DIR="$PWD/bot-plugins"
cargo run -p exchange-server
```

目录结构：

```text
bot-plugins/
  buy-remaining/
    bot.json
    bot.py
  my-strategy/
    bot.json
    strategy.py
```

服务端启动时按目录名顺序读取 `<插件目录>/<包目录>/bot.json`。新增 bot 只需新增包目录和配置，然后重启服务端；不需要修改 Rust 核心或重新编译。每个 ID 当前只能安装一个版本。未知 ID、重复 ID、协议或状态版本不匹配会报错。没有设置目录时，只注册五种内置 bot。

插件目录是运营者安装的可信本地代码。API 请求只能选择已安装的 ID 和版本，不能指定命令。进程不通过 shell 启动；仅继承 PATH 和操作系统启动所需的临时目录变量，不继承服务端认证、数据库环境变量。这是进程隔离，不是系统权限沙箱。

## 2. 声明插件

可以复制仓库的 `bot-plugins/buy-remaining` 示例。它只依赖 Python 标准库，按卖一价逐步买入，并依据实际账户持仓判断完成数量。

```json
{
  "bot": {
    "id": "my.strategy",
    "name": "我的交易策略",
    "version": "1.0.0",
    "protocol_version": "bot.v1",
    "state_version": 1,
    "runtime": "process",
    "parameters": {
      "qty_per_step": {
        "type": "integer",
        "default": 1,
        "minimum": 1,
        "maximum": 100
      }
    }
  },
  "command": ["python3", "strategy.py"],
  "timeout_ms": 1000
}
```

`command` 是直接执行的参数数组，工作目录为该插件包目录。可替换成 Node、编译好的 Rust/C++ 程序或其他语言的可执行文件。使用平台支持的可执行文件和解释器名称；Windows 上可将 `python3` 换成安装的 Python 路径。

参数声明是一个有限的参数描述协议，不是完整 JSON Schema：支持 `integer`、`boolean`、`string`、`object`、`array`，以及 `required`、`default`、整数 `minimum/maximum`、`choices`。对象和数组仅验证顶层类型；插件负责验证内部结构。未声明参数、错误类型和超出范围的值都会拒绝。前端从描述自动生成配置输入项。

## 3. 编写决策入口

每次决策启动一次进程。stdin 收到一行 JSON；进程必须在 stdout 写一个 JSON 响应并退出。诊断写到 stderr。请求包括：

- `protocol_version`、`plugin_id`、`plugin_version`、`state_version`；
- `participant`：实例 ID、房间、账户、明确的交易品种；
- `seed`：实例随机种子；`config`：已补齐默认值的参数；
- `state`：上次保存的状态，首次为 `null`；
- `observation`：公开盘口、成交、仿真时间，以及该账户自己的订单和账户。

最小入口：

```python
import json
import sys

request = json.loads(sys.stdin.readline())
previous = request["state"] or {"steps": 0}
response = {
    "protocol_version": "bot.v1",
    "plugin_id": request["plugin_id"],
    "plugin_version": request["plugin_version"],
    "state_version": request["state_version"],
    "actions": [],
    "state": {"steps": previous["steps"] + 1},
}
print(json.dumps(response), flush=True)
```

交易动作使用现有 `OrderAction` 格式，例如：

```json
{"PlaceImmediateOrCancel":{"side":"Buy","price_tick":101,"qty":1}}
```

插件返回动作，由平台绑定账户、品种并提交到统一网关。插件不需要持有交易 API 凭据。每次最多 64 个动作，request 最大 4 MiB，response 最大 1 MiB，stderr 最大 64 KiB。默认决策超时 1000 ms，清单可设 1–10000 ms。自动模式下，进程失败、超时、超限或响应版本错误只暂停出错的 bot，`bot_errors` 按实例记录原因，`last_error` 保留最近错误；市场和其他 bot 继续运行。重新应用 bot 列表可重试这些实例。手动单步仍在任一 bot 失败时拒绝整个候选步骤。Unix 上同时终止该进程组并回收子进程。

策略内部的随机数状态、计数器和其他需要恢复的数据都必须放入返回的 `state`。不要依靠进程内存跨步骤保存状态；相同配置、seed、state、observation 应产生相同决策。模型权重可从插件目录加载，但每次启动都会重新加载，因此长耗时模型应先预计算或通过插件连接自己的推理服务。当前实现仍使用服务端串行调度步骤，适合短时决策；没有常驻模型进程、热安装或热升级。

## 4. 创建和运行实例

查询插件：

```bash
cargo run -p marketforge-cli -- bot list
```

REST `GET /bots` 返回内置及已安装插件的描述，遵循服务端现有认证。`GET /rooms/{id}/bots` 返回该房间的配置和状态，仅房间 admin 可访问。

`POST /rooms/{id}/agents` 可配置多个实例：

```json
{
  "interval_ms": 700,
  "agents": [
    {
      "Plugin": {
        "participant": {
          "participant_id": "buyer-1",
          "kind": "RuleAgent",
          "room_id": "demo",
          "account_id": 20,
          "instrument_id": "V-BTC-SPOT"
        },
        "plugin_id": "example.buy-remaining",
        "plugin_version": "1.0.0",
        "state_version": 1,
        "config_version": 1,
        "seed": 7,
        "config": {"target_qty": 4, "qty_per_step": 1}
      }
    }
  ]
}
```

`config_version/state_version/seed` 可省略，默认均为 1。内置 bot 也能使用此格式，ID 为原来的模板名，版本为字符串 `"1"`；具体参数查询 `GET /bots`。Noise 和连续做市的随机种子使用统一的顶层 `seed`。

前端交易面板可选择 bot、配置账户与参数、添加多个实例，然后应用整个启动列表。实例 ID 必须唯一，同一个插件可以绑定多个实例或账户。列表为空时启动当前表单中的单个实例。多个实例可以共享账户，但会共享该账户的持仓与挂单。

已有 `/agents` 状态、`/agents/stop` 停止接口继续使用。停止只禁用 bot 决策，保留策略状态，自动市场时钟继续运行；暂停整个市场请调用 `/pause`。`running` 表示 bot worker 是否启用，`market_running` 单独表示自动市场是否运行。再次提交相同配置可继续运行。新增/移除实例时，配置完全相同的实例保留状态；修改某实例配置会重置该实例。未完成手动步骤必须先完成，才能切换到自动交易。空 `agents` 列表会持久化清空 bot 列表，自动时钟仍运行。没有启用自动调度的房间继续使用显式时钟推进。

自动模式使用独立定时器推进市场，每个 tick 向空闲 bot 提供行情快照，决策在市场锁之外并行执行。每个实例最多一个在途决策，忙碌时不积累补跑队列；结果完成后按实际提交顺序进入统一交易网关，根据当时的盘口、账户与训练限制重新校验。`interval_ms` 保留为自动市场 tick / 空闲 bot 观察间隔，执行耗时不再额外累加到间隔。错过的定时唤醒不会批量补跑。bot 不提交订单时，时钟仍推进，但不制造成交或价格变化。

暂停、恢复、停止或替换实例会使此前未提交的决策失效。训练时限只由市场 tick 推进，bot 返回结果不会额外消耗训练步数，慢 bot 可能赶不上截止时间。

配置在首个决策前持久化。自动模式将每个 bot 的动作、更新后的策略状态和训练结果放入同一个 `SchedulerProgress` 事务；未提交的计算结果可丢弃。自动模式按实际时钟/订单日志重放，不承诺仅凭种子重现并发完成次序。手动 `/clock/step` 保留稳定 participant-id 顺序和未完成动作续跑，不重新调用已持久化的决策。服务端重启后恢复配置和状态；自动 worker 需要重新调用启动接口，或在暂停房间后手动步进。保持已安装版本可用；缺少被保存的插件或版本不匹配会拒绝启动恢复。修改策略行为或状态结构应升级对应版本，并明确处理旧状态；本版不提供自动状态迁移。PostgreSQL 和内存 journal 的现有边界不变：跨服务端进程恢复需要持久化 journal。

## 5. 参加训练与批量评估

Python SDK 新增 `list_bots`、`room_bots`、`start_agents`、`stop_agents`、`pause_room`、`step_bots`。

训练 API 新增可选 `manual_agents`（默认 false）。true 时持久化初始调度状态，并禁止启动自动 worker；暂停房间后使用 `/clock/step` 执行完整 bot 步骤。false 保持原来的后台运行方式。

使用 `marketforge.batch` 或 `scripts/batch_runner.py`，将训练 spec 中的 `agents` 换成上述 `Plugin` 实例。批量工具会：

1. 为每个种子隔离房间及 run ID，为插件实例派生顶层 child seed；
2. 将插件 ID、版本、状态版本和参数纳入实验 digest；
3. 使用 `manual_agents=true`，暂停房间并执行服务端 bot 步骤，而不是只推进时钟；
4. 当 bot 绑定训练账户时关闭默认买入逻辑，保留服务端真实评分；
5. 使用基于服务端步骤游标的幂等键支持恢复。

插件评估必须明确提供绑定 `trainee_account_id` 的 bot，避免默认策略混入结果；可以同时添加其他背景 bot。调度动作沿用训练任务的买入方向、剩余买入容量与状态约束，并逐动作更新成交证据和费用。插件超时或违规动作作为评估失败保留，不当作完成得分。

```bash
python3 scripts/batch_runner.py http://127.0.0.1:57305 your-plugin-training-spec.json 1 2 3 --state target/plugin-batch.json
```

## 6. 验证

```bash
export MARKETFORGE_BOT_PLUGIN_DIR="$PWD/bot-plugins"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
# 使用隔离测试数据库；现有恢复测试会读取整个数据库，串行运行避免测试之间的并发快照冲突。
MARKETFORGE_TEST_DATABASE_URL="$TEST_DSN" MARKETFORGE_REQUIRE_POSTGRES_TESTS=1 \
  cargo test --workspace -- --test-threads=1
cargo build -p exchange-server -p marketforge-cli
MARKETFORGE_TEST_DATABASE_URL="$TEST_DSN" MARKETFORGE_REQUIRE_POSTGRES_TESTS=1 \
  python3 -m unittest discover -s python/tests
(cd marketforge-web && npm run build)
```

覆盖注册/参数/版本校验、旧模板兼容、真实进程交易、超时/非法响应、决策与动作阶段恢复不重复下单、HTTP 配置、启停与多实例、PostgreSQL 服务端重启、步骤幂等和真实批量评分。无需修改核心代码即可加入示例插件。

Rust 库调用方：独立 `AgentTemplate::into_participant` 现在返回 `Result`，因为进程插件需要宿主注册表，不能直接转换成内存 `Participant`。调度使用 `run_scheduler_step_with_registry`；宿主的额外动作策略可通过 `BotExecutionPolicy` 接入。
