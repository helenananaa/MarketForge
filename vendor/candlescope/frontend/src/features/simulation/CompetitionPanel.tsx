import { useCallback, useEffect, useState } from "react";
import { MarketForgeHttpError, type SimulationClient } from "./simulationClient.js";
import { wireObject } from "./simulationProtocol.js";
import type { RoomContext } from "./roomPortalProtocol.js";
import "./competition.css";

type Player = { account_id: number; display_name: string; ready: boolean };
type Standing = { rank: number; user_id: string; display_name: string; account_id: number; initial_equity: string; final_equity: string; pnl: string; return_ppm: string };
type Match = { title: string; phase: string; seats: number[]; players: Record<string, Player>; results: Standing[]; server_time_ms: number; starts_at_ms: number | null; ends_at_ms: number | null; my_account_id: number | null; can_manage: boolean; scoring_rule: string; error: string | null };
const phases: Record<string, string> = { Preparation: "等待选手准备", Countdown: "开赛倒计时", Starting: "正在开赛", Running: "比赛进行中", Finishing: "正在结算", Finished: "比赛已结束", Aborted: "比赛已终止" };
const requestSignal = () => AbortSignal.timeout(10_000);

export default function CompetitionPanel({ client, context, compact = false, management = false }: { client: SimulationClient; context: RoomContext; compact?: boolean; management?: boolean }) {
  const root = `/rooms/${encodeURIComponent(context.room_id)}`;
  const [match, setMatch] = useState<Match | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [legacy, setLegacy] = useState(false);
  const [offset, setOffset] = useState(0);
  const [now, setNow] = useState(Date.now());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [title, setTitle] = useState("多人交易比赛");
  const [seats, setSeats] = useState("20,30");
  const [duration, setDuration] = useState("300");
  const [countdown, setCountdown] = useState("5");
  const [spectatorAccounts, setSpectatorAccounts] = useState(false);
  const [inviteRole, setInviteRole] = useState("trader");
  const [inviteAccount, setInviteAccount] = useState("20");
  const [code, setCode] = useState("");
  const admin = ["owner", "admin"].includes(context.role);
  const refresh = useCallback(async () => {
    let raw: unknown;
    try { raw = await client.request(`${root}/competition`, requestSignal()); }
    catch (error) { if (error instanceof MarketForgeHttpError && error.status === 404) { setLegacy(true); setLoaded(true); return; } throw error; }
    const next = raw === null ? null : wireObject(raw) as unknown as Match;
    setMatch(next); setLoaded(true); if (next) setOffset(next.server_time_ms - Date.now());
  }, [client, root]);
  useEffect(() => {
    let active = true; let timer: ReturnType<typeof setTimeout>;
    const poll = async () => { try { if (active) await refresh(); } catch (error) { if (active) setError(error instanceof Error ? error.message : "读取比赛失败"); } if (active) timer = setTimeout(poll, 1000); };
    void poll(); return () => { active = false; clearTimeout(timer); };
  }, [refresh]);
  useEffect(() => { const timer = setInterval(() => setNow(Date.now()), 500); return () => clearInterval(timer); }, []);
  const run = async (action: () => Promise<void>) => { setBusy(true); setError(""); try { await action(); await refresh(); } catch (error) { setError(error instanceof Error ? error.message : "操作失败"); } finally { setBusy(false); } };
  const player = match?.players[context.user_id];
  const deadline = match?.phase === "Countdown" ? match.starts_at_ms : match?.phase === "Running" ? match.ends_at_ms : null;
  const remaining = deadline === null || deadline === undefined ? null : Math.max(0, Math.ceil((deadline - now - offset) / 1000));
  const availableSeats = match ? match.seats.filter((id) => !Object.values(match.players).some((p) => p.account_id === id)) : context.visible_account_ids;
  useEffect(() => { if (!availableSeats.includes(Number(inviteAccount))) setInviteAccount(String(availableSeats[0] ?? "")); }, [availableSeats.join(","), inviteAccount]);
  if (legacy || ((compact || !management) && loaded && !match)) return null;
  return <section className={`competition-panel ${compact ? "competition-compact" : "room-card"}`} data-testid="competition-panel">
    <h2>{match?.title ?? "多人比赛"}</h2>
    {error && <p role="alert" className="room-error">{error}</p>}
    {!loaded && <p role="status">读取比赛状态…</p>}
    {match && <>
      <p role="status" data-testid="competition-phase">{phases[match.phase] ?? match.phase}{remaining !== null && <strong> · {remaining} 秒</strong>}</p>
      <p className="room-muted">{Object.values(match.players).filter((p) => p.ready).length} / {match.seats.length} 位选手已准备</p>
      {!compact && <ul>{Object.values(match.players).map((p) => <li key={p.account_id}>{p.display_name} · #{p.account_id} · {p.ready ? "已准备" : "未准备"}</li>)}</ul>}
      {player && match.phase === "Preparation" && <button disabled={busy} onClick={() => run(async () => { await client.request(`${root}/competition/ready`, requestSignal(), { ready: !player.ready }); })}>{player.ready ? "取消准备" : "准备比赛"}</button>}
      {management && match.can_manage && match.phase === "Preparation" && <button disabled={busy} onClick={() => run(async () => { await client.request(`${root}/competition/start`, requestSignal(), {}); })}>全员准备后开始倒计时</button>}
      {management && match.can_manage && !["Finished", "Finishing", "Aborted"].includes(match.phase) && <button disabled={busy} onClick={() => run(async () => { await client.request(`${root}/competition/abort`, requestSignal(), {}); })}>终止比赛（不生成排名）</button>}
      {match.error && <p>{match.error}</p>}
      {match.results.length > 0 && <div className="competition-results"><table aria-label="比赛最终排名"><thead><tr><th>排名</th><th>选手</th><th>权益</th><th>收益率</th></tr></thead><tbody>{match.results.map((row) => <tr key={row.user_id} data-self={row.user_id === context.user_id}><td>{row.rank}</td><td>{row.display_name}</td><td>{row.final_equity}</td><td>{(Number(row.return_ppm) / 10000).toFixed(2)}%</td></tr>)}</tbody></table><p className="room-muted">结果已归档，由服务端成交和账户记录结算。</p></div>}
      <details><summary>评分和观赛规则</summary><p>{match.scoring_rule}</p><p>参赛者只查看自己的账户。观众的账户可见范围由创建比赛时的设置决定，赛后可查看全部账户；管理者负责组织比赛，不能参赛下单。</p></details>
    </>}
    {management && admin && loaded && !match && <form onSubmit={(event) => { event.preventDefault(); void run(async () => {
      const ids = seats.split(/[,，\s]+/).filter(Boolean).map(Number);
      if (ids.some((id) => !Number.isSafeInteger(id) || id <= 0)) throw new Error("请输入有效的参赛账户 ID");
      await client.request(`${root}/competition`, requestSignal(), { title, seats: ids, duration_seconds: Number(duration), countdown_seconds: Number(countdown), spectator_accounts: spectatorAccounts });
    }); }}><p>先完成市场和 Bot 配置并暂停市场，再锁定比赛。当前支持单市场现货或永续账户；所有选手初始资金和持仓必须相同。</p>
      <label>比赛名称<input required value={title} maxLength={100} onChange={(e) => setTitle(e.target.value)} /></label>
      <label>参赛账户（逗号分隔）<input required value={seats} onChange={(e) => setSeats(e.target.value)} /></label>
      <div className="room-form-row"><label>赛程（秒）<input required type="number" min="10" max="86400" value={duration} onChange={(e) => setDuration(e.target.value)} /></label><label>倒计时（秒）<input required type="number" min="3" max="60" value={countdown} onChange={(e) => setCountdown(e.target.value)} /></label></div>
      <label className="room-check"><input type="checkbox" checked={spectatorAccounts} onChange={(e) => setSpectatorAccounts(e.target.checked)} />比赛期间允许观众查看全部账户</label>
      <button className="room-primary" disabled={busy}>锁定规则并开放报名</button>
    </form>}
    {management && admin && loaded && (!match || match.phase === "Preparation") && <form onSubmit={(event) => { event.preventDefault(); void run(async () => {
      setCode("");
      const raw = wireObject(await client.request(`${root}/invitations`, requestSignal(), { role: inviteRole, ...(inviteRole === "trader" ? { account_id: Number(inviteAccount) } : {}) }));
      setCode(String(raw.code));
    }); }}><h3>邀请成员</h3><label>邀请角色<select value={inviteRole} onChange={(e) => { setInviteRole(e.target.value); setCode(""); }}><option value="trader">交易员 / 参赛者</option><option value="spectator">观众</option>{!match && <option value="admin">管理员</option>}</select></label>
      {inviteRole === "trader" && <label>绑定账户<select aria-label="邀请绑定账户" value={inviteAccount} onChange={(e) => { setInviteAccount(e.target.value); setCode(""); }}>{availableSeats.map((id) => <option value={id} key={id}>#{id}</option>)}</select></label>}
      <button disabled={busy || (inviteRole === "trader" && !availableSeats.includes(Number(inviteAccount)))}>生成一次性邀请码</button>
      {code && <><label>邀请码<input aria-label="生成的邀请码" readOnly value={code} /></label><p className="room-muted">24 小时内有效，仅能兑换一次。复制给受邀成员即可。</p></>}
    </form>}
  </section>;
}
