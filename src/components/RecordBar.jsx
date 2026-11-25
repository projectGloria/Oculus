import { useState } from "react";
import { api, formatElapsed } from "../api.js";
import { Minus, Mic, MicOff, Pause, Play, Speaker, SpeakerOff, Stop } from "./Icons.jsx";
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
  const replaySaved = rec.replay && rec.notice === "Replay saved";
  const system = rec.audio?.system;
  const mic = rec.audio?.mic;
  return (
    <div className="bar" data-tauri-drag-region>
      <span className="sr-only" role="status">{starting ? "Starting recording" : finalizing ? "Saving recording" : rec.replay ? "Pre-recording" : paused ? "Recording paused" : "Recording"}</span>
      <span className={`rec-dot ${paused ? "paused" : ""} ${finalizing ? "idle" : ""}`} />
      <span className="bar-time" data-tauri-drag-region title={`${rec.sourceLabel ?? "Selected source"}. Stop recording to change the source.`}>
        {starting ? "Starting…" : finalizing ? "Saving…" : rec.replay ? `Replay ${formatElapsed(Math.min(elapsed, rec.replaySeconds * 1000))}` : formatElapsed(elapsed)}
      </span>
      <span className="bar-meta" data-tauri-drag-region>
        {(error || rec.error) && <span className="warn" role="alert" title={error || rec.error}>Action failed · </span>}
        {rec.notice && !replaySaved && <span className="warn" title={rec.notice} aria-label={rec.notice} role="status">! </span>}
        {rec.savingReplay ? "Saving…" : replaySaved ? "Saved" : paused ? "Paused" : rec.fps > 0 ? `${rec.fps} fps` : ""}
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
        {rec.replay ? <button className="icon-btn replay-save" onClick={() => action(api.saveReplay)} disabled={starting || finalizing}
          aria-label="Save replay" title={`Save replay${shortcutHint(rec.hotkeys, "saveReplay")}`}>Save</button> :
        <button className="icon-btn" onClick={() => action(api.togglePause)} disabled={starting || finalizing} aria-label={paused ? "Resume recording" : "Pause recording"} title={`${paused ? "Resume" : "Pause"}${shortcutHint(rec.hotkeys, "pause")}`}>
          {paused ? <Play /> : <Pause />}
        </button>}
        {rec.replay && <button className="icon-btn" title="Continue pre-recording in tray" onClick={() => action(api.hideToTray)} aria-label="Continue pre-recording in tray"><Minus /></button>}
        <button className="icon-btn stop" onClick={() => action(api.stop)} disabled={finalizing} aria-label="Stop recording" title={`Stop${shortcutHint(rec.hotkeys, "record")}`}>
          <Stop />
        </button>
      </div>
    </div>
  );
}
