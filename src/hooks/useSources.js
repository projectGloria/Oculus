import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api.js";
import { createSourceRefresh } from "../sourceRefresh.js";

const EMPTY = { monitors: [], windows: [], supported: true };

/** Monitors + capturable windows. Refetched on window focus: the list goes stale the moment
 *  the user alt-tabs away to open the thing they want to record. */
export function useSources() {
  const [sources, setSources] = useState(EMPTY);
  const [error, setError] = useState(null);
  const controller = useRef(null);
  const refresh = useCallback(() => controller.current?.refresh(), []);

  useEffect(() => {
    const current = createSourceRefresh(api.listSources, setSources, setError);
    controller.current = current;
    current.refresh();
    window.addEventListener("focus", current.focus);
    const interval = setInterval(() => {
      if (document.visibilityState === "visible" && document.hasFocus()) current.refresh();
    }, 2000);
    return () => {
      window.removeEventListener("focus", current.focus);
      clearInterval(interval);
      current.dispose();
      controller.current = null;
    };
  }, []);

  return [sources, refresh, error];
}
