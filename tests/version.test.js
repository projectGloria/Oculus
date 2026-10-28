import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, copyFile, readFile, writeFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";

test("version updates keep both manifests and lockfiles aligned; checks reject drift", async (t) => {
  const dir = await mkdtemp(join(tmpdir(), "oculus-version-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  await mkdir(join(dir, "scripts"));
  await mkdir(join(dir, "src-tauri"));
  for (const name of ["package.json", "package-lock.json", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock", "src-tauri/tauri.conf.json", "scripts/version.mjs"]) {
    await copyFile(new URL(`../${name}`, import.meta.url), join(dir, name));
  }
  const run = (argument) => spawnSync(process.execPath, [join(dir, "scripts/version.mjs"), argument], { encoding: "utf8" });
  assert.equal(run("--check").status, 0);
  assert.equal(run("0.2.3").status, 0);
  assert.equal(run("--check").status, 0);
  assert.match(await readFile(join(dir, "src-tauri/Cargo.lock"), "utf8"), /name = "oculus"\r?\nversion = "0.2.3"/);
  assert.notEqual(run("invalid").status, 0);
  const configPath = join(dir, "src-tauri/tauri.conf.json");
  const config = JSON.parse(await readFile(configPath, "utf8"));
  config.version = "9.9.9";
  await writeFile(configPath, JSON.stringify(config));
  assert.notEqual(run("--check").status, 0);
});
