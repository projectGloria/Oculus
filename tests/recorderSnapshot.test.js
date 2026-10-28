import test from "node:test";
import assert from "node:assert/strict";
import { elapsedAt, newestSnapshot } from "../src/recorderSnapshot.js";

test("stale hydration cannot reset the recording time anchor", () => {
  const recording = newestSnapshot({ seq: 1 }, { seq: 3, state: "recording", elapsedMs: 1000 }, 100);
  const hydrated = newestSnapshot(recording, { seq: 2, state: "idle", elapsedMs: 0 }, 800);
  assert.equal(hydrated, recording);
  assert.equal(elapsedAt(hydrated, 850), 1750);
  assert.equal(newestSnapshot(recording, { ...recording }, 900), recording);
});

test("pause and saving freeze elapsed time; resume uses its new anchor", () => {
  let snapshot = { seq: 1, state: "recording", elapsedMs: 2000, receivedAt: 100 };
  assert.equal(elapsedAt(snapshot, 650), 2550);
  snapshot = newestSnapshot(snapshot, { seq: 2, state: "paused", elapsedMs: 2600 }, 700);
  assert.equal(elapsedAt(snapshot, 9000), 2600);
  snapshot = newestSnapshot(snapshot, { seq: 3, state: "recording", elapsedMs: 2600 }, 9100);
  assert.equal(elapsedAt(snapshot, 9500), 3000);
  snapshot = newestSnapshot(snapshot, { seq: 4, state: "finalizing", elapsedMs: 3100 }, 9600);
  assert.equal(elapsedAt(snapshot, 20_000), 3100);
});

test("unhydrated snapshots and a timer older than receipt cannot add negative time", () => {
  assert.equal(elapsedAt({ state: "recording", elapsedMs: 0 }, 999), 0);
  assert.equal(elapsedAt({ state: "recording", elapsedMs: 1000, receivedAt: 500 }, 400), 1000);
});
