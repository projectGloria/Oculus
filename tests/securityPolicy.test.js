import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const config = JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url)));
const capability = JSON.parse(readFileSync(new URL("../src-tauri/capabilities/default.json", import.meta.url)));
const directives = (policy) => new Map(policy.split(";").map((part) => {
  const [name, ...sources] = part.trim().split(/\s+/);
  return [name, sources];
}));

test("production CSP permits bundled assets and IPC without inline code or external connections", () => {
  const policy = directives(config.app.security.csp);
  assert.deepEqual(policy.get("default-src"), ["'self'"]);
  assert.deepEqual(policy.get("style-src"), ["'self'"]);
  assert.deepEqual(policy.get("connect-src"), ["'self'", "ipc:", "http://ipc.localhost"]);
  for (const sources of policy.values()) {
    assert.ok(!sources.includes("'unsafe-inline'"));
    assert.ok(!sources.includes("'unsafe-eval'"));
    assert.ok(!sources.includes("*"));
  }
  assert.deepEqual(policy.get("object-src"), ["'none'"]);
  assert.deepEqual(policy.get("frame-src"), ["'none'"]);
  assert.deepEqual(policy.get("form-action"), ["'none'"]);
});

test("Vite style and hot-reload exceptions are limited to the development policy", () => {
  const policy = directives(config.app.security.devCsp);
  assert.ok(policy.get("style-src").includes("'unsafe-inline'"));
  assert.ok(policy.get("connect-src").includes("ws://localhost:1420"));
  assert.ok(!config.app.security.csp.includes("localhost:1420"));
});

test("the webview receives only recorder subscription, titlebar and folder-picker permissions", () => {
  assert.deepEqual(capability.windows, ["main"]);
  assert.equal(capability.remote, undefined);
  assert.deepEqual(new Set(capability.permissions), new Set([
    "core:event:allow-listen",
    "core:event:allow-unlisten",
    "core:window:allow-start-dragging",
    "core:window:allow-internal-toggle-maximize",
    "core:window:allow-minimize",
    "dialog:allow-open",
  ]));
});

test("screenshot feedback can subscribe without titlebar or folder-picker access", () => {
  const feedback = JSON.parse(readFileSync(new URL("../src-tauri/capabilities/screenshot-feedback.json", import.meta.url)));
  assert.deepEqual(feedback.windows, ["screenshot-feedback"]);
  assert.equal(feedback.remote, undefined);
  assert.deepEqual(new Set(feedback.permissions), new Set(["core:event:allow-listen", "core:event:allow-unlisten"]));
});
