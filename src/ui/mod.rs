pub mod app;
pub mod i18n;
pub mod state;
pub mod theme;
pub mod tray;

pub use app::{OmniClipApp, UiAction};
pub use i18n::{t, Language, TextKey};
pub use state::{
    create_shared_ui_state, AudioAppEntry, HubTab, SavedReplayEntry, SharedUiState, ToastNotification, UiState,
};
pub use theme::{M3Icon, M3Theme};
pub use tray::{TrayIcon, TrayManager};

static OVERLAY_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static OVERLAY_IS_VISIBLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Returns whether the overlay is currently marked as visible.
pub fn is_overlay_visible() -> bool {
    OVERLAY_IS_VISIBLE.load(std::sync::atomic::Ordering::SeqCst)
}

/// Initializes and hides the overlay window using the Win32 window handle created on the current thread.
pub fn init_overlay_window() {
    OVERLAY_IS_VISIBLE.store(false, std::sync::atomic::Ordering::SeqCst);
    unsafe {
        use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
        use windows::Win32::System::Threading::GetCurrentThreadId;
        use windows::Win32::UI::WindowsAndMessaging::{
            EnumThreadWindows, FindWindowW, ShowWindow, SW_HIDE,
        };
        use windows::core::w;

        let thread_id = GetCurrentThreadId();
        let mut found_hwnd = HWND(std::ptr::null_mut());

        unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let out = &mut *(lparam.0 as *mut HWND);
            *out = hwnd;
            BOOL(0) // Stop enumeration once the window for this thread is captured
        }

        let _ = EnumThreadWindows(
            thread_id,
            Some(enum_proc),
            LPARAM(&mut found_hwnd as *mut _ as isize),
        );

        let hwnd = if !found_hwnd.0.is_null() {
            found_hwnd
        } else if let Ok(h) = FindWindowW(None, w!("OmniClip Overlay")) {
            h
        } else if let Ok(h) = FindWindowW(None, w!("OmniClip")) {
            h
        } else {
            HWND(std::ptr::null_mut())
        };

        if !hwnd.0.is_null() {
            OVERLAY_HWND.store(hwnd.0 as isize, std::sync::atomic::Ordering::SeqCst);
            let _ = ShowWindow(hwnd, SW_HIDE);

            // Strip all caption, thick border, and sysmenu styles to guarantee a completely frameless window
            use windows::Win32::Graphics::Dwm::{
                DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE,
            };
            use windows::Win32::UI::Controls::MARGINS;
            use windows::Win32::UI::WindowsAndMessaging::{
                GetWindowLongW, SetWindowLongW, SetWindowPos, GWL_EXSTYLE, GWL_STYLE, HWND_TOPMOST,
                SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WS_BORDER, WS_CAPTION,
                WS_DLGFRAME, WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME, WS_EX_LAYERED, WS_EX_STATICEDGE,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_WINDOWEDGE, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
                WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
            };

            let cur_style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let frameless_style = (cur_style
                & !(WS_CAPTION.0
                    | WS_THICKFRAME.0
                    | WS_MINIMIZEBOX.0
                    | WS_MAXIMIZEBOX.0
                    | WS_SYSMENU.0
                    | WS_BORDER.0
                    | WS_DLGFRAME.0))
                | WS_POPUP.0;
            SetWindowLongW(hwnd, GWL_STYLE, frameless_style as i32);

            let cur_ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            let frameless_ex = (cur_ex
                & !(WS_EX_WINDOWEDGE.0
                    | WS_EX_CLIENTEDGE.0
                    | WS_EX_STATICEDGE.0
                    | WS_EX_DLGMODALFRAME.0))
                | WS_EX_LAYERED.0
                | WS_EX_TOPMOST.0
                | WS_EX_TOOLWINDOW.0;
            SetWindowLongW(hwnd, GWL_EXSTYLE, frameless_ex as i32);

            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_FRAMECHANGED | SWP_NOACTIVATE,
            );

            // Extend DWM frame into client area for full alpha compositing
            let margins = MARGINS {
                cxLeftWidth: -1,
                cxRightWidth: -1,
                cyTopHeight: -1,
                cyBottomHeight: -1,
            };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);

            // Apply Windows 11 DWM native rounded corners (DWMWA_WINDOW_CORNER_PREFERENCE = 33, DWMWCP_ROUND = 2)
            let round_pref: u32 = 2; // DWMWCP_ROUND
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &round_pref as *const u32 as *const _,
                std::mem::size_of::<u32>() as u32,
            );

            log::info!("Initialized borderless transparent overlay window HWND: {:?}", hwnd.0);
        } else {
            log::warn!("Could not detect overlay window HWND during init");
        }
    }
}

/// Helper function to retrieve the cached or discovered overlay HWND.
fn get_overlay_hwnd() -> windows::Win32::Foundation::HWND {
    unsafe {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
        use windows::core::w;

        let hwnd_val = OVERLAY_HWND.load(std::sync::atomic::Ordering::SeqCst);
        if hwnd_val != 0 {
            HWND(hwnd_val as *mut _)
        } else if let Ok(h) = FindWindowW(None, w!("OmniClip Overlay")) {
            OVERLAY_HWND.store(h.0 as isize, std::sync::atomic::Ordering::SeqCst);
            h
        } else if let Ok(h) = FindWindowW(None, w!("OmniClip")) {
            OVERLAY_HWND.store(h.0 as isize, std::sync::atomic::Ordering::SeqCst);
            h
        } else {
            HWND(std::ptr::null_mut())
        }
    }
}

/// Shows or hides the overlay window without legacy GDI regions.
pub fn set_overlay_visible(visible: bool) {
    OVERLAY_IS_VISIBLE.store(visible, std::sync::atomic::Ordering::SeqCst);
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetForegroundWindow, SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE,
            SW_HIDE, SW_SHOW,
        };
        let hwnd = get_overlay_hwnd();
        if !hwnd.0.is_null() {
            if visible {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE,
                );
                let _ = SetForegroundWindow(hwnd);
                log::info!("Overlay window shown");
            } else {
                let _ = ShowWindow(hwnd, SW_HIDE);
                log::info!("Overlay window hidden");
            }
        } else {
            log::warn!("set_overlay_visible: HWND not found");
        }
    }
}

/// Launches the native modern Windows folder picker dialog and returns the selected path.
/// Properly parents the dialog to the overlay window so it appears in front of the topmost UI.
pub fn pick_folder(title: Option<&str>) -> Option<std::path::PathBuf> {
    let title_owned = title.map(|s| s.to_string());
    let parent_hwnd = get_overlay_hwnd();

    // Temporarily demote overlay from HWND_TOPMOST so the modal dialog is guaranteed to sit on top
    if !parent_hwnd.0.is_null() {
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{
                SetWindowPos, HWND_NOTOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_NOACTIVATE,
            };
            let _ = SetWindowPos(
                parent_hwnd,
                HWND_NOTOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
    let parent_hwnd_raw = parent_hwnd.0 as isize;

    let result = std::thread::spawn(move || {
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::System::Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_APARTMENTTHREADED,
            };
            use windows::Win32::UI::Shell::{
                FileOpenDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
            };

            let com_init = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            let parent_hwnd = HWND(parent_hwnd_raw as *mut _);

            let res = (|| -> Option<std::path::PathBuf> {
                let dialog: IFileOpenDialog =
                    CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
                dialog.SetOptions(FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
                let title_str = title_owned.as_deref().unwrap_or("OmniClip — Select Storage Folder");
                let title_u16: Vec<u16> = title_str.encode_utf16().chain(std::iter::once(0)).collect();
                dialog
                    .SetTitle(windows::core::PCWSTR(title_u16.as_ptr()))
                    .ok()?;
                let owner = if !parent_hwnd.0.is_null() {
                    parent_hwnd
                } else {
                    windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow()
                };
                if !owner.0.is_null() {
                    dialog.Show(owner).ok()?;
                } else {
                    dialog.Show(None).ok()?;
                }
                let item = dialog.GetResult().ok()?;
                let display_name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
                let path_str = display_name.to_string().ok()?;
                CoTaskMemFree(Some(display_name.0 as *const _));
                Some(std::path::PathBuf::from(path_str))
            })();

            if com_init {
                CoUninitialize();
            }

            res
        }
    })
    .join()
    .ok()
    .flatten();

    // Re-promote overlay window back to HWND_TOPMOST and restore focus
    if !parent_hwnd.0.is_null() && is_overlay_visible() {
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{
                SetForegroundWindow, SetWindowPos, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE,
            };
            let _ = SetWindowPos(parent_hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
            let _ = SetForegroundWindow(parent_hwnd);
        }
    }

    result
}

/// Toggles the overlay window visibility and returns the new visibility state.
pub fn toggle_overlay_visible() -> bool {
    let current = OVERLAY_IS_VISIBLE.load(std::sync::atomic::Ordering::SeqCst);
    let new_state = !current;
    set_overlay_visible(new_state);
    new_state
}

/// Sends WM_CLOSE to the overlay window handle to trigger clean shutdown of the window event loop.
pub fn close_overlay_window() {
    HUD_STOP_SIGNAL.store(true, std::sync::atomic::Ordering::SeqCst);
    unsafe {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

        let hwnd = get_overlay_hwnd();
        if !hwnd.0.is_null() {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            log::info!("Dispatched WM_CLOSE to overlay HWND {:?}", hwnd.0);
        }
    }
}

/// Notification payload for the on-screen HUD toast.
#[derive(Debug, Clone)]
pub struct HudToastData {
    pub title: String,
    pub filename: String,
    pub duration_secs: u32,
    pub size_mb: f64,
    pub language: crate::ui::i18n::Language,
}

static HUD_SENDER: parking_lot::RwLock<Option<crossbeam_channel::Sender<HudToastData>>> =
    parking_lot::RwLock::new(None);
static HUD_ACTIVE_DATA: parking_lot::RwLock<Option<HudToastData>> = parking_lot::RwLock::new(None);
static HUD_STOP_SIGNAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Triggers a non-intrusive on-screen HUD toast notification over any game or window.
pub fn show_hud_toast(filepath: std::path::PathBuf, duration_secs: u32, size_mb: f64, lang: crate::ui::i18n::Language) {
    let filename = filepath
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "replay.mp4".to_string());

    let title = crate::ui::i18n::t(crate::ui::i18n::TextKey::ToastReplaySaved, lang).to_string();

    let data = HudToastData {
        title,
        filename,
        duration_secs,
        size_mb,
        language: lang,
    };

    if let Some(sender) = HUD_SENDER.read().as_ref() {
        let _ = sender.send(data);
    }
}

/// Spawns the dedicated, non-intrusive on-screen HUD toast notification thread.
pub fn spawn_hud_toast_thread() -> anyhow::Result<()> {
    let (tx, rx) = crossbeam_channel::unbounded::<HudToastData>();
    *HUD_SENDER.write() = Some(tx);

    std::thread::Builder::new()
        .name("omni-hud-toast".to_string())
        .spawn(move || {
            run_hud_toast_loop(rx);
        })?;

    Ok(())
}

fn run_hud_toast_loop(rx: crossbeam_channel::Receiver<HudToastData>) {
    unsafe {
        use std::time::{Duration, Instant};
        use windows::Win32::Foundation::{BOOL, COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
        use windows::Win32::Graphics::Gdi::*;
        use windows::Win32::UI::WindowsAndMessaging::*;
        use windows::core::w;

        let class_name = w!("OmniClipHudToastWindow");

        // Register bundled fonts in memory for GDI rendering
        static FONT_MONTSERRAT: &[u8] = include_bytes!("../../assets/fonts/Montserrat-Medium.ttf");
        static FONT_SEGOE_MDL2: &[u8] = include_bytes!("../../assets/fonts/segmdl2.ttf");
        let mut num_fonts = 0u32;
        let _ = AddFontMemResourceEx(
            FONT_MONTSERRAT.as_ptr() as *const _,
            FONT_MONTSERRAT.len() as u32,
            None,
            &mut num_fonts,
        );
        let _ = AddFontMemResourceEx(
            FONT_SEGOE_MDL2.as_ptr() as *const _,
            FONT_SEGOE_MDL2.len() as u32,
            None,
            &mut num_fonts,
        );

        unsafe extern "system" fn hud_wnd_proc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            match msg {
                WM_PAINT => {
                    let mut ps = PAINTSTRUCT::default();
                    let hdc = BeginPaint(hwnd, &mut ps);
                    draw_hud_toast(hwnd, hdc);
                    let _ = EndPaint(hwnd, &ps);
                    LRESULT(0)
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(hud_wnd_proc),
            hInstance: HINSTANCE(std::ptr::null_mut()),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class_name,
            ..Default::default()
        };

        let _ = RegisterClassExW(&wc);

        let win_w = 420;
        let win_h = 88;

        // Calculate initial taskbar-aware placement
        let mut work_area = windows::Win32::Foundation::RECT::default();
        let (pos_x, pos_y) = if SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut work_area as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        ).is_ok() && work_area.right > work_area.left {
            ((work_area.right - win_w - 24).max(0), work_area.top + 20)
        } else {
            let screen_w = GetSystemMetrics(SM_CXSCREEN);
            ((screen_w - win_w - 24).max(0), 24)
        };

        let hwnd_res = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW,
            class_name,
            w!("OmniClip HUD Toast"),
            WS_POPUP,
            pos_x,
            pos_y,
            win_w,
            win_h,
            None,
            None,
            None,
            None,
        );

        let hwnd = match hwnd_res {
            Ok(h) => h,
            Err(e) => {
                log::error!("Failed to create HUD toast window: {}", e);
                return;
            }
        };

        // Apply Windows 11 DWM native smooth rounded corners (DWMWA_WINDOW_CORNER_PREFERENCE = 33, DWMWCP_ROUND = 2)
        use windows::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE};
        use windows::Win32::UI::Controls::MARGINS;
        let round_pref: u32 = 2; // DWMWCP_ROUND
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &round_pref as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);

        // Start hidden with 0 alpha
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
        let _ = ShowWindow(hwnd, SW_HIDE);

        enum AnimState {
            Idle,
            FadeIn(f32),
            Hold(Instant),
            FadeOut(f32),
        }

        let mut anim_state = AnimState::Idle;

        while !HUD_STOP_SIGNAL.load(std::sync::atomic::Ordering::Relaxed) {
            // Check for new incoming toast requests
            while let Ok(data) = rx.try_recv() {
                *HUD_ACTIVE_DATA.write() = Some(data);
                extern "system" {
                    fn MessageBeep(uType: u32) -> windows::Win32::Foundation::BOOL;
                }
                let _ = MessageBeep(0);

                // Recalculate work-area dynamically in case taskbar or resolution shifted
                let mut current_wa = windows::Win32::Foundation::RECT::default();
                let (target_x, target_y) = if SystemParametersInfoW(
                    SPI_GETWORKAREA,
                    0,
                    Some(&mut current_wa as *mut _ as *mut _),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                ).is_ok() && current_wa.right > current_wa.left {
                    ((current_wa.right - win_w - 24).max(0), current_wa.top + 20)
                } else {
                    let screen_w = GetSystemMetrics(SM_CXSCREEN);
                    ((screen_w - win_w - 24).max(0), 24)
                };

                let _ = InvalidateRect(hwnd, None, BOOL(1));
                let _ = SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    target_x,
                    target_y,
                    win_w,
                    win_h,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                anim_state = AnimState::FadeIn(0.0);
            }

            // Step animation
            match anim_state {
                AnimState::Idle => {}
                AnimState::FadeIn(ref mut progress) => {
                    *progress += 0.15; // ~100ms fade-in
                    if *progress >= 1.0 {
                        // Hold at 255 (100% solid opacity: crisp typography, zero ghosting)
                        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
                        anim_state = AnimState::Hold(Instant::now());
                    } else {
                        let a = (*progress * 255.0) as u8;
                        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), a, LWA_ALPHA);
                    }
                }
                AnimState::Hold(start_time) => {
                    if start_time.elapsed() >= Duration::from_millis(3500) {
                        anim_state = AnimState::FadeOut(1.0);
                    }
                }
                AnimState::FadeOut(ref mut progress) => {
                    *progress -= 0.08; // ~200ms fade-out
                    if *progress <= 0.0 {
                        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
                        let _ = ShowWindow(hwnd, SW_HIDE);
                        anim_state = AnimState::Idle;
                    } else {
                        let a = (*progress * 255.0) as u8;
                        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), a, LWA_ALPHA);
                    }
                }
            }

            // Pump any Win32 messages
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            std::thread::sleep(Duration::from_millis(16));
        }
    }
}

unsafe fn draw_hud_toast(hwnd: windows::Win32::Foundation::HWND, hdc: windows::Win32::Graphics::Gdi::HDC) {
    use windows::Win32::Foundation::{COLORREF, POINT, RECT, SIZE};
    use windows::Win32::Graphics::Gdi::*;
    use windows::core::w;

    let data = match HUD_ACTIVE_DATA.read().clone() {
        Some(d) => d,
        None => return,
    };

    let mut rect = RECT::default();
    let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rect);
    let w = rect.right - rect.left;
    let h = rect.bottom - rect.top;

    // 2x Supersampling buffer for ultra-smooth anti-aliased geometry and crisp ClearType fonts
    let scale = 2i32;
    let w2 = w * scale;
    let h2 = h * scale;

    let mem_dc = CreateCompatibleDC(hdc);
    let mem_bmp = CreateCompatibleBitmap(hdc, w2, h2);
    let old_bmp = SelectObject(mem_dc, mem_bmp);

    let rect2 = RECT {
        left: 0,
        top: 0,
        right: w2,
        bottom: h2,
    };

    // 1. Background Surface: Material 3 Deep Graphite #191A23
    let bg_color = COLORREF(25 | (26 << 8) | (35 << 16));
    let bg_brush = CreateSolidBrush(bg_color);
    let _ = FillRect(mem_dc, &rect2, bg_brush);
    let _ = DeleteObject(bg_brush);

    // 2. Subtle Outer Border: M3 Outline Variant #383B4B with 48px supersampled radius
    let border_color = COLORREF(56 | (59 << 8) | (75 << 16));
    let pen = CreatePen(PS_SOLID, 2, border_color);
    let old_pen = SelectObject(mem_dc, pen);
    let null_brush = GetStockObject(NULL_BRUSH);
    let old_brush = SelectObject(mem_dc, null_brush);
    let _ = RoundRect(mem_dc, 0, 0, w2, h2, 48, 48);
    let _ = SelectObject(mem_dc, old_pen);
    let _ = SelectObject(mem_dc, old_brush);
    let _ = DeleteObject(pen);

    // 2b. Delicate Top Sheen Line (#464A5E) for premium elevated card feel
    let sheen_pen = CreatePen(PS_SOLID, 2, COLORREF(70 | (74 << 8) | (94 << 16)));
    let _ = SelectObject(mem_dc, sheen_pen);
    let mut prev_pt = POINT::default();
    let _ = MoveToEx(mem_dc, 40, 2, Some(&mut prev_pt));
    let _ = LineTo(mem_dc, w2 - 40, 2);
    let _ = DeleteObject(sheen_pen);

    // 3. Left Status Icon Badge: Circular Primary Container (#1C3054) with Vibrant Accent Border (#6CA2FF)
    // 96x96px circle at 2x, vertically centered in 176px height: (176 - 96)/2 = 40
    let icon_bg_color = COLORREF(28 | (48 << 8) | (84 << 16));
    let icon_border_color = COLORREF(108 | (162 << 8) | (255 << 16));
    let icon_bg = CreateSolidBrush(icon_bg_color);
    let icon_pen = CreatePen(PS_SOLID, 2, icon_border_color);
    let _ = SelectObject(mem_dc, icon_bg);
    let _ = SelectObject(mem_dc, icon_pen);
    let _ = RoundRect(mem_dc, 32, 40, 128, 136, 96, 96);
    let _ = DeleteObject(icon_bg);
    let _ = DeleteObject(icon_pen);

    // 4. Vector Geometric Checkmark (Crisp pure white 6px stroke at 2x, mathematically centered)
    let check_pen = CreatePen(PS_SOLID, 6, COLORREF(0x00FFFFFF));
    let _ = SelectObject(mem_dc, check_pen);
    let _ = MoveToEx(mem_dc, 62, 88, Some(&mut prev_pt));
    let _ = LineTo(mem_dc, 74, 100);
    let _ = LineTo(mem_dc, 98, 74);
    let _ = DeleteObject(check_pen);

    // CLEARTYPE_QUALITY = 5 for high-fidelity subpixel anti-aliasing
    const CLEARTYPE_QUALITY: u32 = 5;

    // 5. Title (Line 1): "Повтор сохранен!" (Montserrat Bold 30px, Pure White #FFFFFF)
    let font_title = CreateFontW(
        -30, 0, 0, 0, 700, 0, 0, 0, 1, 0, 0, CLEARTYPE_QUALITY, 0, w!("Montserrat"),
    );
    let _ = SelectObject(mem_dc, font_title);
    let _ = SetBkMode(mem_dc, TRANSPARENT);
    let _ = SetTextColor(mem_dc, COLORREF(0x00FFFFFF));
    let title_utf16: Vec<u16> = data.title.encode_utf16().collect();
    let _ = TextOutW(mem_dc, 152, 32, &title_utf16);
    let _ = DeleteObject(font_title);

    // 6. Top-Right Hotkey Chip: "Alt+F10" (Keycap style badge with Primary Accent)
    let font_tag = CreateFontW(
        -22, 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, CLEARTYPE_QUALITY, 0, w!("Montserrat"),
    );
    let _ = SelectObject(mem_dc, font_tag);
    let tag_utf16: Vec<u16> = "Alt+F10".encode_utf16().collect();
    let mut tag_size = SIZE::default();
    let _ = GetTextExtentPoint32W(mem_dc, &tag_utf16, &mut tag_size);
    let tag_w = (tag_size.cx + 40).max(128);
    let tag_h = 44;
    let tag_x = w2 - tag_w - 32;
    let tag_y = 30;

    let tag_bg = CreateSolidBrush(COLORREF(36 | (38 << 8) | (49 << 16))); // #242631
    let tag_pen = CreatePen(PS_SOLID, 2, COLORREF(60 | (64 << 8) | (82 << 16))); // #3C4052
    let _ = SelectObject(mem_dc, tag_bg);
    let _ = SelectObject(mem_dc, tag_pen);
    let _ = RoundRect(mem_dc, tag_x, tag_y, tag_x + tag_w, tag_y + tag_h, 20, 20);
    let _ = DeleteObject(tag_bg);
    let _ = DeleteObject(tag_pen);

    let _ = SetTextColor(mem_dc, COLORREF(173 | (198 << 8) | (255 << 16))); // #ADC6FF Primary accent
    let tag_text_x = tag_x + (tag_w - tag_size.cx) / 2;
    let tag_text_y = tag_y + (tag_h - tag_size.cy) / 2;
    let _ = TextOutW(mem_dc, tag_text_x, tag_text_y, &tag_utf16);
    let _ = DeleteObject(font_tag);

    // 7. Filename (Line 2): Montserrat Medium 26px (#A6ABB8)
    let font_file = CreateFontW(
        -26, 0, 0, 0, 500, 0, 0, 0, 1, 0, 0, CLEARTYPE_QUALITY, 0, w!("Montserrat"),
    );
    let _ = SelectObject(mem_dc, font_file);
    let _ = SetTextColor(mem_dc, COLORREF(166 | (171 << 8) | (184 << 16)));
    let display_filename = if data.filename.len() > 34 {
        format!("{}…", &data.filename[..33])
    } else {
        data.filename.clone()
    };
    let file_utf16: Vec<u16> = display_filename.encode_utf16().collect();
    let _ = TextOutW(mem_dc, 152, 74, &file_utf16);
    let _ = DeleteObject(font_file);

    // 8. Line 3: Three Material Design 3 Pill Chips with Dynamic Text Measurement and Centering
    let font_chips = CreateFontW(
        -22, 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, CLEARTYPE_QUALITY, 0, w!("Montserrat"),
    );
    let _ = SelectObject(mem_dc, font_chips);

    let chip_y = 118;
    let chip_h = 40;

    // Helper closure to draw mathematically centered, properly padded pill chips
    let draw_pill = |x: i32, text: &str, bg_col: COLORREF, border_col: COLORREF, text_col: COLORREF| -> i32 {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let mut text_size = SIZE::default();
        let _ = GetTextExtentPoint32W(mem_dc, &utf16, &mut text_size);
        let pill_w = (text_size.cx + 40).max(112);

        let bg_brush = CreateSolidBrush(bg_col);
        let pen = CreatePen(PS_SOLID, 2, border_col);
        let _ = SelectObject(mem_dc, bg_brush);
        let _ = SelectObject(mem_dc, pen);
        let _ = RoundRect(mem_dc, x, chip_y, x + pill_w, chip_y + chip_h, 20, 20);
        let _ = DeleteObject(bg_brush);
        let _ = DeleteObject(pen);

        let text_x = x + (pill_w - text_size.cx) / 2;
        let text_y = chip_y + (chip_h - text_size.cy) / 2;
        let _ = SetTextColor(mem_dc, text_col);
        let _ = TextOutW(mem_dc, text_x, text_y, &utf16);

        pill_w
    };

    // Chip 1: Duration Pill
    let sec_suffix = match data.language {
        crate::ui::i18n::Language::Ru => "сек",
        crate::ui::i18n::Language::En => "sec",
    };
    let chip_1_str = format!("{} {}", data.duration_secs, sec_suffix);
    let chip_1_w = draw_pill(
        152,
        &chip_1_str,
        COLORREF(36 | (38 << 8) | (50 << 16)),
        COLORREF(60 | (63 << 8) | (82 << 16)),
        COLORREF(206 | (209 << 8) | (223 << 16)),
    );

    // Chip 2: File Size Pill (Primary Accent Pill)
    let mb_suffix = match data.language {
        crate::ui::i18n::Language::Ru => "МБ",
        crate::ui::i18n::Language::En => "MB",
    };
    let chip_2_str = format!("{:.1} {}", data.size_mb, mb_suffix);
    let chip_2_x = 152 + chip_1_w + 16;
    let chip_2_w = draw_pill(
        chip_2_x,
        &chip_2_str,
        COLORREF(26 | (44 << 8) | (70 << 16)),
        COLORREF(58 | (106 << 8) | (165 << 16)),
        COLORREF(150 | (197 << 8) | (255 << 16)),
    );

    // Chip 3: Video Format Pill (High Contrast, Perfectly Padded)
    let chip_3_str = "MP4 · 60 FPS";
    let chip_3_x = chip_2_x + chip_2_w + 16;
    let _ = draw_pill(
        chip_3_x,
        chip_3_str,
        COLORREF(36 | (38 << 8) | (50 << 16)),
        COLORREF(60 | (63 << 8) | (82 << 16)),
        COLORREF(206 | (209 << 8) | (223 << 16)),
    );

    let _ = DeleteObject(font_chips);

    // High quality downscaling with HALFTONE bilinear smoothing to eliminate all aliasing artifacts
    let _ = SetStretchBltMode(hdc, HALFTONE);
    let _ = SetBrushOrgEx(hdc, 0, 0, None);
    let _ = StretchBlt(hdc, 0, 0, w, h, mem_dc, 0, 0, w2, h2, SRCCOPY);

    let _ = SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(mem_bmp);
    let _ = DeleteDC(mem_dc);
}


