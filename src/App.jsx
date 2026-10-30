import { useEffect, useRef, useState } from "react";
import { api, fileName } from "./api.js";
import { useRecorder } from "./hooks/useRecorder.js";
import { useSettings } from "./hooks/useSettings.js";
import { TitleBar } from "./components/TitleBar.jsx";
import { SourcePicker } from "./components/SourcePicker.jsx";
import { RecordBar } from "./components/RecordBar.jsx";
import { SettingsDrawer } from "./components/SettingsDrawer.jsx";
import { ErrorBanner } from "./components/ErrorBanner.jsx";
import { SavedOutputCard } from "./components/SavedOutputCard.jsx";
import { RecentRecordings } from "./components/RecentRecordings.jsx";
import { Camera, Crop, Mic, Speaker } from "./components/Icons.jsx";
import { shortcutHint } from "./shortcutHint.js";

export default function App() {
  const rec = useRecorder();
  // Rust resizes the window into the compact bar when a session starts; the view just
  // follows state. Same webview, so a reopened window lands in the right mode too.
  if (rec.state !== "idle") return <RecordBar rec={rec} />;
  return <Panel rec={rec} />;
}

function Panel({ rec }) {
  const [settings, update, settingsError] = useSettings();
  const [showSettings, setShowSettings] = useState(false);
  const [actionError, setActionError] = useState(null);
  const content = useRef(null);
  const previousView = useRef(false);
  useEffect(() => {
    if (showSettings === previousView.current || (showSettings && !settings)) return;
    previousView.current = showSettings;
    const first = content.current?.querySelector('[role="tab"][aria-selected="true"], button:not(:disabled), input:not(:disabled)');
    first?.focus();
  }, [showSettings, settings]);

  const start = () => {
    setActionError(null);
    api.start().catch((e) => setActionError(`Could not start recording: ${e}`));
  };
  // Prefer the immediate start failure; otherwise whatever the backend last reported.
  const error = actionError ?? settingsError ?? rec.error;

  return (
    <div className="app">
      <TitleBar settingsOpen={showSettings} onSettings={() => setShowSettings((v) => !v)} />
      <main className="content" ref={content}>
        {showSettings && settings ? (
          <SettingsDrawer settings={settings} update={update} hotkeyErrors={rec.hotkeyErrors} />
        ) : (
          <>
            <SourcePicker source={rec.source} sourceLabel={rec.sourceLabel} />
            {settings && (
              <div className="audio">
                <button className={`chip ${settings.systemAudio ? "on" : ""}`} aria-pressed={settings.systemAudio} onClick={() => update({ systemAudio: !settings.systemAudio })}>
                  <Speaker /> System audio
                </button>
                <button className={`chip ${settings.mic ? "on" : ""}`} aria-pressed={settings.mic} onClick={() => update({ mic: !settings.mic })}>
                  <Mic /> Microphone
                </button>
              </div>
            )}
            <RecentRecordings recordings={rec.recentRecordings} />
          </>
        )}
      </main>

      <footer className="footer">
        <div className="actions">
          <button className="record" onClick={start} title={`Record${shortcutHint(rec.hotkeys, "record")}`}>
            <span className="record-dot" /> Record
          </button>
          <button className="btn square" onClick={() => { setActionError(null); api.screenshot().catch((e) => setActionError(`Could not take screenshot: ${e}`)); }} title={`Screenshot${shortcutHint(rec.hotkeys, "screenshot")}`} aria-label="Screenshot">
            <Camera width={18} height={18} />
          </button>
          <button className="btn square" onClick={() => { setActionError(null); api.screenshotRegion().catch((e) => setActionError(`Could not take area screenshot: ${e}`)); }} title={`Area screenshot${shortcutHint(rec.hotkeys, "area")}`} aria-label="Area screenshot">
            <Crop width={18} height={18} />
          </button>
        </div>
        <Status rec={rec} error={error} onError={(e) => setActionError(e == null ? null : String(e))} />
      </footer>
    </div>
  );
}

function Status({ rec, error, onError }) {
  return (
    <>
      <ErrorBanner error={error} />
      <SavedStatus rec={rec} onError={onError} />
    </>
  );
}

function SavedStatus({ rec, onError }) {
  if (rec.savedOutput) return (
    <>
      {rec.notice && <p className="status" role="status" title={rec.notice}>{rec.notice}</p>}
      <SavedOutputCard key={rec.savedOutput.path} output={rec.savedOutput} />
    </>
  );
  if (rec.notice) return <p className="status" role="status">{rec.notice}</p>;
  const file = rec.lastSaved;
  if (!file)
    return (
      <p className="status">
        <button className="link" onClick={() => { onError(null); api.openOutputDir().catch((e) => onError(`Could not open recordings folder: ${e}`)); }}>Open recordings folder</button>
      </p>
    );
  return (
    <p className="status" role="status">
      Saved <button className="link" onClick={() => { onError(null); api.reveal(file).catch((e) => onError(`Could not show saved file: ${e}`)); }}>{fileName(file)}</button>
    </p>
  );
}
