import test from "node:test";
import assert from "node:assert/strict";
import { dbToGain, formatGain, gainToDb } from "../src/gain.js";

test("decibel controls preserve unity, mute and logarithmic gain", () => {
  assert.equal(dbToGain(0), 1);
  assert.equal(dbToGain(-60), 0);
  assert.equal(gainToDb(0), -60);
  assert.equal(formatGain(0), "−∞ dB");
  assert.equal(formatGain(1), "0.0 dB");
  assert.equal(formatGain(2), "+6.0 dB");
  assert.equal(formatGain(0.5), "-6.0 dB");
  for (const db of [-40, -20, -6, 0, 6]) assert.ok(Math.abs(gainToDb(dbToGain(db)) - db) < 1e-6);
});
