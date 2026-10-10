import type { AgentPolicy } from "./AgentPolicyPanel";

export type AgentRuntime = {
  service_status: string; phase: string; server_time: number;
  framework: { online: boolean; backend?: string; session_id?: string; last_heartbeat?: number; retry_count?: number; error_code?: string; last_delivery_at?: number; last_delivery_seq?: number };
  decision: { id: string | null; valid: boolean; expires_at?: number; generation: number; plan?: { statement?: string }; scheduled_wake?: { at: number } };
  pending_orders: { request_id: string; status: string }[]; pending_calls: number;
  pending_interrupts: { name: string; reason: string }[];
  queued_alerts?: { name: string; reason: string }[]; policy?: AgentPolicy;
  last_alert?: { time: number; data: { name?: string; reason?: string } };
  last_tool?: { name: string; request_id: string; status: string; at: number };
  last_observation?: { at: number; markets: Record<string, { market_time_ms: number; step: number }> };
  strategies: { name: string; running: boolean; error?: string; dependency_status?: string }[];
};

const names: Record<string, string> = { not_connected: "尚未连接框架", idle: "在线 · 空闲", thinking: "在线 · 思考 / 执行中", waiting: "在线 · 等待唤醒", offline: "框架离线", reconnecting: "正在重连", error: "需要处理", paused: "业务已暂停", legacy: "兼容模型循环" };
const toolNames: Record<string, string> = { running: "执行中", done: "已完成", error: "失败，请查看回执" };
function time(value?: number) { return value ? new Date(value * 1000).toLocaleTimeString() : "暂无"; }

export function AgentRuntimePanel({ runtime }: { runtime: AgentRuntime | null }) {
  if (!runtime) return <p>正在读取运行状态…</p>;
  const framework = runtime.framework;
  const remaining = Math.max(0, Math.ceil((runtime.decision.expires_at ?? 0) - runtime.server_time));
  return <div className="agent-runtime" aria-label="交易员运行状态">
    <h4>运行状态</h4>
    <div className="agent-runtime-grid">
      <div><strong>{names[runtime.phase] ?? runtime.phase}</strong><p>业务服务：{runtime.service_status === "running" ? "运行中" : runtime.service_status === "paused" ? "已暂停" : "需要处理"}</p><small>框架：{framework.backend ?? "未绑定"} · {framework.online ? "心跳正常" : "未在线"}</small></div>
      <div><strong>{runtime.decision.valid ? "决策凭证有效" : "需要开始新决策"}</strong><p>代际 {runtime.decision.generation} {runtime.decision.valid && `· 剩余约 ${remaining} 秒`}</p><small>{runtime.decision.id?.slice(0, 12) ?? "暂无活动凭证"}</small></div>
      <div><strong>待核对订单 {runtime.pending_orders.length}</strong><p>待确认工具请求 {runtime.pending_calls}</p><small>结果未知时按原请求身份核对。</small></div>
      <div><strong>待处理警报 {runtime.pending_interrupts.length}</strong><p>最近触发：{runtime.last_alert?.data.name ?? "暂无"} · {time(runtime.last_alert?.time)}</p><small>最近交付：{time(framework.last_delivery_at)} · 事件 #{framework.last_delivery_seq ?? "—"}</small></div>
    </div>
    <p>最近心跳：{time(framework.last_heartbeat)} · 累计重试 {framework.retry_count ?? 0} 次 {framework.error_code && `· ${framework.error_code}`}</p>
    {framework.session_id && <small>原生会话：{framework.session_id}</small>}
    {runtime.last_tool && <p>最近工具：{runtime.last_tool.name} · {toolNames[runtime.last_tool.status] ?? runtime.last_tool.status} · {time(runtime.last_tool.at)}</p>}
    {runtime.decision.scheduled_wake && <p>计划唤醒：{time(runtime.decision.scheduled_wake.at)}</p>}
    {runtime.decision.plan?.statement && <details><summary>保留的计划</summary><p>{runtime.decision.plan.statement}</p></details>}
    {runtime.last_observation && <p>最近观测：{time(runtime.last_observation.at)}（现实时间）<br />{Object.entries(runtime.last_observation.markets).map(([instrument, market]) => `${instrument}：市场 ${market.market_time_ms}ms / 步 ${market.step}`).join(" · ")}</p>}
    {runtime.pending_orders.length > 0 && <details open><summary>待核对订单身份</summary>{runtime.pending_orders.map(order => <p key={order.request_id}>{order.request_id} · {order.status}</p>)}</details>}
    {runtime.pending_interrupts.map(alert => <p key={alert.name}>{alert.name}：{alert.reason}</p>)}
    {(runtime.queued_alerts?.length ?? 0) > 0 && <p>普通警报待下一轮合并处理：{runtime.queued_alerts!.map(alert => alert.name).join("、")}</p>}
    {runtime.strategies.some(strategy => strategy.error) && <p role="alert">策略异常：{runtime.strategies.filter(strategy => strategy.error).map(strategy => `${strategy.name}：${strategy.error}`).join("；")}</p>}
    <small>交付记录表示框架已接受事件；决策与交易结果以工具回执为准。界面约每 2.5 秒刷新。</small>
  </div>;
}
