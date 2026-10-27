import { useState } from "react";
import { api, fileName } from "./api.js";
import { useRecorder } from "./hooks/useRecorder.js";
import { useSettings } from "./hooks/useSettings.js";
import { TitleBar } from "./components/TitleBar.jsx";
import { SourcePicker } from "./components/SourcePicker.jsx";
import { RecordBar } from "./components/RecordBar.jsx";
import { SettingsDrawer } from "./components/SettingsDrawer.jsx";
import { Camera, Crop, Mic, Speaker } from "./components/Icons.jsx";

export default function App() {
  const rec = useRecorder();
  // Rust resizes the window into the compact bar when a session starts; the view just
  // follows state. Same webview, so a reopened window lands in the right mode too.
  if (rec.state !== "idle") return <RecordBar rec={rec} />;
  return <Panel rec={rec} />;
}

function Panel({ rec }) {
  const [settings, update] = useSettings();
  const [showSettings, setShowSettings] = useState(false);
  const [startError, setStartError] = useState(null);

  const start = () => {
    setStartError(null);
    api.start().catch((e) => setStartError(String(e)));
  };
  // Prefer the immediate start failure; otherwise whatever the backend last reported.
  const error = startError ?? rec.error;

  return (
    <div className="app">
      <TitleBar settingsOpen={showSettings} onSettings={() => setShowSettings((v) => !v)} />
      <main className="content">
        {showSettings && settings ? (
          <SettingsDrawer settings={settings} update={update} />
        ) : (
          <>
            <SourcePicker source={rec.source} sourceLabel={rec.sourceLabel} />
            {settings && (
              <div className="audio">
                <button className={`chip ${settings.systemAudio ? "on" : ""}`} onClick={() => update({ systemAudio: !settings.systemAudio })}>
                  <Speaker /> System audio
                </button>
                <button className={`chip ${settings.mic ? "on" : ""}`} onClick={() => update({ mic: !settings.mic })}>
                  <Mic /> Microphone
                </button>
              </div>
            )}
          </>
        )}
      </main>

      <footer className="footer">
        <div className="actions">
          <button className="record" onClick={start} title="Record (Ctrl+Shift+R)">
            <span className="record-dot" /> Record
          </button>
          <button className="btn square" onClick={() => api.screenshot().catch(() => {})} title="Screenshot (Ctrl+Shift+S)">
            <Camera width={18} height={18} />
          </button>
          <button className="btn square" onClick={() => api.screenshotRegion().catch(() => {})} title="Area screenshot (Ctrl+Shift+A)">
            <Crop width={18} height={18} />
          </button>
        </div>
        <Status rec={rec} error={error} />
      </footer>
    </div>
  );
}

function Status({ rec, error }) {
  if (error) return <p className="status err">{error}</p>;
  const file = rec.lastSaved;
  if (!file)
    return (
      <p className="status">
        <button className="link" onClick={api.openOutputDir}>Open recordings folder</button>
      </p>
    );
  return (
    <p className="status">
      Saved <button className="link" onClick={() => api.reveal(file).catch(() => {})}>{fileName(file)}</button>
    </p>
  );
}
