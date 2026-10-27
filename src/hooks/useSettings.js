import { useEffect, useState } from "react";
import { api } from "../api.js";

/** Settings persisted by Rust. Optimistic update, then adopt what Rust actually saved
 *  (it sanitizes, e.g. fps 50 → 60). */
export function useSettings() {
  const [settings, setSettings] = useState(null);

  useEffect(() => {
    let alive = true;
    api.getSettings().then((s) => alive && setSettings(s));
    return () => {
      alive = false;
    };
  }, []);

  // Plain closure, recreated per render: it's only called from event handlers, so it always
  // sees the current `settings`. No memo consumers → useCallback + latest-ref would be noise.
  const update = async (patch) => {
    if (!settings) return;
    const next = { ...settings, ...patch };
    setSettings(next);
    try {
      setSettings(await api.setSettings(next));
    } catch {
      setSettings(settings);
    }
  };

  return [settings, update];
}
