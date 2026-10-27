# Oculus — Master TODO

Everything left between "works on my machine" and "ship it to strangers", grounded in the code as of
`13555bc`. Items that cite a file are real findings from reading that code, not speculation.

**Legend**

| Tag | Meaning |
| --- | --- |
| **P0** | Data loss, wrong output, or crash. Fix before anyone else uses it. |
| **P1** | Visible defect or missing table-stakes feature. Fix before a public release. |
| **P2** | Quality / polish / competitive parity. |
| **P3** | Nice to have, post-1.0. |
| `S` / `M` / `L` | Effort: hours / a day or two / a week+. |
| 🔍 | Needs measurement or investigation before deciding the fix. |

---

## 0. Status snapshot

- [x] Monitor / window / region capture, zero-copy GPU path (WGC → VideoProcessor → NV12 → HW MFT).
- [x] System audio + mic, QPC-aligned mixer, pause/resume, H.264/HEVC + AAC MP4.
- [x] PNG screenshots + area screenshots + clipboard.
- [x] Tray, global hotkeys, compact record bar, capture exclusion of our own UI.
- [x] 60 fps on high-refresh monitors (`MinUpdateInterval`, `13555bc`).
- [ ] Everything below.

---

## 1. P0 — Data safety & correctness bugs

### 1.1 Crash mid-recording = unplayable file · P0 · M
- [ ] The MP4 sink writes the `moov` atom only in `Finalize()` ([mf_writer.rs:59](src-tauri/src/encode/mf_writer.rs:59)).
      A crash, BSOD, power loss, `panic = "abort"`, or Task Manager kill leaves an `mdat` with no index →
      every player rejects it. For a recorder this is the #1 trust killer.
- [ ] Option A: fragmented MP4 (`MFTranscodeContainerType_FMPEG4`) — playable up to the last fragment.
      Verify fragment duration, seeking in VLC/Premiere/DaVinci, and that editors accept fMP4.
- [ ] Option B: record to MKV/fMP4 temp, remux to regular MP4 on stop (keeps faststart, adds a step).
- [ ] Option C (minimum): write to `*.mp4.part`, rename on successful finalize, and on next launch
      detect orphaned `.part` files and offer recovery (`untrunc`-style rebuild from a reference file).
- [ ] Decide + document. Add a selftest flag `--kill-at T` that hard-exits mid-recording to prove recovery.

### 1.2 Quit path can truncate a recording · P0 · S
- [ ] Tray → Quit waits at most 5 s (50 × 100 ms) then `app.exit(0)` ([main.rs:74](src-tauri/src/main.rs:74)).
      A long 4K HEVC recording finalizing on a slow HDD can exceed that → truncated file (see 1.1).
      Wait for the session's `finished` callback instead of polling with a deadline; show "Saving…" in the tray tooltip.
- [ ] Windows shutdown / logoff: handle `WM_QUERYENDSESSION` / `WM_ENDSESSION` (Tauri `RunEvent::ExitRequested` with code)
      to finalize the file before the OS kills us. Use `ShutdownBlockReasonCreate` ("Saving recording…").
- [ ] Sleep/hibernate during recording: on `PBT_APMSUSPEND` stop + finalize (or pause), resume cleanly. 🔍 What does WGC do across sleep?

### 1.3 Filename collisions overwrite files · P0 · S
- [ ] `local_stamp()` has 1-second resolution ([util.rs:36](src-tauri/src/util.rs:36)). Two screenshots in the same
      second ([screenshot.rs:49](src-tauri/src/screenshot.rs:49)) silently overwrite the first — trivially reproducible by
      double-tapping `Ctrl+Shift+S`. Same for recordings ([recorder.rs:166](src-tauri/src/recorder.rs:166)) when stop→start
      within a second.
- [ ] Fix: add milliseconds or a ` (2)` suffix loop with `OpenOptions::create_new(true)`; never overwrite.

### 1.4 Audio device switch records silence · P0 · M
- [ ] Both WASAPI streams bind to `GetDefaultAudioEndpoint(…, eConsole)` once at start ([wasapi.rs:49](src-tauri/src/audio/wasapi.rs:49)).
      Plug in headphones / switch to Bluetooth mid-recording → loopback keeps capturing the *old* endpoint → silence for the rest
      of the file, with no indication.
- [ ] Register `IMMNotificationClient::OnDefaultDeviceChanged`, tear down and reopen the stream on the new device; the mixer
      already zero-fills the gap, so timing stays correct.
- [ ] Handle `AUDCLNT_E_DEVICE_INVALIDATED` from `GetBuffer`/`GetNextPacketSize` (device unplugged) the same way instead of
      exiting the thread.

### 1.5 Audio failures are invisible · P0 · S
- [ ] Audio thread errors go to `eprintln!` ([wasapi.rs:39](src-tauri/src/audio/wasapi.rs:39)); release builds have no console
      (`windows_subsystem = "windows"`). A denied mic permission (Settings → Privacy → Microphone) produces a silent mic track
      and the user finds out after recording a 40-minute tutorial.
- [ ] Propagate per-source status into `Snapshot` (`audio: { system: ok|failed(msg), mic: ok|failed(msg) }`) and show it in the
      record bar.
- [ ] Pre-flight check on Record: open the mic briefly; if access is denied, show a deep link to `ms-settings:privacy-microphone`.

### 1.6 Hotkey registration failures are invisible · P1 · S
- [ ] Failure is only `eprintln!`'d ([main.rs:121](src-tauri/src/main.rs:121)). If another app owns `Ctrl+Shift+R` (OBS, ShareX,
      Chrome extensions), the hotkey silently does nothing. Surface in UI (settings row shows "⚠ in use by another app").

### 1.7 `Recorder::start` holds the state mutex across encoder setup · P1 · S
- [ ] `session::start` (device + MF + WGC init, 100–300 ms) runs under `inner.lock()` ([recorder.rs:169](src-tauri/src/recorder.rs:169)).
      Meanwhile `get_recorder_state`, `set_source`, the tray and the ticker all block on the main thread → UI hitch on every Record.
      Emit a `starting` state, release the lock, init, re-lock to commit (with a generation check so a Stop during init wins).

### 1.8 Hotkey double-press race · P1 · S
- [ ] Each `Ctrl+Shift+R` spawns a thread that calls `toggle()`. Two quick presses = start + immediate stop (a 0.2 s file), or
      worse, interleaving with 1.7. Debounce (ignore toggles within ~400 ms) and/or make toggle a single queued command.

### 1.9 Two pauses within the mixer latency mis-time audio · P2 · S
- [ ] `Clock` remembers only `last_pause` ([clock.rs](src-tauri/src/clock.rs)). Pause→resume→pause→resume within ~100 ms
      (hotkey spam) → late audio packets from the first pause get the wrong PTS. Keep a small ring of recent pauses (or the
      full list; it's tiny). Add a mixer unit test.

### 1.10 Settings file corruption silently resets everything · P1 · S
- [ ] `fs::write` is not atomic ([settings.rs:115](src-tauri/src/settings.rs:115)); a crash mid-write leaves truncated JSON,
      which `load` treats as "no settings" and silently restores defaults (output dir included).
- [ ] Write to `settings.json.tmp` + `rename` (atomic on NTFS). On parse failure, back up the bad file as
      `settings.json.bad` and tell the user.

### 1.11 Output folder unavailable · P1 · S
- [ ] Output dir on an unplugged USB drive / dropped network share → `create_dir_all` error at Record time. Fall back to
      `Videos\Oculus` with a visible warning instead of refusing to record.
- [ ] Disk-space check before start (`GetDiskFreeSpaceExW`) and during recording (every tick): warn at < 2 GB, auto-stop and
      finalize at < 200 MB instead of letting MF fail mid-write.

### 1.12 Stale handles · P1 · S
- [ ] `Source::Monitor { id }` and `Region { monitor }` store raw `HMONITOR`s, which change when displays are re-plugged,
      resolution changes, or the laptop docks. Re-resolve by device name / rect on start; if gone, fall back to primary with a
      warning.
- [ ] `Source::Window { id }` is a raw `HWND`; HWNDs are recycled. Validate on start that the HWND still belongs to the same
      exe + title prefix; if not, clear the source.

### 1.13 "Window closed" is shown as an error · P2 · S
- [ ] [recorder.rs:229](src-tauri/src/recorder.rs:229) puts a *successful* save notice in `error`, rendered red. Add a
      `notice` / `info` field (or `lastEnd: { reason }`) and render it neutrally.

---

## 2. Verification debt (tests never run)

- [ ] **15-minute A/V drift test** (clap at start and end, loopback + mic). Target: < 1 frame offset at the end. P0 · S
- [ ] **1-hour soak**: memory flat, no handle leak (`--mem` every minute), file plays, seekable. P1 · S
- [ ] **Silent loopback + live mic**: pause music mid-recording while talking; mic must stay continuous. P0 · S
- [ ] **Mixed-DPI region**: 150 % laptop + 100 % external; selection crop must match pixel-exact on both. P0 · S
- [ ] **Monitor not at (0,0)**: secondary display to the left/above primary (negative coordinates) — overlay + crop. P1 · S
- [ ] **Portrait / rotated monitor** capture + region. P1 · S
- [ ] **Minimize target window mid-recording**: keep-alive should hold last frame; restore resumes. P1 · S
- [ ] **Static document 30 s** → open in VLC, MPC-HC, Premiere, DaVinci Resolve, Shotcut, Windows Photos; fps floor holds,
      no "variable frame rate" import warnings (or document them). P1 · S
- [ ] **No HW encoder** (Basic Render Driver / VM / RDP session): confirm the software MFT fallback works with DXGI surface
      samples, or detect and fall back to a CPU-readback path. 🔍 P0 · M
- [ ] **Hybrid GPU laptop** (Intel iGPU + NVIDIA dGPU, like this machine): which adapter does `D3D11CreateDevice(None, …)` pick,
      which GPU owns which monitor, does WGC cross-adapter copy cost show up? 🔍 P1 · M
- [ ] **4K / 5K / ultrawide (5120×1440)** — downscale path in `encoder_dims`, bitrate clamp, encoder level limits. P1 · S
- [ ] **HEVC on hardware without HEVC encode** — error message quality. P1 · S
- [ ] **RDP session / Hyper-V enhanced session** — WGC support, error text. P2 · S
- [ ] **Games**: borderless fullscreen, exclusive fullscreen (DX11/DX12/Vulkan), and a 240 Hz game — fps, black frames,
      cursor. P1 · M
- [ ] **DRM content** (Netflix in Edge) — confirm black frames, document. P3 · S
- [ ] **cargo clippy -D warnings** (install clippy: `rustup component add clippy`). P1 · S
- [ ] **NSIS installer**: build, install, uninstall, upgrade-in-place, size check (target 3–5 MB). P1 · S
- [ ] Clean Windows 10 22H2 VM: WGC border behavior, `SetIsBorderRequired` failure path, `MinUpdateInterval` absence,
      WebView2 presence. P1 · M

---

## 3. Video pipeline

### 3.1 Encoder configuration · P1
- [ ] **Rate control.** Only `MF_MT_AVG_BITRATE` is set ([mf_writer.rs:69](src-tauri/src/encode/mf_writer.rs:69)); HW MFTs default to
      CBR, which wastes bits on static screens and starves motion. Use `ICodecAPI` on the encoder
      (`IMFSinkWriterEx::GetTransformForStream`) → `CODECAPI_AVEncCommonRateControlMode` = Quality/VBR (or PeakConstrainedVBR
      with a cap). Measure size vs quality on a screen-content clip. M
- [ ] **GOP / keyframe interval.** Set `CODECAPI_AVEncMPVGOPSize` = 2 × fps. Default GOPs can be huge → editors seek slowly,
      and a corrupted fragment loses more. S
- [ ] **B-frames.** Decide `CODECAPI_AVEncMPVDefaultBPictureCount` (B-frames help compression; some editors scrub worse). S
- [ ] **Faststart.** `MF_MPEG4SINK_MOOV_BEFORE_MDAT` so files stream in browsers/Discord before full download. Interacts with 1.1. S
- [ ] **Content type hint.** `CODECAPI_AVEncVideoContentType` / screen-content tuning where vendors support it (sharper text). 🔍 S
- [ ] **Report the actual encoder** (vendor MFT name via `MFT_FRIENDLY_NAME_Attribute`) in logs and the About panel —
      "NVIDIA H.264 Encoder MFT" vs "H264 Encoder MFT" (software) is the first thing to ask in a bug report. S
- [ ] **AV1** (RTX 40 / Arc / RDNA3+): add when the encoder enumerates; hide the option otherwise. P2 · M
- [ ] **Lossless / near-lossless** option for tutorials with tiny text (high bpp HEVC or QP mode). P3 · M
- [ ] **Bitrate model sanity.** `bpp` × pixels × fps ([settings.rs](src-tauri/src/settings.rs)) doubles the bitrate when going
      30→60 fps; perceptually you need ~1.3–1.5×. Revisit with a measured table (resolution × fps × quality → Mbps). S 🔍
- [ ] Expose **custom bitrate** (Mbps field) in an "Advanced" section. P2 · S

### 3.2 Frame rate · P1
- [ ] Settings only allow 30/60 ([settings.rs:78](src-tauri/src/settings.rs:78) collapses anything else). Add 24, 50, 120,
      144, and "Match display". Check encoder level limits at 4K120. M
- [ ] **Judder on non-divisible refresh**: 144 Hz → 60 fps alternates 13.9/20.8 ms frame gaps. Offer 72 / 48 fps on 144 Hz
      panels or document. Optional: resample to an exact CFR grid by duplicating/dropping with PTS snapped to the grid. 🔍 M
- [ ] **Constant frame rate mode** (`cfr: true`): emit exactly fps frames per second (duplicate last frame on the writer
      thread) for editors that hate VFR (Premiere, older Vegas). Keep VFR as default (smaller files). M
- [ ] **Keep-alive floor configurable** (5 fps now, `KEEPALIVE` in [session.rs](src-tauri/src/session.rs)). S
- [ ] `MinUpdateInterval` = 1 ms means 144–240 callbacks/s on high-refresh panels. Measured cost negligible on 144 Hz; re-check
      on 240/360 Hz and set it to `interval / 2` if callback overhead shows up. 🔍 S

### 3.3 Capture quality · P2
- [ ] **HDR displays**: WGC with `B8G8R8A8` on an HDR monitor gives a washed-out SDR conversion. Capture
      `R16G16B16A16Float` and tone-map (VideoProcessor with `DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709` input, or a compute shader),
      or record HDR10 HEVC Main10. 🔍 L
- [ ] **Downscale quality**: VideoProcessor scaling is driver-defined (often bilinear) → shimmering text on 4K→1080p. Add an
      output-resolution setting (Native / 1440p / 1080p / 720p) and test quality per vendor; fallback to a Lanczos compute shader
      if bad. M
- [ ] **Output resolution setting** (see above): smaller files for sharing. S once the scaler is trusted.
- [ ] **Cursor**: highlight ring, click visualization (left/right ripple), cursor size boost for tutorials. Requires drawing
      the cursor ourselves (disable WGC cursor, read `GetCursorInfo` + cursor bitmap, composite on GPU). P2 · L
- [ ] **Keystroke overlay** for tutorials (P3, low-level keyboard hook, privacy toggle). L
- [ ] **Window capture border**: `SetIsBorderRequired(false)` is ignored on older builds → yellow border. Document. S
- [ ] **Window capture of child/popup menus**: WGC window capture omits context menus and tooltips (separate HWNDs).
      Document, or offer "capture window area of the screen" mode (region tracking the window rect). M
- [ ] **Multi-monitor "all displays"** source (capture each monitor, composite into one canvas). P3 · L

### 3.4 Robustness · P1
- [ ] **Device removed / driver reset (TDR)**: `DXGI_ERROR_DEVICE_REMOVED` on Blt or WriteSample currently fails the session
      somewhere in a callback (`eprintln!` in the WGC callback, [session.rs](src-tauri/src/session.rs)). Detect, finalize what we have, report. M
- [ ] **Blt / pool errors in the WGC callback** are only `eprintln!`'d and dropped silently — after N consecutive failures
      the session should end with an error instead of recording a frozen frame forever. S
- [ ] **Monitor resolution change mid-recording** (monitor source): pool `Recreate` handles size, but the crop rect for
      region mode is in the old coordinate space. Decide: stop, or rescale the region. S
- [ ] **Display scaling change mid-recording** (DPI): same as above. S
- [ ] **Target window moves to another monitor with different DPI**: content size changes → letterbox kicks in. Verify. S
- [ ] **Window capture of an elevated (admin) window** from a non-elevated Oculus: verify behavior, show a hint. S
- [ ] `Converter` input-view cache keyed by raw pointer (max 6) — fine, but add a debug assert counter for cache misses per
      second to catch pathological recreation. S

---

## 4. Audio

### 4.1 Core · P1
- [ ] **Device selection**: dropdowns for output (loopback source) and input (mic) with "Default" option that follows
      default-device changes (see 1.4). Enumerate with `IMMDeviceEnumerator::EnumAudioEndpoints`. M
- [ ] **Mic level meter** in the panel and record bar (peak/RMS from the mixer, sent at ~15 Hz via a separate event,
      *not* the state snapshot). Users must see the mic works before recording. M
- [ ] **Mute toggles while recording** (mic mute especially), independent of pause. S
- [ ] **Separate audio tracks**: system and mic as two AAC streams (plus optional mixed track) for editing. MP4 supports it;
      most editors import multi-track. M
- [ ] **Per-app audio** for window capture: `ActivateAudioInterfaceAsync` with `AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS`
      (Win10 2004+) → record only the captured app's audio (include process tree). Killer feature vs Snipping Tool. L
- [ ] **Limiter instead of hard clip**: mixer does `clamp(-1, 1)` → harsh distortion when system + mic sum > 0 dBFS at gain 2×.
      Add a simple look-ahead limiter or soft-knee compressor on the mix bus. S
- [ ] **Gain in dB** in the UI (slider 0–2 linear is unintuitive; −∞…+6 dB with a 0 dB detent). S
- [ ] **Drift correction**: the mixer realigns by zero-filling/dropping beyond 20 ms → can produce an audible click per
      correction. After the 15-min test (§2), if corrections happen regularly, replace with gradual resampling
      (adjust `AUTOCONVERTPCM` isn't controllable → small linear-interp resampler with ±0.1 % ratio). 🔍 M
- [ ] **Audio ends ~90 ms before video** (observed in `Oculus 2026-10-27 23-09-06.mp4`: 10.304 s audio vs 10.395 s video).
      Cause not pinned down yet — the mixer's final flush is meant to emit up to `clock.now()`, so either the flush loses
      the sub-`MIN_CHUNK` tail or the stop flag is observed before late packets land. Fix generically by padding audio to the
      last video PTS + frame duration. 🔍 S
- [ ] **Audio-only recording** mode (podcast/voice memo). P3 · S

### 4.2 Mic processing · P2
- [ ] Noise suppression (RNNoise via `nnnoiseless` crate — small, pure Rust). M
- [ ] Noise gate / expander. S
- [ ] Mono mic → centered in stereo (AUTOCONVERTPCM upmix does this; verify it's not left-only on some USB mics). 🔍 S
- [ ] Mic monitoring (hear yourself) — P3.
- [ ] Push-to-talk hotkey — P3.

### 4.3 Audio format · P3
- [ ] AAC bitrate option (128/160/192; the MS encoder only accepts 96/128/160/192 kbps).
- [ ] Opus-in-MP4 or FLAC for lossless — only if MF supports it cleanly. 🔍

---

## 5. Sources & picker

- [ ] **Monitor names**: "Display 1/2" is useless with 3 monitors. Use friendly EDID names via
      `DisplayConfigGetDeviceInfo(DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME)` ("DELL U2720Q"). P1 · S
- [ ] **Monitor thumbnails** (live or one-shot WGC grab, downscaled on GPU, sent as small JPEG/WebP data URL). P2 · M
- [ ] Show **refresh rate and scale** per monitor ("2560×1440 · 144 Hz · 125 %"). P2 · S
- [ ] **Window icons** (`WM_GETICON` / `GetClassLongPtr(GCLP_HICON)` → PNG data URL, cached per exe). P1 · M
- [ ] **Window thumbnails** on hover (DWM thumbnail API is native-only; or a one-shot WGC grab). P2 · M
- [ ] **Minimized windows** are filtered out (`IsIconic`, [sources.rs](src-tauri/src/sources.rs)) → user can't find the window they
      want. List them greyed with "minimized — restore to record" or auto-restore on start. P1 · S
- [ ] **Group windows by app**; sort by last-focused (Z-order) instead of enumeration order. P2 · S
- [ ] **Search**: also match on process name without `.exe`, fuzzy match, keyboard navigation (↑/↓/Enter). P2 · S
- [ ] **Live refresh**: windows list goes stale while the panel is focused (only refreshes on `focus`). Use
      `SetWinEventHook(EVENT_OBJECT_CREATE/DESTROY/NAMECHANGE)` → push `sources://changed`. P2 · M
- [ ] **Remember last source** across restarts (by monitor device name / window exe+title), not raw handles. P1 · S
- [ ] **"Pick window by clicking"** mode (crosshair, hover-highlight windows, click to select). P2 · M
- [ ] Exclude more junk windows: "Program Manager", cloaked Settings host, `ApplicationFrameHost` wrappers, NVIDIA overlay. 🔍 P2 · S
- [ ] Region presets: 1920×1080, 1280×720, 1080×1080, 1080×1920 (shorts), "last region". P2 · S

---

## 6. Region overlay ([overlay/region.rs](src-tauri/src/overlay/region.rs))

- [ ] **Only the monitor under the cursor** can be selected; when you click "Select area" in the panel the cursor is over
      *Oculus*, so you always get Oculus's monitor. Show the overlay on **all monitors** at once (one popup per monitor or one
      spanning window) and let the drag happen on any. P1 · M
- [ ] **Window snapping**: hover highlights the window under the cursor; click (no drag) selects its rect. Same for UI
      elements via UI Automation (P3). P1 · M
- [ ] **Adjust after drag**: resize handles on edges/corners, drag to move, Enter to confirm, instead of committing on
      mouse-up. P1 · M
- [ ] **Keyboard**: arrows nudge 1 px (Shift = 10 px), Alt+arrows resize, Enter confirm, Esc cancel, Space toggles move mode. P2 · S
- [ ] **Magnifier loupe** near the cursor with pixel grid + RGB/hex readout (doubles as a color picker). P2 · M
- [ ] **Crosshair guides** across the full monitor. P2 · S
- [ ] **Size label**: fixed 120 px width ([region.rs:213](src-tauri/src/overlay/region.rs:213)) clips long text and runs off the right
      edge near the screen border. Measure text with DirectWrite, clamp inside the monitor. P2 · S
- [ ] **Aspect lock** (hold Shift = 1:1, or preset ratios 16:9/9:16/4:3). P2 · S
- [ ] **Show the recorded region during recording**: a thin accent frame around the area (separate layered window excluded
      from capture via `WDA_EXCLUDEFROMCAPTURE`). Without it the user forgets where the edges are. P1 · M
- [ ] **Allow moving the region while recording** (P3; converter crop is mutable, needs a lock).
- [ ] Freeze-frame for *recording* region selection is fine, but add a "live" option (no freeze) for picking areas over video. P3 · S
- [ ] Overlay above exclusive-fullscreen games doesn't work (by design); document. S
- [ ] Re-use the session `Gpu` instead of `Gpu::new()` per selection ([region.rs:67](src-tauri/src/overlay/region.rs:67)); see §11. S
- [ ] HiDPI: label font is 14 DIP at 96 DPI = tiny on a 200 % display. Scale font by the monitor's DPI. P2 · S

---

## 7. Screenshots

- [ ] **Feedback**: hotkey screenshots from the tray give *zero* feedback. Add: shutter sound (optional), a toast/flash, and a
      small floating thumbnail (bottom-right, 5 s, click to open, drag-out to apps, like macOS). P1 · M
- [ ] **Image formats**: PNG only. Add JPEG (quality) and WebP; lossless WebP is ~30 % smaller than PNG for screen content. P2 · S
- [ ] **PNG encode speed**: `image::save_buffer` default compression on a 4K frame is slow (~200–400 ms 🔍). Use
      `PngEncoder::new_with_quality(Fast, Adaptive)` and do the encode off the hotkey path (clipboard first, file second). P2 · S
- [ ] **Window screenshot with transparent rounded corners/shadow** (alpha from DWM) as an option. P3 · M
- [ ] **Delay timer** (3/5/10 s) for capturing menus/hover states. P2 · S
- [ ] **Annotate after capture** (arrow, rectangle, highlight, blur/pixelate for secrets, text, crop). Large feature; native
      D2D editor or a second webview window. P3 · L
- [ ] **Scrolling capture**. P3 · L
- [ ] **OCR to clipboard** (`Windows.Media.Ocr`, built into Windows, zero dependencies). P3 · M
- [ ] Device per screenshot: `screenshot::capture` creates a new D3D device every time ([screenshot.rs:14](src-tauri/src/screenshot.rs:14)). Cache one (§11). S
- [ ] Screenshot of a **window** source uses WGC window capture — no cursor/menus; fine, document. S
- [ ] **HDR screenshots** washed out (same as 3.3). Save as JXR/HDR PNG or tone-map. P3 · M
- [ ] Clipboard: also put the file path as `CF_HDROP` so paste into Explorer/Slack drops the file. P2 · S

---

## 8. UI (visual)

- [ ] **Record button states**: "Starting…" spinner while `start_recording` is in flight (currently no feedback for
      100–300 ms; double-click possible). P1 · S
- [ ] **Countdown** 3-2-1 overlay before recording starts (optional setting). P2 · M
- [ ] **Record bar** ([RecordBar.jsx](src/components/RecordBar.jsx)):
  - [ ] Shows "0 fps" for the first second (ticker hasn't run). Hide fps until first tick. S
  - [ ] Elapsed only updates on the 1 Hz tick, and `thread::sleep(1s)` drift ([recorder.rs:254](src-tauri/src/recorder.rs:254))
        means seconds occasionally skip or repeat. Interpolate on the frontend from `elapsedMs` + `performance.now()` at receipt
        (rAF or a 250 ms interval, paused when `state !== "recording"`), or align the ticker to wall-clock seconds. S
  - [ ] Mic mute button, mic level meter, file size so far, disk space warning. M
  - [ ] Show audio failure badges (1.5). S
  - [ ] Collapse to a minimal dot/timer (draggable) option. P3 · S
  - [ ] Remember bar position per monitor; currently always docks top-center. S
- [ ] **Recording indicator in the tray icon** (red dot icon while recording, amber while paused) and tooltip with elapsed
      time. P1 · S
- [ ] **Post-recording card**: after stop, show filename, duration, size, thumbnail, buttons: Open, Show in folder, Copy
      file, Delete, Rename. Current "Saved X" status line is easy to miss. P1 · M
- [ ] **Recent recordings list** (last 10 with thumbnails) in the panel. P2 · M
- [ ] **Settings drawer**: no section headings, no "back" affordance (gear toggles), long — split into sections
      (Recording / Audio / Screenshots / Hotkeys / About). P2 · S
- [ ] **Gain sliders** show no value; add dB readout and double-click to reset. S
- [ ] **Icons**: hand-drawn set ([Icons.jsx](src/components/Icons.jsx)) — audit sizes/stroke consistency at 1×, 1.25×, 1.5×, 2×. S
- [ ] **App icon / tray icon**: verify crispness at 16/20/24/32 px (tray uses `default_window_icon`). Design a monochrome tray
      variant for light taskbar. P1 · S
- [ ] **Light theme** following `prefers-color-scheme` (WebView2 passes the system theme) — or explicitly dark-only, decide. P3 · M
- [ ] **Window resize**: fixed 400×640; on a 1366×768 laptop at 125 % that's tall. Test small screens; allow a compact
      height or scroll. P2 · S
- [ ] **Animations**: panel ↔ record bar transition is an instant resize; consider a short fade. Respect
      `prefers-reduced-motion` for the pulsing dot. P3 · S
- [ ] **Empty states**: no monitors (impossible-ish), no windows ("Open the app you want to record"), unsupported WGC. S
- [ ] **Error presentation**: long error strings in `.status.err` wrap and push layout; use a dismissible banner with
      "Copy details". P2 · S
- [ ] **Fonts**: `Segoe UI Variable` only on Win11; verify Win10 fallback metrics. S
- [ ] **Mica / acrylic** background on Win11 (`window-vibrancy` crate) — P3, measure memory cost first.

---

## 9. UX flows

- [ ] **First-run onboarding**: one screen — pick default source, test mic (meter), show hotkeys, choose output folder. P2 · M
- [ ] **Hotkeys are hard-coded** (`Ctrl+Shift+R/P/S/A` in [main.rs](src-tauri/src/main.rs)). Make them rebindable in settings with
      a capture field, conflict detection, and "disable". P1 · M
- [ ] Add hotkeys: mic mute, region record (select area → start immediately), window under cursor record, cancel recording
      (stop + delete). P2 · S
- [ ] **"Record area" one-shot**: select region → recording starts (today: select, then press Record). P1 · S
- [ ] **Discard recording** button/hotkey (stop and delete, with undo toast). P2 · S
- [ ] **Max duration / auto-stop timer** and **scheduled stop**. P3 · S
- [ ] **Replay buffer** ("save last 30 s"), à la ShadowPlay: ring of encoded GOPs in memory. P3 · L
- [ ] **Trim after recording** (lossless keyframe trim via MF remux; precise trim needs re-encode of the edges). P2 · L
- [ ] **Export GIF / short MP4 for chat** (size-targeted). P3 · M
- [ ] **File naming template** (`{date} {time} {source}`), with the window title in the filename for window recordings. P2 · S
- [ ] **Open folder after save** / **copy file to clipboard after save** options. P2 · S
- [ ] **Launch at login** (`tauri-plugin-autostart`), start minimized to tray. P1 · S
- [ ] **Single instance** (`tauri-plugin-single-instance`): launching Oculus twice creates two trays and the second can't
      register hotkeys. Second launch should focus the first. P1 · S
- [ ] **Close button semantics**: titlebar X hides to tray with no explanation the first time. Show a one-time toast
      "Oculus is still running in the tray". P2 · S
- [ ] **Drag & drop** last recording out of the panel into Discord/Slack (`startDrag` via a Tauri plugin). P3 · M
- [ ] **Changing source while recording** is impossible (fine) — make that explicit (disable picker, tooltip). S
- [ ] **Pause indicator** in the video? (Some users want a visible cut marker.) P3.

---

## 10. Accessibility

- [ ] **Toggles are not keyboard-accessible**: `.toggle input { display: none; }` ([theme.css:99](src/styles/theme.css:99)) removes
      the checkbox from the tab order. Use a visually-hidden pattern (`opacity: 0; position: absolute`) and style
      `:focus-visible + .switch`. P1 · S
- [ ] **Tabs**: `role="tablist"` without `aria-controls`, `tabpanel`, or arrow-key navigation (roving tabindex). P2 · S
- [ ] **Segmented controls**: should be `role="radiogroup"` with `aria-checked`, arrow keys. P2 · S
- [ ] **Range sliders** need `aria-label`/`aria-valuetext` ("+3 dB"). S
- [ ] **Icon-only buttons** (camera, crop, gear, minimize, close, pause, stop) rely on `title`; add `aria-label`. S
- [ ] **Record bar**: announce state changes via `aria-live="polite"` (Recording / Paused / Saving / Saved). S
- [ ] **Contrast**: `--muted #8a8f9c` on `--surface #16181d` ≈ 5.6:1 OK; `.badge` 10 px uppercase muted on surface-2 — check
      ≥ 4.5:1 and size. `button:disabled { opacity: .4 }` drops below 3:1. P2 · S
- [ ] **Focus order** after switching settings ↔ picker; focus first control. S
- [ ] **Region overlay** is mouse-only (see §6 keyboard) and invisible to screen readers (fine, but Esc must always work). S
- [ ] Windows **high-contrast mode** (`forced-colors: active`) styles. P3 · S
- [ ] **Text scaling** (Windows "Make text bigger"): WebView2 honors it; verify the fixed 400×640 layout doesn't clip. P2 · S

---

## 11. Performance & resources

### 11.1 Measured baseline (release, this machine: i7 + RTX 3060 Laptop, 1080p144)
| Metric | Value | Target |
| --- | --- | --- |
| RAM, tray only | 5.9 MB private | ✅ < 15 MB |
| RAM, window open | 136 MB private (process tree) | ❌ 60–90 MB |
| CPU recording 1080p60 | ~10–11 % of one core (debug) | measure release |
| Exe size | 5.3 MB | installer 3–5 MB |

### 11.2 Items
- [ ] **Window-open RAM is ~50 % over target.** Investigate per WebView2 process (browser / renderer / GPU / utility /
      crashpad). Try: `--single-process` (unsupported but works 🔍), `--renderer-process-limit=1`, `--disable-extensions`,
      `--js-flags=--max-old-space-size`, more `--disable-features=` entries; drop
      `--disable-gpu` if software compositing costs more in the renderer. Use `OCULUS_WEBVIEW_ARGS` for A/B. P2 · M 🔍
- [ ] **Shared device cache**: every screenshot / region selection / recording calls `Gpu::new()` (D3D11 device + video
      support, ~20–60 ms 🔍). Keep one lazily-created device for the app lifetime; recreate on device-removed. P2 · S
- [ ] **Cold start time**: measure exe launch → window visible. Lazy-create the tray menu/hotkeys before the webview; show the
      window only after first React paint (already `visible(false)` → `show()`; confirm no white flash on slow disks). P2 · S
- [ ] **Webview creation on tray click** (destroy-on-hide design) costs ~300–600 ms 🔍. Optionally keep it alive for N minutes
      after hide, or pre-warm on tray hover. P3 · S
- [ ] **Release CPU profile** during 1440p60 + audio: WPA/ETW trace, confirm encoder on GPU Video Encode engine and CPU
      < 3 %. P1 · S
- [ ] **GPU cost**: the VideoProcessor Blt on the 3D/video engine while gaming — measure fps impact in a GPU-bound game
      (target < 3 %). P1 · M
- [ ] **Audio path allocations**: `Vec` per WASAPI packet, `Vec` per mixer chunk, `MFCreateMemoryBuffer` + `MFCreateSample` per
      chunk ([mf_writer.rs](src-tauri/src/encode/mf_writer.rs)). Pool them. Low impact; do it when touching the mixer. P3 · S
- [ ] **Mixer wake-ups**: 10 ms `recv_timeout` + 5 ms WASAPI polling × 2 threads = several hundred wake-ups/s. Event-driven capture
      (`AUDCLNT_STREAMFLAGS_EVENTCALLBACK`) for the mic (loopback events are unreliable, keep polling there). P3 · S
- [ ] **Writer thread `recv_timeout(50ms)`** keep-alive granularity — fine; document.
- [ ] **Thread priorities**: writer + audio threads → `AvSetMmThreadCharacteristicsW("Pro Audio"/"Capture")` (MMCSS) so
      recording doesn't glitch under load. P2 · S
- [ ] **Power**: on battery, offer "efficiency mode" (30 fps, lower bitrate). Detect via `GetSystemPowerStatus`. P3 · S
- [ ] **Bundle size**: Vite build output size; tree-shake `@tauri-apps/api` imports; no React DevTools in prod. S
- [ ] **Release build of the frontend**: confirm `StrictMode` double-effects are dev-only (they are) and no sourcemaps
      shipped. S
- [ ] **Binary size**: check `cargo bloat` — `image` (png only) and `arboard` are the heavy ones; `windows` features list is
      long but only used items link. P3 · S

---

## 12. Reliability, logging, diagnostics

- [ ] **Logging**: release builds have nowhere to log. Add a rotating file log (`%LOCALAPPDATA%\Oculus\logs`, 5 × 1 MB) via
      `tracing` + `tracing-appender`; replace every `eprintln!`. P1 · M
- [ ] **Crash reporting**: `panic = "abort"` kills without a trace. Install a panic hook that writes the message + backtrace
      to the log before abort; minidump via `minidumper`/WER local dumps (opt-in upload only). P1 · M
- [ ] **Diagnostics page** (Settings → About): app version, Windows build, GPU(s) + driver versions, encoder MFT names,
      WGC features supported (border, `MinUpdateInterval`), audio devices, "Copy diagnostics" button. P2 · M
- [ ] **Session stats in the log**: arrived / encoded / dropped / keep-alives / audio corrections / max writer queue depth. S
- [ ] **Writer channel is unbounded** (`mpsc::channel`): if the sink writer stalls (slow disk), audio `Vec`s queue without limit.
      Bound it or monitor depth; video is already bounded by the NV12 pool. P2 · S
- [ ] **`.unwrap()` on mutexes** everywhere: a panic while holding a lock poisons it → every later call panics → abort.
      Acceptable with `panic = "abort"`, but document the decision. S
- [ ] **Selftest in CI** can't run headless (needs a desktop session) — keep it as a manual pre-release checklist; script the
      checks (ffprobe asserts) into one `npm run verify` command. P2 · M

---

## 13. Security & privacy

- [ ] **Capture exclusion has a debug escape hatch** (`OCULUS_SHOW_IN_CAPTURE`) gated on `debug_assertions` — confirm it is
      compiled out of release (it is via `cfg!`; add a test). S
- [ ] **`reveal` command** takes an arbitrary path from the webview and passes it to `explorer /select,` ([commands.rs](src-tauri/src/commands.rs)).
      Only `is_file()` is checked. Restrict to files under the output dir (canonicalize + prefix check). Low risk (CSP, no
      remote content), but cheap. P2 · S
- [ ] **Explorer argument injection**: `format!("/select,{path}")` with a path containing `,` or quotes. Quote properly or use
      `SHOpenFolderAndSelectItems`. P2 · S
- [ ] **CSP** is tight (`default-src 'self'`); keep it. `style-src 'unsafe-inline'` only needed if inline styles are used —
      check and drop. S
- [ ] **Capabilities**: `core:default` is broad; enumerate the exact permissions used. P2 · S
- [ ] **Privacy defaults**: mic off by default ✅; add a visible "mic is on" indicator while recording. S
- [ ] **Excluded windows list**: let users exclude password managers / chat apps from monitor capture (WGC can't exclude
      other apps' windows directly; would require compositing — document as limitation, or P3 L).
- [ ] **Code signing** (Authenticode, ideally EV or Azure Trusted Signing): unsigned exe + global hotkeys + screen capture =
      SmartScreen and AV heuristics flags. P1 · M (cost $)

---

## 14. Packaging, distribution, updates

- [ ] Build NSIS installer; verify Start Menu shortcut, uninstaller removes `%APPDATA%\dev.gloria.oculus` (optional
      checkbox), upgrade preserves settings. P1 · S
- [ ] **WebView2 install mode is `skip`** ([tauri.conf.json](src-tauri/tauri.conf.json)): on Windows 10 LTSC / Server / stripped
      images without WebView2 the app won't open the UI. Use `downloadBootstrapper` (tiny) or `embedBootstrapper`. P1 · S
- [ ] **Auto-update** via `tauri-plugin-updater` + signed update manifest on GitHub Releases. P2 · M
- [ ] **Versioning**: 0.1.0 everywhere; single source of truth (`tauri.conf.json` ↔ `Cargo.toml` ↔ `package.json`), changelog. S
- [ ] **winget manifest**, Scoop bucket. P3 · S
- [ ] **Portable build** (zip, settings next to exe if `portable.txt` exists). P3 · S
- [ ] **ARM64 build** (Windows on Snapdragon): WGC + MF work; test the Qualcomm encoder MFT. P3 · M
- [ ] **Minimum OS**: declare Windows 10 2004 (19041) or Windows 11 only; check `GraphicsCaptureSession::IsSupported` and
      fail with a clear message. S
- [ ] Remove unused icon files from the working tree (ignored by git but still lying around in `src-tauri/icons`). S

---

## 15. Code quality, architecture, tooling

### 15.1 Frontend
- [ ] **`useSettings` race** ([useSettings.js](src/hooks/useSettings.js)): two quick toggles → response for #1 overwrites
      optimistic #2 (flicker), and a failed #1 rolls back to a snapshot that predates #2. Serialize writes (queue) or keep a
      request counter and only adopt the latest response; roll back with a functional update. P1 · S
- [ ] **Range sliders write to disk on every `input` event** ([SettingsDrawer.jsx:56](src/components/SettingsDrawer.jsx:56)):
      dragging = dozens of IPC calls + `fs::write`s per second. Keep a local draft value while dragging; commit on
      `pointerup`/`change` (or debounce 150 ms). P1 · S
- [ ] `useSources` refetch on every window `focus` runs `EnumWindows` + `OpenProcess` per window each time; fine, but debounce
      (alt-tab spam). S
- [ ] Unhandled promise rejections: `api.setSource(...)` in [SourcePicker.jsx](src/components/SourcePicker.jsx), `api.stop`,
      `api.togglePause`, `api.openOutputDir` have no `.catch`. Centralize in `api.js` (wrap `invoke`, report to a toast). S
- [ ] Silent `.catch(() => {})` on screenshot/region buttons ([App.jsx:59](src/App.jsx:59)) — acceptable only because the
      backend mirrors errors into the snapshot; document that contract in `api.js`. S
- [ ] **TypeScript migration** (planned): start with `api.ts` typed against the Rust types; generate types with `ts-rs` or
      `specta` (+ `tauri-specta` for typed commands) so Rust ↔ TS can't drift. P2 · M
- [ ] ESLint (`eslint-plugin-react-hooks` with the React 19 compiler rules) + Prettier. S
- [ ] **React Compiler** (babel plugin) — measure; the UI is tiny, mostly for hygiene. P3 · S
- [ ] Component tests (Vitest + Testing Library) for `useRecorder` ordering (`seq`), `useSettings` race, `formatElapsed`. P2 · M

### 15.2 Backend
- [ ] **Clippy clean** with `-D warnings`; add `rustfmt.toml`. S
- [ ] **Unit tests** beyond the mixer: `Clock::to_pts` (pause math incl. multi-pause), `encoder_dims`, `fit`, `sel_rect`,
      `Settings::sanitized`, `Source` serde shape. S
- [ ] **Integration tests**: MF writer with synthetic NV12 + PCM → ffprobe assertions (needs GPU; mark `#[ignore]`). M
- [ ] **Split `session::run`** (~150 lines) into setup / capture wiring / audio wiring / loop / teardown functions. S
- [ ] **Settings schema versioning** (`version: 1`) for future migrations (e.g. hotkeys). S
- [ ] **Error enum**: add context (`"opening mic: {e}"`) — raw HRESULT messages ("The parameter is incorrect.") are useless to
      users. Map common HRESULTs (`E_ACCESSDENIED`, `MF_E_TRANSFORM_NOT_POSSIBLE_FOR_CURRENT_MEDIATYPE_COMBINATION`,
      `DXGI_ERROR_UNSUPPORTED`) to human text. P1 · M
- [ ] Replace `Mta<T>` blanket `unsafe impl Send/Sync` with narrower wrappers per interface (document why each is safe). P3 · S
- [ ] `selftest.rs` is debug-only; promote the pure assertions to tests, keep the driver as an `examples/` binary. P3 · S

### 15.3 Repo & CI
- [ ] **GitHub Actions**: `windows-latest` → `npm ci`, `vite build`, `cargo fmt --check`, `cargo clippy -D warnings`,
      `cargo test`, `tauri build`, upload installer artifact. P1 · M
- [ ] **LICENSE** (the repo is public with no license = all rights reserved by default). Decide MIT/Apache-2.0/GPL. P1 · S
- [ ] Issue templates (bug report asks for diagnostics from §12), CONTRIBUTING, CHANGELOG. P3 · S
- [ ] Dependabot / `cargo audit` / `npm audit` in CI. P2 · S
- [ ] Pin Rust toolchain (`rust-toolchain.toml`). S

---

## 16. Documentation

- [ ] README: screenshots/GIF of the UI, feature list, system requirements, known limitations (DRM black, exclusive
      fullscreen, HDR, menus in window capture), hotkey table, FAQ ("why is my file VFR?"). P1 · S
- [ ] `ARCHITECTURE.md`: the pipeline diagram, threading model (WGC callback thread / writer / audio ×2 / mixer / ticker), the
      single-device rule, timing rules, why webview is destroyed on hide. Mostly lift from the plan. P2 · S
- [ ] Inline docs for the non-obvious invariants already explained in chat but not in code: tracked-sample pool lifetime,
      clock latch, `seq` ordering. Most are there; audit. S
- [ ] Privacy statement (no network, no telemetry) — a selling point; say it. S

---

## 17. Feature backlog (post-1.0, P3)

- [ ] Webcam overlay (Media Foundation source reader → GPU composite into the NV12 output, draggable bubble).
- [ ] Scene composition: monitor + webcam + image/text overlays (scope creep warning: this is OBS).
- [ ] Live drawing/annotation on screen while recording (pen, highlighter, spotlight, zoom-in on cursor).
- [ ] Auto-zoom on clicks (Screen Studio style) as a post-process.
- [ ] Blur regions / auto-blur detected secrets.
- [ ] Upload / share link integrations (S3, custom endpoint) — conflicts with "no network"; opt-in only.
- [ ] Streaming (RTMP) — out of scope; say so in README.
- [ ] CLI (`oculus record --region … --out …`) for automation; selftest already proves the pipeline can run headless.
- [ ] Linux/macOS — no: the whole capture/encode stack is Windows-native by design. Document as a non-goal.

---

## 18. Explicit non-goals (so they stop coming back)

- No FFmpeg bundling. No Electron. No telemetry. No account. No cloud.
- No full NLE; trimming at most.
- Windows 10 2004+ / Windows 11 only.

---

## Suggested order

1. **P0 batch (≈2–3 days):** 1.1 crash-safe container, 1.3 filenames, 1.4/1.5 audio device + visibility, single instance,
   1.2 quit/shutdown finalize, 15-min drift + mixed-DPI + no-HW-encoder tests.
2. **Ship-blockers (≈1 week):** logging + panic hook, error text, settings atomic write + slider debounce + `useSettings` race,
   tray recording indicator, post-recording card, screenshot feedback, toggles a11y, rebindable hotkeys, WebView2 bootstrapper,
   installer, LICENSE, CI, code signing.
3. **Parity (≈2 weeks):** encoder rate control/GOP, fps options + CFR mode, device pickers + meters + separate tracks,
   multi-monitor region overlay with snapping/handles, region frame during recording, window icons, monitor names.
4. **Differentiators:** per-app audio, cursor effects, trim, replay buffer, annotation.
