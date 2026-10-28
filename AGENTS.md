# Repository Guidelines

## Project Structure & Module Organization
Oculus is a Windows screen recorder and screenshot tool built with Tauri 2, React 19, and Rust 2021.
- `src/`: React UI; reusable components in `components/`, state subscriptions in `hooks/`, Tauri command wrappers in `api.js`, and shared styling in `styles/theme.css`.
- `src-tauri/src/`: native backend. `capture/` handles Windows Graphics Capture and D3D11, `encode/` handles Media Foundation, and `audio/` handles WASAPI and mixing. `session.rs` wires recordings; `recorder.rs` owns application state.
- `src-tauri/icons/`: application assets; `tauri.conf.json` and `capabilities/` configure packaging and permissions.
- Rust tests live beside their modules; JavaScript logic regression tests live in `tests/`. `selftest.rs` provides manual pipeline checks. `TODO.md` tracks planned work.

## Build, Test, and Development Commands
Run commands from the repository root on Windows with Node/npm, Rust, Windows build tools, and WebView2 available.
- `npm ci`: install dependencies from the lockfile.
- `npm run tauri dev`: launch the desktop app with Vite and Rust development builds.
- `npm run dev`: serve the frontend on port 1420; native commands require Tauri.
- `npm run build`: compile frontend assets into `dist/`.
- `npm test`: run JavaScript logic regression tests with Node's test runner.
- `npm run tauri build`: build the release app and NSIS installer.
- `cargo test --manifest-path src-tauri/Cargo.toml`: run Rust unit tests.

## Coding Style & Naming Conventions
Use two-space indentation, double quotes, and semicolons in JavaScript/JSX; use four spaces in Rust. Name components `PascalCase.jsx`, hooks `useSomething.js`, and Rust modules/functions `snake_case`. Reuse CSS variables from `theme.css`.

No ESLint or Prettier configuration is checked in. Rust formatting follows `rustfmt.toml`. Match surrounding code and avoid unrelated reformatting. Keep recorder state in Rust; preserve the subscribe-before-hydrate and increasing-`seq` snapshot logic in `useRecorder.js`.

## Testing Guidelines
Add Rust `#[test]` cases inside `#[cfg(test)] mod tests`, using descriptive names such as `silence_gap_is_zero_filled`. No coverage threshold or frontend component-test runner is configured; `npm test` covers JavaScript logic.

After a debug desktop build, exercise recording changes in an interactive Windows session:
```powershell
.\src-tauri\target\debug\oculus.exe --selftest out.mp4 --secs 8 --pause 2,3 --sys --mic
```
Check playback, audio/video sync, pause/resume, and affected UI flows. Keep generated recordings out of commits.

## Commit & Pull Request Guidelines
Use short, imperative commit subjects, following history such as `Fix 48fps cap on high-refresh monitors`. Keep changes focused. PRs should explain behavior changes, link relevant issues or backlog entries, record validation performed, and include screenshots for UI changes. For capture or encoder fixes, report relevant Windows, GPU, and monitor details.
