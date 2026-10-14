use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use anyhow::{anyhow, Result};
use crossbeam_channel::Sender;
use log::info;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GetCursorPos, GetMessageW, LoadIconW, PostQuitMessage, PostThreadMessageW, RegisterClassExW,
    SetForegroundWindow, TrackPopupMenu, HICON, IDI_APPLICATION, MF_SEPARATOR, MF_STRING, MSG,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WM_DESTROY, WM_LBUTTONDBLCLK,
    WM_LBUTTONUP, WM_QUIT, WM_RBUTTONUP, WNDCLASSEXW, WS_OVERLAPPED,
};

use crate::ui::app::UiAction;
use crate::ui::i18n::{t, Language, TextKey};
use crate::ui::state::SharedUiState;

/// Tray message ID sent back to the window proc.
pub const WM_APP_TRAY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

/// Tray menu command identifiers.
pub const TRAY_CMD_TOGGLE_OVERLAY: u32 = 1001;
pub const TRAY_CMD_SAVE_REPLAY: u32 = 1002;
pub const TRAY_CMD_EXIT: u32 = 1003;

/// Tray icon identifier.
const TRAY_ICON_UID: u32 = 101;

/// System tray notification icon manager using native Windows Shell API.
pub struct TrayIcon {
    hwnd: HWND,
    added: bool,
}

unsafe impl Send for TrayIcon {}
unsafe impl Sync for TrayIcon {}

impl TrayIcon {
    /// Creates and registers a new system tray notification icon for the specified window.
    pub fn new(hwnd: HWND, tip_text: &str) -> Result<Self> {
        let mut nid = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ICON_UID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_APP_TRAY,
            ..Default::default()
        };

        // Load standard default application icon
        unsafe {
            nid.hIcon = LoadIconW(None, IDI_APPLICATION).unwrap_or(HICON(std::ptr::null_mut()));
        }

        // Copy tooltip text into UTF-16 buffer
        let tip_utf16: Vec<u16> = tip_text.encode_utf16().chain(std::iter::once(0)).collect();
        let copy_len = tip_utf16.len().min(nid.szTip.len());
        nid.szTip[..copy_len].copy_from_slice(&tip_utf16[..copy_len]);

        let success = unsafe { Shell_NotifyIconW(NIM_ADD, &nid).as_bool() };
        if !success {
            return Err(anyhow!("Failed to add icon to Windows system tray"));
        }

        info!("Registered system tray icon for OmniClip");
        Ok(Self { hwnd, added: true })
    }

    /// Displays the right-click context menu and returns the selected command ID.
    pub fn show_context_menu(&self, lang: Language) -> Option<u32> {
        unsafe {
            let hmenu = CreatePopupMenu().ok()?;
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);

            // Populate menu items
            let s_overlay_str = format!("{}\0", t(TextKey::TrayOpenOverlay, lang));
            let s_save_str = format!("{}\0", t(TextKey::TraySaveReplay, lang));
            let s_exit_str = format!("{}\0", t(TextKey::TrayExit, lang));

            let s_overlay: Vec<u16> = s_overlay_str.encode_utf16().collect();
            let s_save: Vec<u16> = s_save_str.encode_utf16().collect();
            let s_exit: Vec<u16> = s_exit_str.encode_utf16().collect();

            let _ = AppendMenuW(hmenu, MF_STRING, TRAY_CMD_TOGGLE_OVERLAY as usize, windows::core::PCWSTR(s_overlay.as_ptr()));
            let _ = AppendMenuW(hmenu, MF_STRING, TRAY_CMD_SAVE_REPLAY as usize, windows::core::PCWSTR(s_save.as_ptr()));
            let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
            let _ = AppendMenuW(hmenu, MF_STRING, TRAY_CMD_EXIT as usize, windows::core::PCWSTR(s_exit.as_ptr()));

            // Set foreground window to avoid menu hanging bug in Win32
            let _ = SetForegroundWindow(self.hwnd);

            let cmd = TrackPopupMenu(
                hmenu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                pt.x,
                pt.y,
                0,
                self.hwnd,
                None,
            );

            let _ = DestroyMenu(hmenu);

            if cmd.0 > 0 {
                Some(cmd.0 as u32)
            } else {
                None
            }
        }
    }

    /// Removes the tray icon from the notification area.
    pub fn remove(&mut self) {
        if self.added {
            let nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_ICON_UID,
                ..Default::default()
            };
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            }
            self.added = false;
        }
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        self.remove();
    }
}

struct TrayContext {
    action_tx: Sender<UiAction>,
    egui_ctx: Arc<parking_lot::Mutex<Option<eframe::egui::Context>>>,
    ui_state: Option<SharedUiState>,
    running: Arc<AtomicBool>,
    tray_icon: Option<TrayIcon>,
}

static TRAY_CTX: parking_lot::RwLock<Option<TrayContext>> = parking_lot::RwLock::new(None);

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_APP_TRAY => {
            let event = lparam.0 as u32;
            if event == WM_LBUTTONUP || event == WM_LBUTTONDBLCLK {
                let is_now_visible = crate::ui::toggle_overlay_visible();
                if let Some(ctx_guard) = TRAY_CTX.read().as_ref() {
                    if let Some(ctx) = ctx_guard.egui_ctx.lock().as_ref() {
                        ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Visible(is_now_visible));
                        if is_now_visible {
                            ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Focus);
                        }
                        ctx.request_repaint();
                    }
                }
            } else if event == WM_RBUTTONUP {
                let mut action_to_send = None;
                if let Some(ctx_guard) = TRAY_CTX.read().as_ref() {
                    let lang = ctx_guard
                        .ui_state
                        .as_ref()
                        .map(|s| s.read().language)
                        .unwrap_or_default();
                    if let Some(ref tray) = ctx_guard.tray_icon {
                        if let Some(cmd) = tray.show_context_menu(lang) {
                            match cmd {
                                TRAY_CMD_TOGGLE_OVERLAY => {
                                    let is_now_visible = crate::ui::toggle_overlay_visible();
                                    if let Some(ctx) = ctx_guard.egui_ctx.lock().as_ref() {
                                        ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Visible(is_now_visible));
                                        if is_now_visible {
                                            ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Focus);
                                        }
                                        ctx.request_repaint();
                                    }
                                }
                                TRAY_CMD_SAVE_REPLAY => {
                                    action_to_send = Some(UiAction::SaveReplay);
                                }
                                TRAY_CMD_EXIT => {
                                    log::info!("Tray context menu: Exit requested. Initiating full application shutdown...");
                                    ctx_guard.running.store(false, Ordering::SeqCst);
                                    if let Some(ctx) = ctx_guard.egui_ctx.lock().as_ref() {
                                        ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
                                        ctx.request_repaint();
                                    }
                                    crate::ui::close_overlay_window();
                                    action_to_send = Some(UiAction::QuitApplication);

                                    // Watchdog fallback: ensure process terminates cleanly within 2 seconds
                                    std::thread::spawn(|| {
                                        std::thread::sleep(std::time::Duration::from_millis(2000));
                                        log::info!("Shutdown watchdog: forcing process exit...");
                                        std::process::exit(0);
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                }
                if let Some(action) = action_to_send {
                    if let Some(ctx_guard) = TRAY_CTX.read().as_ref() {
                        let _ = ctx_guard.action_tx.send(action);
                    }
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(ctx_guard) = TRAY_CTX.write().as_mut() {
                if let Some(mut tray) = ctx_guard.tray_icon.take() {
                    tray.remove();
                }
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Spawns the dedicated background thread hosting the native Windows notification tray icon.
pub struct TrayManager {
    thread_handle: Option<JoinHandle<()>>,
    thread_id: u32,
}

impl TrayManager {
    /// Launches the system tray notification icon event loop on a dedicated thread.
    pub fn spawn(
        action_tx: Sender<UiAction>,
        egui_ctx: Arc<parking_lot::Mutex<Option<eframe::egui::Context>>>,
        ui_state: Option<SharedUiState>,
        running: Arc<AtomicBool>,
    ) -> Result<Self> {
        let (init_tx, init_rx) = crossbeam_channel::bounded::<Result<u32>>(1);

        let handle = std::thread::Builder::new()
            .name("omniclip-tray".to_string())
            .spawn(move || {
                let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
                let class_name = windows::core::w!("OmniClipTrayMsgWindow");

                let wc = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(tray_wnd_proc),
                    lpszClassName: class_name,
                    ..Default::default()
                };

                unsafe {
                    let _ = RegisterClassExW(&wc);
                }

                let hwnd = match unsafe {
                    CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        class_name,
                        windows::core::w!("OmniClipTrayHost"),
                        WS_OVERLAPPED,
                        0,
                        0,
                        0,
                        0,
                        HWND(std::ptr::null_mut()),
                        None,
                        None,
                        None,
                    )
                } {
                    Ok(h) => h,
                    Err(e) => {
                        let _ = init_tx.send(Err(anyhow!("Failed to create tray message window: {e}")));
                        return;
                    }
                };

                let tray_icon = match TrayIcon::new(hwnd, "OmniClip — Ultra-Lightweight Instant Replay") {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };

                *TRAY_CTX.write() = Some(TrayContext {
                    action_tx,
                    egui_ctx,
                    ui_state,
                    running: running.clone(),
                    tray_icon: Some(tray_icon),
                });

                let _ = init_tx.send(Ok(tid));

                let mut msg = MSG::default();
                while running.load(Ordering::Relaxed) {
                    let ret = unsafe { GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0) };
                    if ret.0 <= 0 || msg.message == WM_QUIT {
                        break;
                    }
                    unsafe {
                        DispatchMessageW(&msg);
                    }
                }

                if let Some(ctx_guard) = TRAY_CTX.write().as_mut() {
                    if let Some(mut tray) = ctx_guard.tray_icon.take() {
                        tray.remove();
                    }
                }
            })?;

        let thread_id = init_rx.recv()??;
        Ok(Self {
            thread_handle: Some(handle),
            thread_id,
        })
    }

    /// Stops the tray notification thread and removes the tray icon.
    pub fn stop(&mut self) {
        if self.thread_id != 0 {
            unsafe {
                let _ = PostThreadMessageW(
                    self.thread_id,
                    WM_QUIT,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            if let Some(handle) = self.thread_handle.take() {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for TrayManager {
    fn drop(&mut self) {
        self.stop();
    }
}

