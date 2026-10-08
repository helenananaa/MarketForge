import { useCallback, useEffect, useState } from "react";

type Trader = { id: string; room: string; account_id: number; instruments: string[]; status: string; error: string | null; model_calls: number; max_model_calls: number };
type Receipt = { seq: number; kind: string; time: number; data: unknown };
type Strategy = { name: string; version: string; running: boolean; tested: boolean; error: string | null; state: unknown; dependency_status?: string; requirements?: string[]; files?: string[] };
type Project = { project: { files: Record<string, string>; entrypoint: string; requirements: string[]; system_packages: string[] }; state: { installation: { status: string; packages?: string[]; log?: string; error?: string } } };
type Status = { exchange_url: string; sandbox: { available: boolean; detail: string }; plugins: { id: string; name: string }[] };

const phases: Record<string, string> = { running: "运行中", paused: "已暂停", error: "需要处理" };
const buildNames: Record<string, string> = { not_requested: "未安装额外依赖", queued: "等待安装", running: "依赖安装中", ready: "依赖已就绪", failed: "安装失败，可修正后重试", cancelled: "安装已取消", interrupted: "安装被中断，可重试" };
const eventNames: Record<string, string> = { created: "选手已创建", started: "开始交易", paused: "已暂停", model_request: "查询模型", model_response: "模型回复", tool_request: "执行操作", tool_result: "操作结果", strategy_tick: "策略执行", strategy_error: "策略错误", session_error: "交易员停止", account_wakeup: "账户变化唤醒", strategy_tick_aborted: "策略剩余动作已取消" };
function eventLabel(receipt: Receipt) {
  if (receipt.kind === "dependencies_installed") return "策略依赖安装结果";
  if (receipt.kind === "dependency_error") return "策略依赖安装未完成";
  const data = receipt.data as { name?: string; result?: { statement?: string } };
  if (receipt.kind === "tool_result" && data.name === "announce") return `交易声明：${data.result?.statement ?? ""}`;
  return eventNames[receipt.kind] ?? receipt.kind;
}

export function AgentTraders({ room, instrument }: { room: string; instrument: string }) {
  const [open, setOpen] = useState(false);
  const [token, setToken] = useState("");
  const [status, setStatus] = useState<Status | null>(null);
  const [traders, setTraders] = useState<Trader[]>([]);
  const [selected, setSelected] = useState("");
  const [receipts, setReceipts] = useState<Receipt[]>([]);
  const [strategies, setStrategies] = useState<Strategy[]>([]);
  const [projectName, setProjectName] = useState("");
  const [project, setProject] = useState<Project | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [connection, setConnection] = useState("model-1");
  const [endpoint, setEndpoint] = useState("https://api.openai.com/v1");
  const [model, setModel] = useState("");
  const [key, setKey] = useState("");
  const [plugin, setPlugin] = useState("marketforge.llm-trader");
  const [name, setName] = useState("trader-1");
  const [account, setAccount] = useState(20);
  const [markets, setMarkets] = useState(instrument);
  const [prompt, setPrompt] = useState("自主研究现货与合约，管理自己的资产。你可以直接交易，也可以编写、测试和运行自己的策略。适时发布简短交易声明。");
  const [calls, setCalls] = useState(100);
  const [maxQty, setMaxQty] = useState(100);
  const [exchangeTokenEnv, setExchangeTokenEnv] = useState("");
  const [connected, setConnected] = useState("");
  const api = useCallback(async <T,>(path: string, body?: unknown): Promise<T> => {
    const response = await fetch(`http://127.0.0.1:57306${path}`, {
      method: body === undefined ? "GET" : "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || `HTTP ${response.status}`);
    return data as T;
  }, [token]);

  const refresh = useCallback(async () => {
    setTraders(await api<Trader[]>("/traders"));
  }, [api]);

  const action = async (work: () => Promise<void>) => {
    setBusy(true); setError("");
    try { await work(); } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  };

  useEffect(() => {
    if (!open || !status) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const list = await api<Trader[]>("/traders");
        let events: Receipt[] = [], bots: Strategy[] = [];
        if (selected) {
          [events, bots] = await Promise.all([api<Receipt[]>(`/traders/${selected}/events?tail=1`), api<Strategy[]>(`/traders/${selected}/strategies`)]);
        }
        const nextProject = selected && projectName ? await api<Project>(`/traders/${selected}/projects/${encodeURIComponent(projectName)}`) : null;
        if (!cancelled) { setTraders(list); setReceipts(events); setStrategies(bots); setProject(nextProject); }
      } catch (e) { if (!cancelled) setError(e instanceof Error ? e.message : String(e)); }
      if (!cancelled) timer = setTimeout(poll, 2500);
    };
    void poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [open, status, selected, projectName, api]);

  const exportEvents = async () => {
    const all: Receipt[] = [];
    let after = 0;
    const tail = await api<Receipt[]>(`/traders/${selected}/events?tail=1`);
    const until = tail.at(-1)?.seq ?? 0;
    for (;;) {
      const page = await api<Receipt[]>(`/traders/${selected}/events?after=${after}&until=${until}`);
      all.push(...page);
      if (page.length < 200) break;
      after = page[page.length - 1].seq;
    }
    const url = URL.createObjectURL(new Blob([JSON.stringify(all, null, 2)], { type: "application/json" }));
    const anchor = document.createElement("a"); anchor.href = url; anchor.download = `${selected}-receipts.json`; anchor.click();
    URL.revokeObjectURL(url);
  };

  return <section className="agent-traders">
    <button className="agent-toggle" onClick={() => setOpen(!open)} aria-expanded={open}>AI 交易员 · 模型与自编策略 {open ? "收起" : "打开"}</button>
    {open && <>
      <p>每位交易员使用独立参赛账户。模型可以查询行情、联网研究、交易，并编写多文件 Python 工程、安装依赖和部署自己的策略。</p>
      <div className="agent-connect">
        <label>运行服务令牌<input type="password" autoComplete="off" value={token} onChange={e => { setToken(e.target.value); setStatus(null); }} /></label>
        <button disabled={busy || !token} onClick={() => void action(async () => { setStatus(await api<Status>("/status")); await refresh(); })}>连接运行服务</button>
      </div>
      <small>启动命令：python -m marketforge.agents（先设置 PYTHONPATH=python）。令牌位于 .local/agents/operator.token。</small>
      {error && <p role="alert" className="bot-error">{error}</p>}
      {status && <>
        <p>交易所：{status.exchange_url} · 自编策略：{status.sandbox.available ? "隔离环境就绪" : "隔离环境不可用；直接交易仍可用"}</p>
        <div className="agent-grid">
          <fieldset><legend>1 · 接入模型</legend>
            <label>连接名称<input value={connection} onChange={e => { setConnection(e.target.value); setConnected(""); }} /></label>
            <label>交易员插件<select value={plugin} onChange={e => { setPlugin(e.target.value); setConnected(""); }}>{status.plugins.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}</select></label>
            <label>模型服务地址<input value={endpoint} onChange={e => { setEndpoint(e.target.value); setConnected(""); }} /></label>
            <label>模型名称<input value={model} onChange={e => { setModel(e.target.value); setConnected(""); }} /></label>
            <label>API Key<input type="password" autoComplete="off" value={key} onChange={e => { setKey(e.target.value); setConnected(""); }} /></label>
            <small>使用兼容 Chat Completions 工具调用的服务。密钥仅保存在当前服务进程内；测试会发起一次模型请求。</small>
            <button disabled={busy || !model} onClick={() => void action(async () => { await api("/connections/test", { id: connection, plugin_id: plugin, base_url: endpoint, model, api_key: key }); setKey(""); setConnected(connection); })}>测试并连接模型</button>
            {connected && <p role="status">{connected} 已连接</p>}
          </fieldset>
          <fieldset><legend>2 · 创建交易员</legend>
            <label>选手名称<input value={name} onChange={e => setName(e.target.value)} /></label>
            <label>已分配的账户 ID<input type="number" min="1" value={account} onChange={e => setAccount(Number(e.target.value))} /></label>
            <label>允许交易的品种（逗号分隔）<input value={markets} onChange={e => setMarkets(e.target.value)} /></label>
            <label>交易目标<textarea value={prompt} onChange={e => setPrompt(e.target.value)} /></label>
            <label>模型请求总预算<input type="number" min="1" value={calls} onChange={e => setCalls(Number(e.target.value))} /></label>
            <label>每笔最大数量<input type="number" min="1" value={maxQty} onChange={e => setMaxQty(Number(e.target.value))} /></label>
            <label>交易身份令牌环境变量（可选）<input placeholder="MARKETFORGE_TRADER_TOKEN_A" value={exchangeTokenEnv} onChange={e => setExchangeTokenEnv(e.target.value)} /></label>
            <small>房间：{room || "先载入房间"}。账户需预先出资并授权；本地模式的身份为 agent-选手名称。</small>
            <button disabled={busy || !room || connected !== connection} onClick={() => void action(async () => {
              await api("/traders", { id: name, room, account_id: account, instruments: markets.split(",").map(s => s.trim()).filter(Boolean), connection, prompt, max_model_calls: calls, max_order_qty: maxQty, exchange_token_env: exchangeTokenEnv });
              setSelected(name); await refresh();
            })}>创建交易员</button>
          </fieldset>
        </div>
        <div className="agent-roster">{traders.map(t => <article className={selected === t.id ? "selected" : ""} key={t.id}>
          <button onClick={() => { setSelected(t.id); setProjectName(""); setProject(null); }}><strong>{t.id}</strong> · {phases[t.status] ?? t.status}</button>
          <p>{t.room} · 账户 {t.account_id} · {t.instruments.join(" / ")}</p>
          <p>模型请求 {t.model_calls} / {t.max_model_calls}</p>
          {t.error && <p className="bot-error">{t.error}</p>}
          <button disabled={busy || t.status === "running"} onClick={() => void action(async () => { await api(`/traders/${t.id}/start`, {}); await refresh(); })}>开始 / 恢复</button>
          <button disabled={busy || t.status !== "running"} onClick={() => void action(async () => { await api(`/traders/${t.id}/stop`, {}); await refresh(); })}>暂停决策与策略</button>
          <small>暂停保留已有挂单。</small>
        </article>)}</div>
        {selected && <div className="agent-records"><h3>{selected} · 策略与执行记录</h3>
          {strategies.map(s => <div key={s.name}><p>{s.name} · {s.version.slice(0, 10)} · {s.running ? "已启用（随交易员暂停）" : "停止"} · {s.tested ? "已测试" : "未测试"} · {buildNames[s.dependency_status ?? "not_requested"]} {s.error}</p>
            <small>{s.files?.length ?? 1} 个文件 · {s.requirements?.join(", ") || "Python 标准库"}</small>
            <button onClick={() => setProjectName(s.name)}>查看工程与安装日志</button></div>)}
          {project && <div className="agent-project"><h4>{projectName} · 工程文件</h4>
            <p>入口：{project.project.entrypoint} · 系统依赖：{project.project.system_packages.join(", ") || "无额外系统包"}</p>
            <p>{buildNames[project.state.installation.status]} {project.state.installation.error}</p>
            {project.state.installation.packages && <details><summary>已安装的实际版本</summary><pre>{project.state.installation.packages.join("\n")}</pre></details>}
            <details><summary>安装日志</summary><pre>{project.state.installation.log || "暂无安装日志"}</pre></details>
            {Object.entries(project.project.files).map(([file, code]) => <details key={file}><summary>{file}</summary><pre>{code}</pre></details>)}
          </div>}
          <button disabled={busy} onClick={() => void action(exportEvents)}>导出完整记录</button>
          <div className="agent-events">{receipts.slice(-50).reverse().map(r => <details key={r.seq}><summary>#{r.seq} · {new Date(r.time * 1000).toLocaleTimeString()} · {eventLabel(r)}</summary><pre>{JSON.stringify(r.data, null, 2)}</pre></details>)}</div>
        </div>}
      </>}
    </>}
  </section>;
}
