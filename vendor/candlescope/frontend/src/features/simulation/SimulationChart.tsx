import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import IndicatorPanel from "../indicators/IndicatorPanel.js";
import { useProvidedBarsIndicatorRuntime, providedBarsIndicatorSupport } from "../indicators/useProvidedBarsIndicatorRuntime.js";
import { createActiveIndicatorPersistence } from "../indicators/activeIndicatorStore.js";
import { createKlineOrderFlowProjectionMemo } from "../indicators/klineOrderFlowProjection.js";
import { KLINE_ORDER_FLOW_INDICATOR_DEFINITIONS } from "../indicators/klineOrderFlowStudy.js";
import type { TradeFlowRuntime } from "../trade-flow/tradeFlowTypes.js";
import type { ReactNode } from "react";
import MarketChartWorkspace from "../../app/MarketChartWorkspace.js";
import type { SimulationClient } from "./simulationClient.js";
import type { SimulationSelection } from "./simulationProtocol.js";
import SingleChartPanes from "../../components/SingleChartPanes.js";
import DrawingToolbar from "../../components/DrawingToolbar.js";
import { useChartSurfaceRuntime } from "../../chart-adapter/useChartSurfaceRuntime.js";
import { useDrawingRuntime } from "../drawings/useDrawingRuntime.js";
import { useExportRuntime } from "../export/useExportRuntime.js";
import ExportPanel from "../export/ExportPanel.js";
import { SeriesWindowStore } from "../market-data/window/seriesWindowStore.js";
import type { KlineBarInput } from "../market-data/marketDataTypes.js";
import type { ChartSettingsRuntime } from "../settings/chartAppearanceSettings.js";
import { t } from "../../i18n/index.js";
import { SIMULATION_CHART_EPOCH, elapsedLabel } from "./simulationProtocol.js";

const simulationTime = (seconds: number) => elapsedLabel(seconds - SIMULATION_CHART_EPOCH);

export default function SimulationChart({ datasetKey, symbol, interval, bars, appearance, stale, client, selection, rightRail, tradeFlow }: {
  datasetKey: string; symbol: string; interval: string; bars: KlineBarInput[];
  appearance: ChartSettingsRuntime; stale: boolean; client: SimulationClient; selection: SimulationSelection | null; rightRail: ReactNode; tradeFlow: TradeFlowRuntime;
}) {
  const surface = useChartSurfaceRuntime();
  const { settings, resolvedTheme } = appearance;
  const intervalMs = selection?.intervalMs ?? 1000;
  const store = useMemo(() => new SeriesWindowStore({ maxBars: 10_000, seriesKey: datasetKey, intervalSeconds: intervalMs / 1000 }), [datasetKey, intervalMs]);
  const revision = useSyncExternalStore(useCallback((listener) => store.subscribe(() => listener()), [store]), () => store.version, () => store.version);
  const indicatorBars = useMemo(() => { void revision; return store.snapshot(); }, [store, revision]);
  const dataMeta = useMemo(() => ({ version: revision, status: "ready" as const, seriesKey: store.seriesKey,
    source: "marketforge", optimistic: false, committedAt: null }), [store, revision]);
  const persistence = useMemo(() => createActiveIndicatorPersistence(`candlescope:simulation-indicators:v1:${datasetKey}`), [datasetKey]);
  const indicators = useProvidedBarsIndicatorRuntime({ bars: indicatorBars, datasetKey, exchange: "marketforge", symbol,
    interval, marketType: "simulation", persistence, sourceOrdinal: revision, sourceScopeKey: datasetKey, seriesReady: revision,
    chartDataMeta: dataMeta, candleUpColor: settings.upColor, candleDownColor: settings.downColor, visibleThroughSeconds: indicatorBars.at(-1)?.time ?? null });
  const [indicatorOpen, setIndicatorOpen] = useState(false);
  const [orderFlowProjection] = useState(createKlineOrderFlowProjectionMemo);
  const orderFlowPanes = useMemo(() => orderFlowProjection.project({ bars: indicatorBars, enabled: true, forceFull: true,
    interval: `${intervalMs / 1000}s`, intervalSeconds: intervalMs / 1000,
  }).filter((pane) => { const preference = tradeFlow.view.preferences.indicators[pane.id === "trade-flow-cvd" ? "cvd" : "delta"]; return preference.added && preference.visible; }), [indicatorBars, orderFlowProjection, intervalMs, tradeFlow.view.preferences.indicators]);
  const [historyBusy, setHistoryBusy] = useState(false);
  const [historyDone, setHistoryDone] = useState(false);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const historyController = useRef<AbortController | null>(null);
  useEffect(() => () => historyController.current?.abort(), []);
  const loadHistory = async () => {
    if (!selection || historyController.current || historyDone || store.snapshot().length >= 10_000) return false;
    const first = store.snapshot()[0];
    if (!first) return false;
    const controller = new AbortController(); historyController.current = controller; setHistoryBusy(true); setHistoryError(null);
    try {
      const older = await client.history(selection, first.time, AbortSignal.any([controller.signal, AbortSignal.timeout(10_000)]));
      if (controller.signal.aborted) return false;
      const remaining = Math.max(0, 10_000 - store.snapshot().length);
      if (remaining > 0) store.applyRange(older.slice(-remaining), { source: "marketforge-history" });
      if (older.length < 500) setHistoryDone(true);
      return older.length > 0;
    } catch (error) { if (!controller.signal.aborted) setHistoryError(error instanceof Error ? error.message : "History failed"); return false; }
    finally { if (!controller.signal.aborted) setHistoryBusy(false); if (historyController.current === controller) historyController.current = null; }
  };
  const drawings = useDrawingRuntime({ chartSurfaceActions: surface.actions, drawingScopeBase: datasetKey, session: null });
  const [ready, setReady] = useState(false);
  const [follow, setFollow] = useState(true);
  const pageExportRef = useRef<HTMLElement | null>(null);
  const exportFlow = useExportRuntime({ session: null, metadata: { exchange: "marketforge", symbol, interval },
    resolvedTheme, chartSurfaceActions: surface.actions, pageExportRef, drawings });
  useEffect(() => {
    store.applyRange(bars, { source: "marketforge" });
  }, [bars, store]);
  return <>
    <MarketChartWorkspace toolbar={
      <DrawingToolbar activeTool={drawings.view.drawingTool} onToolChange={drawings.actions.setDrawingTool}
        drawingInteractionReady={ready} penColor={drawings.view.penColor} onPenColorChange={drawings.actions.setPenColor}
        penSize={drawings.view.penSize} onPenSizeChange={drawings.actions.setPenSize}
        onClearAll={drawings.actions.handleClearDrawing} drawingsHidden={drawings.view.drawingsHidden}
        onToggleDrawingsHidden={drawings.actions.handleToggleDrawingsHidden}
        drawingSnapEnabled={drawings.view.drawingSnapEnabled} onDrawingSnapEnabledChange={drawings.actions.handleDrawingSnapEnabledChange}
        drawingContinuousEnabled={drawings.view.drawingContinuousEnabled} onDrawingContinuousEnabledChange={drawings.actions.handleDrawingContinuousEnabledChange}
        drawingAutoSelectEnabled={drawings.view.drawingAutoSelectEnabled} onDrawingAutoSelectEnabledChange={drawings.actions.handleDrawingAutoSelectEnabledChange}
        textFontSize={drawings.view.textFontSize} onTextFontSizeChange={drawings.actions.setTextFontSize}
        textBold={drawings.view.textBold} onTextBoldChange={drawings.actions.setTextBold}
        textItalic={drawings.view.textItalic} onTextItalicChange={drawings.actions.setTextItalic}
        fibLevels={drawings.view.fibLevels} onFibLevelsChange={drawings.actions.handleFibLevelsChange}
        fibInverted={drawings.view.fibInverted} onFibInvertedChange={drawings.actions.handleFibInvertedChange}
        positionSize={drawings.view.positionSize} selectedDrawing={drawings.view.selectedDrawing}
        onSelectedDrawingStyleChange={drawings.actions.handleSelectedDrawingStyleChange}
        exportPanelOpen={exportFlow.view.isOpen} exportInProgress={exportFlow.status.inProgress} onToggleExportPanel={exportFlow.actions.togglePanel}
        onPositionSizeChange={drawings.actions.handlePositionSizeChange} chartType={settings.chartType}
        onChartTypeChange={(chartType) => appearance.setSettings((previous) => ({ ...previous, chartType }))} />
    } exportOverlay={null} rightRail={rightRail}
      chart={<section ref={pageExportRef} className="simulation-chart" aria-label={t("simulation.chart")} data-testid="simulation-chart" data-bars={indicatorBars.length}>
    <div className="simulation-chart-heading"><strong>{symbol}</strong><span>{t("simulation.bars", { count: indicatorBars.length })}</span>
      <button onClick={() => setIndicatorOpen(true)}>{t("simulation.indicators")}</button>
      <button disabled={!selection || historyBusy || historyDone || indicatorBars.length >= 10_000} onClick={() => void loadHistory()}>{t(historyBusy ? "simulation.pending" : "simulation.history")}</button>
      <label><input type="checkbox" checked={follow} onChange={(event) => setFollow(event.target.checked)} />{t("simulation.follow")}</label>
    </div>
      <div className="simulation-chart-surface">
        <SingleChartPanes ref={surface.ref} symbol={symbol} interval={interval} datasetKey={datasetKey}
          dataMeta={dataMeta}
          mainOverlayLines={indicators.view.mainOverlayLines} subPanes={[...indicators.view.subPanes, ...orderFlowPanes]}
          indicatorMarkers={indicators.view.markers} indicatorFills={indicators.view.fills} indicatorHlines={indicators.view.hlines}
          indicatorBgcolors={indicators.view.bgcolors} indicatorBarcolors={indicators.view.barcolors}
          drawingKeyBase={datasetKey} paneLayoutScope="marketforge" seriesStore={store}
          upColor={settings.upColor} downColor={settings.downColor} chartType={settings.chartType}
          theme={resolvedTheme} customBg={settings.customBg} timezone="UTC" timeFormatter={simulationTime} tickMarkFormatter={simulationTime}
          followLatest={follow} canLoadMoreLeft={!historyDone && indicatorBars.length < 10_000} onNeedMoreLeft={async () => { await loadHistory(); }} drawingTool={ready ? drawings.view.drawingTool : null}
          onDrawingToolChange={drawings.actions.setDrawingTool} onDrawingInteractionReadyChange={setReady}
          penColor={drawings.view.penColor} penSize={drawings.view.penSize}
          textFontSize={drawings.view.textFontSize} textBold={drawings.view.textBold} textItalic={drawings.view.textItalic}
          fibLevels={drawings.view.fibLevels} fibInverted={drawings.view.fibInverted} positionSize={drawings.view.positionSize}
          drawingSnapEnabled={drawings.view.drawingSnapEnabled} drawingContinuousEnabled={drawings.view.drawingContinuousEnabled}
          drawingAutoSelectEnabled={drawings.view.drawingAutoSelectEnabled} onSelectedDrawingChange={drawings.actions.handleSelectedDrawingChange} />
        {bars.length === 0 && <div className="simulation-chart-notice" role="status">{t("simulation.noTrades")}</div>}
        {stale && <div className="simulation-stale" role="status">{t("simulation.stale")}</div>}
      </div>
      </section>} />
    {historyError && <p className="simulation-error" role="alert">{historyError}</p>}
    <IndicatorPanel isOpen={indicatorOpen} onClose={() => setIndicatorOpen(false)} activeIndicators={indicators.view.activeIndicators}
      paramSchemas={indicators.view.paramSchemas} onAddIndicator={indicators.actions.addIndicator} onRemoveIndicator={indicators.actions.removeIndicator}
      onToggleVisibility={indicators.actions.toggleVisibility} onUpdateParams={indicators.actions.updateIndicatorParams}
      onUpdateScript={indicators.actions.updateIndicatorScript} computing={indicators.status.computing} onRecompute={indicators.actions.recompute}
      resolveIndicatorSupport={providedBarsIndicatorSupport} allowedScriptLanguages={["pyne", "pine"]} allowedSecurityModes={["safe"]}
      modeNotice={{ label: t("simulation.indicators"), description: t("simulation.indicatorNotice") }}
      marketStudies={KLINE_ORDER_FLOW_INDICATOR_DEFINITIONS.map((definition) => ({ id: definition.id, name: t(definition.nameKey),
        description: t(definition.descriptionKey), category: definition.category, ...tradeFlow.view.preferences.indicators[definition.key],
        supported: indicatorBars.some((bar) => bar.taker_buy_base != null), unsupportedReason: t("simulation.flowMissing"), status: "ready" }))}
      onAddMarketStudy={(id) => tradeFlow.actions.addIndicator(id as "trade-flow:cvd" | "trade-flow:delta")}
      onRemoveMarketStudy={(id) => tradeFlow.actions.removeIndicator(id as "trade-flow:cvd" | "trade-flow:delta")}
      onToggleMarketStudyVisibility={(id) => tradeFlow.actions.toggleIndicatorVisibility(id as "trade-flow:cvd" | "trade-flow:delta")} />
    <ExportPanel isOpen={exportFlow.view.isOpen} options={exportFlow.view.options} onOptionsChange={exportFlow.actions.updateOptions}
      onExport={exportFlow.actions.exportChart} onClose={exportFlow.actions.closePanel} inProgress={exportFlow.status.inProgress}
      error={exportFlow.view.error} notice={exportFlow.view.notice} metadata={exportFlow.view.metadata} preview={exportFlow.view.preview} />
  </>;
}
