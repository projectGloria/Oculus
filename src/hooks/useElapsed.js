import { useEffect, useState } from "react";
import { elapsedAt } from "../recorderSnapshot.js";

export function useElapsed(snapshot) {
  const [now, setNow] = useState(() => performance.now());
  useEffect(() => {
    if (snapshot.state !== "recording") return;
    const timer = setInterval(() => setNow(performance.now()), 100);
    return () => clearInterval(timer);
  }, [snapshot.state]);
  return elapsedAt(snapshot, Math.max(now, snapshot.receivedAt ?? now));
}
