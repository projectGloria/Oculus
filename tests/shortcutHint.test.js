import test from "node:test";
import assert from "node:assert/strict";
import { shortcutHint } from "../src/shortcutHint.js";
import { formatElapsed } from "../src/api.js";

test("shortcut hints follow configured values and disappear when disabled", () => {
  assert.equal(shortcutHint({ enabled: true, record: "Alt+R" }, "record"), " (Alt+R)");
  assert.equal(shortcutHint({ enabled: false, record: "Alt+R" }, "record"), "");
  assert.equal(shortcutHint({ enabled: true, record: "" }, "record"), "");
  assert.equal(shortcutHint(undefined, "record"), "");
});

test("elapsed formatting handles second, minute and hour boundaries", () => {
  for (const [ms, expected] of [[0, "00:00"], [999, "00:00"], [1000, "00:01"], [60000, "01:00"], [3599999, "59:59"], [3600000, "1:00:00"], [3723000, "1:02:03"]]) {
    assert.equal(formatElapsed(ms), expected);
  }
});
