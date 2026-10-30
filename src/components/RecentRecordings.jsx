import { useState } from "react";
import { SavedOutputCard } from "./SavedOutputCard.jsx";

export function RecentRecordings({ recordings = [] }) {
  const [open, setOpen] = useState(false);
  if (recordings.length === 0) return null;

  return (
    <details className="recent-recordings" onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary>Recent recordings ({recordings.length})</summary>
      {open && (
        <div className="recent-list">
          <p className="muted">Last ten recordings from this app session. Closing to the tray keeps this list.</p>
          {recordings.map((output) => <SavedOutputCard key={output.path} output={output} announce={false} />)}
        </div>
      )}
    </details>
  );
}
