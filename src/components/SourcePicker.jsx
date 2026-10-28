import { useState } from "react";
import { api } from "../api.js";
import { useSources } from "../hooks/useSources.js";
import { AppWindow, Crop, Monitor, Refresh } from "./Icons.jsx";
import { ErrorBanner } from "./ErrorBanner.jsx";

const TABS = [
  { id: "monitor", label: "Screen", Icon: Monitor },
  { id: "window", label: "Window", Icon: AppWindow },
  { id: "region", label: "Area", Icon: Crop },
];

export function SourcePicker({ source, sourceLabel }) {
  const [sources, refresh, sourcesError] = useSources();
  // The tab follows the backend's source unless the user is browsing another tab. Derived,
  // not synced: no effect copying `source.kind` into state.
  const [browsing, setBrowsing] = useState(null);
  const tab = browsing ?? source?.kind ?? "monitor";
  const [query, setQuery] = useState("");
  const [error, setError] = useState(null);

  const choose = (src, label) => {
    setError(null);
    setBrowsing(null);
    api.setSource(src, label).catch((e) => setError(`Could not select capture source: ${e}`));
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
      <ErrorBanner error={error ?? sourcesError} />
      <div className="tabs" role="tablist">
        {TABS.map(({ id, label, Icon }) => (
          <button key={id} role="tab" id={`${id}-tab`} aria-controls={`${id}-panel`} aria-selected={tab === id}
            tabIndex={tab === id ? 0 : -1} className={`tab ${tab === id ? "on" : ""}`} onClick={() => setBrowsing(id)}
            onKeyDown={(e) => {
              const index = TABS.findIndex((t) => t.id === id);
              let next;
              if (e.key === "ArrowRight") next = (index + 1) % TABS.length;
              else if (e.key === "ArrowLeft") next = (index + TABS.length - 1) % TABS.length;
              else if (e.key === "Home") next = 0;
              else if (e.key === "End") next = TABS.length - 1;
              else return;
              e.preventDefault();
              setBrowsing(TABS[next].id);
              e.currentTarget.parentElement.children[next].focus();
            }}>
            <Icon /> {label}
          </button>
        ))}
      </div>

      {!sources.supported && <p className="warn">Screen capture isn't supported on this Windows build.</p>}

      {TABS.map(({ id }) => (
        <div key={id} className="picker-panel" role="tabpanel" id={`${id}-panel`} aria-labelledby={`${id}-tab`} hidden={tab !== id}>
          {tab === id && (
            <>
              {tab === "monitor" && (
                <ul className="list">
                  {sources.monitors.length === 0 && <li className="empty">No displays found. Connect a display and refresh the source list.</li>}
                  {sources.monitors.map((m) => (
                    <li key={m.id}>
                      <button className={`card ${selectedMonitor === m.id ? "on" : ""}`} aria-pressed={selectedMonitor === m.id} onClick={() => choose({ kind: "monitor", id: m.id }, m.name)}>
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
                    <input aria-label="Search windows" placeholder="Search windows…" value={query} onChange={(e) => setQuery(e.target.value)} spellCheck={false} />
                    <button className="icon-btn" onClick={refresh} title="Refresh" aria-label="Refresh sources">
                      <Refresh />
                    </button>
                  </div>
                  <ul className="list scroll">
                    {windows.map((w) => (
                      <li key={w.id}>
                        <button
                          className={`card ${source?.kind === "window" && source.id === w.id ? "on" : ""}`}
                          disabled={w.minimized} aria-pressed={source?.kind === "window" && source.id === w.id}
                          onClick={() => choose({ kind: "window", id: w.id }, w.title)}
                          title={w.minimized ? `${w.title} · Restore this window before recording` : w.title}
                        >
                          <AppWindow width={18} height={18} />
                          <span className="card-main">
                            <span className="card-title ellipsis">{w.title}</span>
                            <span className="card-sub">{w.exe}{w.minimized && " · Minimized: restore to record"}</span>
                          </span>
                        </button>
                      </li>
                    ))}
                    {windows.length === 0 && <li className="empty">{q ? "No windows match your search." : "Open or restore the app you want to record, then refresh."}</li>}
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
                  <button className="btn" onClick={() => { setError(null); api.selectRegion().then(() => setBrowsing(null)).catch((e) => setError(`Could not select area: ${e}`)); }}>
                    {source?.kind === "region" ? "Reselect area" : "Select area"}
                  </button>
                  <button className="btn" onClick={() => { setError(null); api.recordRegion().catch((e) => setError(`Could not record area: ${e}`)); }}>Select area and record</button>
                </div>
              )}
            </>
          )}
        </div>
      ))}
    </section>
  );
}
