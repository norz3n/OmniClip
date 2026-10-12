use std::mem::size_of;
use anyhow::{anyhow, Result};
use log::info;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW, SetForegroundWindow,
    TrackPopupMenu, HICON, IDI_APPLICATION, MF_SEPARATOR, MF_STRING, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON,
};

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
    pub fn show_context_menu(&self) -> Option<u32> {
        unsafe {
            let hmenu = CreatePopupMenu().ok()?;
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);

            // Populate menu items
            let s_overlay: Vec<u16> = "Открыть оверлей (Alt+Z)\0".encode_utf16().collect();
            let s_save: Vec<u16> = "Сохранить повтор (Alt+F10)\0".encode_utf16().collect();
            let s_exit: Vec<u16> = "Выход\0".encode_utf16().collect();

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
