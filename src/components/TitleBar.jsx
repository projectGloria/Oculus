import { getCurrentWindow } from "@tauri-apps/api/window";
import { api } from "../api.js";
import { Close, Gear, Minus } from "./Icons.jsx";

export function TitleBar({ onSettings, settingsOpen }) {
  return (
    // data-tauri-drag-region: Tauri turns mousedown here into a native window drag.
    <header className="titlebar" data-tauri-drag-region>
      <div className="brand" data-tauri-drag-region>
        <span className="brand-dot" />
        Oculus
      </div>
      <div className="titlebar-actions">
        <button className={`icon-btn ${settingsOpen ? "on" : ""}`} onClick={onSettings} title="Settings">
          <Gear />
        </button>
        <button className="icon-btn" onClick={() => getCurrentWindow().minimize()} title="Minimize">
          <Minus />
        </button>
        {/* Close = send to tray: the webview is destroyed, hotkeys + tray keep working. */}
        <button className="icon-btn close" onClick={api.hideToTray} title="Hide to tray">
          <Close />
        </button>
      </div>
    </header>
  );
}
