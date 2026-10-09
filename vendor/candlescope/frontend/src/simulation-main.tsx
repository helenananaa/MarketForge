import "@fontsource/inter/latin-400.css";
import "@fontsource/inter/latin-600.css";
import "@fontsource/jetbrains-mono/latin-400.css";
import "./index.css";
import "./editor/simulationEditorSetup.js";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { initializeLocale } from "./i18n/index.js";
import { readPersistedLocale } from "./features/settings/chartAppearanceSettings.js";
import RoomPortalApp from "./features/simulation/RoomPortalApp.js";

async function mount(): Promise<void> {
  await initializeLocale(readPersistedLocale());
  const root = document.getElementById("root");
  if (!root) throw new Error("Simulation document root is missing");
  createRoot(root).render(<StrictMode><RoomPortalApp /></StrictMode>);
}
void mount();
