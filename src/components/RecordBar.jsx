import { api, formatElapsed } from "../api.js";
import { Pause, Play, Stop } from "./Icons.jsx";

/** The whole window while recording: a 300×52 always-on-top pill (excluded from capture). */
export function RecordBar({ rec }) {
  const paused = rec.state === "paused";
  const finalizing = rec.state === "finalizing";
  return (
    <div className="bar" data-tauri-drag-region>
      <span className={`rec-dot ${paused ? "paused" : ""} ${finalizing ? "idle" : ""}`} />
      <span className="bar-time" data-tauri-drag-region>
        {finalizing ? "Saving…" : formatElapsed(rec.elapsedMs)}
      </span>
      <span className="bar-meta" data-tauri-drag-region>
        {paused ? "Paused" : `${rec.fps} fps`}
        {rec.dropped > 0 && ` · ${rec.dropped} dropped`}
      </span>
      <div className="bar-actions">
        <button className="icon-btn" onClick={api.togglePause} disabled={finalizing} title={paused ? "Resume (Ctrl+Shift+P)" : "Pause (Ctrl+Shift+P)"}>
          {paused ? <Play /> : <Pause />}
        </button>
        <button className="icon-btn stop" onClick={api.stop} disabled={finalizing} title="Stop (Ctrl+Shift+R)">
          <Stop />
        </button>
      </div>
    </div>
  );
}
