pub mod app;
pub mod state;
pub mod theme;
pub mod tray;

pub use app::{OmniClipApp, UiAction};
pub use state::{create_shared_ui_state, SharedUiState, ToastNotification, UiState};
pub use theme::M3Theme;
pub use tray::TrayIcon;

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
            log::info!("Initialized and hidden overlay window HWND: {:?}", hwnd.0);
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

/// Shows or hides the overlay window.
pub fn set_overlay_visible(visible: bool) {
    OVERLAY_IS_VISIBLE.store(visible, std::sync::atomic::Ordering::SeqCst);
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindow, SW_HIDE, SW_SHOW};
        let hwnd = get_overlay_hwnd();
        if !hwnd.0.is_null() {
            if visible {
                let _ = ShowWindow(hwnd, SW_SHOW);
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

/// Toggles the overlay window visibility and returns the new visibility state.
pub fn toggle_overlay_visible() -> bool {
    let current = OVERLAY_IS_VISIBLE.load(std::sync::atomic::Ordering::SeqCst);
    let new_state = !current;
    set_overlay_visible(new_state);
    new_state
}

