import { useCallback, useEffect, useState } from "react";
import { t } from "../../../i18n/index.js";
import { useLocale } from "../../../i18n/useLocale.js";
import type { AiConnectionMode, AiConnectionSnapshot } from "../aiConnectionTypes.js";
import { formatAiConnectionConfig } from "../aiConnectionConfig.js";

export default function AiConnectionPanel() {
  useLocale();
  const bridge = typeof window === "undefined" ? undefined : window.candlescopeDesktop;
  const [state, setState] = useState<AiConnectionSnapshot | null>(null);
  const [mode, setMode] = useState<AiConnectionMode>("off");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [format, setFormat] = useState<"json" | "toml">("json");
  const refresh = useCallback(async () => {
    if (!bridge?.getAiConnection) return;
    setBusy(true); setError(null);
    try { const next = await bridge.getAiConnection(); setState(next); setMode(next.preferences.mode); }
    catch { setError(t("aiConnection.loadFailed")); }
    finally { setBusy(false); }
  }, [bridge]);
  useEffect(() => { void refresh(); }, [refresh]);
  const save = async () => {
    if (!bridge?.saveAiConnection) return;
    setBusy(true); setError(null);
    try { setState(await bridge.saveAiConnection({ mode })); }
    catch { setError(t("aiConnection.saveFailed")); }
    finally { setBusy(false); }
  };
  const config = state ? formatAiConnectionConfig(state.config, format) : "";
  const copy = async () => {
    try { await navigator.clipboard.writeText(config); setCopied(true); }
    catch { setError(t("aiConnection.copyFailed")); }
  };
  if (!bridge?.getAiConnection) return <div className="st-info-box">{t("aiConnection.desktopOnly")}</div>;
  return <section className="ai-connection-panel" aria-label={t("settings.category.ai")}>
    <p>{t("aiConnection.intro")}</p>
    <div className="st-info-box" role="status">
      <strong>{t("aiConnection.status")}: {state ? t(`aiConnection.status.${state.status}`) : t("aiConnection.loading")}</strong>
      <p>{t("aiConnection.statusHelp")}</p>
    </div>
    <label className="ai-connection-mode">{t("aiConnection.permission")}
      <select className="st-input" value={mode} disabled={busy || !state} onChange={(event) => { setMode(event.target.value as AiConnectionMode); setCopied(false); }}>
        <option value="off">{t("aiConnection.mode.off")}</option>
        <option value="observe">{t("aiConnection.mode.observe")}</option>
        <option value="edit">{t("aiConnection.mode.edit")}</option>
      </select>
    </label>
    <p>{t("aiConnection.permissionHelp")}</p>
    <div className="ai-connection-actions">
      <button type="button" className="st-btn st-btn-primary" onClick={() => void save()} disabled={busy || !state || (mode === state.preferences.mode && !state.preferenceError)}>{t("aiConnection.save")}</button>
      <button type="button" className="st-btn" onClick={() => void refresh()} disabled={busy}>{t("aiConnection.refresh")}</button>
    </div>
    {state?.restartRequired && <p className="st-info-box">{t("aiConnection.restart")}</p>}
    {state?.launchOverride && <p>{t("aiConnection.override")}</p>}
    {state?.preferenceError && <p role="alert">{t("aiConnection.invalidPreferences")}</p>}
    {state && !state.adapterAvailable && <p role="alert">{t("aiConnection.adapterMissing")}</p>}
    {error && <p role="alert">{error}</p>}
    <h3>{t("aiConnection.connect")}</h3>
    <ol><li>{t("aiConnection.stepEnable")}</li><li>{t("aiConnection.stepCopy")}</li><li>{t("aiConnection.stepUse")}</li></ol>
    <label className="ai-connection-mode">{t("aiConnection.format")}<select className="st-input" value={format} onChange={(event) => { setFormat(event.target.value as "json" | "toml"); setCopied(false); }}><option value="json">{t("aiConnection.format.json")}</option><option value="toml">{t("aiConnection.format.toml")}</option></select></label>
    <button type="button" className="st-btn st-btn-primary" onClick={() => void copy()} disabled={!state?.adapterAvailable || busy}>{t(copied ? "aiConnection.copied" : "aiConnection.copy")}</button>
    <details><summary>{t("aiConnection.showConfig")}</summary><textarea className="st-input ai-connection-config" readOnly value={config} aria-label={t("aiConnection.showConfig")} onFocus={(event) => event.target.select()} /></details>
    <p>{t("aiConnection.reconnect")}</p>
    <style>{`.ai-connection-panel p{line-height:1.6}.ai-connection-mode{display:grid;gap:8px;margin-top:20px}.ai-connection-actions{display:flex;gap:10px;margin:16px 0}.ai-connection-panel details{margin-top:16px}.ai-connection-config{box-sizing:border-box;width:100%;min-height:200px;margin-top:12px;font-family:monospace;font-size:12px;resize:vertical}.ai-connection-panel li{margin:8px 0}`}</style>
  </section>;
}
