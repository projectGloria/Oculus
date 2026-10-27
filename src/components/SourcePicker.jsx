import { useState } from "react";
import { api } from "../api.js";
import { useSources } from "../hooks/useSources.js";
import { AppWindow, Crop, Monitor, Refresh } from "./Icons.jsx";

const TABS = [
  { id: "monitor", label: "Screen", Icon: Monitor },
  { id: "window", label: "Window", Icon: AppWindow },
  { id: "region", label: "Area", Icon: Crop },
];

export function SourcePicker({ source, sourceLabel }) {
  const [sources, refresh] = useSources();
  // The tab follows the backend's source unless the user is browsing another tab. Derived,
  // not synced: no effect copying `source.kind` into state.
  const [browsing, setBrowsing] = useState(null);
  const tab = browsing ?? source?.kind ?? "monitor";
  const [query, setQuery] = useState("");

  const choose = (src, label) => {
    setBrowsing(null);
    api.setSource(src, label);
  };

  // Nothing chosen yet → the backend records the primary display; show it as selected.
  const primaryId = sources.monitors.find((m) => m.primary)?.id;
  const selectedMonitor = source?.kind === "monitor" ? source.id : source ? null : primaryId;

  const q = query.trim().toLowerCase();
  const windows = q
    ? sources.windows.filter((w) => w.title.toLowerCase().includes(q) || w.exe.toLowerCase().includes(q))
    : sources.windows;

  return (
    <section className="picker">
      <div className="tabs" role="tablist">
        {TABS.map(({ id, label, Icon }) => (
          <button key={id} role="tab" aria-selected={tab === id} className={`tab ${tab === id ? "on" : ""}`} onClick={() => setBrowsing(id)}>
            <Icon /> {label}
          </button>
        ))}
      </div>

      {!sources.supported && <p className="warn">Screen capture isn't supported on this Windows build.</p>}

      {tab === "monitor" && (
        <ul className="list">
          {sources.monitors.map((m) => (
            <li key={m.id}>
              <button className={`card ${selectedMonitor === m.id ? "on" : ""}`} onClick={() => choose({ kind: "monitor", id: m.id }, m.name)}>
                <Monitor width={18} height={18} />
                <span className="card-main">
                  <span className="card-title">
                    {m.name} {m.primary && <span className="badge">Primary</span>}
                  </span>
                  <span className="card-sub">
                    {m.width} × {m.height}
                  </span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}

      {tab === "window" && (
        <>
          <div className="search">
            <input placeholder="Search windows…" value={query} onChange={(e) => setQuery(e.target.value)} spellCheck={false} />
            <button className="icon-btn" onClick={refresh} title="Refresh">
              <Refresh />
            </button>
          </div>
          <ul className="list scroll">
            {windows.map((w) => (
              <li key={w.id}>
                <button
                  className={`card ${source?.kind === "window" && source.id === w.id ? "on" : ""}`}
                  onClick={() => choose({ kind: "window", id: w.id }, w.title)}
                  title={w.title}
                >
                  <AppWindow width={18} height={18} />
                  <span className="card-main">
                    <span className="card-title ellipsis">{w.title}</span>
                    <span className="card-sub">{w.exe}</span>
                  </span>
                </button>
              </li>
            ))}
            {windows.length === 0 && <li className="empty">No windows match.</li>}
          </ul>
        </>
      )}

      {tab === "region" && (
        <div className="region">
          {source?.kind === "region" ? (
            <p className="region-current">
              <Crop /> {sourceLabel}
            </p>
          ) : (
            <p className="muted">Drag a rectangle on the screen under your cursor.</p>
          )}
          {/* select_region sets the source in Rust; the new snapshot flips us to it. */}
          <button className="btn" onClick={() => api.selectRegion().then(() => setBrowsing(null)).catch(() => {})}>
            {source?.kind === "region" ? "Reselect area" : "Select area"}
          </button>
        </div>
      )}
    </section>
  );
}
