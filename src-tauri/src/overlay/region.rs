//! Snipping-Tool-style region picker: freeze the monitor under the cursor, show it in an opaque
//! borderless popup, let the user drag a rect. Native Win32 + D2D on the capture device: no
//! webview, no WS_EX_LAYERED alpha games, no CPU readback of the frozen frame.

use std::cell::RefCell;

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
use windows::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::capture::gpu::Gpu;
use crate::capture::wgc::{self, Target};
use crate::error::{msg, Result};
use crate::sources::{monitor_at_cursor, monitor_info};
use crate::util::{com_init, Mta};

pub struct Pick {
    pub monitor: isize,
    /// Monitor-relative physical px, even-aligned (encoder-safe).
    pub rect: RECT,
    /// The frozen frame, so a region *screenshot* is exactly what the user saw.
    pub frozen: Mta<(Gpu, ID3D11Texture2D)>,
}

const MIN_SIZE: i32 = 8;

struct State {
    dc: ID2D1DeviceContext,
    swap: IDXGISwapChain1,
    frozen: ID2D1Bitmap1,
    dim: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
    label_bg: ID2D1SolidColorBrush,
    text: ID2D1SolidColorBrush,
    font: IDWriteTextFormat,
    w: f32,
    h: f32,
    drag: Option<(POINT, POINT)>,
    result: Option<RECT>,
}

thread_local! {
    // The wndproc can't capture; one overlay per thread at a time, so a thread-local is enough.
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Blocks the calling thread (runs its own message loop). Ok(None) = cancelled.
pub fn select() -> Result<Option<Pick>> {
    unsafe {
        // Client coords must be physical px to match the WGC frame 1:1, even on a 150% display.
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        com_init();
        let hmon = monitor_at_cursor();
        let (mr, _) = monitor_info(hmon).ok_or_else(|| msg("No monitor under cursor"))?;
        let (w, h) = (mr.right - mr.left, mr.bottom - mr.top);

        let gpu = Gpu::new()?;
        let frozen_tex = wgc::grab(&gpu, Target::Monitor(hmon.0 as isize), false)?;

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

        let state = build_state(&gpu, hwnd, &frozen_tex, w as u32, h as u32);
        let state = match state {
            Ok(s) => s,
            Err(e) => {
                let _ = DestroyWindow(hwnd);
                return Err(e);
            }
        };
        STATE.with(|s| *s.borrow_mut() = Some(state));

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        render();

        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).as_bool() {
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
    let target = dc.CreateBitmapFromDxgiSurface(&back, Some(&props(D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW)))?;
    dc.SetTarget(&target);
    let frozen = dc.CreateBitmapFromDxgiSurface(&frozen.cast::<IDXGISurface>()?, Some(&props(D2D1_BITMAP_OPTIONS_NONE)))?;

    let brush = |r, g, b, a| dc.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a }, None);
    let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
    let font = dw.CreateTextFormat(
        w!("Segoe UI"),
        None,
        DWRITE_FONT_WEIGHT_SEMI_BOLD,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        14.0,
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
        w: w as f32,
        h: h as f32,
        drag: None,
        result: None,
    })
}

fn sel_rect(a: POINT, b: POINT) -> RECT {
    let (l, t) = (a.x.min(b.x), a.y.min(b.y));
    let (w, h) = ((a.x - b.x).abs() & !1, (a.y - b.y).abs() & !1);
    let (l, t) = (l & !1, t & !1);
    RECT { left: l, top: t, right: l + w, bottom: t + h }
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
            let _ = dc.DrawBitmap(&s.frozen, Some(&full), 1.0, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
            match s.drag.map(|(a, b)| sel_rect(a, b)) {
                Some(r) if r.right > r.left && r.bottom > r.top => {
                    let (l, t, rr, b) = (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
                    // Dim everything except the selection: four bands around it.
                    for band in [rf(0.0, 0.0, s.w, t), rf(0.0, b, s.w, s.h), rf(0.0, t, l, b), rf(rr, t, s.w, b)] {
                        dc.FillRectangle(&band, &s.dim);
                    }
                    dc.DrawRectangle(&rf(l - 1.0, t - 1.0, rr + 1.0, b + 1.0), &s.accent, 2.0, None);
                    let label: Vec<u16> = format!("{} × {}", r.right - r.left, r.bottom - r.top).encode_utf16().collect();
                    let ly = if b + 34.0 < s.h { b + 8.0 } else { (t - 34.0).max(0.0) };
                    let lr = rf(l, ly, l + 120.0, ly + 26.0);
                    dc.FillRectangle(&lr, &s.label_bg);
                    dc.DrawText(&label, &s.font, &lr, &s.text, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }
                _ => {
                    dc.FillRectangle(&full, &s.dim);
                    let hint: Vec<u16> = "Drag to select an area  ·  Esc to cancel".encode_utf16().collect();
                    let hr = rf(s.w / 2.0 - 180.0, 24.0, s.w / 2.0 + 180.0, 60.0);
                    dc.FillRectangle(&hr, &s.label_bg);
                    dc.DrawText(&hint, &s.font, &hr, &s.text, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
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
        WM_LBUTTONDOWN => {
            SetCapture(hwnd);
            let p = lparam_point(lp);
            with(&mut |s| s.drag = Some((p, p)));
            render();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let p = lparam_point(lp);
            let mut moved = false;
            with(&mut |s| {
                if let Some((a, _)) = s.drag {
                    s.drag = Some((a, p));
                    moved = true;
                }
            });
            if moved {
                render();
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            let mut done = false;
            with(&mut |s| {
                if let Some((a, b)) = s.drag {
                    let r = sel_rect(a, b);
                    if r.right - r.left >= MIN_SIZE && r.bottom - r.top >= MIN_SIZE {
                        s.result = Some(r);
                        done = true;
                    } else {
                        s.drag = None; // a click, not a drag: keep waiting
                    }
                }
            });
            if done {
                let _ = DestroyWindow(hwnd);
            } else {
                render();
            }
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
            let _ = DestroyWindow(hwnd);
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
