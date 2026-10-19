use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use anyhow::{anyhow, Result};
use crossbeam_channel::Sender;
use log::{info, warn};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PostThreadMessageW,
    TranslateMessage, MSG, WM_HOTKEY, WM_QUIT,
};

/// Hotkey identification IDs.
const OMNICLIP_HOTKEY_REPLAY_ID: i32 = 0x4F43; // 'OC' (Alt+F10)
const OMNICLIP_HOTKEY_OVERLAY_ID: i32 = 0x4F5A; // 'OZ' (Alt+Z)
const OMNICLIP_HOTKEY_RECORD_ID: i32 = 0x4F52;  // 'OR' (Alt+F9)
const OMNICLIP_HOTKEY_SCREENSHOT_ID: i32 = 0x4F53; // 'OS' (Alt+F1)

/// Event emitted when a global hotkey is triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// Trigger circular buffer replay dump to disk (Alt+F10).
    TriggerReplay,
    /// Toggle visibility of Material 3 control overlay (Alt+Z).
    ToggleOverlay,
    /// Toggle continuous manual recording mode (Alt+F9).
    ToggleRecord,
    /// Capture instant screenshot of current display (Alt+F1).
    TakeScreenshot,
}

/// Global hotkey listener running a Win32 message loop on a dedicated thread.
pub struct HotkeyListener {
    running: Arc<AtomicBool>,
    thread_handle: Option<JoinHandle<()>>,
    thread_id: Arc<std::sync::atomic::AtomicU32>,
}

impl HotkeyListener {
    /// Spawns the hotkey listener thread.
    ///
    /// * `modifiers`: Bitflag modifiers for replay trigger (e.g. MOD_ALT | MOD_NOREPEAT)
    /// * `vk_code`: Virtual-key code for replay trigger (e.g. 0x79 for VK_F10)
    /// * `sender`: Channel sender to notify when hotkey is pressed
    pub fn spawn(
        modifiers: HOT_KEY_MODIFIERS,
        replay_vk: u32,
        overlay_vk: u32,
        record_vk: u32,
        screenshot_vk: u32,
        sender: Sender<HotkeyEvent>,
    ) -> Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let thread_id = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let thread_id_clone = thread_id.clone();

        let (init_tx, init_rx) = crossbeam_channel::bounded::<Result<()>>(1);

        let handle = thread::Builder::new()
            .name("omniclip-hotkey".to_string())
            .spawn(move || {
                let tid = unsafe { GetCurrentThreadId() };
                thread_id_clone.store(tid, Ordering::SeqCst);

                // Register global hotkey for replay buffer (Alt+F10 default)
                if let Err(e) = unsafe {
                    RegisterHotKey(
                        HWND(std::ptr::null_mut()),
                        OMNICLIP_HOTKEY_REPLAY_ID,
                        modifiers | MOD_NOREPEAT,
                        replay_vk,
                    )
                } {
                    warn!("Replay hotkey (0x{:X}) registration failed (possibly used by GeForce Experience/ShadowPlay): {}", replay_vk, e);
                }

                // Register global hotkey for overlay toggle (Alt+Z default)
                if let Err(e) = unsafe {
                    RegisterHotKey(
                        HWND(std::ptr::null_mut()),
                        OMNICLIP_HOTKEY_OVERLAY_ID,
                        modifiers | MOD_NOREPEAT,
                        overlay_vk,
                    )
                } {
                    warn!("Overlay hotkey (0x{:X}) registration failed: {}", overlay_vk, e);
                }

                // Register global hotkey for manual record toggle (Alt+F9 default)
                if let Err(e) = unsafe {
                    RegisterHotKey(
                        HWND(std::ptr::null_mut()),
                        OMNICLIP_HOTKEY_RECORD_ID,
                        modifiers | MOD_NOREPEAT,
                        record_vk,
                    )
                } {
                    warn!("Record hotkey (0x{:X}) registration failed: {}", record_vk, e);
                }

                // Register global hotkey for screenshot capture (Alt+F1 default)
                if let Err(e) = unsafe {
                    RegisterHotKey(
                        HWND(std::ptr::null_mut()),
                        OMNICLIP_HOTKEY_SCREENSHOT_ID,
                        modifiers | MOD_NOREPEAT,
                        screenshot_vk,
                    )
                } {
                    warn!("Screenshot hotkey (0x{:X}) registration failed: {}", screenshot_vk, e);
                }

                let _ = init_tx.send(Ok(()));
                info!(
                    "Hotkey registration complete: Replay (0x{:X}), Overlay (0x{:X}), Record (0x{:X}), Screenshot (0x{:X})",
                    replay_vk, overlay_vk, record_vk, screenshot_vk
                );

                let mut msg = MSG::default();
                while running_clone.load(Ordering::SeqCst) {
                    let ret = unsafe { GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0) };
                    if ret.0 <= 0 || msg.message == WM_QUIT {
                        break;
                    }

                    if msg.message == WM_HOTKEY {
                        let hotkey_id = msg.wParam.0 as i32;
                        let event = match hotkey_id {
                            OMNICLIP_HOTKEY_REPLAY_ID => Some(HotkeyEvent::TriggerReplay),
                            OMNICLIP_HOTKEY_OVERLAY_ID => Some(HotkeyEvent::ToggleOverlay),
                            OMNICLIP_HOTKEY_RECORD_ID => Some(HotkeyEvent::ToggleRecord),
                            OMNICLIP_HOTKEY_SCREENSHOT_ID => Some(HotkeyEvent::TakeScreenshot),
                            _ => None,
                        };

                        if let Some(ev) = event {
                            info!("Global hotkey pressed: {:?} (ID: 0x{:X})", ev, hotkey_id);
                            if let Err(e) = sender.send(ev) {
                                warn!("Failed to dispatch hotkey event: {}", e);
                            }
                        }
                    }

                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }

                // Cleanup hotkey registrations
                unsafe {
                    let _ = UnregisterHotKey(HWND(std::ptr::null_mut()), OMNICLIP_HOTKEY_REPLAY_ID);
                    let _ = UnregisterHotKey(HWND(std::ptr::null_mut()), OMNICLIP_HOTKEY_OVERLAY_ID);
                    let _ = UnregisterHotKey(HWND(std::ptr::null_mut()), OMNICLIP_HOTKEY_RECORD_ID);
                    let _ = UnregisterHotKey(HWND(std::ptr::null_mut()), OMNICLIP_HOTKEY_SCREENSHOT_ID);
                }
                info!("Unregistered all global hotkeys");
            })
            .map_err(|e| anyhow!("Failed to spawn hotkey thread: {}", e))?;

        // Wait for thread initialization
        init_rx
            .recv()
            .map_err(|e| anyhow!("Hotkey initialization failed to receive confirmation: {}", e))??;

        Ok(Self {
            running,
            thread_handle: Some(handle),
            thread_id,
        })
    }

    /// Stops the hotkey listener thread gracefully.
    pub fn stop(&mut self) {
        if self.running.swap(false, Ordering::SeqCst) {
            let tid = self.thread_id.load(Ordering::SeqCst);
            if tid != 0 {
                unsafe {
                    let _ = PostThreadMessageW(tid, WM_QUIT, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0));
                }
            }
            if let Some(handle) = self.thread_handle.take() {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        self.stop();
    }
}
