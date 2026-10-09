import { useCallback, useId, useState } from 'react';
import { PluginSettingsPanel } from '../plugins/PluginCenter.js';
import DataWorkbenchModal from '../data-workbench/DataWorkbenchModal.js';
import { t } from '../../i18n/index.js';
import { useLocale } from '../../i18n/useLocale.js';
import SettingsPanelHost from './SettingsPanelHost.js';
import SettingsModalStyles from './SettingsModalStyles.js';
import { buildSettingsPanelViewModel } from './settingsPanelViewModel.js';
import { SETTINGS_CATEGORIES, resolveSettingsTab } from './settingsTabRegistry.js';
import { useSettingsRuntime } from './useSettingsRuntime.js';
import { useControlCommands } from '../app-control/useControlCommands.js';
import { maintenanceCommands } from '../app-control/maintenanceCommands.js';
import { command } from '../app-control/commandRegistry.js';
import { bool, choice, empty, object } from '../app-control/commandSchema.js';
import type { MouseEvent } from 'react';
import type { PluginPlatformRuntime } from '../plugins/pluginPlatformTypes.js';
import type { SettingsCategory } from './settingsTypes.js';
import type { UseSettingsRuntimeOptions } from './useSettingsRuntime.js';

export interface SettingsModalProps extends UseSettingsRuntimeOptions {
    plugins?: PluginPlatformRuntime;
    allowedCategories?: readonly SettingsCategory[];
    backendFeaturesEnabled?: boolean;
    dataWorkbenchEnabled?: boolean;
    onClose(): void;
}

export default function SettingsModal({
    isOpen,
    onClose,
    plugins,
    allowedCategories = SETTINGS_CATEGORIES.map((category) => category.key),
    backendFeaturesEnabled = true,
    dataWorkbenchEnabled = true,
    settings,
    onUpdate,
    currentSymbol = '',
    currentMarketType = 'spot',
    currentExchange = 'binance',
    watchlists = [],
    chartDataCacheDiagnostics = null,
    trimChartDataCacheEntries = null,
}: SettingsModalProps) {
    const [activeCategory, setActiveCategory] = useState<SettingsCategory>('appearance');
    const [pluginCenterOpen, setPluginCenterOpen] = useState(false);
    const closePluginCenter = useCallback(() => setPluginCenterOpen(false), []);
    const [dataWorkbenchOpen, setDataWorkbenchOpen] = useState(false);
    useLocale();
  const settingsRuntime = useSettingsRuntime({
        isOpen,
    settings,
    onUpdate,
        currentSymbol,
        currentMarketType,
        currentExchange,
        watchlists,
        chartDataCacheDiagnostics,
        trimChartDataCacheEntries,
    });
  const { view, actions } = settingsRuntime;
  // Instance identity allows several cells to open settings for the same symbol.
  const controlId = useId();
  useControlCommands(() => maintenanceCommands(controlId, settingsRuntime, backendFeaturesEnabled), isOpen);
  useControlCommands(() => ({ id: `settings-panel:${controlId}`, title: "Settings categories and tools", context: () => ({ activeCategory, pluginCenterOpen, dataWorkbenchOpen }), snapshot: () => ({ activeCategory, allowedCategories, backendFeaturesEnabled, dataWorkbenchEnabled, pluginCenterOpen, dataWorkbenchOpen }), commands: [
    command("category", "Select an available settings category.", object({ category: choice(["appearance", "network", "exchanges", "data", "plugins", "ai", "about"]) }), ({ category }) => {
      if (!allowedCategories.includes(category) || (!backendFeaturesEnabled && category !== "appearance" && category !== "about" && category !== "ai")) throw new Error("CATEGORY_UNAVAILABLE");
      if (category === "plugins" && plugins) setPluginCenterOpen(true); else setActiveCategory(category);
    }),
    command("dataWorkbench", "Open/close the data workbench.", object({ open: bool }), ({ open }) => setDataWorkbenchOpen(open), { available: () => dataWorkbenchEnabled && backendFeaturesEnabled }),
    command("close", "Close settings.", empty, () => onClose()),
  ] }), isOpen);

    if (!isOpen) return null;

    const panelModel = buildSettingsPanelViewModel({ view, actions });
    const visibleCategories = SETTINGS_CATEGORIES.filter((category) => (
      allowedCategories.includes(category.key)
      && (backendFeaturesEnabled || category.key === "appearance" || category.key === "about" || category.key === "ai")
    ));
    const resolvedActiveCategory = visibleCategories.some(
      (category) => category.key === activeCategory,
    ) ? activeCategory : visibleCategories[0]?.key ?? "appearance";
    const activeCatObj = resolveSettingsTab(resolvedActiveCategory);

    return (
      <>
        <div className="st-overlay" inert={pluginCenterOpen} aria-hidden={pluginCenterOpen || undefined} onClick={onClose}>
            <div className="st-panel" onClick={(event: MouseEvent<HTMLDivElement>) => event.stopPropagation()}>
                {/* Sidebar */}
                <nav className="st-sidebar">
                    <div className="st-sidebar-title">{t("settings.title")}</div>
                    <div className="st-sidebar-nav">
                        {visibleCategories.map(cat => (
                            <button
                                key={cat.key}
                                className={`st-nav-item ${activeCategory === cat.key ? 'active' : ''}`}
                                onClick={() => { if (cat.key === "plugins" && plugins) setPluginCenterOpen(true); else setActiveCategory(cat.key); }}
                            >
                                <span className="st-nav-icon" aria-hidden="true">{cat.icon}</span>
                                <span className="st-nav-label">{t(cat.labelKey)}</span>
                            </button>
                        ))}
                    </div>
                    <div className="st-sidebar-footer">
                        <button className="st-btn st-btn-primary st-btn-close" onClick={onClose}>
                            {t("settings.saveAndClose")}
                        </button>
                    </div>
                </nav>

                {/* Content */}
                <main className="st-content">
                    <div className="st-content-header">
                        <h2 className="st-content-title">
                            {activeCatObj.icon && <span>{activeCatObj.icon}</span>}
                            {t(activeCatObj.labelKey)}
                        </h2>
                        <button className="st-close-x" aria-label={t("settings.close")} onClick={onClose}>✕</button>
                    </div>
                    <div className="st-content-body">
                        <SettingsPanelHost
                            activeCategory={resolvedActiveCategory}
                            onOpenDataWorkbench={() => {
                              if (dataWorkbenchEnabled) setDataWorkbenchOpen(true);
                            }}
                            panelModel={panelModel}
                            plugins={plugins}
                        />
                    </div>
                </main>
            </div>
            <SettingsModalStyles />
        </div>
        {pluginCenterOpen && plugins && <PluginSettingsPanel runtime={plugins} onClose={closePluginCenter} />}
        {dataWorkbenchEnabled && <DataWorkbenchModal
            currentExchange={currentExchange}
            currentMarketType={currentMarketType}
            currentSymbol={currentSymbol}
            isOpen={dataWorkbenchOpen}
            onClose={() => setDataWorkbenchOpen(false)}
        />}
      </>
    );
}
