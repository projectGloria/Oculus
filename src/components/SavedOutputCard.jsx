import { useEffect, useId, useRef, useState } from "react";
import { api, fileName, formatElapsed } from "../api.js";
import { Camera, Play } from "./Icons.jsx";
import { ErrorBanner } from "./ErrorBanner.jsx";
import { useFileDrag } from "../hooks/useFileDrag.js";

export function SavedOutputCard({ output, announce = true }) {
  const nameId = useId();
  const [details, setDetails] = useState(null);
  const [detailsFailed, setDetailsFailed] = useState(false);
  const [error, setError] = useState(null);
  const drag = useFileDrag(output.path, setError);
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(null);
  const [message, setMessage] = useState("");
  const name = fileName(output.path);
  const [draft, setDraft] = useState(() => name.replace(/\.[^.]+$/, ""));
  const editControl = useRef(null);
  useEffect(() => {
    let alive = true;
    api.savedFileDetails(output.path).then((d) => { if (alive) setDetails(d); })
      .catch((e) => {
        if (alive) { setDetailsFailed(true); setError(`Could not read saved file details: ${e}`); }
      });
    return () => { alive = false; };
  }, [output.path]);
  useEffect(() => { editControl.current?.focus(); }, [editing]);

  const action = async (label, run, success = "") => {
    setError(null);
    setMessage("");
    setBusy(true);
    try {
      await run();
      setMessage(success);
      setEditing(null);
    } catch (e) { setError(`Could not ${label}: ${e}`); }
    finally { setBusy(false); }
  };
  const screenshot = output.kind === "screenshot";
  const label = output.complete === false ? "Partial recording retained" : screenshot ? "Screenshot saved" : "Recording saved";

  return (
    <section className="saved-card" aria-label="Saved output">
      <div className="saved-summary">
        <button className="saved-preview" {...drag} disabled={busy} title="Click to open, or drag into another app" onClick={() => action("open saved file", () => api.openSavedFile(output.path))} aria-label={`Open ${name}`}>
          {details?.thumbnail ? <img src={details.thumbnail} alt="" draggable={false} /> : screenshot ? <Camera /> : <Play />}
        </button>
        <div className="saved-info">
          <p className="saved-label" role={announce ? "status" : undefined}>{label}</p>
          <p className="ellipsis saved-name" title={output.path}>{name}</p>
          <p className="muted">
            {output.durationMs != null && `${formatElapsed(output.durationMs)} · `}
            {details ? `${(details.sizeBytes / 1024 / 1024).toFixed(2)} MiB` : detailsFailed ? "File details unavailable" : "Reading file details…"}
          </p>
        </div>
      </div>
      <div className="saved-actions">
        <button className="link" disabled={busy} onClick={() => action("open saved file", () => api.openSavedFile(output.path))}>Open</button>
        <button className="link" disabled={busy} onClick={() => action("show saved file", () => api.reveal(output.path))}>Show in folder</button>
        <button className="link" disabled={busy} onClick={() => action("copy saved file", () => api.copySavedFile(output.path), "File copied to clipboard.")}>Copy file</button>
        <button className="link" disabled={busy} onClick={() => setEditing("rename")}>Rename</button>
        <button className="link" disabled={busy} onClick={() => setEditing("delete")}>Delete</button>
      </div>
      {editing === "rename" && (
        <form className="saved-edit" onSubmit={(e) => { e.preventDefault(); if (!busy) action("rename saved file", () => api.renameSavedFile(output.path, draft)); }}>
          <label htmlFor={nameId}>Filename (extension stays the same)</label>
          <input id={nameId} ref={editControl} value={draft} maxLength={180} onChange={(e) => setDraft(e.target.value)} disabled={busy} />
          <div className="saved-actions">
            <button className="link" type="submit" disabled={busy || !draft.trim()}>Save name</button>
            <button className="link" type="button" disabled={busy} onClick={() => setEditing(null)}>Cancel</button>
          </div>
        </form>
      )}
      {editing === "delete" && (
        <div className="saved-edit" role="group" aria-label="Confirm permanent deletion">
          <p>Permanently delete <strong>{name}</strong>? This cannot be undone.</p>
          <div className="saved-actions">
            <button className="link" disabled={busy} onClick={() => action("delete saved file", () => api.deleteSavedFile(output.path))}>Delete permanently</button>
            <button className="link" ref={editControl} disabled={busy} onClick={() => setEditing(null)}>Cancel</button>
          </div>
        </div>
      )}
      {message && <p className="muted" role="status">{message}</p>}
      <ErrorBanner error={error} />
    </section>
  );
}
