import { invoke } from "@tauri-apps/api/core";

// Thin wrappers over the Rust commands. No client-side state lives here: the backend owns
// recorder state and pushes it back via `recorder://state`.
export const api = {
  listSources: () => invoke("list_sources"),
  getRecorderState: () => invoke("get_recorder_state"),
  setSource: (source, label) => invoke("set_source", { source, label }),
  start: () => invoke("start_recording"),
  stop: () => invoke("stop_recording"),
  togglePause: () => invoke("toggle_pause"),
  selectRegion: () => invoke("select_region"),
  screenshot: () => invoke("take_screenshot"),
  screenshotRegion: () => invoke("screenshot_region"),
  getSettings: () => invoke("get_settings"),
  setSettings: (settings) => invoke("set_settings", { settings }),
  hideToTray: () => invoke("hide_to_tray"),
  reveal: (path) => invoke("reveal", { path }),
  openOutputDir: () => invoke("open_output_dir"),
};

export const fileName = (p) => p?.split(/[\\/]/).pop() ?? "";

export function formatElapsed(ms) {
  const s = Math.floor(ms / 1000);
  const hh = Math.floor(s / 3600);
  const mm = String(Math.floor((s % 3600) / 60)).padStart(2, "0");
  const ss = String(s % 60).padStart(2, "0");
  return hh ? `${hh}:${mm}:${ss}` : `${mm}:${ss}`;
}
