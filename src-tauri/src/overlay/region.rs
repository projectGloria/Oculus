//! Snipping-Tool-style region picker: freeze the monitor under the cursor, show it in an opaque
//! borderless popup, let the user drag a rect. Native Win32 + D2D on the capture device: no
//! webview, no WS_EX_LAYERED alpha games, no CPU readback of the frozen frame.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use windows::core::{w, Interface};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::ValidateRect;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_MENU, VK_RETURN, VK_RIGHT,
    VK_SHIFT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::capture::gpu::Gpu;
use crate::capture::wgc::{self, Target};
use crate::error::{msg, Result};
use crate::sources::{monitor_at_cursor, monitor_info};
use crate::util::{com_init, Mta};

pub struct Pick {
    pub monitor: isize,
    /// Monitor-relative physical pixels. Video output dimensions are aligned separately.
    pub rect: RECT,
    /// The frozen frame, so a region *screenshot* is exactly what the user saw.
    pub frozen: Mta<(Gpu, ID3D11Texture2D)>,
}

const MIN_SIZE: i32 = 8;
static SELECTING: AtomicBool = AtomicBool::new(false);

pub fn is_selecting() -> bool {
    SELECTING.load(Ordering::Acquire)
}
struct SelectionGuard;
impl Drop for SelectionGuard {
    fn drop(&mut self) {
        SELECTING.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
enum EditKind {
    Draw,
    Move,
    Resize(u8),
}
#[derive(Clone, Copy)]
struct Edit {
    kind: EditKind,
    anchor: POINT,
    original: RECT,
}

struct State {
    dc: ID2D1DeviceContext,
    swap: IDXGISwapChain1,
    frozen: ID2D1Bitmap1,
    dim: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
    label_bg: ID2D1SolidColorBrush,
    text: ID2D1SolidColorBrush,
    font: IDWriteTextFormat,
    write: IDWriteFactory,
    scale: f32,
    w: f32,
    h: f32,
    drag: Option<(POINT, POINT)>,
    edit: Option<Edit>,
    cursor: POINT,
    move_mode: bool,
    snap_rects: Vec<RECT>,
    result: Option<RECT>,
}

thread_local! {
    // The wndproc can't capture; one overlay per thread at a time, so a thread-local is enough.
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Blocks the calling thread (runs its own message loop). Ok(None) = cancelled.
pub fn select() -> Result<Option<Pick>> {
    SELECTING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| msg("Area selection is already in progress. Confirm it or press Escape first."))?;
    let _selection = SelectionGuard;
    let displays = crate::sources::monitors();
    if displays.is_empty() {
        return Err(msg("No display is available for area selection"));
    }
    com_init();
    let gpu = Gpu::new()?;
    let (tx, rx) = mpsc::channel();
    let coordinator = Arc::new(Coordinator { finished: AtomicBool::new(false), windows: Mutex::new(Vec::new()), tx });
    let mut workers = Vec::new();
    for display in displays {
        let worker_coordinator = coordinator.clone();
        let gpu = gpu.clone();
        let worker = std::thread::Builder::new().name("oculus-area-overlay".into()).spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                select_monitor(display.id, gpu, worker_coordinator.clone())
            }))
            .unwrap_or_else(|_| Err(msg("Area selection worker failed")));
            worker_coordinator.finish(outcome);
        });
        match worker {
            Ok(worker) => workers.push(worker),
            Err(error) => {
                coordinator.finish(Err(error.into()));
                break;
            }
        }
    }
    let result = rx.recv().map_err(|_| msg("Area selection did not finish"))?;
    for worker in workers {
        let _ = worker.join();
    }
    result
}

const CANCEL_SELECTION: u32 = WM_APP + 41;

struct Coordinator {
    finished: AtomicBool,
    windows: Mutex<Vec<isize>>,
    tx: mpsc::Sender<Result<Option<Pick>>>,
}

impl Coordinator {
    fn finish(&self, result: Result<Option<Pick>>) {
        if !self.finished.swap(true, Ordering::AcqRel) {
            let _ = self.tx.send(result);
            // Use an app-private message rather than WM_CLOSE on cross-thread handles.
            // Keep registration locked until all cancellation messages are queued.
            for &handle in self.windows.lock().unwrap().iter() {
                unsafe {
                    let _ = PostMessageW(Some(HWND(handle as _)), CANCEL_SELECTION, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
}

struct OverlayWindow {
    hwnd: HWND,
    coordinator: Arc<Coordinator>,
}
impl Drop for OverlayWindow {
    fn drop(&mut self) {
        self.coordinator.windows.lock().unwrap().retain(|&id| id != self.hwnd.0 as isize);
        unsafe {
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

fn select_monitor(monitor: isize, gpu: Gpu, coordinator: Arc<Coordinator>) -> Result<Option<Pick>> {
    unsafe {
        // Client coords must be physical px to match the WGC frame 1:1, even on a 150% display.
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        com_init();
        let hmon = windows::Win32::Graphics::Gdi::HMONITOR(monitor as _);
        let (mr, _) = monitor_info(hmon).ok_or_else(|| msg("No monitor under cursor"))?;
        let (w, h) = (mr.right - mr.left, mr.bottom - mr.top);

        let frozen_tex = wgc::grab(&gpu, Target::Monitor(hmon.0 as isize), false)?;
        tracing::info!(monitor, "Area selection frame frozen");

        let hinst = GetModuleHandleW(None)?;
        let class = w!("OculusRegion");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_CROSS)?,
            ..Default::default()
        };
        RegisterClassW(&wc); // fails harmlessly with "already registered" on reuse
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("Oculus region"),
            WS_POPUP,
            mr.left,
            mr.top,
            w,
            h,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        crate::ui::exclude_native_window(hwnd);
        coordinator.windows.lock().unwrap().push(hwnd.0 as isize);
        let _window = OverlayWindow { hwnd, coordinator: coordinator.clone() };

        let state = build_state(&gpu, hwnd, &frozen_tex, w as u32, h as u32);
        tracing::info!(monitor, "Area selection drawing resources created");
        let state = match state {
            Ok(s) => s,
            Err(e) => {
                let _ = DestroyWindow(hwnd);
                return Err(e);
            }
        };
        let mut state = state;
        state.snap_rects = snap_rects(mr);
        STATE.with(|s| *s.borrow_mut() = Some(state));
        if coordinator.finished.load(Ordering::Acquire) {
            return Ok(None);
        }

        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        if monitor_at_cursor() == hmon {
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(hwnd));
        }
        render();

        let mut m = MSG::default();
        loop {
            let status = GetMessageW(&mut m, None, 0, 0).0;
            if status == 0 {
                break;
            }
            if status == -1 {
                return Err(msg("Windows could not read area selection input"));
            }
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }

        let result = STATE.with(|s| s.borrow_mut().take()).and_then(|s| s.result);
        Ok(result.map(|rect| Pick { monitor: hmon.0 as isize, rect, frozen: Mta((gpu, frozen_tex)) }))
    }
}

unsafe fn build_state(gpu: &Gpu, hwnd: HWND, frozen: &ID3D11Texture2D, w: u32, h: u32) -> Result<State> {
    let dxgi_dev: IDXGIDevice = gpu.device.cast()?;
    let factory: IDXGIFactory2 = dxgi_dev.GetAdapter()?.GetParent()?;
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: w,
        Height: h,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_NONE,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        ..Default::default()
    };
    let swap = factory.CreateSwapChainForHwnd(&gpu.device, hwnd, &desc, None, None)?;

    let d2d: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let dc = d2d.CreateDevice(&dxgi_dev)?.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
    // 96 DPI: 1 DIP = 1 physical px, so mouse coords map straight onto the frozen frame.
    let props = |opts| D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: opts,
        ..Default::default()
    };
    let back: IDXGISurface = swap.GetBuffer(0)?;
    let target = dc.CreateBitmapFromDxgiSurface(
        &back,
        Some(&props(D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW)),
    )?;
    dc.SetTarget(&target);
    let frozen =
        dc.CreateBitmapFromDxgiSurface(&frozen.cast::<IDXGISurface>()?, Some(&props(D2D1_BITMAP_OPTIONS_NONE)))?;

    let brush = |r, g, b, a| dc.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a }, None);
    let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
    let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
    let font = dw.CreateTextFormat(
        w!("Segoe UI"),
        None,
        DWRITE_FONT_WEIGHT_SEMI_BOLD,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        14.0 * scale,
        w!(""),
    )?;
    font.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    font.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;

    Ok(State {
        dim: brush(0.0, 0.0, 0.0, 0.55)?,
        accent: brush(1.0, 0.231, 0.306, 1.0)?,
        label_bg: brush(0.067, 0.075, 0.09, 0.92)?,
        text: brush(0.91, 0.92, 0.94, 1.0)?,
        dc,
        swap,
        frozen,
        font,
        write: dw,
        scale,
        w: w as f32,
        h: h as f32,
        drag: None,
        edit: None,
        cursor: POINT::default(),
        move_mode: false,
        snap_rects: Vec::new(),
        result: None,
    })
}

fn sel_rect(a: POINT, b: POINT) -> RECT {
    RECT { left: a.x.min(b.x), top: a.y.min(b.y), right: a.x.max(b.x), bottom: a.y.max(b.y) }
}

fn set_rect(state: &mut State, rect: RECT) {
    state.drag = Some((POINT { x: rect.left, y: rect.top }, POINT { x: rect.right, y: rect.bottom }));
}

fn contains(rect: RECT, point: POINT) -> bool {
    point.x >= rect.left && point.x <= rect.right && point.y >= rect.top && point.y <= rect.bottom
}

fn hit_edges(rect: RECT, point: POINT, margin: i32) -> u8 {
    if !contains(
        RECT {
            left: rect.left - margin,
            top: rect.top - margin,
            right: rect.right + margin,
            bottom: rect.bottom + margin,
        },
        point,
    ) {
        return 0;
    }
    let x = if (point.x - rect.left).abs() <= (point.x - rect.right).abs() { (rect.left, 1) } else { (rect.right, 2) };
    let y = if (point.y - rect.top).abs() <= (point.y - rect.bottom).abs() { (rect.top, 4) } else { (rect.bottom, 8) };
    (if (point.x - x.0).abs() <= margin { x.1 } else { 0 }) | (if (point.y - y.0).abs() <= margin { y.1 } else { 0 })
}

fn move_rect(mut rect: RECT, dx: i32, dy: i32, width: i32, height: i32) -> RECT {
    let dx = dx.clamp(-rect.left, width - rect.right);
    let dy = dy.clamp(-rect.top, height - rect.bottom);
    rect.left += dx;
    rect.right += dx;
    rect.top += dy;
    rect.bottom += dy;
    rect
}

fn resize_rect(mut rect: RECT, edges: u8, dx: i32, dy: i32, width: i32, height: i32) -> RECT {
    if edges & 1 != 0 {
        rect.left = (rect.left + dx).clamp(0, rect.right - MIN_SIZE);
    }
    if edges & 2 != 0 {
        rect.right = (rect.right + dx).clamp(rect.left + MIN_SIZE, width);
    }
    if edges & 4 != 0 {
        rect.top = (rect.top + dy).clamp(0, rect.bottom - MIN_SIZE);
    }
    if edges & 8 != 0 {
        rect.bottom = (rect.bottom + dy).clamp(rect.top + MIN_SIZE, height);
    }
    rect
}

fn edit_rect(edit: Edit, mut point: POINT, square: bool, width: i32, height: i32) -> RECT {
    let dx = point.x - edit.anchor.x;
    let dy = point.y - edit.anchor.y;
    match edit.kind {
        EditKind::Move => move_rect(edit.original, dx, dy, width, height),
        EditKind::Resize(edges) => resize_rect(edit.original, edges, dx, dy, width, height),
        EditKind::Draw => {
            point.x = point.x.clamp(0, width);
            point.y = point.y.clamp(0, height);
            if square {
                let sx = if dx < 0 { -1 } else { 1 };
                let sy = if dy < 0 { -1 } else { 1 };
                let side = dx
                    .abs()
                    .max(dy.abs())
                    .min(if sx < 0 { edit.anchor.x } else { width - edit.anchor.x })
                    .min(if sy < 0 { edit.anchor.y } else { height - edit.anchor.y });
                point = POINT { x: edit.anchor.x + sx * side, y: edit.anchor.y + sy * side };
            }
            sel_rect(edit.anchor, point)
        }
    }
}

fn snap_rects(monitor: RECT) -> Vec<RECT> {
    crate::sources::windows()
        .into_iter()
        .filter(|window| !window.minimized)
        .filter_map(|window| unsafe {
            let mut rect = RECT::default();
            if GetWindowRect(HWND(window.id as _), &mut rect).is_err() {
                return None;
            }
            let rect = RECT {
                left: (rect.left - monitor.left).max(0),
                top: (rect.top - monitor.top).max(0),
                right: (rect.right - monitor.left).min(monitor.right - monitor.left),
                bottom: (rect.bottom - monitor.top).min(monitor.bottom - monitor.top),
            };
            (rect.right - rect.left >= MIN_SIZE && rect.bottom - rect.top >= MIN_SIZE).then_some(rect)
        })
        .collect()
}

unsafe fn draw_label(state: &State, text: &str, x: f32, y: f32) {
    let text: Vec<u16> = text.encode_utf16().collect();
    let padding = 6.0 * state.scale;
    let available_width = (state.w - padding * 4.0).max(1.0);
    let Ok(layout) = state.write.CreateTextLayout(&text, &state.font, available_width, state.h.max(1.0)) else {
        return;
    };
    let mut metrics = DWRITE_TEXT_METRICS::default();
    if layout.GetMetrics(&mut metrics).is_err() {
        return;
    }
    let width = (metrics.widthIncludingTrailingWhitespace + padding * 2.0).min(state.w);
    let height = (metrics.height + padding * 2.0).min(state.h);
    let left = if x < 0.0 { (state.w - width) / 2.0 } else { x.clamp(0.0, (state.w - width).max(0.0)) };
    let top = y.clamp(0.0, (state.h - height).max(0.0));
    let bounds = rf(left, top, left + width, top + height);
    state.dc.FillRectangle(&bounds, &state.label_bg);
    state.dc.DrawText(
        &text,
        &state.font,
        &bounds,
        &state.text,
        D2D1_DRAW_TEXT_OPTIONS_CLIP,
        DWRITE_MEASURING_MODE_NATURAL,
    );
}

fn rf(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn render() {
    STATE.with(|s| {
        let s = s.borrow();
        let Some(s) = s.as_ref() else { return };
        unsafe {
            let dc = &s.dc;
            dc.BeginDraw();
            let full = rf(0.0, 0.0, s.w, s.h);
            dc.DrawBitmap(&s.frozen, Some(&full), 1.0, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
            dc.FillRectangle(&rf(s.cursor.x as f32, 0.0, s.cursor.x as f32 + 1.0, s.h), &s.accent);
            dc.FillRectangle(&rf(0.0, s.cursor.y as f32, s.w, s.cursor.y as f32 + 1.0), &s.accent);
            let selected = s.drag.map(|(a, b)| sel_rect(a, b));
            let preview = selected.or_else(|| s.snap_rects.iter().find(|&&rect| contains(rect, s.cursor)).copied());
            match preview {
                Some(r) if r.right > r.left && r.bottom > r.top => {
                    let (l, t, rr, b) = (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
                    // Dim everything except the selection: four bands around it.
                    for band in [rf(0.0, 0.0, s.w, t), rf(0.0, b, s.w, s.h), rf(0.0, t, l, b), rf(rr, t, s.w, b)] {
                        dc.FillRectangle(&band, &s.dim);
                    }
                    dc.DrawRectangle(&rf(l - 1.0, t - 1.0, rr + 1.0, b + 1.0), &s.accent, 2.0, None);
                    if selected.is_some() {
                        let handle = 3.0 * s.scale;
                        for (x, y) in [
                            (l, t),
                            ((l + rr) / 2.0, t),
                            (rr, t),
                            (l, (t + b) / 2.0),
                            (rr, (t + b) / 2.0),
                            (l, b),
                            ((l + rr) / 2.0, b),
                            (rr, b),
                        ] {
                            dc.FillRectangle(&rf(x - handle, y - handle, x + handle, y + handle), &s.accent);
                        }
                    }
                    let hint = if selected.is_some() {
                        if s.move_mode {
                            "Move mode · Enter confirms · Esc cancels"
                        } else {
                            "Enter confirms · Arrows adjust · Esc cancels"
                        }
                    } else {
                        "Click to select window · Esc cancels"
                    };
                    let label = format!("{} × {} · {hint}", r.right - r.left, r.bottom - r.top);
                    let ly = if b + 60.0 * s.scale < s.h { b + 8.0 * s.scale } else { t - 60.0 * s.scale };
                    draw_label(s, &label, l, ly);
                }
                _ => {
                    dc.FillRectangle(&full, &s.dim);
                    draw_label(
                        s,
                        "Drag or click a window · Arrows select · Enter confirms · Esc cancels",
                        -1.0,
                        24.0 * s.scale,
                    );
                }
            }
            let _ = dc.EndDraw(None, None);
            let _ = s.swap.Present(1, DXGI_PRESENT(0));
        }
    });
}

fn lparam_point(lp: LPARAM) -> POINT {
    // GET_X_LPARAM / GET_Y_LPARAM: signed 16-bit halves.
    POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let with = |f: &mut dyn FnMut(&mut State)| {
        STATE.with(|s| {
            if let Some(s) = s.borrow_mut().as_mut() {
                f(s)
            }
        })
    };
    match msg {
        WM_SETCURSOR => {
            let mut cursor = IDC_CROSS;
            with(&mut |s| {
                if let Some(rect) = s.drag.map(|(a, b)| sel_rect(a, b)) {
                    cursor = match hit_edges(rect, s.cursor, (6.0 * s.scale) as i32) {
                        1 | 2 => IDC_SIZEWE,
                        4 | 8 => IDC_SIZENS,
                        5 | 10 => IDC_SIZENWSE,
                        6 | 9 => IDC_SIZENESW,
                        _ if s.move_mode || contains(rect, s.cursor) => IDC_SIZEALL,
                        _ => IDC_CROSS,
                    };
                }
            });
            if let Ok(cursor) = LoadCursorW(None, cursor) {
                SetCursor(Some(cursor));
            }
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            let _ = SetFocus(Some(hwnd));
            SetCapture(hwnd);
            let p = lparam_point(lp);
            with(&mut |s| {
                let p = POINT { x: p.x.clamp(0, s.w as i32), y: p.y.clamp(0, s.h as i32) };
                let selected = s.drag.map(|(a, b)| sel_rect(a, b));
                let kind = selected
                    .map(|rect| {
                        let edges = hit_edges(rect, p, (6.0 * s.scale) as i32);
                        if s.move_mode {
                            EditKind::Move
                        } else if edges != 0 {
                            EditKind::Resize(edges)
                        } else if contains(rect, p) {
                            EditKind::Move
                        } else {
                            EditKind::Draw
                        }
                    })
                    .unwrap_or(EditKind::Draw);
                s.edit = Some(Edit { kind, anchor: p, original: selected.unwrap_or_default() });
                s.cursor = p;
                if matches!(kind, EditKind::Draw) {
                    s.drag = Some((p, p));
                }
            });
            render();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let p = lparam_point(lp);
            with(&mut |s| {
                s.cursor = POINT { x: p.x.clamp(0, s.w as i32), y: p.y.clamp(0, s.h as i32) };
                if let Some(edit) = s.edit {
                    let rect = edit_rect(edit, s.cursor, GetKeyState(VK_SHIFT.0 as i32) < 0, s.w as i32, s.h as i32);
                    set_rect(s, rect);
                }
            });
            render();
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            with(&mut |s| {
                s.edit = None;
                if let Some((a, b)) = s.drag {
                    let r = sel_rect(a, b);
                    if r.right - r.left < MIN_SIZE || r.bottom - r.top < MIN_SIZE {
                        let snap = s.snap_rects.iter().find(|&&r| contains(r, s.cursor)).copied();
                        s.drag = None;
                        if let Some(rect) = snap {
                            set_rect(s, rect);
                        }
                    }
                }
            });
            render();
            LRESULT(0)
        }
        CANCEL_SELECTION => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let key = wp.0 as u16;
            if key == VK_ESCAPE.0 {
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }
            if key == VK_RETURN.0 {
                let mut ready = false;
                with(&mut |s| {
                    if let Some(rect) = s.drag.map(|(a, b)| sel_rect(a, b)) {
                        if rect.right - rect.left >= MIN_SIZE && rect.bottom - rect.top >= MIN_SIZE {
                            s.result = Some(rect);
                            ready = true;
                        }
                    }
                });
                if ready {
                    let _ = DestroyWindow(hwnd);
                }
                return LRESULT(0);
            }
            if [VK_LEFT.0, VK_RIGHT.0, VK_UP.0, VK_DOWN.0, VK_SPACE.0].contains(&key) {
                let _ = ReleaseCapture();
                with(&mut |s| {
                    s.edit = None;
                    let width = s.w as i32;
                    let height = s.h as i32;
                    let rw = width.min(320);
                    let rh = height.min(180);
                    let rect = s.drag.map(|(a, b)| sel_rect(a, b)).unwrap_or(RECT {
                        left: (width - rw) / 2,
                        top: (height - rh) / 2,
                        right: (width + rw) / 2,
                        bottom: (height + rh) / 2,
                    });
                    let step = if GetKeyState(VK_SHIFT.0 as i32) < 0 { 10 } else { 1 };
                    let dx = if key == VK_LEFT.0 {
                        -step
                    } else if key == VK_RIGHT.0 {
                        step
                    } else {
                        0
                    };
                    let dy = if key == VK_UP.0 {
                        -step
                    } else if key == VK_DOWN.0 {
                        step
                    } else {
                        0
                    };
                    let rect = if GetKeyState(VK_MENU.0 as i32) < 0 {
                        resize_rect(rect, 2 | 8, dx, dy, width, height)
                    } else {
                        move_rect(rect, dx, dy, width, height)
                    };
                    if key == VK_SPACE.0 {
                        s.move_mode = !s.move_mode;
                    }
                    set_rect(s, rect);
                    s.cursor = POINT { x: (rect.left + rect.right) / 2, y: (rect.top + rect.bottom) / 2 };
                });
                render();
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_CAPTURECHANGED => {
            with(&mut |s| s.edit = None);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_PAINT => {
            render();
            let _ = ValidateRect(Some(hwnd), None);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_monitor_can_complete_or_cancel_selection() {
        let (tx, rx) = mpsc::channel();
        let coordinator = Coordinator { finished: AtomicBool::new(false), windows: Mutex::new(Vec::new()), tx };
        coordinator.finish(Ok(None));
        coordinator.finish(Err(msg("late monitor result")));
        assert!(rx.recv().unwrap().unwrap().is_none());
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn region_selection_normalizes_reverse_and_negative_coordinates() {
        for (a, b) in
            [(POINT { x: 101, y: 99 }, POINT { x: 3, y: 5 }), (POINT { x: -81, y: -21 }, POINT { x: 19, y: 39 })]
        {
            let forward = sel_rect(a, b);
            let reverse = sel_rect(b, a);
            assert_eq!(
                (forward.left, forward.top, forward.right, forward.bottom),
                (reverse.left, reverse.top, reverse.right, reverse.bottom)
            );
            assert!(forward.right > forward.left && forward.bottom > forward.top);
            assert_eq!(forward.left, a.x.min(b.x));
            assert_eq!(forward.top, a.y.min(b.y));
            assert_eq!(forward.right - forward.left, (a.x - b.x).abs());
            assert_eq!(forward.bottom - forward.top, (a.y - b.y).abs());
        }
    }

    #[test]
    fn moving_and_resizing_stay_in_bounds_and_preserve_minimum_area() {
        let rect = RECT { left: 10, top: 20, right: 110, bottom: 80 };
        let moved = move_rect(rect, -1000, 1000, 200, 150);
        assert_eq!((moved.left, moved.top, moved.right, moved.bottom), (0, 90, 100, 150));
        let one_pixel = move_rect(rect, 1, 1, 200, 150);
        assert_eq!((one_pixel.left, one_pixel.top), (11, 21));
        let resized = resize_rect(rect, 1 | 4, 1000, 1000, 200, 150);
        assert_eq!((resized.right - resized.left, resized.bottom - resized.top), (MIN_SIZE, MIN_SIZE));
        let expanded = resize_rect(rect, 2 | 8, 1000, 1000, 200, 150);
        assert_eq!((expanded.right, expanded.bottom), (200, 150));
        assert_eq!(hit_edges(rect, POINT { x: 10, y: 20 }, 6), 1 | 4);
    }

    #[test]
    fn square_drawing_respects_display_edges() {
        let edit = Edit { kind: EditKind::Draw, anchor: POINT { x: 150, y: 100 }, original: RECT::default() };
        let rect = edit_rect(edit, POINT { x: 300, y: 200 }, true, 200, 150);
        assert_eq!((rect.left, rect.top, rect.right, rect.bottom), (150, 100, 200, 150));
    }
}
