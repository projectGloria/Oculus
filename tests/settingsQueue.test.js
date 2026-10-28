import test from "node:test";
import assert from "node:assert/strict";
import { createSettingsQueue } from "../src/settingsQueue.js";

function harness() {
  const requests = [];
  let visible;
  let error;
  const queue = createSettingsQueue(
    (settings) => new Promise((resolve, reject) => requests.push({ settings, resolve, reject })),
    (settings) => { visible = settings; },
    (next) => { error = next; },
  );
  queue.hydrate({ mic: false, systemAudio: false });
  return { queue, requests, visible: () => visible, error: () => error };
}

test("quick toggles serialize writes and retain newer optimistic values", async () => {
  const h = harness();
  const first = h.queue.update({ mic: true });
  const second = h.queue.update({ systemAudio: true });
  await Promise.resolve();
  assert.equal(h.requests.length, 1);
  assert.deepEqual(h.visible(), { mic: true, systemAudio: true });
  h.requests[0].resolve(h.requests[0].settings);
  await first;
  assert.deepEqual(h.visible(), { mic: true, systemAudio: true });
  await Promise.resolve();
  assert.equal(h.requests.length, 2);
  h.requests[1].resolve(h.requests[1].settings);
  await second;
  assert.deepEqual(h.visible(), { mic: true, systemAudio: true });
});

test("a failed earlier write does not roll back a later edit", async () => {
  const h = harness();
  const first = h.queue.update({ mic: true });
  const second = h.queue.update({ systemAudio: true });
  await Promise.resolve();
  h.requests[0].reject(new Error("disk busy"));
  await first;
  assert.deepEqual(h.visible(), { mic: true, systemAudio: true });
  await Promise.resolve();
  h.requests[1].resolve(h.requests[1].settings);
  await second;
  assert.equal(h.error(), null);
});

test("a failed latest write rolls back to the last successful settings", async () => {
  const h = harness();
  const first = h.queue.update({ mic: true });
  const second = h.queue.update({ systemAudio: true });
  await Promise.resolve();
  h.requests[0].resolve(h.requests[0].settings);
  await first;
  await Promise.resolve();
  h.requests[1].reject(new Error("disk full"));
  await second;
  assert.deepEqual(h.visible(), { mic: true, systemAudio: false });
  assert.match(h.error(), /disk full/);
});
