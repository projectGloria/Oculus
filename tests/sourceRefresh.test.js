import test from "node:test";
import assert from "node:assert/strict";
import { createSourceRefresh } from "../src/sourceRefresh.js";

function harness() {
  const requests = [];
  const sources = [];
  const errors = [];
  const controller = createSourceRefresh(
    () => new Promise((resolve, reject) => requests.push({ resolve, reject })),
    (value) => sources.push(value),
    (value) => errors.push(value),
  );
  return { controller, requests, sources, errors };
}

test("focus bursts debounce; manual refresh cancels pending focus work", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const h = harness();
  h.controller.focus();
  t.mock.timers.tick(150);
  h.controller.focus();
  t.mock.timers.tick(199);
  assert.equal(h.requests.length, 0);
  t.mock.timers.tick(1);
  assert.equal(h.requests.length, 1);
  h.requests[0].resolve({ monitors: [1] });
  await Promise.resolve();
  h.controller.focus();
  const manual = h.controller.refresh();
  assert.equal(h.requests.length, 2);
  t.mock.timers.tick(200);
  assert.equal(h.requests.length, 2);
  h.requests[1].resolve({ monitors: [2] });
  await manual;
  assert.deepEqual(h.sources, [{ monitors: [1] }, { monitors: [2] }]);
  h.controller.dispose();
});

test("older responses and failures cannot replace newer sources or errors", async () => {
  const h = harness();
  const old = h.controller.refresh();
  const latest = h.controller.refresh();
  h.requests[1].resolve({ monitors: [2] });
  await latest;
  h.requests[0].resolve({ monitors: [1] });
  await old;
  assert.deepEqual(h.sources, [{ monitors: [2] }]);

  const staleFailure = h.controller.refresh();
  const failure = h.controller.refresh();
  h.requests[3].reject(new Error("access denied"));
  await failure;
  h.requests[2].reject(new Error("old failure"));
  await staleFailure;
  assert.match(h.errors.at(-1), /Could not refresh capture sources: Error: access denied/);
  assert.deepEqual(h.sources, [{ monitors: [2] }]);
  h.controller.dispose();
});

test("unmount cancels debounce and ignores requests still in flight", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const h = harness();
  const request = h.controller.refresh();
  h.controller.focus();
  h.controller.dispose();
  t.mock.timers.tick(200);
  h.requests[0].resolve({ monitors: [1] });
  await request;
  await h.controller.refresh();
  h.controller.focus();
  t.mock.timers.tick(200);
  assert.equal(h.requests.length, 1);
  assert.deepEqual(h.sources, []);
  assert.deepEqual(h.errors, [null]);
});
