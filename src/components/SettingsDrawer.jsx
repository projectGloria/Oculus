import { useEffect, useId, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { Folder } from "./Icons.jsx";
import { ErrorBanner } from "./ErrorBanner.jsx";
import { HotkeySettings } from "./HotkeySettings.jsx";
import { About } from "./About.jsx";
import { AudioDevices } from "./AudioDevices.jsx";
import { dbToGain, formatGain, gainToDb } from "../gain.js";

function Segmented({ value, options, onChange, label }) {
  return (
    <div className="seg" role="radiogroup" aria-label={label}>
      {options.map(([v, text], index) => (
        <button key={v} role="radio" aria-checked={value === v} tabIndex={value === v ? 0 : -1}
          className={value === v ? "on" : ""} onClick={() => onChange(v)}
          onKeyDown={(e) => {
            let next;
            if (["ArrowRight", "ArrowDown"].includes(e.key)) next = (index + 1) % options.length;
            else if (["ArrowLeft", "ArrowUp"].includes(e.key)) next = (index + options.length - 1) % options.length;
            else if (e.key === "Home") next = 0;
            else if (e.key === "End") next = options.length - 1;
            else return;
            e.preventDefault();
            onChange(options[next][0]);
            e.currentTarget.parentElement.children[next].focus();
          }}>
          {text}
        </button>
      ))}
    </div>
  );
}

function GainSlider({ label, value, onCommit }) {
  const detent = useId();
  const [draft, setDraft] = useState(null);
  const pending = useRef(null);
  const timer = useRef(null);
  useEffect(() => () => clearTimeout(timer.current), []);
  const commit = () => {
    clearTimeout(timer.current);
    if (pending.current === null) return;
    onCommit(pending.current);
    pending.current = null;
    setDraft(null);
  };
  const shown = draft ?? value;
  return (
    <label className="row">
      <span>{label} <span className="muted">{formatGain(shown)}</span></span>
      <input type="range" min={-60} max={6} step={0.5} value={gainToDb(shown)} aria-label={label} list={detent}
        aria-valuetext={shown > 0 ? formatGain(shown) : "Muted, minus infinity decibels"}
        onChange={(e) => {
          pending.current = dbToGain(+e.target.value);
          setDraft(pending.current);
          clearTimeout(timer.current);
          timer.current = setTimeout(commit, 150);
        }}
        onPointerUp={commit} onPointerCancel={commit} onKeyUp={commit} onBlur={commit}
        onDoubleClick={() => { pending.current = 1; commit(); }} />
      <datalist id={detent}><option value={0} label="0 dB" /></datalist>
    </label>
  );
}

function Toggle({ checked, onChange, label, disabled = false }) {
  return (
    <label className="row toggle">
      <span>{label}</span>
      <input type="checkbox" checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
      <span className="switch" aria-hidden />
    </label>
  );
}

function Bitrate({ value, onCommit }) {
  const [draft, setDraft] = useState(null);
  const [error, setError] = useState(null);
  const commit = () => {
    if (draft === null) return;
    const number = Number(draft);
    if (draft !== "" && (!Number.isInteger(number) || number < 2 || number > 150)) {
      setError("Enter a whole number from 2 to 150 Mbps, or leave empty for automatic.");
      return;
    }
    setError(null);
    onCommit(draft === "" ? null : number);
    setDraft(null);
  };
  return (
    <>
      <label className="row device-row">
        <span>Custom bitrate (Mbps)</span>
        <input type="number" aria-label="Custom bitrate in Mbps" min={2} max={150} step={1}
          placeholder="Automatic" value={draft ?? value ?? ""} onChange={(e) => setDraft(e.target.value)}
          onBlur={commit} onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }} />
      </label>
      {error && <p className="hint warn" role="alert">{error}</p>}
    </>
  );
}

export function SettingsDrawer({ settings, update, hotkeyErrors = [] }) {
  const [error, setError] = useState(null);
  const pickFolder = async () => {
    try {
      setError(null);
      const dir = await open({ directory: true, defaultPath: settings.outputDir });
      if (typeof dir === "string") update({ outputDir: dir });
    } catch (e) { setError(`Could not choose output folder: ${e}`); }
  };

  return (
    <section className="drawer">
      <ErrorBanner error={error} />
      <h2 className="settings-heading">Recording</h2>
      <Toggle label="Launch at login, minimized to tray" checked={settings.launchAtLogin} onChange={(launchAtLogin) => update({ launchAtLogin })} />
      <div className="row">
        <span>Save to</span>
        <button className="btn ghost path" onClick={pickFolder} title={settings.outputDir}>
          <Folder /> <span className="ellipsis">{settings.outputDir}</span>
        </button>
      </div>
      <label className="row device-row">
        <span>Frame rate</span>
        <select aria-label="Frame rate" value={settings.fps} onChange={(e) => update({ fps: +e.target.value })}>
          {[24, 30, 48, 50, 60, 72, 120].map((fps) => <option key={fps} value={fps}>{fps} fps</option>)}
        </select>
      </label>
      <p className="hint">High frame rates require a compatible display and encoder.</p>
      <Toggle label="Constant frame rate" checked={settings.constantFrameRate ?? true}
        disabled={settings.codec === "h264"} onChange={(constantFrameRate) => update({ constantFrameRate })} />
      <p className="hint">Repeats frames on static screens for consistent timing and editor compatibility.
        {settings.codec === "h264" && " Required for H.264 crash-retaining recordings."}</p>
      <label className="row device-row">
        <span>Output height</span>
        <select aria-label="Output height" value={settings.outputHeight ?? 0} onChange={(e) => update({ outputHeight: +e.target.value })}>
          <option value={0}>Original (within encoder limits)</option>
          {[720, 1080, 1440, 2160].map((height) => <option key={height} value={height}>Up to {height} pixels tall</option>)}
        </select>
      </label>
      <div className="row">
        <span>Quality</span>
        <Segmented label="Quality" value={settings.quality} options={[["low", "Low"], ["medium", "Medium"], ["high", "High"]]} onChange={(quality) => update({ quality })} />
      </div>
      <div className="row">
        <span>Codec</span>
        <Segmented label="Codec" value={settings.codec} options={[["h264", "H.264"], ["hevc", "HEVC"]]} onChange={(codec) => update({ codec, constantFrameRate: codec === "h264" ? true : settings.constantFrameRate })} />
      </div>
      {settings.codec === "hevc" && <p className="hint warn">HEVC requires successful finalization. A crash or power loss can leave an unplayable file.</p>}
      <label className="row device-row">
        <span>Rate control</span>
        <select aria-label="Rate control" value={settings.rateControl ?? "default"} onChange={(e) => update({ rateControl: e.target.value })}>
          <option value="default">Encoder default</option><option value="cbr">Constant bitrate</option><option value="vbr">Variable bitrate</option>
        </select>
      </label>
      <Bitrate value={settings.customBitrateMbps} onCommit={(customBitrateMbps) => update({ customBitrateMbps })} />
      <Toggle label="Capture cursor" checked={settings.cursor} onChange={(cursor) => update({ cursor })} />
      <h2 className="settings-heading">Audio</h2>
      <AudioDevices settings={settings} update={update} />
      <GainSlider label="System volume" value={settings.systemGain} onCommit={(systemGain) => update({ systemGain })} />
      <GainSlider label="Mic volume" value={settings.micGain} onCommit={(micGain) => update({ micGain })} />
      <h2 className="settings-heading">Screenshots</h2>
      <Toggle label="Copy screenshots to clipboard" checked={settings.copyScreenshots} onChange={(copyScreenshots) => update({ copyScreenshots })} />
      <Toggle label="Screenshot sound" checked={settings.screenshotSound} onChange={(screenshotSound) => update({ screenshotSound })} />
      <HotkeySettings value={settings.hotkeys} update={update} errors={hotkeyErrors} />
      <About />
    </section>
  );
}
