import { useEffect, useState } from "react";
import type { SimulationClient } from "./simulationClient.js";
import { wireObject, type Observation, type SimulationSelection, type ProtectionSpec, type PositionSide } from "./simulationProtocol.js";

export function PositionRiskPanel({ observation, selection, client, canTrade, busy, onProtect }: {
  observation: Observation; selection: SimulationSelection; client: SimulationClient; canTrade: boolean; busy: boolean;
  onProtect: (side: PositionSide, spec: ProtectionSpec | null) => void;
}) {
  const [tp, setTp] = useState(""); const [sl, setSl] = useState("");
  const [leg, setLeg] = useState<PositionSide>("Both"); const [trigger, setTrigger] = useState<"Mark" | "Last">("Mark");
  const [events, setEvents] = useState<Record<string, unknown>[]>([]); const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!canTrade) return;
    const controller = new AbortController(); let cursor: unknown = null; let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const query = new URLSearchParams({ account_id: String(selection.accountId), limit: "100" });
        if (cursor != null) query.set("after_command_seq", String(cursor));
        const page = wireObject(await client.request(`/rooms/${encodeURIComponent(selection.roomId)}/instruments/${encodeURIComponent(selection.instrumentId)}/risk-events?${query}`, controller.signal));
        if (controller.signal.aborted) return;
        const incoming = page.events;
        if (Array.isArray(incoming)) setEvents((previous) => [...previous, ...incoming.map(wireObject)].slice(-10));
        cursor = page.next_after_command_seq ?? cursor; setError(null);
        timer = setTimeout(() => void poll(), page.has_more ? 0 : 1000);
      } catch (e) {
        if (!controller.signal.aborted) { setError(e instanceof Error ? e.message : "风险提醒读取失败"); timer = setTimeout(() => void poll(), 3000); }
      }
    };
    void poll(); return () => { controller.abort(); clearTimeout(timer); };
  }, [client, selection.roomId, selection.instrumentId, selection.accountId, canTrade]);
  const active = (observation.position_protections ?? []).filter((p) => ["armed", "awaiting_fill", "triggered"].includes(String(p.status)));
  const status = observation.margin_status;
  return <section className="simulation-panel simulation-risk" aria-label="持仓风险与止盈止损">
    <h2>持仓风险与止盈止损</h2>
    <p role={status === "margin_call" || status === "liquidatable" ? "alert" : undefined}>
      {status === "liquidatable" ? "强平风险：账户已达到可强平状态" : status === "margin_call" ? "保证金预警：权益低于初始保证金要求" : status === "healthy" ? "保证金状态正常" : "当前无合约仓位"}
    </p>
    {observation.risk && <dl>{[["标记价", "mark_price_tick"], ["保证金缓冲", "margin_buffer"], ["风险比例（ppm）", "margin_ratio_ppm"], ["预计强平价", "liquidation_price_estimate_tick"]].map(([label, key]) => <div key={key}><dt>{label}</dt><dd>{String(observation.risk?.[key!] ?? "—")}</dd></div>)}</dl>}
    {observation.account && <dl>{[["开仓均价", "avg_entry_price_tick"], ["已实现盈亏", "realized_pnl"], ["资金费盈亏", "funding_pnl"], ["委托占用保证金", "reserved_margin"]].map(([label, key]) => <div key={key}><dt>{label}</dt><dd>{String(observation.account?.[key!] ?? "—")}</dd></div>)}</dl>}
    {observation.hedge_positions && ["long", "short"].map((side) => { const leg = wireObject(observation.hedge_positions?.[side]); return <p key={side}>{side === "long" ? "多头" : "空头"} · 数量 {String(leg.qty)} · 均价 {String(leg.avg_entry_price_tick)}</p>; })}
    {observation.risk && <small>强平价为其他品种价格不变时的估算；成交、费用、资金费和流动性会改变实际结果。</small>}
    {canTrade && <form onSubmit={(e) => { e.preventDefault(); onProtect(leg, { take_profit_tick: tp ? Number(tp) : null, stop_loss_tick: sl ? Number(sl) : null, trigger }); }}>
      <label>保护仓位<select aria-label="保护仓位" value={leg} onChange={(e) => setLeg(e.target.value as PositionSide)}><option value="Both">单向持仓</option><option value="Long">多头仓位</option><option value="Short">空头仓位</option></select></label>
      <label>持仓止盈价<input aria-label="持仓止盈价" type="number" min="1" step="1" value={tp} onChange={(e) => setTp(e.target.value)} /></label>
      <label>持仓止损价<input aria-label="持仓止损价" type="number" min="1" step="1" value={sl} onChange={(e) => setSl(e.target.value)} /></label>
      <label>触发价格<select aria-label="持仓触发价格" value={trigger} onChange={(e) => setTrigger(e.target.value as "Mark" | "Last")}><option value="Mark">标记价</option><option value="Last">最新成交价</option></select></label>
      <button disabled={busy || (!tp && !sl)}>保存持仓保护</button><button type="button" disabled={busy} onClick={() => onProtect(leg, null)}>取消持仓保护</button>
      <small>保存会替换该仓位全部止盈止损；触发后以市价减仓，实际成交价可能滑点。</small>
    </form>}
    {active.map((p) => { const spec = wireObject(p.spec); return <p key={`${p.account_id}:${p.position_side}`}>{String(p.position_side)} · 止盈 {String(spec.take_profit_tick ?? "—")} / 止损 {String(spec.stop_loss_tick ?? "—")} · {p.status === "triggered" ? "已触发，等待剩余仓位成交" : p.status === "awaiting_fill" ? "等待开仓成交" : "保护中"}</p>; })}
    <div aria-live="polite">{events.filter((row) => { const event = wireObject(row.event); return event.type === "PerpLiquidationSettled" || event.type === "PerpMarginStatusChanged" && ["margin_call", "liquidatable"].includes(String(event.new_status)); }).map((row, i) => {
      const event = wireObject(row.event); return <p key={`${row.command_seq}:${i}`} role="alert">{event.type === "PerpLiquidationSettled" ? `强平已发生 · 强平费用 ${String(event.liquidation_fee)} · 剩余坏账 ${String(event.bad_debt)}` : `保证金风险变化 · ${String(event.new_status)}`} · 仿真时间 {String(row.market_time_ms)}ms</p>;
    })}</div>
    {error && <p role="alert">风险提醒连接异常：{error}</p>}
  </section>;
}
