import { useEffect, useState } from "react";
import { api } from "../api.js";
import { createSettingsQueue } from "../settingsQueue.js";

/** Settings persisted by Rust. Optimistic update, then adopt what Rust actually saved
 *  (it sanitizes, e.g. fps 50 → 60). */
export function useSettings() {
  const [settings, setSettings] = useState(null);
  const [error, setError] = useState(null);
  const [queue] = useState(() => createSettingsQueue(api.setSettings, setSettings,
    (e) => setError(e ? `Could not save settings: ${e}` : null)));

  useEffect(() => {
    let alive = true;
    api.getSettings().then((s) => alive && queue.hydrate(s)).catch((e) => alive && setError(`Could not load settings: ${e}`));
    return () => {
      alive = false;
    };
  }, [queue]);

  return [settings, queue.update, error];
}
