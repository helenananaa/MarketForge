import { useCallback, useEffect, useMemo, useState } from "react";
import SimulationApp from "./SimulationApp.js";
import CompetitionPanel from "./CompetitionPanel.js";
import { MarketForgeHttpError, SimulationClient } from "./simulationClient.js";
import { wireObject, wireText, type SimulationConnection } from "./simulationProtocol.js";
import { parseConfiguration, parseRoomContext, roleLabel, type RoomContext, type RoomOverview } from "./roomPortalProtocol.js";
import { useChartSettingsRuntime } from "../settings/chartAppearanceSettings.js";
import "./roomPortal.css";

type Route = { page: "lobby" | "create" | "join" | "manage" | "chart"; room: string };
function route(): Route {
  const [page = "lobby", room = ""] = location.hash.slice(1).split("/");
  try { return { page: ["create", "join", "manage", "chart"].includes(page) ? page as Route["page"] : "lobby", room: decodeURIComponent(room) }; }
  catch { return { page: "lobby", room: "" }; }
}
function savedConnection(): SimulationConnection {
  const requested = new URLSearchParams(location.search).get("server");
  let saved: SimulationConnection | null = null;
  try { const raw = JSON.parse(sessionStorage.getItem("marketforge.connection") ?? "null"); if (raw?.baseUrl && typeof raw.userId === "string" && typeof raw.token === "string") saved = raw; } catch { /* Empty browser session. */ }
  if (requested) {
    try { const url = new URL(requested); if (["http:", "https:"].includes(url.protocol) && !url.username && !url.password && !url.search && !url.hash) {
      const baseUrl = url.toString().replace(/\/$/, "");
      if (saved?.baseUrl === baseUrl) return saved;
      return { baseUrl, userId: "", token: "" };
    } } catch { /* The service field remains editable. */ }
  }
  return saved ?? { baseUrl: "http://127.0.0.1:57307", userId: "local-user", token: "" };
}

const signal = () => AbortSignal.timeout(10_000);
const errorText = (error: unknown) => error instanceof Error ? error.message : "请求失败";

export default function RoomPortalApp() {
  const appearance = useChartSettingsRuntime();
  const [connection, setConnection] = useState(savedConnection);
  const [draft, setDraft] = useState(connection);
  const [authMode, setAuthMode] = useState<string | null>(null);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [register, setRegister] = useState(false);
  const [inviteCode, setInviteCode] = useState("");
  useEffect(() => {
    let active = true; setAuthMode(null);
    const timer = setTimeout(() => {
      const detect = async () => {
        const publicClient = new SimulationClient({ baseUrl: draft.baseUrl, userId: "", token: "" });
        try { return wireText(wireObject(await publicClient.request("/auth/config", signal())).mode); }
        catch (error) {
          if (!(error instanceof MarketForgeHttpError && error.status === 404)) throw error;
          // Older room-portal services expose their development/static-token mode through identity.
          try { return wireText(wireObject(await publicClient.request("/identity", signal())).auth_mode); }
          catch (identityError) { if (identityError instanceof MarketForgeHttpError && identityError.status === 401) return "bearer"; throw identityError; }
        }
      };
      void detect().then((mode) => { if (active) setAuthMode(mode); }).catch((error) => { if (active) setError(errorText(error)); });
    }, 250);
    return () => { active = false; clearTimeout(timer); };
  }, [draft.baseUrl]);
  const [identity, setIdentity] = useState<{ user_id: string; auth_mode: string; display_name?: string | undefined } | null>(null);
  const client = useMemo(() => new SimulationClient(connection), [connection]);
  const [view, setView] = useState(route);
  const [rooms, setRooms] = useState<string[]>([]);
  const [context, setContext] = useState<RoomContext | null>(null);
  const [overview, setOverview] = useState<RoomOverview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const go = useCallback((page: Route["page"], room = "") => { location.hash = `${page}${room ? `/${encodeURIComponent(room)}` : ""}`; setView({ page, room }); setError(null); }, []);
  useEffect(() => { const change = () => setView(route()); window.addEventListener("hashchange", change); return () => window.removeEventListener("hashchange", change); }, []);
  const forget = useCallback(() => { sessionStorage.removeItem("marketforge.connection"); setIdentity(null); setContext(null); setOverview(null); setPassword(""); setDraft({ ...connection, token: "", userId: "" }); go("lobby"); }, [connection, go]);
  const run = async (action: () => Promise<void>) => { setBusy(true); setError(null); try { await action(); } catch (error) { setError(errorText(error)); } finally { setBusy(false); } };
  const login = async () => {
    let next = draft;
    if (authMode === "accounts") {
      const raw = wireObject(await new SimulationClient({ ...draft, token: "", userId: "" }).request(register ? "/auth/register" : "/auth/login", signal(), { username, password, display_name: displayName }));
      next = { baseUrl: draft.baseUrl, userId: wireText(wireObject(raw.user).user_id), token: wireText(raw.token) };
    }
    const value = wireObject(await new SimulationClient(next).request("/identity", signal()));
    const user = { user_id: wireText(value.user_id), auth_mode: wireText(value.auth_mode), display_name: typeof value.display_name === "string" ? value.display_name : undefined };
    sessionStorage.setItem("marketforge.connection", JSON.stringify(next)); setPassword(""); setConnection(next); setDraft(next); setIdentity(user); go("lobby");
  };
  // Revalidate credentials on reload; the server remains the source of identity.
  useEffect(() => {
    const cached = sessionStorage.getItem("marketforge.connection");
    if (!cached) return;
    try { if (JSON.parse(cached).baseUrl !== connection.baseUrl) return; } catch { return; }
    let active = true;
    void client.request("/identity", signal()).then((raw) => { const value = wireObject(raw); if (active) setIdentity({ user_id: wireText(value.user_id), auth_mode: wireText(value.auth_mode), display_name: typeof value.display_name === "string" ? value.display_name : undefined }); }).catch((error) => { if (active) { if (error instanceof MarketForgeHttpError && error.status === 401) forget(); setError(errorText(error)); } });
    return () => { active = false; };
  }, [client, forget]);
  useEffect(() => {
    if (!identity || view.page !== "lobby") return;
    let active = true;
    void client.listRooms(signal()).then((rooms) => { if (active) setRooms(rooms); }).catch((error) => { if (active) { if (error instanceof MarketForgeHttpError && error.status === 401) forget(); setError(errorText(error)); } });
    return () => { active = false; };
  }, [client, identity, view.page, forget]);
  useEffect(() => {
    setContext(null); setOverview(null);
    if (!identity || !view.room) return;
    let active = true; let timer: ReturnType<typeof setTimeout> | undefined; let signature = "";
    const refresh = async () => {
      try {
        const next = parseRoomContext(await client.request(`/rooms/${encodeURIComponent(view.room)}/session`, signal()));
        if (next.room_id !== view.room || next.user_id !== identity.user_id) throw new Error("房间身份不匹配");
        if (!active) return;
        const nextSignature = JSON.stringify(next);
        if (signature !== nextSignature) { signature = nextSignature; setOverview(null); setContext(next); }
        if (view.page === "manage" && !next.capabilities.manage_bots) { go("join", view.room); return; }
        if (view.page === "chart" || view.page === "manage") {
          const raw = wireObject(await client.request(`/rooms/${encodeURIComponent(view.room)}/workbench`, signal()));
          const fresh = parseRoomContext(raw.context);
          if (!active) return;
          if (JSON.stringify(fresh) === signature) setOverview(raw as unknown as RoomOverview);
          else { signature = ""; setContext(null); setOverview(null); }
        }
      } catch (error) {
        if (!active) return;
        setError(errorText(error));
        if (error instanceof MarketForgeHttpError && [401, 403, 404].includes(error.status)) {
          setContext(null); setOverview(null);
          if (error.status === 401) { forget(); setError("登录已过期或已退出，请重新登录"); } else { go("lobby"); setError("房间访问权限已失效，请联系管理员重新加入"); }
          return;
        }
      }
      if (active) timer = setTimeout(() => { void refresh(); }, 2_000);
    };
    void refresh(); return () => { active = false; if (timer) clearTimeout(timer); };
  }, [client, identity, view.page, view.room, go, forget]);

  const [newRoom, setNewRoom] = useState("");
  const [recipe, setRecipe] = useState("");
  const [startBots, setStartBots] = useState(false);
  const [cadence, setCadence] = useState("1000");
  useEffect(() => {
    if (!identity || view.page !== "create") return;
    let active = true;
    void client.request("/scenarios/background-market", signal()).then((recipe) => { if (active) setRecipe(JSON.stringify(recipe, null, 2)); }).catch((error) => { if (active) setError(errorText(error)); });
    return () => { active = false; };
  }, [client, identity, view.page]);
  const create = async () => {
    if (!newRoom.trim()) throw new Error("请输入房间名称");
    const configuration = wireObject(parseConfiguration(recipe));
    configuration.autostart_agents = startBots; configuration.agent_interval_ms = positive(cadence);
    await client.createBackgroundRoom(newRoom.trim(), signal(), configuration);
    go("manage", newRoom.trim());
  };
  const join = async () => {
    const next = parseRoomContext(await client.request(`/rooms/${encodeURIComponent(view.room)}/session`, signal(), {}));
    setContext(next); go("chart", view.room);
  };
  const logout = async () => {
    if (identity?.auth_mode === "accounts") { try { await client.request("/auth/logout", signal(), {}); } catch (error) { if (!(error instanceof MarketForgeHttpError && error.status === 401)) throw error; } }
    forget();
  };

  if (identity && context && view.page === "chart") return <SimulationApp key={JSON.stringify(context)} connection={connection} context={context} overview={overview} onLeave={() => go("lobby")} onManage={() => go("manage", view.room)} />;
  return <div className="room-portal" data-theme={appearance.resolvedTheme}>
    <header><a href="#lobby" className="room-brand">MarketForge <span>× CandleScope</span></a><nav>{identity && <><span>{identity.display_name ?? identity.user_id}{context && ` · ${roleLabel(context.role)}`}</span><button onClick={() => go("lobby")}>房间大厅</button><button disabled={busy} onClick={() => run(logout)}>退出登录</button></>}<button onClick={() => appearance.setSettings((s) => ({ ...s, theme: appearance.resolvedTheme === "dark" ? "light" : "dark" }))}>切换主题</button></nav></header>
    <main>{error && <p className="room-error" role="alert">{error}</p>}
      {!identity ? <section className="room-card room-login"><span className="room-eyebrow">进入模拟市场</span><h1>登录房间平台</h1><p>登录后选择房间，确认身份，再进入 CandleScope 看盘。</p>
        <form onSubmit={(event) => { event.preventDefault(); void run(login); }}><label>服务地址<input required type="url" value={draft.baseUrl} onChange={(e) => { setDraft({ baseUrl: e.target.value, userId: "", token: "" }); setError(null); }} /></label>
          {!authMode ? <p role="status">正在确认登录方式…</p> : authMode === "accounts" ? <>
            <label>用户名<input required autoComplete="username" value={username} minLength={3} maxLength={32} onChange={(e) => setUsername(e.target.value)} /></label>
            {register && <label>昵称<input maxLength={64} value={displayName} onChange={(e) => setDisplayName(e.target.value)} /></label>}
            <label>密码<input required type="password" autoComplete={register ? "new-password" : "current-password"} minLength={register ? 12 : undefined} maxLength={128} value={password} onChange={(e) => setPassword(e.target.value)} /></label>
            <p className="room-muted">账号由服务端验证；登录会话有效期 12 小时，退出后立即失效。</p>
            <button className="room-primary" disabled={busy}>{register ? "注册并登录" : "登录"}</button>
            <button type="button" onClick={() => { setRegister(!register); setError(null); }}>{register ? "已有账号，去登录" : "创建账号"}</button>
          </> : <><label>访问令牌<input type="password" autoComplete="off" value={draft.token} onChange={(e) => setDraft({ ...draft, token: e.target.value })} /></label>
            <label>本地开发用户<input value={draft.userId} disabled={Boolean(draft.token)} onChange={(e) => setDraft({ ...draft, userId: e.target.value })} /></label>
            <p className="room-muted">本地开发模式使用用户 ID；令牌模式由服务端验证身份。</p><button className="room-primary" disabled={busy || !authMode}>登录</button></>}
          </form>
      </section> : view.page === "lobby" ? <><div className="room-heading"><div><span className="room-eyebrow">房间平台</span><h1>选择一个市场</h1><p>配置和成员管理在房间平台完成，进入后专注看盘与交易。</p></div><button className="room-primary" onClick={() => go("create")}>创建房间</button></div>
        {identity.auth_mode === "accounts" && <section className="room-card"><h2>通过邀请加入</h2><form onSubmit={(event) => { event.preventDefault(); void run(async () => { const value = wireObject(await client.request("/invitations/redeem", signal(), { code: inviteCode.trim() })); setInviteCode(""); go("join", wireText(value.room_id)); }); }}><label>邀请码<input required autoComplete="off" value={inviteCode} onChange={(e) => setInviteCode(e.target.value)} /></label><button disabled={busy}>接受邀请并进入房间</button></form></section>}
        <div className="room-grid">{rooms.map((room) => <section className="room-card" key={room}><span className="room-eyebrow">已加入的房间</span><h2>{room}</h2><p>进入前确认你的角色和可见账户。</p><button onClick={() => go("join", room)}>查看并加入</button></section>)}</div>{!rooms.length && <section className="room-card"><h2>还没有可访问的房间</h2><p>创建自己的房间，或使用管理员提供的邀请码加入。</p></section>}
        {identity.auth_mode === "local-development" && <p className="room-muted">当前为本机开发身份模式。</p>}
      </> : view.page === "create" ? <section className="room-card"><span className="room-eyebrow">1 / 配置市场</span><h1>创建房间</h1><p>市场规则、账户初始资金和 Bot 参数在创建前确定。创建后可继续分配成员和账户。</p>
        <form onSubmit={(e) => { e.preventDefault(); void run(create); }}><div className="room-form-row"><label>房间名称<input required value={newRoom} onChange={(e) => setNewRoom(e.target.value)} /></label><label>Bot 更新间隔（毫秒）<input type="number" min="1" required value={cadence} onChange={(e) => setCadence(e.target.value)} /></label></div>
          <label className="room-check"><input type="checkbox" checked={startBots} onChange={(e) => setStartBots(e.target.checked)} />创建后立即启动 Bot</label>
          <details><summary>市场、账户与 Bot 完整配置</summary><p>支持现有服务端场景格式。可调整市场类型、手续费、账户资金、Bot 策略等；已有市场的合约与初始资金需通过新建房间变更。</p><textarea aria-label="房间配置 JSON" value={recipe} onChange={(e) => setRecipe(e.target.value)} spellCheck={false} /></details>
          <button className="room-primary" disabled={busy || !recipe}>创建并进入房间管理</button></form>
      </section> : !context ? <section className="room-card"><p role="status">正在验证房间身份…</p><button onClick={() => go("lobby")}>返回大厅</button></section>
      : view.page === "manage" ? <RoomManagement client={client} context={context} overview={overview} onJoin={() => go("join", view.room)} />
      : <section className="room-card room-join"><span className="room-eyebrow">2 / 确认身份</span><h1>{context.room_id}</h1><dl><div><dt>用户</dt><dd>{identity.display_name ?? context.user_id}</dd></div><div><dt>房间角色</dt><dd>{roleLabel(context.role)}</dd></div><div><dt>市场</dt><dd>{context.instruments.join("、")}</dd></div><div><dt>可见账户</dt><dd>{context.visible_account_ids.join("、") || "公共行情"}</dd></div><div><dt>可交易账户</dt><dd>{context.trade_account_ids.join("、") || "只读"}</dd></div></dl>
        <CompetitionPanel client={client} context={context} />
        <p>{context.role === "spectator" ? (context.capabilities.read_all_accounts ? "可以查看全部账户、委托和 Bot 状态；无法下单或更改房间。" : "可以查看公共行情；比赛期间的账户和策略信息受观赛规则限制。") : context.capabilities.manage_bots ? "可以查看全部账户、分配成员和账户、管理 Bot 与市场运行。" : "只能查看和操作服务端分配给你的账户。"}</p>
        <div className="room-actions"><button className="room-primary" disabled={busy} onClick={() => run(join)}>加入并进入看盘</button>{context.capabilities.manage_bots && <button onClick={() => go("manage", view.room)}>先管理房间</button>}</div>
      </section>}
    </main>
  </div>;
}

function positive(input: string): number { const value = Number(input); if (!Number.isSafeInteger(value) || value < 1) throw new Error("请输入有效的正整数"); return value; }

function RoomManagement({ client, context, overview, onJoin }: { client: SimulationClient; context: RoomContext; overview: RoomOverview | null; onJoin: () => void }) {
  const root = `/rooms/${encodeURIComponent(context.room_id)}`;
  const [members, setMembers] = useState<Record<string, string>>({});
  const [profiles, setProfiles] = useState<Record<string, { display_name: string }>>({});
  const [user, setUser] = useState(""); const [role, setRole] = useState("trader"); const [account, setAccount] = useState("");
  const [agents, setAgents] = useState(""); const [interval, setInterval] = useState("1000");
  const [busy, setBusy] = useState(false); const [error, setError] = useState<string | null>(null); const [notice, setNotice] = useState("");
  const refresh = useCallback(async () => { const response = wireObject(await client.request(`${root}/members`, signal())); setMembers(wireObject(response.members) as Record<string, string>); setProfiles((response.profiles ?? {}) as Record<string, { display_name: string }>); }, [client, root]);
  useEffect(() => { void refresh().catch((error) => setError(errorText(error))); }, [refresh]);
  useEffect(() => { if (overview?.bots && !agents) { setAgents(JSON.stringify(overview.bots.agents, null, 2)); setInterval(String(overview.bots.status.interval_ms || 1000)); } }, [overview, agents]);
  const run = async (action: () => Promise<void>) => { setBusy(true); setError(null); setNotice(""); try { await action(); setNotice("操作已完成"); await refresh(); } catch (error) { setError(errorText(error)); } finally { setBusy(false); } };
  return <><div className="room-heading"><div><span className="room-eyebrow">房间管理 · {roleLabel(context.role)}</span><h1>{context.room_id}</h1><p>房间规则、成员和 Bot 在这里管理，个人图表设置在看盘页调整。</p></div><button className="room-primary" onClick={onJoin}>完成配置，前往看盘</button></div>
    {error && <p className="room-error" role="alert">{error}</p>}{notice && <p role="status">{notice}</p>}
    {overview?.competition && <p className="room-muted">比赛已锁定成员、账户和市场参数。使用比赛面板控制开赛或终止。</p>}
    <div className="room-grid room-management">
      <CompetitionPanel client={client} context={context} management />
      <section className="room-card"><h2>成员与账户</h2><fieldset disabled={Boolean(overview?.competition)}><table><thead><tr><th>用户</th><th>角色</th><th>操作</th></tr></thead><tbody>{Object.entries(members).map(([id, role]) => <tr key={id}><td title={id}>{profiles[id]?.display_name ?? id}</td><td>{roleLabel(role as RoomContext["role"])}</td><td>{id !== context.user_id && role !== "owner" && <button disabled={busy} onClick={() => run(async () => { await client.request(`${root}/members/${encodeURIComponent(id)}`, signal(), {}); })}>移除</button>}</td></tr>)}</tbody></table>
        <details><summary>手动调整成员和账户</summary><form onSubmit={(e) => { e.preventDefault(); void run(async () => { await client.request(`${root}/members`, signal(), { user_id: user.trim(), role }); }); }}><label>成员用户 ID<input required value={user} onChange={(e) => setUser(e.target.value)} /></label><label>角色<select value={role} onChange={(e) => setRole(e.target.value)}><option value="trader">交易员</option><option value="spectator">观众</option><option value="admin">管理员</option><option value="instructor">教练</option></select></label><button disabled={busy}>添加或更新成员</button></form>
        <label>账户分配<select aria-label="分配账户" value={account} onChange={(e) => setAccount(e.target.value)}><option value="">选择账户</option>{context.visible_account_ids.map((id) => <option key={id} value={id}>#{id}</option>)}</select></label><p className="room-muted">账户分配给上方的成员用户 ID。观众无需分配账户；训练开始后服务端会锁定成员配置。</p><button disabled={busy || !account || !user.trim()} onClick={() => run(async () => { await client.request(`${root}/accounts/${positive(account)}/owners`, signal(), { user_id: user.trim() }); })}>授予该账户交易权限</button></details>
      </fieldset></section>
      <section className="room-card"><h2>市场运行</h2><fieldset disabled={Boolean(overview?.competition)}><p>状态：{overview?.status ?? "读取中"}</p><div className="room-actions"><button disabled={busy || overview?.status !== "Running"} onClick={() => run(async () => { await client.control(context.room_id, "pause", signal(), crypto.randomUUID()); })}>暂停市场</button><button disabled={busy || overview?.status !== "Paused"} onClick={() => run(async () => { await client.control(context.room_id, "resume", signal(), crypto.randomUUID()); })}>继续市场</button><button disabled={busy || overview?.status !== "Paused"} onClick={() => run(async () => { await client.control(context.room_id, "clock/step", signal(), crypto.randomUUID()); })}>推进一步</button></div>
        <h2>Bot 配置</h2><p>{overview?.bots?.status.lifecycle ?? "读取中"} · Bot {overview?.bots?.agents.length ?? 0} 个</p><label>更新间隔（毫秒）<input type="number" min="1" value={interval} onChange={(e) => setInterval(e.target.value)} /></label><details><summary>编辑 Bot 策略配置</summary><textarea aria-label="Bot 配置 JSON" value={agents} onChange={(e) => setAgents(e.target.value)} spellCheck={false} /></details>
        <div className="room-actions"><button disabled={busy || !agents} onClick={() => run(async () => { const configs = parseConfiguration(agents); if (!Array.isArray(configs)) throw new Error("Bot 配置必须是数组"); await client.request(`${root}/agents`, signal(), { agents: configs, interval_ms: positive(interval) }); })}>应用配置并启动 Bot</button><button disabled={busy} onClick={() => run(async () => { await client.request(`${root}/agents/stop`, signal(), {}); })}>停止 Bot</button></div>
      </fieldset></section>
    </div>
  </>;
}
