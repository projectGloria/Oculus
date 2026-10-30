import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api.js";
import { newestSnapshot } from "../recorderSnapshot.js";

const INITIAL = {
  seq: 0,
  state: "idle",
  elapsedMs: 0,
  fps: 0,
  dropped: 0,
  source: null,
  sourceLabel: null,
  lastSaved: null,
  recentRecordings: [],
  error: null,
};

// Every snapshot is complete, so "newest seq wins" is the whole merge strategy. It also makes
// the hydrate call and live events order-independent.
const newest = (prev, next) => newestSnapshot(prev, next, performance.now());

/**
 * Recorder state, owned by Rust. This webview may be created mid-recording (it's destroyed
 * when sent to tray), so it must hydrate — and do it race-free:
 *   1. subscribe first, so nothing emitted from here on is missed;
 *   2. then fetch the snapshot; anything older than what we hold is dropped by `seq`.
 * Fetch-then-subscribe would lose an `ended` event landing in between → phantom recording.
 */
export function useRecorder() {
  const [snap, setSnap] = useState(INITIAL);

  useEffect(() => {
    let alive = true;
    let unlisten;
    (async () => {
      const un = await listen("recorder://state", (e) => setSnap((p) => newest(p, e.payload)));
      // StrictMode double-mounts: cleanup may have run while `listen` was pending, in which
      // case the cleanup saw `unlisten` undefined. Unsubscribe here instead of leaking.
      if (!alive) return un();
      unlisten = un;
      const s = await api.getRecorderState();
      if (alive) setSnap((p) => newest(p, s));
    })().catch((error) => {
      if (alive) setSnap((prev) => ({ ...prev, error: `Cannot read recorder state: ${error}` }));
    });
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  return snap;
}
