import { useCallback, useEffect, useState } from "react";
import { api } from "../api.js";

const EMPTY = { monitors: [], windows: [], supported: true };

/** Monitors + capturable windows. Refetched on window focus: the list goes stale the moment
 *  the user alt-tabs away to open the thing they want to record. */
export function useSources() {
  const [sources, setSources] = useState(EMPTY);

  const refresh = useCallback(() => {
    api.listSources().then(setSources).catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [refresh]);

  return [sources, refresh];
}
