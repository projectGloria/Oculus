import { useState } from "react";
import { api } from "../api.js";
import { ErrorBanner } from "./ErrorBanner.jsx";

const DEFAULTS = { enabled: true, record: "Ctrl+Shift+R", pause: "Ctrl+Shift+P", screenshot: "Ctrl+Shift+S", area: "Ctrl+Shift+A", recordArea: "", recordWindow: "", muteMic: "" };
const FIELDS = [["record", "Start / stop"], ["pause", "Pause / resume"], ["screenshot", "Screenshot"], ["area", "Area screenshot"], ["recordArea", "Record an area"], ["recordWindow", "Record window under cursor"], ["muteMic", "Mute / unmute microphone"]];

export function HotkeySettings({ value, update, errors }) {
  const [draft, setDraft] = useState(value ?? DEFAULTS);
  const [error, setError] = useState(null);
  return (
    <section className="settings-section" aria-label="Hotkeys">
      <h2>Hotkeys</h2>
      <label className="row toggle">
        <span>Enable global hotkeys</span>
        <input type="checkbox" checked={draft.enabled} onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })} />
        <span className="switch" aria-hidden />
      </label>
      <p className="hint">Use Ctrl, Alt or Win, such as Ctrl+Shift+R. Leave a field empty to disable that action.</p>
      {FIELDS.map(([key, label]) => (
        <label className="row hotkey-row" key={key}>
          <span>{label}</span>
          <input aria-label={`${label} shortcut`} value={draft[key] ?? ""} disabled={!draft.enabled} maxLength={80}
            spellCheck={false} onChange={(e) => setDraft({ ...draft, [key]: e.target.value })} />
        </label>
      ))}
      <div className="saved-actions">
        <button className="btn" onClick={() => update({ hotkeys: draft })}>Save hotkeys</button>
        <button className="link" onClick={() => { setDraft(DEFAULTS); update({ hotkeys: DEFAULTS }); }}>Reset defaults</button>
        <button className="link" onClick={() => { setError(null); api.retryHotkeys().catch((e) => setError(String(e))); }}>Retry saved hotkeys</button>
      </div>
      {errors.map((message) => <p key={message} className="hint warn" role="alert">{message}</p>)}
      <ErrorBanner error={error} />
    </section>
  );
}
