// Receipt time belongs to the accepted snapshot. An old hydrate response or event
// must not reset the clock anchor for a newer recording/paused state.
export function newestSnapshot(previous, next, receivedAt) {
  return next.seq > previous.seq ? { ...next, receivedAt } : previous;
}

export function elapsedAt(snapshot, now) {
  const delta = snapshot.state === "recording" && snapshot.receivedAt != null
    ? Math.max(0, now - snapshot.receivedAt) : 0;
  return snapshot.elapsedMs + delta;
}
