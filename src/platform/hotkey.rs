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

/// Hotkey identification ID.
const OMNICLIP_HOTKEY_ID: i32 = 0x4F43; // 'OC'

/// Event emitted when the replay hotkey is triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    TriggerReplay,
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
    /// * `modifiers`: Bitflag modifiers (e.g. MOD_ALT | MOD_NOREPEAT)
    /// * `vk_code`: Virtual-key code (e.g. 0x79 for VK_F10)
    /// * `sender`: Channel sender to notify when hotkey is pressed
    pub fn spawn(
        modifiers: HOT_KEY_MODIFIERS,
        vk_code: u32,
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

                // Register global hotkey for this thread
                let reg_res = unsafe {
                    RegisterHotKey(
                        HWND(std::ptr::null_mut()),
                        OMNICLIP_HOTKEY_ID,
                        modifiers | MOD_NOREPEAT,
                        vk_code,
                    )
                };

                if let Err(e) = reg_res {
                    let _ = init_tx.send(Err(anyhow!("Failed to register hotkey: {}", e)));
                    return;
                }

                let _ = init_tx.send(Ok(()));
                info!("Registered global hotkey (ID: 0x{:X})", OMNICLIP_HOTKEY_ID);

                let mut msg = MSG::default();
                while running_clone.load(Ordering::SeqCst) {
                    let ret = unsafe { GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0) };
                    if ret.0 <= 0 || msg.message == WM_QUIT {
                        break;
                    }

                    if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == OMNICLIP_HOTKEY_ID {
                        info!("Global hotkey pressed! Sending trigger event...");
                        if let Err(e) = sender.send(HotkeyEvent::TriggerReplay) {
                            warn!("Failed to dispatch hotkey event: {}", e);
                        }
                    }

                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }

                // Cleanup hotkey registration
                unsafe {
                    let _ = UnregisterHotKey(HWND(std::ptr::null_mut()), OMNICLIP_HOTKEY_ID);
                }
                info!("Unregistered global hotkey");
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
