import { useState } from "react";
import { api, formatElapsed } from "../api.js";
import { Mic, MicOff, Pause, Play, Speaker, SpeakerOff, Stop } from "./Icons.jsx";
import { useElapsed } from "../hooks/useElapsed.js";
import { shortcutHint } from "../shortcutHint.js";

/** The whole window while recording: a 300×52 always-on-top pill (excluded from capture). */
export function RecordBar({ rec }) {
  const [error, setError] = useState(null);
  const elapsed = useElapsed(rec);
  const action = (run) => { setError(null); run().catch((e) => setError(String(e))); };
  const paused = rec.state === "paused";
  const finalizing = rec.state === "finalizing";
  const starting = rec.state === "starting";
  const system = rec.audio?.system;
  const mic = rec.audio?.mic;
  return (
    <div className="bar" data-tauri-drag-region>
      <span className="sr-only" role="status">{starting ? "Starting recording" : finalizing ? "Saving recording" : paused ? "Recording paused" : "Recording"}</span>
      <span className={`rec-dot ${paused ? "paused" : ""} ${finalizing ? "idle" : ""}`} />
      <span className="bar-time" data-tauri-drag-region title={`${rec.sourceLabel ?? "Selected source"}. Stop recording to change the source.`}>
        {starting ? "Starting…" : finalizing ? "Saving…" : formatElapsed(elapsed)}
      </span>
      <span className="bar-meta" data-tauri-drag-region>
        {error && <span className="warn" role="alert" title={error}>Action failed · </span>}
        {rec.notice && <span className="warn" title={rec.notice} aria-label={rec.notice} role="status">! </span>}
        {paused ? "Paused" : rec.fps > 0 ? `${rec.fps} fps` : ""}
        {rec.dropped > 0 && ` · ${rec.dropped} dropped`}
      </span>
      <div className="bar-actions">
        {system && system.state !== "off" && (
          <button className={`icon-btn ${system.state === "failed" ? "warn" : ""}`} disabled={finalizing}
            onClick={() => action(() => api.toggleAudioMute("system"))} aria-pressed={Boolean(rec.systemMuted)}
            aria-label={rec.systemMuted ? "Unmute system audio" : "Mute system audio"}
            title={system.message ?? (rec.systemMuted ? "System audio muted" : "Mute system audio")}>
            {rec.systemMuted ? <SpeakerOff /> : <Speaker />}{system.state === "failed" && "!"}
          </button>
        )}
        {mic && mic.state !== "off" && (
          <button className={`icon-btn ${mic.state === "failed" ? "warn" : ""}`}
            disabled={finalizing} onClick={() => action(mic.state === "failed" ? api.openMicrophoneSettings : () => api.toggleAudioMute("mic"))}
            aria-pressed={Boolean(rec.micMuted)} aria-label={mic.state === "failed" ? "Open microphone privacy settings" : rec.micMuted ? "Unmute microphone" : "Mute microphone"}
            title={mic.state === "failed" ? `${mic.message} · Open microphone privacy settings` : rec.micMuted ? "Microphone muted" : "Mute microphone"}>
            {rec.micMuted ? <MicOff /> : <Mic />}{mic.state === "failed" && "!"}
          </button>
        )}
        <button className="icon-btn" onClick={() => action(api.togglePause)} disabled={starting || finalizing} aria-label={paused ? "Resume recording" : "Pause recording"} title={`${paused ? "Resume" : "Pause"}${shortcutHint(rec.hotkeys, "pause")}`}>
          {paused ? <Play /> : <Pause />}
        </button>
        <button className="icon-btn stop" onClick={() => action(api.stop)} disabled={finalizing} aria-label="Stop recording" title={`Stop${shortcutHint(rec.hotkeys, "record")}`}>
          <Stop />
        </button>
      </div>
    </div>
  );
}
