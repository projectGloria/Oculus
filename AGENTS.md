# AGENTS.md — Oculus

Oculus is a Windows-only recorder and screenshot tool. React 19 renders the controls, Tauri 2 hosts the WebViews, and Rust owns capture, encoding and recorder state.

## Commands

- From the repository root on Windows, use Node 24, Rust 1.99.1 (`rust-toolchain.toml`), Visual Studio C++ build tools and WebView2. Install frontend dependencies with `npm ci`.
- Run the desktop app with `npm run tauri dev`. Use `npm run dev` for UI-only work that can run without native commands.
- Run frontend tests with `npm test`, or one file with `node --test tests/settingsQueue.test.js`.
- Run Rust tests with `cargo test --locked --manifest-path src-tauri/Cargo.toml`. Append a test name to select one, e.g. `stalled_audio_preflight_ends_at_the_deadline_and_reports_the_endpoint`.
- Check Rust formatting with `cargo fmt --manifest-path src-tauri/Cargo.toml --check` and lint with `cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings`.
- `.github/workflows/ci.yml` runs `npm run check:version`, `npm test`, `npm run build`, the Rust checks above and `npm run tauri build -- --ci` to build the NSIS installer.

## Architecture

- `src-tauri/src/main.rs` sets up Tauri and registers native commands. `src/api.js` wraps those commands for the frontend.
- `src-tauri/src/recorder.rs` owns the recording state machine. Sending the panel to the tray destroys its WebView. `src/hooks/useRecorder.js` must subscribe before fetching state and accept only the newest complete snapshot by `seq`.
- `src/main.jsx` also serves the capture-excluded screenshot feedback window, which can outlive the panel. `src/settingsQueue.js` serializes optimistic settings writes. File mutations wait for backend state.
- `src-tauri/src/session.rs` connects Windows Graphics Capture to D3D11 conversion and the Media Foundation writer. WASAPI system and mic workers feed the 48 kHz stereo mixer.
- `src-tauri/src/clock.rs` maps QPC timestamps and pause intervals onto one timeline. The first accepted video frame establishes its origin.
- `src-tauri/src/overlay/region.rs` runs one picker worker per monitor on a shared device. Keep selections in monitor-relative physical pixels, even-align only encoded output, and join all workers on confirm or cancel.

## Recording and desktop contracts

- Use one multithread-protected D3D11 device per recording session. Keep textures on that device and device caches scoped to the session.
- Reuse an NV12 surface only after Media Foundation fires its tracked-sample callback. Keep the CFR repeat pool separate.
- Keep capture producers nonblocking, media queues bounded and Stop on its own one-slot channel. Report overload and retain partial output.
- Drain media while polling audio preflight. After 10 seconds, report stalled endpoints and allow recording to start.
- Run setup outside the recorder mutex. Preserve session generation checks and the completion barrier across cancellation, callbacks and ticks.
- Dispatch native tray and window work to the UI thread, release the recorder lock before waiting, and reject stale tray snapshots by `seq`.
- Use fragmented MP4 for H.264 and the regular MP4 sink for HEVC. Remove failed-start placeholders and retain partial recordings after runtime failure.
- Quit, suspend and logoff wait for finalization without an app-side deadline. Keep `Mta<T>` sendability specific to audited COM types and release shell COM/PIDL resources inside their STA.
- Keep hotkey registration and persistence transactional. Read settings during hotkey dispatch without taking the registration mutex.
- Validate saved-file paths in Rust against the resolved output directory, including junctions and symlinks. `tests/securityPolicy.test.js` pins production CSP and window permissions.
- Use variables from `src/styles/theme.css` for UI styling. Production CSP requires stylesheet-based styles. Match existing JS/JSX formatting and keep Rust unit tests beside the implementation.

## Verification and release

- Recording-path changes require `npm run verify` on an unlocked Windows desktop with Oculus closed and `ffmpeg` and `ffprobe` on PATH. The verifier exercises cancellation, slow endpoints, overload, interrupted files, decode, seek and A/V duration.
- For HEVC changes use `npm run verify -- -IncludeHevc`. For repeated clips and a five-minute run, build with `cargo build --manifest-path src-tauri/Cargo.toml` before `npm run verify:stress`.
- Selftest drivers are debug-only. Use disposable output paths and keep generated recordings out of commits.
- Set versions with `npm run set:version -- <version>` and add change notes to `OCULUS_PROJECT_GUIDE.md`. Check `gloria.toml` separately because `scripts/version.mjs` omits the installer manifest.
- `.github/workflows/build-installers.yml` builds the unbundled executable and packages it with GloriaSetup and the WebView2 bootstrapper for tagged releases.
- Keep project documentation in `README.md` (quick start), `OCULUS_PROJECT_GUIDE.md` (contracts, validation evidence and history), and `TODO.md` (backlog). Record hardware and installer validation only when exercised.
- Use short imperative commit subjects.
