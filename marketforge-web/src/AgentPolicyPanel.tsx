import { useEffect, useState } from "react";

export type AgentPolicy = {
  rules: { account: Record<string, { max_abs_position?: number; max_leverage_bps?: number; max_loss?: number }>; model: { max_admissions?: number; max_tokens?: number; max_estimated_cost_microusd?: number; microusd_per_million_tokens?: number } };
  model_usage: { admissions: number; tokens: number; estimated_cost_microusd: number | null; blocked_by: string[] };
};

export function AgentPolicyPanel({ instruments, policy, save }: { instruments: string[]; policy?: AgentPolicy; save: (rules: AgentPolicy["rules"]) => Promise<void> }) {
  const [draft, setDraft] = useState<AgentPolicy["rules"]>({ account: {}, model: {} });
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => { if (policy && !dirty) setDraft(policy.rules); }, [policy, dirty]);
  const changeAccount = (instrument: string, field: string, value: string) => {
    const rules = { ...draft.account[instrument] };
    if (value === "") delete rules[field as keyof typeof rules];
    else Object.assign(rules, { [field]: Number(value) });
    setDraft({ ...draft, account: { ...draft.account, [instrument]: rules } }); setDirty(true);
  };
  const changeModel = (field: string, value: string) => {
    const model = { ...draft.model };
    if (value === "") delete model[field as keyof typeof model];
    else Object.assign(model, { [field]: Number(value) });
    setDraft({ ...draft, model }); setDirty(true);
  };
  return <details className="agent-policy"><summary>可选账户与模型预算</summary>
    <p>留空关闭该规则。只由运营者设置，AI 可以读取；保存后需重新决策。仓位与杠杆计入未成交挂单，亏损以启用时的权益为基准。</p>
    {instruments.map(instrument => <fieldset key={instrument}><legend>{instrument}</legend>
      <label>最大绝对仓位<input type="number" min="1" step="1" value={draft.account[instrument]?.max_abs_position ?? ""} onChange={e => changeAccount(instrument, "max_abs_position", e.target.value)} /></label>
      <label>最大杠杆（10000 = 1倍）<input type="number" min="1" step="1" value={draft.account[instrument]?.max_leverage_bps ?? ""} onChange={e => changeAccount(instrument, "max_leverage_bps", e.target.value)} /></label>
      <label>最大权益亏损（市场资金单位）<input type="number" min="1" step="1" value={draft.account[instrument]?.max_loss ?? ""} onChange={e => changeAccount(instrument, "max_loss", e.target.value)} /></label>
    </fieldset>)}
    <fieldset><legend>外部框架模型预算</legend>
      <label>最多交付模型消息次数<input type="number" min="1" step="1" value={draft.model.max_admissions ?? ""} onChange={e => changeModel("max_admissions", e.target.value)} /></label>
      <label>最多累计 Token<input type="number" min="1" step="1" value={draft.model.max_tokens ?? ""} onChange={e => changeModel("max_tokens", e.target.value)} /></label>
      <label>费用上限（微美元，1000000 = $1）<input type="number" min="1" step="1" value={draft.model.max_estimated_cost_microusd ?? ""} onChange={e => changeModel("max_estimated_cost_microusd", e.target.value)} /></label>
      <label>估算单价（每百万 Token 的微美元）<input type="number" min="1" step="1" value={draft.model.microusd_per_million_tokens ?? ""} onChange={e => changeModel("microusd_per_million_tokens", e.target.value)} /></label>
      <small>使用你填写的统一单价估算；框架返回用量后才可中断，单次推理可能超额。次数包含紧急消息交付，不等于模型内部调用次数。</small>
    </fieldset>
    {policy && <p>已用消息许可 {policy.model_usage.admissions} 次 · {policy.model_usage.tokens} Token · {policy.model_usage.estimated_cost_microusd === null ? "费用未估算（未填写单价）" : `估算 $${(policy.model_usage.estimated_cost_microusd / 1000000).toFixed(6)}`} {policy.model_usage.blocked_by.length > 0 && "· 已达到预算，等待调整"}</p>}
    {error && <p role="alert">{error}</p>}
    <button disabled={!dirty || busy} onClick={async () => { setBusy(true); setError(""); try { await save(draft); setDirty(false); } catch (e) { setError(e instanceof Error ? e.message : String(e)); } finally { setBusy(false); } }}>保存可选规则</button>
  </details>;
}
