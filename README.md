# Oculus

Lean Windows screen recorder + screenshot tool. Tauri 2 + React 19 UI; capture, encode and
audio are native Rust on OS APIs — no Electron, no FFmpeg.

```
WGC frame (BGRA, GPU) ─▶ ID3D11VideoProcessor: crop + letterbox + BGRA→NV12 (BT.709), one pass
                      ─▶ tracked-sample NV12 pool ─▶ IMFSinkWriter (HW H.264/HEVC) ─┐
WASAPI loopback + mic ─▶ QPC-clocked mixer (zero-fill, drift realign) ─▶ AAC ───────┴▶ MP4
```

## Run

```bash
npm install
npm run tauri dev
```

## Build installer

```bash
npm run tauri build
```

## Hotkeys

`Ctrl+Shift+R` record/stop · `Ctrl+Shift+P` pause · `Ctrl+Shift+S` screenshot of current source ·
`Ctrl+Shift+A` area screenshot. Closing the window sends Oculus to the tray (the webview is
destroyed; ~6 MB private memory while idle).

## Headless pipeline checks (debug builds only)

```bash
src-tauri/target/debug/oculus.exe --selftest out.mp4 --secs 8 --pause 2,3 --sys --mic
```

Flags: `--window TITLE`, `--region x,y,w,h`, `--fps N`, `--hevc`, `--resize-at T,W,H`,
`--shot out.png`, `--mem`, `--list`. See `src-tauri/src/selftest.rs`.

## Layout

| Path | Role |
| --- | --- |
| `src-tauri/src/capture/` | Shared D3D11 device, raw WGC session, video processor + NV12 pool |
| `src-tauri/src/encode/mf_writer.rs` | Two-stream `IMFSinkWriter` |
| `src-tauri/src/audio/` | WASAPI capture threads, timeline mixer (unit-tested) |
| `src-tauri/src/clock.rs` | Shared QPC timeline, first-frame latch, pause math |
| `src-tauri/src/session.rs` | One recording: wiring, writer loop, keep-alive, teardown |
| `src-tauri/src/recorder.rs` | App state machine, `recorder://state` snapshots |
| `src-tauri/src/overlay/region.rs` | Native freeze-frame region picker (Win32 + D2D) |
| `src/hooks/useRecorder.js` | Subscribe-then-hydrate, `seq`-ordered snapshots |
