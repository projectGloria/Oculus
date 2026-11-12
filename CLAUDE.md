# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Oculus is a Windows-only screen recorder and screenshot tool: Tauri 2 shell, React 19 (plain JS/JSX) UI, Rust 2021 backend talking directly to Windows Graphics Capture, D3D11, Media Foundation and WASAPI. FFmpeg is not bundled; it is only used by the developer verification scripts.

## Commands

Run from the repository root on Windows (Node 24, Rust pinned to 1.99.1 by `rust-toolchain.toml`, VS C++ build tools, WebView2).

```powershell
npm ci
npm run tauri dev                              # full desktop app (Vite on :1420 + Rust debug build)
npm run dev                                    # frontend only; native commands will not work
npm run build                                  # frontend to dist/ (prebuild runs check:version)
npm run tauri build                            # release exe + NSIS installer
npm run tauri build -- --debug --no-bundle     # debug exe with embedded frontend assets
```

Deterministic checks (this is exactly what CI runs, on `windows-latest`):

```powershell
npm run check:version
npm test
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

Single tests:

```powershell
node --test tests/settingsQueue.test.js
node --test --test-name-pattern "production CSP" tests/*.test.js
cargo test --manifest-path src-tauri/Cargo.toml silence_gap_is_zero_filled
```

Interactive pipeline verification (needs an unlocked desktop, `ffmpeg`/`ffprobe` on PATH, and **no running Oculus instance** — the app is single-instance and the verifier launches its own):

```powershell
npm run verify                    # tests + builds + selftest recordings, decode/seek/A-V endpoint checks
npm run verify -- -IncludeHevc
cargo build --manifest-path src-tauri/Cargo.toml; npm run verify:stress   # repeated clips + 5-minute run
```

Debug-only selftest drivers (`selftest.rs` is `#[cfg(debug_assertions)]`, absent from release builds):

```powershell
.\src-tauri\target\debug\oculus.exe --selftest out.mp4 --secs 8 --pause 2,3 --sys --mic
.\src-tauri\target\debug\oculus.exe --selftest out-hevc.mp4 --secs 5 --hevc --vfr --sys --mic
```

`--kill-at T` deliberately exits without finalizing (expected exit code 86). `--selftest-app <dir>` drives the real app lifecycle (startup cancellation, delayed-finalize quit). Use disposable output paths and never commit generated recordings.

Versioning: the version lives in `package.json`, `package-lock.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` and `tauri.conf.json`. Never edit one by hand — use `npm run set:version -- 0.2.0`, then add notes to the Change history section of `OCULUS_PROJECT_GUIDE.md`. `check:version` fails the build on drift.

## Architecture

### Rust owns all state; the WebView is disposable

- `recorder.rs` holds the authoritative state machine (`Idle → Starting → Recording ⇄ Paused → Finalizing → Idle`). Every snapshot is complete and carries an increasing `seq`, emitted on `recorder://state`.
- Sending the panel to the tray **destroys the WebView**; reopening creates a fresh UI mid-session. So `src/hooks/useRecorder.js` must subscribe first, then hydrate via `get_recorder_state`, and accept only newer `seq` (`src/recorderSnapshot.js`). Fetch-then-subscribe would produce a phantom recording. Do not move recorder state into React.
- `src/api.js` is the only frontend → backend surface (thin `invoke` wrappers for commands in `commands.rs`). Settings writes go through a serialized optimistic queue (`src/settingsQueue.js`); the UI waits for backend state after file mutations.
- The pure-logic modules at the top of `src/` (`gain.js`, `recorderSnapshot.js`, `settingsQueue.js`, `sourceRefresh.js`, `shortcutHint.js`) exist so `tests/` can cover them with Node's test runner — there is no component test runner.

### Recording pipeline (`session.rs` wires it)

WGC frame (BGRA GPU texture) → `capture/convert.rs` D3D11 VideoProcessor (crop/letterbox/NV12, BT.709) → tracked sample pool → `encode/mf_writer.rs` (Media Foundation H.264/HEVC + AAC → MP4). WASAPI system/mic workers → `audio/mixer.rs` (48 kHz stereo timeline, ~100 ms behind the clock) → AAC. Everything is timestamped against the QPC clock in `clock.rs`, which also owns pause-interval mapping; the first accepted video frame is the timeline origin.

Invariants that span files and must be preserved:

- **One D3D11 device per recording session**, multithread-protected, no global device cache. Never substitute a texture from another device into capture/convert/encode.
- **Tracked-sample lifetime**: a pooled NV12 surface is reusable only after Media Foundation's tracked-sample callback fires — `WriteSample` returning is not enough. Pools start at 6 and cap at 12; the CFR repeat pool is separate.
- **Bounded queues, non-blocking producers** (`queue.rs`): writer media queue 128 packets, audio input 64, plus a separate one-slot control channel so Stop can never be blocked by a full media queue. Overload is reported as a failure with partial output retained — do not hide it with unbounded buffering.
- **Session generations**: setup runs outside the recorder mutex; a generation check and completion barrier stop cancelled setups, stale callbacks and stale tickers from becoming or touching the live session. Tray updates reject stale `seq`, and native tray/window calls are dispatched to the UI thread without waiting while holding the recorder lock.
- **Audio preflight is non-blocking**: capture and the mixer are already running while endpoints open, so the writer drains media while polling preflight health instead of waiting on a slow device.
- **Container choice**: H.264 uses fragmented MP4 for crash resilience; HEVC uses the regular sink because the fragmented sink rejects it (`MF_E_INVALIDMEDIATYPE`). Runtime failures keep partial output; failed startup removes its placeholder file.
- **Threading wrappers**: `Mta<T>` in `util.rs` is `Send` only for specific listed COM types — no blanket impls. Shell helpers (`shell.rs`) enter an STA and release COM/PIDL resources inside it.
- **Quit/suspend/logoff** (`lifecycle.rs`, `Recorder::request_exit`) wait for finalization with no app-side deadline. Release builds are `panic = "abort"`.

Threads to be aware of: Tauri main (windows/tray/short IPC), recording start worker, WGC callback (never waits on the writer), `oculus-writer` (owns Media Foundation), up to two WASAPI workers, mixer, per-second recorder ticker (stats + free-space check), Windows lifecycle thread, per-monitor region-selection workers.

### Other subsystems

- `overlay/region.rs`: native Direct2D freeze-frame area picker, one worker/window per monitor sharing one device. Geometry is monitor-relative physical pixels; only encoded output dimensions are even-aligned. First confirm/cancel closes and joins all workers.
- Screenshot feedback is a second, capture-excluded WebView (`screenshot-feedback`) that can outlive the main one; `src/main.jsx` serves both entry points.
- `hotkeys.rs`: registration + persistence is a transaction; event dispatch reads settings without the registration mutex (avoids a main-thread deadlock).

### Security boundary

`tests/securityPolicy.test.js` pins the production CSP in `tauri.conf.json` (no inline, no external connections) and the exact permission sets in `src-tauri/capabilities/*.json`. Adding a Tauri permission or loosening the CSP requires updating that test deliberately. Custom Rust commands are the file-access boundary: saved-file commands must validate that paths stay inside the output directory (junction/symlink resolution included) — frontend checks are not sufficient.

## Conventions

- JS/JSX: two spaces, double quotes, semicolons; no ESLint/Prettier configured — match surrounding code. Rust: `rustfmt.toml` (`max_width = 120`, `use_small_heuristics = "Max"`). Clippy warnings are errors in CI.
- Reuse CSS variables from `src/styles/theme.css`. Production CSP forbids inline styles (`style-src 'self'`).
- Rust tests live in `#[cfg(test)] mod tests` beside the code, with descriptive names.
- Commits: short imperative subjects (e.g. `Fix 48fps cap on high-refresh monitors`).
- Documentation lives in exactly three files — `README.md` (intro/quick start), `OCULUS_PROJECT_GUIDE.md` (architecture contracts in §6, validation evidence, change history), `TODO.md` (prioritized backlog). Record completed work and real evidence there rather than adding new docs; do not describe a passing build as hardware or installer validation.
- Recording-path changes need the relevant `npm run verify` checks, not just unit tests.
