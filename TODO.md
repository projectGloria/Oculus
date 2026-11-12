# Oculus — Master TODO

Everything left between "works on my machine" and "ship it to strangers", grounded in the code as of
`13555bc`. Items that cite a file are real findings from reading that code, not speculation.

Documentation lives in [README.md](README.md) (quick start), [OCULUS_PROJECT_GUIDE.md](OCULUS_PROJECT_GUIDE.md)
(complete guide, architecture, privacy and change history), and this task checklist. Historical findings below
retain their context; implementation status and remaining validation are recorded per item.

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

Current intended use: personal use on the owner's computer. Code signing is unnecessary for
that scope and is deferred to any future public distribution. No signing identity is required
to continue local development or use the existing executable. Public-release items below
remain a separate roadmap; recording reliability and data safety still apply locally.

- [x] Monitor / window / region capture, zero-copy GPU path (WGC → VideoProcessor → NV12 → HW MFT).
- [x] System audio + mic, QPC-aligned mixer, pause/resume, H.264/HEVC + AAC MP4.
- [x] PNG screenshots + area screenshots + clipboard.
- [x] Tray, global hotkeys, compact record bar, capture exclusion of our own UI.
- [x] 60 fps on high-refresh monitors (`MinUpdateInterval`, `13555bc`).
- [ ] Remaining unchecked work below. Completed implementations retain their original finding for context.

---

## Audit findings

Static audit of the recording path at `b8e3452` (2026-11-12): `recorder.rs`, `session.rs`, `queue.rs`, `clock.rs`,
`capture/`, `encode/mf_writer.rs`, `audio/`, `lifecycle.rs`. These findings come from reading the code only. None has
been reproduced at runtime. Ordered by severity. Confidence describes how sure we are that the failure can happen,
not how often it will.

Checked and found sound: tracked-sample reuse (session pool, CFR repeat pool, keep-alive `CopyResource` ordering on the
one protected immediate context), session-generation gating of `finished`/`tick`, Stop never waiting on the media
or control queue, and mutex `unwrap()`s (no poisoning under `panic = "abort"`).

### High

- [x] **A stuck audio preflight cannot be cancelled and records without a disk-space guard** · High · P0 · M
      *Fixed (2026-11-12, uncommitted):* preflight now has a 10 s deadline (`AUDIO_PREFLIGHT_DEADLINE`,
      `poll_audio_preflight` in `session.rs`). When it expires, endpoints still `Starting` are reported `Failed`
      (`Health::fail_unresponsive`) and the session commits, so Stop, quit, suspend and the disk-space ticker reach it
      within 10 s and a cancel no longer discards an arbitrarily long recording. The worker keeps running and reports
      `Ok` if its open completes later. Unit tests: `stalled_audio_preflight_ends_at_the_deadline_and_reports_the_endpoint`,
      `completed_audio_preflight_does_not_wait_for_the_deadline`, `unresponsive_preflight_fails_only_endpoints_still_starting`.
      *Not fixed here:* a WASAPI call that never returns still hangs teardown, which joins the worker (see the Medium
      item below). Pre-commit Stop is bounded by the deadline rather than immediate.
      *Evidence (2026-11-12, debug build, this machine):* `npm run verify` passed; its 5 s delayed-preflight clip
      still waited for readiness (setup 5.30 s). An extra `--selftest … --secs 3 --sys --mic
      --audio-preflight-delay-ms 15000` committed at 10.29 s instead of 15 s, exit 0, 0 rejected packets, and
      `ffprobe` read h264 + aac streams of 13.03 s. The delay is synthetic; no real stalled device was tested.
      [`session.rs:173`](src-tauri/src/session.rs#L173) blocks `Recorder::start` on `ready_rx.recv()` with no deadline.
      Preflight completes only when every WASAPI worker has opened its endpoint
      ([`session.rs:347`](src-tauri/src/session.rs#L347)). `GetDevice`/`Activate`/`Initialize`
      ([`wasapi.rs:216-247`](src-tauri/src/audio/wasapi.rs#L216)) have no timeout. No `Handle` exists until commit.
      As a result, Stop only sets `cancel_start` ([`recorder.rs:452-455`](src-tauri/src/recorder.rs#L452)), and the
      ticker that enforces the 200 MB stop threshold is spawned only after commit
      ([`recorder.rs:446`](src-tauri/src/recorder.rs#L446)).
      *Scenario:* the mic is a Bluetooth headset that is still connecting, or a driver or `audiodg` stall blocks
      `IAudioClient::Initialize` indefinitely. The writer is already capturing and writing video to the MP4. The UI
      stays on "Starting" and Stop has no effect. Quit sets `quitting`, but `ExitRequested` keeps being prevented
      ([`main.rs:202-205`](src-tauri/src/main.rs#L202)). `WM_QUERYENDSESSION` and suspend block in `stop_and_wait`
      ([`recorder.rs:496-498`](src-tauri/src/recorder.rs#L496)). Meanwhile the file grows with no free-space check. If
      the call eventually returns, the cancel deletes everything recorded so far
      ([`recorder.rs:556-557`](src-tauri/src/recorder.rs#L556)).
      *Confidence:* high that the code behaves this way. Medium that a WASAPI call blocks for long periods in the field.
      The selftest `--audio-preflight-delay-ms` (capped at 60 s) can show the unbounded wait without real hardware.
      *Direction:* route a pre-commit cancel to the writer's control channel. Give preflight a deadline after which
      that source is marked `Failed` and startup continues. Run the disk check from the writer, or start the ticker
      at setup.

### Medium

- [ ] **Finalization joins WASAPI workers that can be blocked inside a device reopen** · Medium · P1 · S
      Teardown sets `stop` and then `join()`s every audio thread before it drains or finalizes
      ([`session.rs:383-386`](src-tauri/src/session.rs#L383)). A worker checks `stop` only between WASAPI calls. After
      any endpoint notification it reopens its device through the same untimed `Activate`/`Initialize` path
      ([`wasapi.rs:174-194`](src-tauri/src/audio/wasapi.rs#L174), [`216-247`](src-tauri/src/audio/wasapi.rs#L216)).
      *Scenario:* a Bluetooth headset drops mid-recording, the mic worker blocks in `Initialize` on the new default
      device, and the user presses Stop. The recorder shows "Saving recording…" forever, and quit and shutdown wait
      on it by design. An HEVC recording (regular MP4 sink) has no `moov` until `Finalize`, so killing the stuck
      process loses the whole recording. *Confidence:* medium.
      *Direction:* finalize video and the audio already mixed, then join workers with a bound. A stuck worker can be
      abandoned, since it only owns its own COM objects.

- [ ] **Overload budgets are well under a second and recording threads have no priority** · Medium · P1 · S
      Any rejected packet ends the recording ([`session.rs:354`](src-tauri/src/session.rs#L354)). Video in the writer
      queue is limited by the NV12 pool cap of 12 ([`convert.rs:193`](src-tauri/src/capture/convert.rs#L193)). The
      mixer emits ~100 packets/s of 10 ms chunks ([`mixer.rs:21`](src-tauri/src/audio/mixer.rs#L21),
      [`196-203`](src-tauri/src/audio/mixer.rs#L196)). The 128-slot writer queue
      ([`session.rs:132`](src-tauri/src/session.rs#L132)) therefore tolerates only ~1.1 s of writer stall. The 64-slot
      audio input queue ([`session.rs:133`](src-tauri/src/session.rs#L133)) takes ~10 ms packets per source, so with
      system and mic both active it tolerates only ~320 ms of mixer starvation. In CFR mode the repeat pool waits
      250 ms for a free surface and then fails the session
      ([`mf_writer.rs:278-287`](src-tauri/src/encode/mf_writer.rs#L278)).
      *Scenario:* a fullscreen game saturates the GPU and CPU, and a single slow fragment write or a 250 ms encoder
      backlog ends the recording with an overload error. The partial file is kept, as designed. *Confidence:*
      medium. The 2-second stall tests in §12 confirm that a stall ends the session, but the real-world stall
      distribution has not been measured. *Direction:* emit larger mixer chunks (for example 50 ms) to raise the
      writer budget without unbounded buffering. Measure before changing caps. Pairs with the MMCSS item in §11.2.

### Low

- [ ] **A recording with no frames leaves an empty MP4 labelled "partial output"** · Low · P2 · S
      WGC delivers no frames for a minimized window, so the clock never latches
      ([`clock.rs:27-37`](src-tauri/src/clock.rs#L27)), the mixer drops all audio, and keep-alive has no last frame
      ([`session.rs:483`](src-tauri/src/session.rs#L483)). `run` returns "No frames were captured"
      ([`session.rs:433-435`](src-tauri/src/session.rs#L433)) after commit, so the file is kept
      ([`session.rs:164-167`](src-tauri/src/session.rs#L164)) and added to recent recordings as incomplete
      ([`recorder.rs:575-583`](src-tauri/src/recorder.rs#L575)).
      *Scenario:* pick a window, minimize it, press the Record hotkey, then Stop. The result is a sample-less MP4 in
      the output folder, and history reports "Partial output retained at …". *Confidence:* high.
      *Direction:* delete the file when `!had_video`, or refuse to start a window target that is `IsIconic`.

- [ ] **Any audio device event restarts both capture streams** · Low · P2 · S
      `OnDeviceStateChanged`, `OnDeviceAdded` and `OnDeviceRemoved` set `changed` for every endpoint of every flow,
      even when a specific device was selected ([`wasapi.rs:72-83`](src-tauri/src/audio/wasapi.rs#L72)).
      *Scenario:* an HDMI/DP monitor with an audio endpoint sleeps or wakes, or an unrelated USB audio device is
      plugged in, during a recording. Both workers tear down and reopen their unchanged endpoints. Buffered packets
      are discarded, and the gap is zero-filled: tens of ms, longer for Bluetooth. Every such event also exposes the
      reopen hang above. *Confidence:* high. *Direction:* compare the notified device ID with the open endpoint, and
      for "Default" restart only on `OnDefaultDeviceChanged` or on removal of the open device.

- [ ] **Tray updates run synchronously under the recorder mutex on the main thread** · Low · P2 · S
      `emit` calls `tray_status::update` while holding `inner` ([`recorder.rs:193-196`](src-tauri/src/recorder.rs#L193)).
      `run_on_main_thread` executes inline when called from the main thread: `send_user_message` in
      tauri-runtime-wry 2.12.3 checks `current_thread().id() == main_thread_id`. So `set_icon`/`set_tooltip` run with
      the lock held for main-thread callers: tray Quit ([`main.rs:103`](src-tauri/src/main.rs#L103)), the pause/mute
      hotkeys ([`main.rs:56`](src-tauri/src/main.rs#L56), [`69`](src-tauri/src/main.rs#L69)), and the sync commands
      `stop_recording`, `toggle_pause`, `set_source`, `toggle_audio_mute`. This breaks the §6 invariant
      ("dispatched … without waiting while holding the recorder lock").
      *Scenario:* Explorer is hung or restarting when the user presses Stop. `Shell_NotifyIcon` blocks the main thread
      while it holds the recorder mutex, so the writer's `finished` callback, the ticker and the lifecycle thread's
      `stop_and_wait` also wait on Explorer. *Confidence:* medium. The inline dispatch is verified in the crate
      source. `Shell_NotifyIcon` blocking on a hung Explorer has not been verified. *Direction:* collect the snapshot
      under the lock and call `tray_status::update` after dropping it. The `seq` check already tolerates reordering.

- [ ] **Cancelled-start cleanup deletes the file while holding the recorder mutex** · Low · P2 · S
      [`recorder.rs:556-557`](src-tauri/src/recorder.rs#L556) calls `remove_file` inside `finished` with `inner`
      locked. `tick` deliberately avoids this for disk queries ([`recorder.rs:607`](src-tauri/src/recorder.rs#L607)).
      *Scenario:* the output folder is on an SMB share or a slow USB drive, and the user cancels during Starting. The
      delete blocks for the redirector timeout, and every main-thread command (`get_recorder_state` from a reopened
      panel, Stop, `set_source`) stalls with it. *Confidence:* high for the mechanism, low for the likelihood.
      *Direction:* take the path out of `Inner`, then delete it after `drop(i)`.

- [ ] **Thread-spawn `expect`s abort the process while a session is live** · Low · P2 · S
      [`wasapi.rs:142`](src-tauri/src/audio/wasapi.rs#L142), [`mixer.rs:206`](src-tauri/src/audio/mixer.rs#L206)
      and `thread::spawn` in [`recorder.rs:638`](src-tauri/src/recorder.rs#L638) panic if thread creation fails. The
      writer thread uses `map_err` ([`session.rs:171`](src-tauri/src/session.rs#L171)).
      *Scenario:* the system is at its commit limit when audio workers or the ticker start. The MP4 already exists and
      WGC is running, so `panic = "abort"` kills the process and leaves the placeholder or partial file (unplayable
      for HEVC). *Confidence:* high for the mechanism, low for the likelihood. *Direction:* use `Builder::spawn` with
      errors propagated as setup failures, or mark the audio source `Failed`.

- [ ] **The mixer trusts WASAPI timestamps unconditionally, and its zero-fill is unbounded** · Low · P2 · S
      Only `AUDCLNT_BUFFERFLAGS_SILENT` is checked. `AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR` is ignored, and the reported
      QPC is used as-is ([`wasapi.rs:263-271`](src-tauri/src/audio/wasapi.rs#L263)). `Track::push` zero-fills any
      forward gap with no upper bound ([`mixer.rs:48-52`](src-tauri/src/audio/mixer.rs#L48)).
      *Scenario:* a driver flags a packet with a bogus QPC seconds or hours ahead. That track jumps forward, and
      correctly timed packets are then dropped as overlap until real time catches up, losing that span of audio. A
      gap of hours would need GBs of zeros, and the allocation failure aborts mid-recording. *Confidence:* low; this
      depends on driver behavior. *Direction:* drop or re-time `TIMESTAMP_ERROR` packets from the previous packet's
      end, and clamp `gap` to the distance between the mixer's current position and `clock.now()`.

### Non-recording code

Static audit of the non-recording code at `b8e3452` (2026-11-12): `src/` (`api.js`, `settingsQueue.js`,
`recorderSnapshot.js`, `hooks/`, the hotkey, settings and screenshot-feedback components), `commands.rs`,
`hotkeys.rs`, `shell.rs`, `saved_output.rs`, `overlay/region.rs`, plus the parts of `settings.rs`, `recorder.rs`,
`ui.rs` and `main.rs` they call. Same method and caveats as above: code reading only, nothing reproduced at
runtime. Ordered by severity. No High findings.

Checked and found sound: subscribe-then-hydrate in `useRecorder` (every snapshot, including the one `get_recorder_state`
returns, takes its `seq` under the recorder lock, so a hydrate response is never older than an event already emitted,
and the StrictMode double-mount unsubscribes a late `listen`); Rust-side listener cleanup for destroyed webviews
(tauri 2.12.5 `on_window_close` and `on_webview_close` call `remove_webview_listeners`, so the recreated `main` and
`screenshot-feedback` webviews do not inherit stale registrations, and every JS timer and listener in `hooks/` is
cleared on unmount); held hotkeys do not auto-repeat (`global-hotkey` 0.8.0 registers with `MOD_NOREPEAT`); the
register/unregister/persist rollback in `hotkeys::transition`; saved-file path validation (`output_file` canonicalizes
both sides, so a junction or symlink inside the output folder that points outside it, `..` segments and sibling-prefix
folders are rejected, and every command except `reveal` also requires the path to be a remembered output); `rename`
never overwrites (`MoveFileExW` without `REPLACE_EXISTING`); `region::select` versus `Recorder::start` mutual
exclusion; and the overlay's `STATE` `RefCell` is never borrowed across `ReleaseCapture`/`DestroyWindow`.

### Medium

- [ ] **One hotkey that failed to register at startup blocks every later hotkey edit** · Medium · P1 · S
      [`Registry::initialize`](src-tauri/src/hotkeys.rs#L102) skips a conflicting key, so it is in the saved settings
      but not in `registered`. [`Registry::save`](src-tauri/src/hotkeys.rs#L119) builds `desired` from the whole
      saved set and tries to register everything missing, so it tries that key again and `transition` rolls the
      entire change back ([`hotkeys.rs:150-164`](src-tauri/src/hotkeys.rs#L150)).
      *Scenario:* OBS already owns `Ctrl+Shift+R` at launch, and the Settings row shows the warning. The user changes
      the screenshot hotkey to `Ctrl+Alt+S` and gets "Shortcut Ctrl+Shift+R is unavailable…". Nothing can be saved
      until the conflicting field is cleared or the other app is closed. "Retry saved hotkeys" fails the same way, and
      rolls back the keys it registered. The unrelated edit is rejected with a message about a different shortcut.
      *Confidence:* high. *Direction:* register the keys that changed, and report conflicts for the others as
      per-key warnings instead of failing the whole transaction.

- [ ] **Area selection fails on every monitor when any one monitor cannot be frozen** · Medium · P1 · S
      Each monitor runs `select_monitor`, which calls `wgc::grab` ([`region.rs:181`](src-tauri/src/overlay/region.rs#L181)).
      `grab` waits 1.5 s for a frame ([`wgc.rs:170-172`](src-tauri/src/capture/wgc.rs#L170)). Any worker error goes
      to `Coordinator::finish` ([`region.rs:143`](src-tauri/src/overlay/region.rs#L143)), and the first result wins and
      posts `CANCEL_SELECTION` to every overlay already open. `monitor_info` failing for a display that disappears
      between enumeration and the worker ([`region.rs:178`](src-tauri/src/overlay/region.rs#L178), "No monitor under
      cursor") ends it the same way.
      *Scenario:* a display that is asleep, a DisplayLink or other virtual adapter that WGC does not deliver frames
      for, or a monitor unplugged mid-selection. The overlays on the healthy monitors flash up and are torn down with
      "Capture timed out (is the target minimized?)", so Area screenshot and Record area do not work at all
      while that display is attached. *Confidence:* medium. The mechanism is certain; which displays miss the
      1.5 s window is unmeasured. *Direction:* treat a failed monitor as skipped and fail only when no monitor
      produced an overlay.

- [ ] **Recent recordings from a previous output folder stay listed, but every action on them fails** · Medium · P2 · S
      `saved_path` ([`commands.rs:191`](src-tauri/src/commands.rs#L191)) validates a remembered path against the
      *current* output folder ([`shell.rs:12-22`](src-tauri/src/shell.rs#L12)). The remembered list is not cleared or
      re-rooted when `outputDir` changes, and `set_settings` accepts any `outputDir`.
      *Scenario:* record two clips, change the output folder in Settings, return to the panel. The Recent list
      still shows both with Open, Show in folder, Copy, Rename and Delete. All fail with "Only files in the current
      output folder can be shown", and the thumbnails fail to load. The same happens if the configured folder becomes
      unavailable and `output_dir()` silently switches to the fallback (see the Low item on `output_dir()`).
      *Confidence:* high. *Direction:* record the canonical folder each entry was saved into and validate against
      that, or drop entries that no longer resolve under the current folder.

### Low

- [ ] **Invalid persisted hotkeys block every unrelated settings save, and the startup message is wrong** · Low · P2 · S
      `read_settings`/`sanitized` never validate `hotkeys`, but [`SettingsStore::set`](src-tauri/src/settings.rs#L171)
      calls `hotkeys.parsed()?` on every write, and the frontend always sends the full snapshot.
      *Scenario:* a hand-edited `settings.json`, or a shortcut string a newer `global-hotkey` stops accepting after an
      upgrade. At startup `initialize` registers nothing and reports it through `hotkey_failed`, which labels it
      "unavailable (possibly in use by another app)" ([`recorder.rs:233-237`](src-tauri/src/recorder.rs#L233)). Then
      toggling "Microphone" or moving a gain slider fails with "Invalid shortcut …" until the user finds the Hotkeys
      section. *Confidence:* high. *Direction:* validate on load and fall back to defaults for the hotkeys with a notice,
      and use a different message for parse errors.

- [ ] **A rejected patch poisons later queued writes, and the hotkey draft keeps showing the rejected value** · Low · P2 · S
      `update` merges each patch into `optimistic` and sends the whole snapshot
      ([`settingsQueue.js:14-36`](src/settingsQueue.js#L14)). If request A is rejected by validation and request B
      was queued while A was in flight, B carries A's rejected field and fails too. The revert to `confirmed` then
      discards B's valid change. Only the latest request reports the error. `HotkeySettings` copies `value` into
      `draft` once ([`HotkeySettings.jsx:9`](src/components/HotkeySettings.jsx#L9)), so after the revert the fields
      still show the rejected shortcuts while the saved ones differ.
      *Scenario:* save `Ctrl+Shift+Q` while another app owns it, and flip the System audio chip before the
      `set_settings` round trip returns (it registers shortcuts, so it is not instant). The chip flip is reverted
      along with the hotkey change. *Confidence:* high for the mechanism, low for the likelihood. *Direction:* send
      patches (or rebase each request on `confirmed`), and reset `draft` when `value` changes.

- [ ] **`SettingsStore::output_dir` creates the folder and writes a probe file on every call, including validation** · Low · P2 · S
      [`settings.rs:198-213`](src-tauri/src/settings.rs#L198) runs `create_dir_all` plus a create/delete probe each
      time. `saved_path`, `reveal` and `open_output_dir` call it, so each thumbnail fetch, Open and Rename does.
      *Scenario:* the user deletes the output folder while Oculus runs, then clicks Open on a recent entry. The
      folder is recreated empty. On a slow or offline share each click pays the redirector timeout, and a failure
      swaps in the fallback folder and sets a warning from inside a read-only action.
      *Confidence:* high. *Direction:* probe only when starting a save, and have validation only canonicalize.

- [ ] **Window snapping uses `GetWindowRect`, which includes the invisible resize border** · Low · P2 · S
      [`snap_rects`](src-tauri/src/overlay/region.rs#L405) (and [`sources.rs:147`](src-tauri/src/sources.rs#L147))
      take `GetWindowRect`. On Windows 10 and 11 that rect extends about 7 px past the visible frame on the left,
      right and bottom for most top-level windows.
      *Scenario:* click a window in the area picker. The selection, and the screenshot or recording, includes a strip
      of whatever is behind the window on three sides. *Confidence:* high for the platform behavior, not yet
      confirmed in this app. *Direction:* use `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)`.

- [ ] **Alt+Enter may trigger DXGI's fullscreen toggle on the overlay** · Low · P3 · S
      The overlay swap chain is created with `CreateSwapChainForHwnd` and no `MakeWindowAssociation`
      ([`region.rs:268`](src-tauri/src/overlay/region.rs#L268)). DXGI's default is to watch the window's messages for
      Alt+Enter. The `WM_SYSKEYDOWN` arm also treats Enter as confirm ([`region.rs:618-638`](src-tauri/src/overlay/region.rs#L618)),
      and Alt+arrows resize, so the user may well still be holding Alt when pressing Enter.
      *Scenario:* resize with Alt+arrows, then press Enter without releasing Alt. The overlay may flicker through a
      mode switch before it confirms. *Confidence:* low. Not tested here. *Direction:*
      `MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER)`.

- [ ] **A panic in an overlay worker aborts the process, including a live recording** · Low · P3 · S
      [`region.rs:113-116`](src-tauri/src/overlay/region.rs#L113) wraps each worker in `catch_unwind`, but release
      builds are `panic = "abort"`, so that fallback never runs. Area screenshot is allowed while recording, and
      `render` holds `STATE.borrow()` across D2D/DXGI calls ([`region.rs:456-518`](src-tauri/src/overlay/region.rs#L456)).
      *Scenario:* any panic in the picker (a `RefCell` double borrow if a call under that borrow ever pumps a
      message, an unwrap in a later change) kills the recorder with it, and an HEVC file has no `moov`. No concrete
      trigger found. *Confidence:* low. *Direction:* keep the picker free of panicking paths, and drop the
      misleading `catch_unwind`.

- [ ] **`open` runs `ShellExecuteW` on a thread that exits immediately** · Low · P3 · S
      [`saved_output.rs:40-52`](src-tauri/src/saved_output.rs#L40) spawns a thread, enters an STA, calls
      `ShellExecuteW` and tears the apartment down on return. The documentation recommends keeping the thread alive,
      or `SEE_MASK_NOASYNC` with `ShellExecuteEx`, for handlers that start asynchronously (DDE).
      `details` and `open` also use `std::thread::spawn`, which aborts on thread-creation failure.
      *Scenario:* a default app that is launched through a DDE verb occasionally never opens, and returns no error.
      *Confidence:* low. Handlers differ and none was tested. *Direction:* use `ShellExecuteExW` with
      `SEE_MASK_NOASYNC`, and `Builder::spawn`.

- [ ] **`reveal` is the only saved-file command that skips the remembered-output check** · Low · P3 · S
      [`commands.rs:169-177`](src-tauri/src/commands.rs#L169) goes straight to `shell::output_file`, while the other
      six go through `saved_path`. The path check still holds, so this only lets the webview reveal any regular file
      under the output folder, not outside it. It is still inconsistent with the §6 statement that the webview can
      only act on remembered outputs. *Confidence:* high. *Direction:* route it through `saved_path`.

- [ ] **A failed hydrate or `listen` leaves the recorder UI on an idle default with no retry** · Low · P3 · S
      On a rejection, `useRecorder` keeps `INITIAL` (`seq: 0`, idle) and shows an error
      ([`useRecorder.js:44-46`](src/hooks/useRecorder.js#L44)). A later event heals it, and while recording the
      ticker emits once a second. If `listen` rejects, or the session is paused or finalizing when
      `get_recorder_state` fails, the panel shows Record while a recording is running. `useSettings` likewise leaves
      `settings` null on a failed load ([`useSettings.js:15`](src/hooks/useSettings.js#L15)), so the chips and the
      Settings drawer never appear. *Confidence:* high for the mechanism, low for the likelihood. *Direction:* retry
      with backoff and keep the error visible until it succeeds.

- [ ] **An empty screenshot-feedback window stays up after the screenshot is deleted** · Low · P3 · S
      `ScreenshotFeedback` renders `null` when `lastScreenshot` is empty
      ([`ScreenshotFeedback.jsx:7-11`](src/components/ScreenshotFeedback.jsx#L7)), but the always-on-top 340×210
      window is only destroyed by the five-second timer ([`ui.rs:78-88`](src-tauri/src/ui.rs#L78)).
      *Scenario:* take a screenshot and delete it from the main panel card within five seconds. A blank dark
      rectangle remains in the corner until the timer fires. *Confidence:* medium. *Direction:* call
      `dismiss_screenshot_feedback` from `update_saved_output` when the removed path is the preview's.

---

## 1. P0 — Data safety & correctness bugs

### 1.1 Crash mid-recording = unplayable file · P0 · M
- [x] H.264 now uses fragmented MP4: headers and indexes are written during recording.
      Runtime write/finalize errors preserve partial output instead of deleting it.
- [x] Add `--kill-at T` to the debug selftest (exit code 86, without Stop or Finalize).
      Hard exit at 8 seconds retained 7.44 seconds of H.264 video / 7.79 seconds of AAC; completed fragments decoded.
      The measured file contained 24 `moof`/`mdat` pairs, with the `moov` header before them.
- [ ] **HEVC crash safety remains open.** This machine's fragmented sink rejects HEVC with `MF_E_INVALIDMEDIATYPE`
      (`0xC00D36B4`); the regular sink records HEVC successfully. Keep that working path until a crash-safe HEVC muxer
      or recoverable intermediate/remux path is available.
- [ ] Verify finalized/interrupted H.264 seeking/import in VLC, Premiere and DaVinci; repeat on other Windows/GPU builds.
      Completed fragments survive process termination; OS-buffered data is not guaranteed after power loss.
      Loss before the first complete fragment remains possible.

### 1.2 Quit path can truncate a recording · P0 · S
- [x] Tray Quit and explicit Tauri exits wait for the session callback without a deadline, and show Saving in the tooltip.
      A real-app test with seven seconds of finalization delay exited after 10.16 seconds with a decodable MP4.
- [x] A dedicated hidden top-level window handles `WM_QUERYENDSESSION` / `WM_ENDSESSION`, registers
      `ShutdownBlockReasonCreate`, and waits for finalization without blocking the UI thread.
- [x] `PBT_APMSUSPEND` stops/finalizes; resume permits new recordings with fresh capture/audio devices.
      Startup cancellation is included in the wait, and new recordings are blocked while Windows suspends/ends the session.
- [ ] Exercise actual shutdown/logoff and sleep/resume on hardware. Windows imposes its own limits;
      forced termination can still interrupt finalization (see 1.1).

### 1.3 Filename collisions overwrite files · P0 · S
- [x] `local_stamp()` has 1-second resolution ([util.rs](src-tauri/src/util.rs)). Two screenshots in the same
      second ([screenshot.rs:49](src-tauri/src/screenshot.rs:49)) silently overwrite the first — trivially reproducible by
      double-tapping `Ctrl+Shift+S`. Same for recordings ([recorder.rs:166](src-tauri/src/recorder.rs:166)) when stop→start
      within a second.
- [x] Fix: add milliseconds or a ` (2)` suffix loop with `OpenOptions::create_new(true)`; never overwrite.
      Implemented atomic reservations shared by generated screenshot and recording filenames; PNG encoding writes
      directly to the reserved handle, and MF opens its reserved placeholder. Failed screenshots/startup clean up their output;
      runtime recording failures preserve partial output.
      Validated: 8 Rust tests (including existing-file preservation, 16 concurrent reservations, missing directory),
      debug build, 8-second H.264 + system/mic selftest with pause/resume into a reserved file (247 frames, 0 dropped),
      full MP4 decode with ffmpeg, and a valid 1920×1080 PNG. Interactive playback/sync review remains in §2.

### 1.4 Audio device switch records silence · P0 · M
Implemented default-endpoint notifications, stream teardown/reopen, and retry after capture errors.
- [ ] Verify physical unplug/replug and Bluetooth switching while recording; notification routing is unit-tested.
- [x] Both WASAPI streams bind to `GetDefaultAudioEndpoint(…, eConsole)` once at start ([wasapi.rs:49](src-tauri/src/audio/wasapi.rs:49)).
      Plug in headphones / switch to Bluetooth mid-recording → loopback keeps capturing the *old* endpoint → silence for the rest
      of the file, with no indication.
- [x] Register `IMMNotificationClient::OnDefaultDeviceChanged`, tear down and reopen the stream on the new device; the mixer
      already zero-fills the gap, so timing stays correct.
- [x] Handle `AUDCLNT_E_DEVICE_INVALIDATED` from `GetBuffer`/`GetNextPacketSize` (device unplugged) the same way instead of
      exiting the thread.

### 1.5 Audio failures are invisible · P0 · S
Implemented per-source health snapshots and accessible record-bar indicators. Initial device open is the preflight;
its status is available before the recording snapshot. Denied microphone access includes a privacy-settings link.
- [x] Audio thread errors go to `eprintln!` ([wasapi.rs:39](src-tauri/src/audio/wasapi.rs:39)); release builds have no console
      (`windows_subsystem = "windows"`). A denied mic permission (Settings → Privacy → Microphone) produces a silent mic track
      and the user finds out after recording a 40-minute tutorial.
- [x] Propagate per-source status into `Snapshot` (`audio: { system: ok|failed(msg), mic: ok|failed(msg) }`) and show it in the
      record bar.
- [x] Pre-flight check on Record: open the mic briefly; if access is denied, show a deep link to `ms-settings:privacy-microphone`.

### 1.6 Hotkey registration failures are invisible · P1 · S
Registration failures now appear in Settings and remain in recorder snapshots.
- [x] Failure is only `eprintln!`'d ([main.rs:121](src-tauri/src/main.rs:121)). If another app owns `Ctrl+Shift+R` (OBS, ShareX,
      Chrome extensions), the hotkey silently does nothing. Surface in UI (settings row shows "⚠ in use by another app").

### 1.7 `Recorder::start` holds the state mutex across encoder setup · P1 · S
Implemented starting state, unlocked setup, cancellation, generation checks and a callback commit barrier.
The real-app startup-cancellation test observed Stop returning in 0 ms.
- [x] `session::start` (device + MF + WGC init, 100–300 ms) runs under `inner.lock()` ([recorder.rs:169](src-tauri/src/recorder.rs:169)).
      Meanwhile `get_recorder_state`, `set_source`, the tray and the ticker all block on the main thread → UI hitch on every Record.
      Emit a `starting` state, release the lock, init, re-lock to commit (with a generation check so a Stop during init wins).

### 1.8 Hotkey double-press race · P1 · S
Record hotkeys now ignore repeated toggles within 400 ms; boundary behavior is unit-tested.
- [x] Each `Ctrl+Shift+R` spawns a thread that calls `toggle()`. Two quick presses = start + immediate stop (a 0.2 s file), or
      worse, interleaving with 1.7. Debounce (ignore toggles within ~400 ms) and/or make toggle a single queued command.

### 1.9 Two pauses within the mixer latency mis-time audio · P2 · S
Clock now retains the full pause history and avoids latching frame zero while paused.
Tests cover late audio placement across multiple pauses, pause boundaries, and pre-roll.
- [x] `Clock` remembers only `last_pause` ([clock.rs](src-tauri/src/clock.rs)). Pause→resume→pause→resume within ~100 ms
      (hotkey spam) → late audio packets from the first pause get the wrong PTS. Keep a small ring of recent pauses (or the
      full list; it's tiny). Add a mixer unit test.

### 1.10 Settings file corruption silently resets everything · P1 · S
Implemented flushed same-directory temporary files + atomic replacement, serialized commits, unique bad-file backups
and a visible startup notice. Tests cover replacement, corrupt backups, and failed saves leaving memory unchanged.
- [x] `fs::write` is not atomic ([settings.rs:115](src-tauri/src/settings.rs:115)); a crash mid-write leaves truncated JSON,
      which `load` treats as "no settings" and silently restores defaults (output dir included).
- [x] Write to `settings.json.tmp` + `rename` (atomic on NTFS). On parse failure, back up the bad file as
      `settings.json.bad` and tell the user.

### 1.11 Output folder unavailable · P1 · S
Implemented write-probed fallback with a visible warning, a pre-start space check, and a per-tick disk guard.
- [ ] Validate live low-space and removable/network-drive behavior on hardware; fallback failure is unit-tested.
- [x] Output dir on an unplugged USB drive / dropped network share → `create_dir_all` error at Record time. Fall back to
      `Videos\Oculus` with a visible warning instead of refusing to record.
- [x] Disk-space check before start (`GetDiskFreeSpaceExW`) and during recording (every tick): warn at < 2 GB, auto-stop and
      finalize at < 200 MB instead of letting MF fail mid-write.

### 1.12 Stale handles · P1 · S
Implemented monitor device-name resolution, region bounds/size checks, and window PID + full exe path + title-prefix
validation. Invalid windows clear the selection; monitor handle changes and fallback are unit-tested.
- [x] `Source::Monitor { id }` and `Region { monitor }` store raw `HMONITOR`s, which change when displays are re-plugged,
      resolution changes, or the laptop docks. Re-resolve by device name / rect on start; if gone, fall back to primary with a
      warning.
- [x] `Source::Window { id }` is a raw `HWND`; HWNDs are recycled. Validate on start that the HWND still belongs to the same
      exe + title prefix; if not, clear the source.

### 1.13 "Window closed" is shown as an error · P2 · S
Successful target closure is now a neutral notice, separate from errors.
- [x] [recorder.rs:229](src-tauri/src/recorder.rs:229) puts a *successful* save notice in `error`, rendered red. Add a
      `notice` / `info` field (or `lastEnd: { reason }`) and render it neutrally.

---

### Latest local validation — 2026-10-28

- [x] Phase 1 startup investigation: a debug-only five-second audio-preflight readiness gate
      reproduced a writer overload with 298 rejected packets and a 128-packet queue peak,
      despite maximum sample writes below 44 ms. The writer now drains media while polling
      preflight; the same case saved 8.016667 s H.264 / 8.021333 s AAC, zero rejections and
      a two-packet queue peak, with full decoding passing. Queue capacities remain unchanged.
      Two focused preflight tests and the expanded full H.264/HEVC verifier pass (69 Rust tests,
      one existing ignored test, 17 JavaScript tests, Clippy and builds). This confirms a startup
      defect and its fix; it does not conclusively identify the earlier isolated overload.
      Artifacts: `%TEMP%\oculus-preflight-343be8a5fc0b415cb1d22c4362e8cb82` and
      `%TEMP%\oculus-verify-67fc9c61fa9c467c8be688e09224eb61`.
      The fixed eight-second clip also played to the end in mpv; forward/backward seeking
      displayed valid frames. Editor import and acoustic synchronization remain open.
- [x] Stress tooling records OS/GPU drivers and executable SHA-256, samples output size/free
      space, and fails safely below storage thresholds while retaining partial output/logs.
      The one-hour result is recorded separately once completed; physical/acoustic checks stay open.

- [x] Timing-reliability batch: sparse fragmented H.264 was reproducibly shorter than its audio
      timeline. H.264 now requires CFR with a separate bounded GPU repeat pool; HEVC permits optional VFR.
      Stop freezes the timeline before device teardown. The expanded verifier checks both sides of the
      A/V endpoint difference and Stop while paused. 63 Rust tests, 17 JavaScript tests, Clippy, frontend/
      debug builds, the complete media verifier and release/NSIS build passed. New diagnostics record
      slow sample writes, maximum write times, written/timeline endpoints and the first queue rejection.
- [x] `npm run verify:stress`: six short pause/resume repeats plus a five-minute recording passed
      decoding, seeking, complete CFR grids and endpoint checks. Long output: 17,706 frames / 295.100 s
      video / 295.104 s AAC, no rejected packets, no writes over 100 ms. Across 61 resource samples,
      warm-up-to-end mean private-memory growth was 6.89 MiB and handle change was −8.33. This headless
      CFR process used about 246–253 MiB private memory; the earlier performance baseline predates CFR.
      Artifacts: `%TEMP%\oculus-stress-4faa45abfa5f45d7a836aa7f7df61c39`.
- [x] Static-window CFR regression: one WGC frame produced 1,811 encoded frames over 30.183 s;
      AAC ended 3 ms later and full decoding passed. 72 fps, 120 fps/720p and HEVC VFR also decoded.
      Native codec-switching checks enforce H.264 CFR and preserve optional HEVC VFR. A further short
      stress run passed during release compilation and exercised incremental resource CSV writing.
      The intermittent original overload did not recur; its root cause remains unconfirmed. Acoustic
      synchronization and a one-hour soak remain unchecked below.

- [x] Backlog implementation batch: configurable transactional hotkeys (including optional mic mute,
      area recording and window-under-cursor recording), audio endpoint selection/live mute/dB gains,
      output height/custom bitrate/CBR/VBR controls, additional frame rates, diagnostics with GPU drivers
      and actual codec-filtered encoder identification, settings schema version 1 and launch-at-login.
      Source polling/minimized-window hints, settings sections/focus/scrolling and forced-colors styling
      are implemented. A high-contrast overlap defect found during review was fixed; 320×420 settings
      checks show no horizontal overflow or overflowing device rows.
- [x] Current checks: 60 Rust tests passed (one desktop Shell test ignored by the regular runner),
      17 JavaScript tests passed, Clippy clean, frontend/debug builds and full media verifier passed.
      H.264/HEVC decode, seeking, hard-exit H.264 retention, injected overload, startup cancellation
      and slow quit passed. One earlier normal run overflowed the writer queue; direct repeat and
      full repeat passed. This isolated failure remains a reliability investigation, not a resolved defect.
- [x] Additional native recordings: CBR 24 fps/720p/8 Mbps and VBR 50 fps/720p/6 Mbps decoded;
      120 fps/720p produced 487 frames over 4.063 s and decoded. Keyframes appeared at 0, 1.992 and
      4.006 s. An odd 641×479 source crop encoded to 640×478 and decoded. This is not a quality comparison.
- [x] Native diagnostics lists Windows build 26100, Intel/NVIDIA driver versions and local audio endpoints.
      Logs identify Intel Quick Sync H.264 and Intel Hardware H265 correctly. Selected audio devices,
      live mic mute/unmute, pause/stop and shortcut conflict rollback were exercised. Screenshot clipboard
      contains both image and file-drop formats; Copy file removes stale image data. Thumbnail click,
      drag-threshold/click suppression and keyboard Open passed with intercepted Shell actions.
- [x] All-monitor area-selector implementation uses coordinated native workers, adjustable handles,
      window snapping, crosshairs, physical-pixel coordinates and DPI-scaled measured labels. Native
      single-display drag, corner resizing from 200×170 to 221×183, and right-click cancellation passed.
      Keyboard geometry/coordinator behavior is unit-tested. Interactive keyboard confirmation,
      multi-monitor/mixed-DPI behavior and actual cross-app file drops still need validation.
- [x] CI/security workflows, dependency updates, version synchronization, pinned toolchain, formatting
      baseline and contributor/privacy documentation are present. npm audit found no vulnerabilities;
      cargo audit found no vulnerabilities but reported informational upstream warnings for packages
      outside this Windows target's dependency tree. Workflows have not run on GitHub yet.
- [ ] Investigate the isolated normal-recording writer overload above; repeat under controlled GPU/load conditions.
- [x] Current optimized release and NSIS installer rebuilt successfully (1.77 MiB). Installation,
      uninstall and upgrade lifecycle validation remain open. CI/repository YAML files parse successfully;
      local syntax validation does not establish GitHub runner success.

- [x] Webview policy batch: removed broad core permissions in favor of event listen/unlisten, titlebar drag/
      double-click/minimize and folder selection. Production styles must come from bundled stylesheets; Vite's
      inline-style and localhost hot-reload exceptions are confined to `devCsp`. Nine JavaScript tests and the
      frontend build pass, including policy regression checks. All 39 regular Rust tests pass as well.
      Release/NSIS rebuild succeeds (1.68 MiB), including the preceding saved-file security changes.
      Added architecture documentation, now consolidated in OCULUS_PROJECT_GUIDE.md, with pipeline,
      threading/lifetime/timing rules and panic policy.
- [ ] Recheck live subscription, drag/minimize and output-folder selection with the tightened webview policy.
      Desktop automation discovery timed out after retry and helper reinitialization in this batch.

- [x] Saved-file security batch: canonical output-folder checks reject outside paths, sibling-prefix paths,
      directories and missing files. Native Shell item parsing accepts nested Unicode/comma filenames;
      extended disk/UNC prefixes are normalized for the Shell. The interactive Explorer API check passes.
      39 regular Rust tests, six JavaScript tests, Clippy and frontend/debug builds pass; the Explorer test is opt-in
      for desktop sessions and was also run successfully on this machine.

- [x] Tray/timer batch: 36 Rust tests, six JavaScript tests, Clippy and frontend/debug/release builds pass.
      An eight-second system/mic recording with pause/resume decoded, and the real-app seven-second delayed
      finalization check exited successfully with decodable output. The release UI timer stayed at 00:04 while
      paused, resumed counting and saved successfully. NSIS rebuilt at 1.68 MiB. Tray artwork across taskbar
      themes/scales still needs the visual review listed in §8.

- [x] Bounded-queue/audio-tail batch: 35 Rust tests and three JavaScript tests pass, with Clippy clean. The extended
      `npm run verify -- -IncludeHevc` passes a writer-stall overload check, media decoding/seeking, audio-tail checks,
      crash retention, startup cancellation and slow quit. An additional mixer-stall check hit the 64-packet input cap
      and retained decodable output. Debug `--stall-writer-ms` / `--stall-mixer-ms` flags reproduce backpressure.
      Video-only recording also decoded with exactly one video stream and no audio stream. Release/NSIS was rebuilt
      with this batch (1.68 MiB).

- [x] Earlier reliability/UI batch: 26 Rust tests, three JavaScript settings-queue tests, Clippy, frontend/debug builds,
      H.264/HEVC + audio decoding/seeking, interrupted H.264, startup cancellation, and delayed-finalization quit.
- [x] Actual UI: settings layout, keyboard checkbox focus/toggle, slider key adjustment and restore, Starting/Recording/
      Paused/Saving/Saved, global resume/stop hotkeys, and the compact audio-status indicator.
      The UI-created file decoded; H.264 duration was 11.197 s and AAC was 11.200 s. This is a duration check, not an acoustic sync test.
- [x] Keyboard source-tab navigation; inactive tabs retain their panel targets. Recording state changes are announced
      separately from the timer so screen readers do not announce every elapsed second.
- [x] Second-launch check during seven-second finalization delay: second process exited, primary continued saving,
      and its recording decoded. Panic-hook driver produced a fresh report and exited with code 101.
- [x] Release + NSIS build: `Oculus_0.1.0_x64-setup.exe`, 1.68 MiB, with WebView2 download bootstrapper enabled.
      The release app opened its embedded `http://tauri.localhost/` interface successfully. Installation, uninstall
      and upgrade checks remain open in §14.

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
- [x] **cargo clippy -D warnings** (install clippy: `rustup component add clippy`). P1 · S
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
      Encoder default/CBR/peak-constrained VBR and mean/peak bitrate controls are implemented and
      produced decodable files. Controlled screen-content size/quality comparisons remain.
- [x] **GOP / keyframe interval.** Set `CODECAPI_AVEncMPVGOPSize` = 2 × fps. Default GOPs can be huge → editors seek slowly,
      and a corrupted fragment loses more. S
- [ ] **B-frames.** Decide `CODECAPI_AVEncMPVDefaultBPictureCount` (B-frames help compression; some editors scrub worse). S
- [ ] **Faststart.** `MF_MPEG4SINK_MOOV_BEFORE_MDAT` so files stream in browsers/Discord before full download. Interacts with 1.1. S
- [ ] **Content type hint.** `CODECAPI_AVEncVideoContentType` / screen-content tuning where vendors support it (sharper text). 🔍 S
- [x] **Report the actual encoder** (vendor MFT name via `MFT_FRIENDLY_NAME_Attribute`) in logs and the About panel —
      "NVIDIA H.264 Encoder MFT" vs "H264 Encoder MFT" (software) is the first thing to ask in a bug report. S
- [ ] **AV1** (RTX 40 / Arc / RDNA3+): add when the encoder enumerates; hide the option otherwise. P2 · M
- [ ] **Lossless / near-lossless** option for tutorials with tiny text (high bpp HEVC or QP mode). P3 · M
- [ ] **Bitrate model sanity.** `bpp` × pixels × fps ([settings.rs](src-tauri/src/settings.rs)) doubles the bitrate when going
      30→60 fps; perceptually you need ~1.3–1.5×. Revisit with a measured table (resolution × fps × quality → Mbps). S 🔍
- [x] Expose **custom bitrate** (Mbps field) in an "Advanced" section. P2 · S

### 3.2 Frame rate · P1
- [ ] Settings only allow 30/60 ([settings.rs:78](src-tauri/src/settings.rs:78) collapses anything else). Add 24, 50, 120,
      144, and "Match display". Check encoder level limits at 4K120. M
      Implemented 24/30/48/50/60/72/120. 144/Match display and high-resolution hardware checks remain.
- [ ] **Judder on non-divisible refresh**: 144 Hz → 60 fps alternates 13.9/20.8 ms frame gaps. Offer 72 / 48 fps on 144 Hz
      panels or document. Optional: resample to an exact CFR grid by duplicating/dropping with PTS snapped to the grid. 🔍 M
- [x] **Constant frame rate mode**: emit a complete integer frame grid with GPU surface repeats.
      CFR is now the default and required for fragmented H.264: native sparse-frame tests exposed
      accumulated timing loss at fragment boundaries. HEVC can optionally retain VFR. Capture and
      repeat pools are independently capped at 12 surfaces; repeat-pool wait/catch-up are bounded.
- [ ] Restore H.264 VFR only with a muxing path that preserves sparse durations across fragments;
      accurate input durations alone did not fix the tested Windows fragmented sink.
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
- [x] **Output resolution setting** (see above): smaller files for sharing. S once the scaler is trusted.
- [ ] **Cursor**: highlight ring, click visualization (left/right ripple), cursor size boost for tutorials. Requires drawing
      the cursor ourselves (disable WGC cursor, read `GetCursorInfo` + cursor bitmap, composite on GPU). P2 · L
- [ ] **Keystroke overlay** for tutorials (P3, low-level keyboard hook, privacy toggle). L
- [x] **Window capture border**: `SetIsBorderRequired(false)` is ignored on older builds → yellow border. Document. S
- [x] **Window capture of child/popup menus**: WGC window capture omits context menus and tooltips (separate HWNDs).
      Document, or offer "capture window area of the screen" mode (region tracking the window rect). M
- [ ] **Multi-monitor "all displays"** source (capture each monitor, composite into one canvas). P3 · L

### 3.4 Robustness · P1
- [ ] **Device removed / driver reset (TDR)**: `DXGI_ERROR_DEVICE_REMOVED` on Blt or WriteSample currently fails the session
      somewhere in a callback (`eprintln!` in the WGC callback, [session.rs](src-tauri/src/session.rs)). Detect, finalize what we have, report. M
- [x] **Blt / pool errors in the WGC callback** end the session after five consecutive failures, with an error and
      partial-file retention. The first and fifth failures are logged; the stop threshold has a regression test. S
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
- [x] **Device selection**: dropdowns for output (loopback source) and input (mic) with "Default" option that follows
      default-device changes (see 1.4). Enumerate with `IMMDeviceEnumerator::EnumAudioEndpoints`. M
- [ ] **Mic level meter** in the panel and record bar (peak/RMS from the mixer, sent at ~15 Hz via a separate event,
      *not* the state snapshot). Users must see the mic works before recording. M
- [x] **Mute toggles while recording** (mic mute especially), independent of pause. S
- [ ] **Separate audio tracks**: system and mic as two AAC streams (plus optional mixed track) for editing. MP4 supports it;
      most editors import multi-track. M
- [ ] **Per-app audio** for window capture: `ActivateAudioInterfaceAsync` with `AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS`
      (Win10 2004+) → record only the captured app's audio (include process tree). Killer feature vs Snipping Tool. L
- [ ] **Limiter instead of hard clip**: mixer does `clamp(-1, 1)` → harsh distortion when system + mic sum > 0 dBFS at gain 2×.
      Add a simple look-ahead limiter or soft-knee compressor on the mix bus. S
- [x] **Gain in dB** in the UI (slider 0–2 linear is unintuitive; −∞…+6 dB with a 0 dB detent). S
- [ ] **Drift correction**: the mixer realigns by zero-filling/dropping beyond 20 ms → can produce an audible click per
      correction. After the 15-min test (§2), if corrections happen regularly, replace with gradual resampling
      (adjust `AUTOCONVERTPCM` isn't controllable → small linear-interp resampler with ±0.1 % ratio). 🔍 M
- [x] **Audio ends ~90 ms before video**: fixed a disconnected-input path that skipped the mixer final flush, and
      included the sub-10 ms tail in that flush. Finalization pads any remaining audio gap to the end of the last
      successfully written video sample, with at most one second of padding allocated at once. S
      Regression tests cover disconnected input, sub-chunk tails, and sample rounding. The verifier rejects finalized
      recordings whose audio ends more than 40 ms before video; acoustic sync/drift still needs the manual tests in §2.
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
- [x] **Minimized windows** are filtered out (`IsIconic`, [sources.rs](src-tauri/src/sources.rs)) → user can't find the window they
      want. List them greyed with "minimized — restore to record" or auto-restore on start. P1 · S
- [ ] **Group windows by app**; sort by last-focused (Z-order) instead of enumeration order. P2 · S
- [ ] **Search**: also match on process name without `.exe`, fuzzy match, keyboard navigation (↑/↓/Enter). P2 · S
- [x] **Live refresh**: windows list goes stale while the panel is focused (only refreshes on `focus`). Use
      `SetWinEventHook(EVENT_OBJECT_CREATE/DESTROY/NAMECHANGE)` → push `sources://changed`. P2 · M
- [ ] **Remember last source** across restarts (by monitor device name / window exe+title), not raw handles. P1 · S
- [ ] **"Pick window by clicking"** mode (crosshair, hover-highlight windows, click to select). P2 · M
- [ ] Exclude more junk windows: "Program Manager", cloaked Settings host, `ApplicationFrameHost` wrappers, NVIDIA overlay. 🔍 P2 · S
- [ ] Region presets: 1920×1080, 1280×720, 1080×1080, 1080×1920 (shorts), "last region". P2 · S

---

## 6. Region overlay ([overlay/region.rs](src-tauri/src/overlay/region.rs))

- [x] **Only the monitor under the cursor** can be selected; when you click "Select area" in the panel the cursor is over
      *Oculus*, so you always get Oculus's monitor. Show the overlay on **all monitors** at once (one popup per monitor or one
      spanning window) and let the drag happen on any. P1 · M
- [ ] **Window snapping**: hover highlights the window under the cursor; click (no drag) selects its rect. Same for UI
      elements via UI Automation (P3). P1 · M
      Window-rectangle snapping is implemented; UI-element snapping and interaction validation remain.
- [x] **Adjust after drag**: resize handles on edges/corners, drag to move, Enter to confirm, instead of committing on
      mouse-up. P1 · M
- [x] **Keyboard**: arrows nudge 1 px (Shift = 10 px), Alt+arrows resize, Enter confirm, Esc cancel, Space toggles move mode. P2 · S
- [ ] **Magnifier loupe** near the cursor with pixel grid + RGB/hex readout (doubles as a color picker). P2 · M
- [x] **Crosshair guides** across the full monitor. P2 · S
- [x] **Size label**: fixed 120 px width ([region.rs:213](src-tauri/src/overlay/region.rs:213)) clips long text and runs off the right
      edge near the screen border. Measure text with DirectWrite, clamp inside the monitor. P2 · S
- [ ] **Aspect lock** (hold Shift = 1:1, or preset ratios 16:9/9:16/4:3). P2 · S
      Shift-square drawing is implemented and geometry-tested; ratio presets remain.
- [ ] **Show the recorded region during recording**: a thin accent frame around the area (separate layered window excluded
      from capture via `WDA_EXCLUDEFROMCAPTURE`). Without it the user forgets where the edges are. P1 · M
- [ ] **Allow moving the region while recording** (P3; converter crop is mutable, needs a lock).
- [ ] Freeze-frame for *recording* region selection is fine, but add a "live" option (no freeze) for picking areas over video. P3 · S
- [x] Overlay above exclusive-fullscreen games doesn't work (by design); document. S
- [ ] Re-use the session `Gpu` instead of `Gpu::new()` per selection ([region.rs:67](src-tauri/src/overlay/region.rs:67)); see §11. S
- [x] HiDPI: label font is 14 DIP at 96 DPI = tiny on a 200 % display. Scale font by the monitor's DPI. P2 · S

---

## 7. Screenshots

- [x] **Screenshot feedback**: successful saves show a floating thumbnail at the bottom-right for five seconds,
      including when the panel is closed. Click to open; dismiss early with the close button. New captures refresh the
      lifetime, and the preview is excluded from capture. Native thumbnail/render/expiry and panel-closed capture checks passed. P1 · M
- [ ] Add optional shutter sound and drag-out from the screenshot preview to other apps. P3 · M
      Optional synthesized shutter sound and native Shell file dragging are implemented. Drag expiry is held
      and resumed safely. Native cross-app drop verification remains; pointer/click/keyboard behavior passed.
- [ ] **Image formats**: PNG only. Add JPEG (quality) and WebP; lossless WebP is ~30 % smaller than PNG for screen content. P2 · S
- [ ] **PNG encode speed**: `image::save_buffer` default compression on a 4K frame is slow (~200–400 ms 🔍). Use
      `PngEncoder::new_with_quality(Fast, Adaptive)` and do the encode off the hotkey path (clipboard first, file second). P2 · S
      Fast compression with adaptive filtering is implemented; benchmark 4K cost and revisit clipboard latency.
- [ ] **Window screenshot with transparent rounded corners/shadow** (alpha from DWM) as an option. P3 · M
- [ ] **Delay timer** (3/5/10 s) for capturing menus/hover states. P2 · S
- [ ] **Annotate after capture** (arrow, rectangle, highlight, blur/pixelate for secrets, text, crop). Large feature; native
      D2D editor or a second webview window. P3 · L
- [ ] **Scrolling capture**. P3 · L
- [ ] **OCR to clipboard** (`Windows.Media.Ocr`, built into Windows, zero dependencies). P3 · M
- [ ] Device per screenshot: `screenshot::capture` creates a new D3D device every time ([screenshot.rs:14](src-tauri/src/screenshot.rs:14)). Cache one (§11). S
- [x] Screenshot of a **window** source uses WGC window capture — no cursor/menus; fine, document. S
- [ ] **HDR screenshots** washed out (same as 3.3). Save as JXR/HDR PNG or tone-map. P3 · M
- [x] Clipboard: also put the file path as `CF_HDROP` so paste into Explorer/Slack drops the file. P2 · S

---

## 8. UI (visual)

- [x] **Record button states**: the compact bar shows Starting… during encoder/capture setup; pause is disabled,
      and Stop cancels startup. The backend rejects duplicate starts. P1 · S
- [ ] **Countdown** 3-2-1 overlay before recording starts (optional setting). P2 · M
- [ ] **Record bar** ([RecordBar.jsx](src/components/RecordBar.jsx)):
  - [x] Shows "0 fps" for the first second (ticker hasn't run). Hide fps until first tick. S
  - [x] Elapsed time interpolates from the accepted snapshot's `elapsedMs` and `performance.now()` anchor every 100 ms.
        Paused/saving states do not extrapolate; stale events/hydration cannot replace the anchor. S
  - [ ] Mic mute button, mic level meter, file size so far, disk space warning. M
  - [x] Show audio failure badges (1.5). S
  - [ ] Collapse to a minimal dot/timer (draggable) option. P3 · S
  - [ ] Remember bar position per monitor; currently always docks top-center. S
- [x] **Recording indicator in the tray icon**: red while recording, amber with pause bars while paused, grey during
      starting/saving, and the application icon when idle. Tooltips include state and elapsed time. Updates run on the UI
      thread with a sequence guard; recording locks never wait on native tray setters. P1 · S
- [x] **Post-recording card**: after stop, show filename, duration, size, thumbnail, buttons: Open, Show in folder, Copy
      file, Delete, Rename. Screenshot saves use the same card. Duration comes from the last encoded video sample;
      missing shell thumbnails fall back to a media icon. Partial recordings are labeled explicitly. Rename preserves
      extensions and cannot overwrite an existing file; permanent Delete requires confirmation in the panel. Native
      screenshot/video thumbnails, rename, file-drop clipboard and pause/resume/save checks passed. P1 · M
- [x] **Recent recordings list** (last 10 with thumbnails) in the panel. Rust keeps a bounded,
      newest-first list of completed/partial recordings for the current app session, including
      across tray WebView recreation. Expand it for thumbnails and existing file actions;
      rename/delete update every reference. Screenshots do not evict recordings. P2 · M
      Validation (2026-10-28): four history regression tests; 67 Rust tests passed, one existing
      Explorer test ignored; 17 JavaScript tests passed; formatting, Clippy, frontend and debug
      desktop builds passed. Three short H.264/system-audio UI recordings decoded; pause/resume,
      newest-first ordering, thumbnails, expanded layout and tray recreation checked locally.
- [ ] Persist recent recordings across app restarts, with validation of restored paths. P2 · M
- [x] **Settings drawer**: no section headings, no "back" affordance (gear toggles), long — split into sections
      (Recording / Audio / Screenshots / Hotkeys / About). P2 · S
- [x] **Gain sliders** show no value; add dB readout and double-click to reset. S
- [ ] **Icons**: hand-drawn set ([Icons.jsx](src/components/Icons.jsx)) — audit sizes/stroke consistency at 1×, 1.25×, 1.5×, 2×. S
- [ ] **App icon / tray icon**: verify crispness at 16/20/24/32 px (tray uses `default_window_icon`). Design a monochrome tray
      variant for light taskbar. P1 · S
- [ ] **Light theme** following `prefers-color-scheme` (WebView2 passes the system theme) — or explicitly dark-only, decide. P3 · M
- [ ] **Window resize**: fixed 400×640; on a 1366×768 laptop at 125 % that's tall. Test small screens; allow a compact
      height or scroll. P2 · S
- [ ] **Animations**: panel ↔ record bar transition is an instant resize; consider a short fade. Respect
      `prefers-reduced-motion` for the pulsing dot. P3 · S
- [x] **Empty states**: no monitors (impossible-ish), no windows ("Open the app you want to record"), unsupported WGC. S
- [x] **Error presentation**: long error strings in `.status.err` wrap and push layout; use a dismissible banner with
      "Copy details". P2 · S
      Shared banners bound and scroll long details, support text selection and copy-failure guidance, and keep saved-file
      access visible. Action failures include context. Browser checks covered long messages, copy success/failure,
      dismissal and repeated errors; native WebView2 copying was verified against the Windows clipboard.
- [ ] **Fonts**: `Segoe UI Variable` only on Win11; verify Win10 fallback metrics. S
- [ ] **Mica / acrylic** background on Win11 (`window-vibrancy` crate) — P3, measure memory cost first.

---

## 9. UX flows

- [ ] **First-run onboarding**: one screen — pick default source, test mic (meter), show hotkeys, choose output folder. P2 · M
- [x] **Hotkeys are hard-coded** (`Ctrl+Shift+R/P/S/A` in [main.rs](src-tauri/src/main.rs)). Make them rebindable in settings with
      editable shortcut text, conflict detection, and "disable". P1 · M
- [ ] Add press-to-capture shortcut fields, temporarily suspending existing actions while a binding is captured.
- [ ] Add hotkeys: mic mute, region record (select area → start immediately), window under cursor record, cancel recording
      (stop + delete). P2 · S
- [x] **"Record area" one-shot**: select region → recording starts (today: select, then press Record). P1 · S
- [ ] **Discard recording** button/hotkey (stop and delete, with undo toast). P2 · S
- [ ] **Max duration / auto-stop timer** and **scheduled stop**. P3 · S
- [ ] **Replay buffer** ("save last 30 s"), à la ShadowPlay: ring of encoded GOPs in memory. P3 · L
- [ ] **Trim after recording** (lossless keyframe trim via MF remux; precise trim needs re-encode of the edges). P2 · L
- [ ] **Export GIF / short MP4 for chat** (size-targeted). P3 · M
- [ ] **File naming template** (`{date} {time} {source}`), with the window title in the filename for window recordings. P2 · S
- [ ] **Open folder after save** / **copy file to clipboard after save** options. P2 · S
- [x] **Launch at login** (`tauri-plugin-autostart`), start minimized to tray. P1 · S
- [x] **Single instance** (`tauri-plugin-single-instance`): launching Oculus twice creates two trays and the second can't
      register hotkeys. Second launch should focus the first. P1 · S
- [ ] **Close button semantics**: titlebar X hides to tray with no explanation the first time. Show a one-time toast
      "Oculus is still running in the tray". P2 · S
- [ ] **Drag & drop** last recording out of the panel into Discord/Slack (`startDrag` via a Tauri plugin). P3 · M
- [x] **Changing source while recording** is impossible (fine) — make that explicit (disable picker, tooltip). S
- [ ] **Pause indicator** in the video? (Some users want a visible cut marker.) P3.

---

## 10. Accessibility

- [x] **Toggles are not keyboard-accessible**: `.toggle input { display: none; }` ([theme.css:99](src/styles/theme.css:99)) removes
      the checkbox from the tab order. Use a visually-hidden pattern (`opacity: 0; position: absolute`) and style
      `:focus-visible + .switch`. P1 · S
- [x] **Tabs**: `role="tablist"` without `aria-controls`, `tabpanel`, or arrow-key navigation (roving tabindex). P2 · S
- [x] **Segmented controls**: should be `role="radiogroup"` with `aria-checked`, arrow keys. P2 · S
- [x] **Range sliders** need `aria-label`/`aria-valuetext` ("+3 dB"). S
- [x] **Icon-only buttons** (camera, crop, gear, minimize, close, pause, stop) rely on `title`; add `aria-label`. S
- [x] **Record bar**: announce state changes via `aria-live="polite"` (Recording / Paused / Saving / Saved). S
- [ ] **Contrast**: `--muted #8a8f9c` on `--surface #16181d` ≈ 5.6:1 OK; `.badge` 10 px uppercase muted on surface-2 — check
      ≥ 4.5:1 and size. `button:disabled { opacity: .4 }` drops below 3:1. P2 · S
- [x] **Focus order** after switching settings ↔ picker; focus first control. S
- [ ] **Region overlay** is mouse-only (see §6 keyboard) and invisible to screen readers (fine, but Esc must always work). S
- [x] Windows **high-contrast mode** (`forced-colors: active`) styles. P3 · S
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
- [x] **Writer thread `recv_timeout(50ms)`** keep-alive granularity — fine; document.
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

- [x] **Logging**: release builds have nowhere to log. Add a rotating file log (`%LOCALAPPDATA%\Oculus\logs`, 5 × 1 MB) via
      `tracing` + a size-bounded writer; production diagnostics use structured tracing. The debug CLI intentionally keeps console output. P1 · M
      Implemented five 1 MiB log files plus a bounded emergency panic file; rotation/reopening tests pass.
- [x] **Panic diagnostics**: the panic hook writes the message and forced backtrace to `panic.log` before termination.
      Verified with the debug `--selftest --panic` driver (exit 101 and a fresh report). P1 · M
- [ ] **Minidumps**: add `minidumper`/WER local dumps; any upload must be opt-in. P1 · M
- [x] **Diagnostics page** (Settings → About): app version, Windows build, GPU(s) + driver versions, encoder MFT names,
      WGC features supported (border, `MinUpdateInterval`), audio devices, "Copy diagnostics" button. P2 · M
- [x] **Session stats in the log**: arrived / encoded / dropped / keep-alives / audio corrections / max writer queue depth. S
      Also records audio zero-fill/overlap frame counts, audio input queue peak, and rejected packet counts. Queue peaks
      are conservative high-water marks capped at the queue capacities.
- [x] **Bound recording queues**: writer media queue is capped at 128 packets and WASAPI input at 64 packets.
      Producers use nonblocking sends; any full-queue rejection ends recording with an overload error and retains partial
      output. Stop/capture errors use a separate one-slot control channel, so media backpressure cannot block Stop. P2 · S
      Two-second writer/mixer stall tests hit the respective caps, reported failures, and left decodable partial files.
- [x] **`.unwrap()` on mutexes** everywhere: a panic while holding a lock poisons it → every later call panics → abort.
      Acceptable with `panic = "abort"`, but document the decision. S
      Documented the current fail-stop behavior and debug/release distinction in OCULUS_PROJECT_GUIDE.md; there is no
      attempt to recover a possibly inconsistent active session from a poisoned lock.
- [x] **Selftest in CI** can't run headless (needs a desktop session) — keep it as a manual pre-release checklist; script the
      checks (ffprobe asserts) into one `npm run verify` command. P2 · M

---

## 13. Security & privacy

- [x] **Capture exclusion has a debug escape hatch** (`OCULUS_SHOW_IN_CAPTURE`) gated on `debug_assertions` — confirm it is
      compiled out of release (it is via `cfg!`; add a test). S
- [x] **`reveal` command** takes an arbitrary path from the webview and passes it to `explorer /select,` ([commands.rs](src-tauri/src/commands.rs)).
      Only `is_file()` is checked. Restrict to files under the output dir (canonicalize + prefix check). Low risk (CSP, no
      remote content), but cheap. P2 · S
- [x] **Explorer argument injection**: `format!("/select,{path}")` with a path containing `,` or quotes. Quote properly or use
      `SHOpenFolderAndSelectItems`. P2 · S
      Implemented native Shell selection on a dedicated STA thread, with scoped COM/PIDL cleanup. File paths must
      resolve inside the current output directory; changing that directory intentionally prevents opening older
      saved-file links through this command. No Explorer command line is constructed.
- [x] **CSP** is tight (`default-src 'self'`); keep it. `style-src 'unsafe-inline'` only needed if inline styles are used —
      check and drop. S
      Source audit found no inline style usage. Production allows bundled styles and local Tauri IPC; objects,
      frames and form submission are blocked. The development policy separately permits Vite styles/HMR.
- [x] **Capabilities**: `core:default` is broad; enumerate the exact permissions used. P2 · S
      Explicit event listen/unlisten, native titlebar drag/double-click/minimize and folder-open permissions only.
      No event emit, image, menu, tray, resource or general window-query permissions are granted to the webview.
- [x] **Privacy defaults**: mic off by default ✅; add a visible "mic is on" indicator while recording. S
- [ ] **Excluded windows list**: let users exclude password managers / chat apps from monitor capture (WGC can't exclude
      other apps' windows directly; would require compositing — document as limitation, or P3 L).
- [ ] **Code signing** (Authenticode, ideally EV or Azure Trusted Signing): unsigned exe + global hotkeys + screen capture =
      SmartScreen and AV heuristics flags. P1 · M (cost $)
      Deferred: the owner confirmed personal use only. This is not a blocker for local use.

---

## 14. Packaging, distribution, updates

- [ ] Build NSIS installer; verify Start Menu shortcut, uninstaller removes `%APPDATA%\dev.gloria.oculus` (optional
      checkbox), upgrade preserves settings. P1 · S
- [x] **WebView2 install mode is `skip`** ([tauri.conf.json](src-tauri/tauri.conf.json)): on Windows 10 LTSC / Server / stripped
      images without WebView2 the app won't open the UI. Use `downloadBootstrapper` (tiny) or `embedBootstrapper`. P1 · S
- [ ] **Auto-update** via `tauri-plugin-updater` + signed update manifest on GitHub Releases. P2 · M
- [x] **Versioning**: 0.1.0 everywhere; single source of truth (`tauri.conf.json` ↔ `Cargo.toml` ↔ `package.json`), changelog. S
- [ ] **winget manifest**, Scoop bucket. P3 · S
- [ ] **Portable build** (zip, settings next to exe if `portable.txt` exists). P3 · S
- [ ] **ARM64 build** (Windows on Snapdragon): WGC + MF work; test the Qualcomm encoder MFT. P3 · M
- [x] **Minimum OS**: declare Windows 10 2004 (19041) or Windows 11 only; check `GraphicsCaptureSession::IsSupported` and
      fail with a clear message. S
- [ ] Remove unused icon files from the working tree (ignored by git but still lying around in `src-tauri/icons`). S

---

## 15. Code quality, architecture, tooling

### 15.1 Frontend
- [x] **`useSettings` race** ([useSettings.js](src/hooks/useSettings.js)): two quick toggles → response for #1 overwrites
      optimistic #2 (flicker), and a failed #1 rolls back to a snapshot that predates #2. Serialize writes (queue) or keep a
      request counter and only adopt the latest response; roll back with a functional update. P1 · S
- [x] **Range sliders write to disk on every `input` event** ([SettingsDrawer.jsx:56](src/components/SettingsDrawer.jsx:56)):
      dragging = dozens of IPC calls + `fs::write`s per second. Keep a local draft value while dragging; commit on
      `pointerup`/`change` (or debounce 150 ms). P1 · S
- [x] `useSources` refetch on every window `focus` runs `EnumWindows` + `OpenProcess` per window each time; fine, but debounce
      (alt-tab spam). S
      Focus events debounce for 200 ms; manual refresh stays immediate and cancels pending focus work. Old responses
      cannot replace newer results, unmount cancels pending work, and refresh failures retain the previous list and show
      a banner. Three focused regression tests and a browser focus-burst/manual-refresh check passed.
- [x] Handle rejected source, stop, pause and output-folder requests at their UI call sites with visible errors. S
- [x] Screenshot and region-selection failures now appear in the UI, including failures before the backend save callback. S
- [ ] **TypeScript migration** (planned): start with `api.ts` typed against the Rust types; generate types with `ts-rs` or
      `specta` (+ `tauri-specta` for typed commands) so Rust ↔ TS can't drift. P2 · M
- [ ] ESLint (`eslint-plugin-react-hooks` with the React 19 compiler rules) + Prettier. S
- [ ] **React Compiler** (babel plugin) — measure; the UI is tiny, mostly for hygiene. P3 · S
- [ ] Component tests (Vitest + Testing Library) for `useRecorder` ordering (`seq`), `useSettings` race, `formatElapsed`. P2 · M

### 15.2 Backend
- [x] **Clippy clean** with `--all-targets -- -D warnings`. S
- [x] Add `rustfmt.toml` and establish a formatting baseline. S
- [x] **Unit tests** beyond the mixer: `Clock::to_pts` (pause math incl. multi-pause), `encoder_dims`, `fit`, `sel_rect`,
      `Settings::sanitized`, `Source` serde shape. S
- [ ] **Integration tests**: MF writer with synthetic NV12 + PCM → ffprobe assertions (needs GPU; mark `#[ignore]`). M
- [ ] **Split `session::run`** (~150 lines) into setup / capture wiring / audio wiring / loop / teardown functions. S
- [x] **Settings schema versioning** (`version: 1`) for future migrations (e.g. hotkeys). S
- [x] **Error enum**: add context (`"opening mic: {e}"`) — raw HRESULT messages ("The parameter is incorrect.") are useless to
      users. Map common HRESULTs (`E_ACCESSDENIED`, `MF_E_TRANSFORM_NOT_POSSIBLE_FOR_CURRENT_MEDIATYPE_COMBINATION`,
      `DXGI_ERROR_UNSUPPORTED`) to human text. P1 · M
- [x] Replace `Mta<T>` blanket `unsafe impl Send/Sync` with narrower wrappers per interface (document why each is safe). P3 · S
- [ ] `selftest.rs` is debug-only; promote the pure assertions to tests, keep the driver as an `examples/` binary. P3 · S

### 15.3 Repo & CI
- [x] **GitHub Actions**: `windows-latest` → `npm ci`, `vite build`, `cargo fmt --check`, `cargo clippy -D warnings`,
      `cargo test`, `tauri build`, upload installer artifact. P1 · M
- [x] **LICENSE** (the repo is public with no license = all rights reserved by default). Decide MIT/Apache-2.0/GPL. P1 · S
- [x] Issue templates (bug report asks for diagnostics from §12), contributor instructions and change history
      consolidated in OCULUS_PROJECT_GUIDE.md. P3 · S
- [x] Dependabot / `cargo audit` / `npm audit` in CI. P2 · S
- [x] Pin Rust toolchain (`rust-toolchain.toml`). S

---

## 16. Documentation

- [x] README: screenshots/GIF of the UI, feature list, system requirements, known limitations (DRM black, exclusive
      fullscreen, HDR, menus in window capture), hotkey table, FAQ ("why is my file VFR?"). P1 · S
- [x] Architecture in `OCULUS_PROJECT_GUIDE.md`: pipeline diagram, threading model (WGC callback thread / writer / audio ×2 / mixer / ticker), the
      single-device rule, timing rules, why webview is destroyed on hide. Mostly lift from the plan. P2 · S
- [ ] Inline docs for the non-obvious invariants already explained in chat but not in code: tracked-sample pool lifetime,
      clock latch, `seq` ordering. Most are there; audit. S
- [x] Privacy statement (no network, no telemetry) — a selling point; say it. S

---

## 17. Feature backlog (post-1.0, P3)

- [ ] Webcam overlay (Media Foundation source reader → GPU composite into the NV12 output, draggable bubble).
- [ ] Scene composition: monitor + webcam + image/text overlays (scope creep warning: this is OBS).
- [ ] Live drawing/annotation on screen while recording (pen, highlighter, spotlight, zoom-in on cursor).
- [ ] Auto-zoom on clicks (Screen Studio style) as a post-process.
- [ ] Blur regions / auto-blur detected secrets.
- [ ] Upload / share link integrations (S3, custom endpoint) — conflicts with "no network"; opt-in only.
- [x] Streaming (RTMP) — out of scope; say so in README.
- [ ] CLI (`oculus record --region … --out …`) for automation; selftest already proves the pipeline can run headless.
- [x] Linux/macOS — no: the whole capture/encode stack is Windows-native by design. Document as a non-goal.

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
