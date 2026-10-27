import { open } from "@tauri-apps/plugin-dialog";
import { Folder } from "./Icons.jsx";

function Segmented({ value, options, onChange }) {
  return (
    <div className="seg">
      {options.map(([v, label]) => (
        <button key={v} className={value === v ? "on" : ""} onClick={() => onChange(v)}>
          {label}
        </button>
      ))}
    </div>
  );
}

function Toggle({ checked, onChange, label }) {
  return (
    <label className="row toggle">
      <span>{label}</span>
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span className="switch" aria-hidden />
    </label>
  );
}

export function SettingsDrawer({ settings, update }) {
  const pickFolder = async () => {
    const dir = await open({ directory: true, defaultPath: settings.outputDir });
    if (typeof dir === "string") update({ outputDir: dir });
  };

  return (
    <section className="drawer">
      <div className="row">
        <span>Save to</span>
        <button className="btn ghost path" onClick={pickFolder} title={settings.outputDir}>
          <Folder /> <span className="ellipsis">{settings.outputDir}</span>
        </button>
      </div>
      <div className="row">
        <span>Frame rate</span>
        <Segmented value={settings.fps} options={[[30, "30"], [60, "60"]]} onChange={(fps) => update({ fps })} />
      </div>
      <div className="row">
        <span>Quality</span>
        <Segmented value={settings.quality} options={[["low", "Low"], ["medium", "Medium"], ["high", "High"]]} onChange={(quality) => update({ quality })} />
      </div>
      <div className="row">
        <span>Codec</span>
        <Segmented value={settings.codec} options={[["h264", "H.264"], ["hevc", "HEVC"]]} onChange={(codec) => update({ codec })} />
      </div>
      <Toggle label="Capture cursor" checked={settings.cursor} onChange={(cursor) => update({ cursor })} />
      <Toggle label="Copy screenshots to clipboard" checked={settings.copyScreenshots} onChange={(copyScreenshots) => update({ copyScreenshots })} />
      <div className="row">
        <span>System volume</span>
        <input type="range" min={0} max={2} step={0.05} value={settings.systemGain} onChange={(e) => update({ systemGain: +e.target.value })} />
      </div>
      <div className="row">
        <span>Mic volume</span>
        <input type="range" min={0} max={2} step={0.05} value={settings.micGain} onChange={(e) => update({ micGain: +e.target.value })} />
      </div>
      <p className="hint">
        <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>R</kbd> record · <kbd>P</kbd> pause · <kbd>S</kbd> screenshot · <kbd>A</kbd> area shot
      </p>
    </section>
  );
}
