import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const root = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, root), "utf8");
const pkg = JSON.parse(read("package.json"));
const config = JSON.parse(read("src-tauri/tauri.conf.json"));
const cargo = read("src-tauri/Cargo.toml");
const cargoVersion = cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
const lock = JSON.parse(read("package-lock.json"));
const cargoLock = read("src-tauri/Cargo.lock");
const cargoLockVersion = cargoLock.match(/\[\[package\]\]\r?\nname = "oculus"\r?\nversion = "([^"]+)"/)?.[1];
const next = process.argv[2];
if (next && next !== "--check") {
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(next)) throw Error("Use a semantic version, such as 0.2.0 or 0.2.0-beta.1");
  pkg.version = config.version = lock.version = lock.packages[""].version = next;
  for (const [path, value] of [["package.json", pkg], ["package-lock.json", lock], ["src-tauri/tauri.conf.json", config]]) {
    writeFileSync(fileURLToPath(new URL(path, root)), JSON.stringify(value, null, 2) + "\n");
  }
  writeFileSync(new URL("src-tauri/Cargo.toml", root), cargo.replace(/^version\s*=\s*"[^"]+"/m, `version = "${next}"`));
  writeFileSync(new URL("src-tauri/Cargo.lock", root), cargoLock.replace(/(\[\[package\]\]\r?\nname = "oculus"\r?\nversion = ")[^"]+/, (_, prefix) => prefix + next));
  console.log(`Version set to ${next}. Add its release notes to the Change history section of OCULUS_PROJECT_GUIDE.md.`);
} else {
  const versions = [pkg.version, config.version, cargoVersion, lock.version, lock.packages[""].version, cargoLockVersion];
  if (versions.some((version) => version !== pkg.version)) throw Error(`Version mismatch: ${versions.join(", ")}. Run npm run set:version -- <version>.`);
  console.log(`All package versions agree: ${pkg.version}`);
}
