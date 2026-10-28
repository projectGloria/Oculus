import { useEffect, useState } from "react";
import { api } from "../api.js";
import { ErrorBanner } from "./ErrorBanner.jsx";

export function AudioDevices({ settings, update }) {
  const [devices, setDevices] = useState([]);
  const [error, setError] = useState(null);
  useEffect(() => {
    let alive = true;
    api.listAudioDevices().then((value) => { if (alive) setDevices(value); })
      .catch((e) => { if (alive) setError(`Could not list audio devices: ${e}`); });
    return () => { alive = false; };
  }, []);
  return (
    <>
      {[["system", "systemDevice", "System audio device"], ["mic", "micDevice", "Microphone device"]].map(([source, key, label]) => (
        <label className="row device-row" key={key}>
          <span>{label}</span>
          <select aria-label={label} value={settings[key] ?? ""} onChange={(e) => update({ [key]: e.target.value || null })}>
            <option value="">Default (follows Windows)</option>
            {settings[key] && !devices.some((device) => device.id === settings[key]) && <option value={settings[key]}>Saved device unavailable</option>}
            {devices.filter((device) => device.source === source).map((device) => <option key={device.id} value={device.id}>{device.name}{device.default ? " (current default)" : ""}</option>)}
          </select>
        </label>
      ))}
      <button className="link" onClick={() => { setError(null); api.listAudioDevices().then(setDevices).catch((e) => setError(`Could not refresh audio devices: ${e}`)); }}>Refresh audio devices</button>
      <ErrorBanner error={error} />
    </>
  );
}
