use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use anyhow::Result;
use log::info;
use windows::Win32::Foundation::BOOL;

use omni_clip::audio::WasapiLoopbackCapture;
use omni_clip::buffer::{create_shared_ring_buffer, RingBufferSnapshot};
use omni_clip::capture::{CaptureStatus, D3D11Context, DxgiDuplicator};
use omni_clip::config::Config;
use omni_clip::encoder::{DxgiDeviceManager, H264Encoder, MfPlatformGuard};
use omni_clip::muxer::Mp4Muxer;
use omni_clip::platform::hotkey::HotkeyListener;
use omni_clip::platform::time::QpcTimer;
use omni_clip::ui::{create_shared_ui_state, OmniClipApp, UiAction};

/// Global shared run flag; also exported to the console handler via a static pointer.
/// Using Arc<AtomicBool> so the eframe app can read it on the main thread without unsafe.
static RUNNING_PTR: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();

/// Win32 console control event handler for clean shutdown on Ctrl+C / close.
unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> BOOL {
    match ctrl_type {
        windows::Win32::System::Console::CTRL_C_EVENT
        | windows::Win32::System::Console::CTRL_BREAK_EVENT
        | windows::Win32::System::Console::CTRL_CLOSE_EVENT => {
            log::info!("Termination signal received, exiting gracefully...");
            // Signal all threads and the egui window to close
            if let Some(arc) = RUNNING_PTR.get() {
                arc.store(false, Ordering::SeqCst);
            }
            BOOL(1)
        }
        _ => BOOL(0),
    }
}

fn main() -> Result<()> {
    // Initialize shared run flag before registering the console handler
    let running = Arc::new(AtomicBool::new(true));
    RUNNING_PTR.set(running.clone()).ok();

    unsafe {
        let _ = windows::Win32::System::Console::SetConsoleCtrlHandler(
            Some(console_ctrl_handler),
            BOOL(1),
        );
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    info!("=======================================================");
    info!("  OmniClip — Ultra-Lightweight Instant Replay System   ");
    info!("  egui / Material Design 3 Overlay Active              ");
    info!("=======================================================");

    let _mf_guard = MfPlatformGuard::init()?;
    let config = Config::default();

    let ring_buffer = create_shared_ring_buffer(
        config.buffer_duration_hns(),
        1920,
        1080,
        config.fps,
        config.audio_sample_rate,
        config.audio_channels,
    );

    // Audio capture channel
    let timer = QpcTimer::new()?;
    let (audio_tx, audio_rx) = crossbeam_channel::unbounded();
    let mut audio_capture = WasapiLoopbackCapture::spawn(
        timer.clone(),
        config.audio_bitrate,
        config.mic_enabled,
        config.mic_volume,
        audio_tx,
    )?;

    // Shared egui context handle to wake up the overlay event loop from background threads
    let egui_ctx_shared: Arc<parking_lot::Mutex<Option<eframe::egui::Context>>> =
        Arc::new(parking_lot::Mutex::new(None));
    let egui_ctx_writer = egui_ctx_shared.clone();
    let egui_ctx_hk = egui_ctx_shared.clone();

    // Channel to notify UI thread when disk writer saves a replay
    let (save_notify_tx, save_notify_rx) = crossbeam_channel::unbounded::<(PathBuf, u32, f64)>();

    // Background Disk Writer thread
    let (dump_tx, dump_rx) = crossbeam_channel::unbounded::<RingBufferSnapshot>();
    let output_dir_clone = config.output_dir.clone();
    let writer_handle = thread::Builder::new()
        .name("omniclip-disk-writer".to_string())
        .spawn(move || {
            while let Ok(snapshot) = dump_rx.recv() {
                let target_file = Mp4Muxer::generate_replay_filename(&output_dir_clone);
                info!("Disk Writer: Saving replay to '{}'...", target_file.display());
                match Mp4Muxer::write_replay(&snapshot, &target_file) {
                    Ok(path) => {
                        info!(">>> REPLAY SAVED SUCCESSFULLY: {} <<<", path.display());
                        let duration_secs = (snapshot.duration_hns() / 10_000_000) as u32;
                        let size_mb = std::fs::metadata(&path)
                            .map(|m| m.len() as f64 / (1024.0 * 1024.0))
                            .unwrap_or(0.0);
                        let _ = save_notify_tx.send((path, duration_secs, size_mb));
                        if let Some(ctx) = egui_ctx_writer.lock().as_ref() {
                            ctx.request_repaint();
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to save replay: {}", e);
                    }
                }
            }
        })?;

    // Global hotkey listener — single raw receiver
    let (hotkey_tx, hotkey_raw_rx) = crossbeam_channel::bounded(16);
    let hotkey_listener = match HotkeyListener::spawn(config.hotkey_modifiers, config.hotkey_vk, hotkey_tx) {
        Ok(l) => {
            info!("Hotkeys active: Alt+F10 (Save Replay), Alt+Z (Overlay Hub), Alt+F9 (Manual Record)");
            Some(l)
        }
        Err(e) => {
            log::warn!("Hotkey listener registration failed: {}", e);
            None
        }
    };

    // Shared UI state and action channel
    let ui_state = create_shared_ui_state(
        config.buffer_duration_secs,
        config.fps,
        config.video_bitrate,
        config.mic_enabled,
        config.mic_volume,
        config.output_dir.clone(),
    );
    let (ui_action_tx, ui_action_rx) = crossbeam_channel::unbounded::<UiAction>();

    // Overlay-only events for egui (ToggleOverlay goes here)
    let (_overlay_tx, overlay_rx) = crossbeam_channel::bounded::<omni_clip::platform::hotkey::HotkeyEvent>(16);

    // Hotkey dispatch thread: decouples hotkey processing from the egui event loop.
    // TriggerReplay and ToggleRecord are sent directly to the capture thread so they
    // work even when the egui window is hidden (Visible=false) and update() isn't called.
    let ui_action_tx_hk = ui_action_tx.clone();
    let ui_state_hk = ui_state.clone();
    let running_hk = running.clone();
    thread::Builder::new()
        .name("omniclip-hotkey-dispatch".to_string())
        .spawn(move || {
            use omni_clip::platform::hotkey::HotkeyEvent;
            while running_hk.load(std::sync::atomic::Ordering::Relaxed) {
                match hotkey_raw_rx.recv_timeout(std::time::Duration::from_millis(200)) {
                    Ok(HotkeyEvent::TriggerReplay) => {
                        info!("Hotkey dispatch: SaveReplay → capture thread");
                        ui_state_hk.write().is_saving_disk = true;
                        let _ = ui_action_tx_hk.send(UiAction::SaveReplay);
                        if let Some(ctx) = egui_ctx_hk.lock().as_ref() {
                            ctx.request_repaint();
                        }
                    }
                    Ok(HotkeyEvent::ToggleRecord) => {
                        info!("Hotkey dispatch: ToggleManualRecord → capture thread");
                        let active = {
                            let mut st = ui_state_hk.write();
                            st.manual_record_active = !st.manual_record_active;
                            st.manual_record_active
                        };
                        info!("Manual recording: {}", if active { "started" } else { "stopped" });
                        let _ = ui_action_tx_hk.send(UiAction::ToggleManualRecord);
                    }
                    Ok(HotkeyEvent::ToggleOverlay) => {
                        info!("Hotkey dispatch: ToggleOverlay");
                        let is_now_visible = omni_clip::ui::toggle_overlay_visible();
                        if let Some(ctx) = egui_ctx_hk.lock().as_ref() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(is_now_visible));
                            if is_now_visible {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                            }
                            ctx.request_repaint();
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                }
            }
        })?;

    // Dedicated Capture and Encoding Thread.
    // IMPORTANT: capture_start_rx gates DXGI Duplication initialization until AFTER eframe's
    // OpenGL context is created. eframe/glow internally acquires a DXGI device which would
    // otherwise cause DXGI_ERROR_ACCESS_LOST in an already-running DxgiDuplicator.
    let (capture_start_tx, capture_start_rx) = crossbeam_channel::bounded::<()>(1);
    let ring_buffer_capture = ring_buffer.clone();
    let dump_tx_capture = dump_tx.clone();
    let config_clone = config.clone();
    let ui_state_clone = ui_state.clone();
    let running_capture = running.clone();

    let capture_handle = thread::Builder::new()
        .name("omniclip-capture".to_string())
        .spawn(move || {
            let res: Result<()> = (|| {
                let _ = capture_start_rx.recv(); // wait for eframe GL context to be ready

                let timer = QpcTimer::new()?;
                let d3d11_context = D3D11Context::new()?;
                let mut duplicator = DxgiDuplicator::new(d3d11_context.clone(), 0, timer.clone())?;

                let width = duplicator.width();
                let height = duplicator.height();
                let fps = config_clone.fps;

                info!("Display capture ready: {}x{} @ {} fps", width, height, fps);
                let dev_manager = DxgiDeviceManager::new(d3d11_context.device())?;
                let mut encoder = H264Encoder::new(
                    &dev_manager,
                    width,
                    height,
                    fps,
                    config_clone.video_bitrate,
                    config_clone.gop_size,
                )?;

                let frame_duration_hns = config_clone.frame_duration_hns();
                let mut captured_frames = 0u64;
                let mut encoded_chunks = 0u64;

                while running_capture.load(Ordering::Relaxed) {
                    // Drain incoming audio packets into ring buffer
                    while let Ok(audio_chunk) = audio_rx.try_recv() {
                        ring_buffer_capture.lock().push_audio(audio_chunk);
                    }

                    // Handle user actions from UI
                    while let Ok(action) = ui_action_rx.try_recv() {
                        match action {
                            UiAction::SaveReplay => {
                                info!("UI requested replay dump!");
                                let mut rb = ring_buffer_capture.lock();
                                if let Some(snapshot) = rb.take_snapshot() {
                                    ui_state_clone.write().set_current_ram_bytes(0);
                                    let _ = dump_tx_capture.send(snapshot);
                                } else {
                                    log::warn!("Ring buffer not ready yet (waiting for keyframe)");
                                    ui_state_clone.write().is_saving_disk = false;
                                }
                            }
                            UiAction::SetBufferDuration(secs) => {
                                let new_hns = secs as i64 * 10_000_000i64;
                                ring_buffer_capture.lock().set_max_duration_hns(new_hns);
                            }
                            UiAction::QuitApplication => {
                                running_capture.store(false, Ordering::SeqCst);
                            }
                            _ => {}
                        }
                    }

                    // Update UI RAM usage every ~60 frames
                    if captured_frames % 60 == 0 {
                        let ram = ring_buffer_capture.lock().total_bytes();
                        ui_state_clone.write().set_current_ram_bytes(ram);
                    }

                    // Capture desktop frame
                    match duplicator.acquire_frame(16)? {
                        CaptureStatus::Frame(frame) => {
                            captured_frames += 1;
                            if let Ok(chunks) = encoder.poll_output() {
                                let mut rb = ring_buffer_capture.lock();
                                for chunk in chunks {
                                    encoded_chunks += 1;
                                    rb.push_video(chunk);
                                }
                            }
                            if encoder.can_feed() {
                                let _ = encoder.feed_frame(&frame, frame_duration_hns);
                            }
                        }
                        CaptureStatus::Timeout => {
                            if let Ok(chunks) = encoder.poll_output() {
                                let mut rb = ring_buffer_capture.lock();
                                for chunk in chunks {
                                    encoded_chunks += 1;
                                    rb.push_video(chunk);
                                }
                            }
                        }
                        CaptureStatus::AccessLost => {
                            thread::sleep(Duration::from_millis(100));
                            let _ = duplicator.init_duplication();
                        }
                        CaptureStatus::DeviceRemoved => {
                            log::error!("D3D11 device removed");
                            break;
                        }
                    }
                }

                info!(
                    "Capture thread finished: {} frames captured, {} chunks encoded",
                    captured_frames, encoded_chunks
                );
                Ok(())
            })();

            if let Err(e) = res {
                log::error!("Capture thread encountered error: {}", e);
            }
        })?;

    // Determine screen dimensions for borderless overlay window.
    // Using GetSystemMetrics is safe here since we are on the main Win32 thread.
    let (screen_w, screen_h) = unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
        (GetSystemMetrics(SM_CXSCREEN) as f32, GetSystemMetrics(SM_CYSCREEN) as f32)
    };

    // Center a 960x560 Material Design 3 Control Hub on the primary monitor.
    let win_w = 960.0;
    let win_h = 560.0;
    let pos_x = ((screen_w - win_w) / 2.0).max(0.0);
    let pos_y = ((screen_h - win_h) / 2.0).max(0.0);

    // Launch egui / eframe Material Design 3 Overlay on Main Thread
    info!("Starting egui Material Design 3 Overlay application...");
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("OmniClip Overlay")
            .with_decorations(false)
            .with_transparent(false)
            .with_always_on_top()
            .with_visible(false)
            .with_inner_size([win_w, win_h])
            .with_position([pos_x, pos_y]),
        ..Default::default()
    };

    let app = OmniClipApp::new(
        ui_state,
        ui_action_tx,
        overlay_rx,
        save_notify_rx,
        running.clone(),
    );

    let egui_ctx_init = egui_ctx_shared.clone();
    let run_res = eframe::run_native(
        "OmniClip",
        native_options,
        Box::new(move |cc| {
            *egui_ctx_init.lock() = Some(cc.egui_ctx.clone());
            // Immediately discover HWND and hide overlay window so it runs silently in background
            omni_clip::ui::init_overlay_window();
            // GL context is now fully initialized — safe to start DXGI Duplication.
            let _ = capture_start_tx.send(());
            Ok(Box::new(app))
        }),
    );

    if let Err(e) = run_res {
        log::error!("eframe run_native error: {}", e);
    }

    // Clean application shutdown
    running.store(false, Ordering::SeqCst);
    if let Some(mut h) = hotkey_listener {
        h.stop();
    }
    audio_capture.stop();
    drop(dump_tx);
    let _ = capture_handle.join();
    let _ = writer_handle.join();

    info!("OmniClip shutdown complete.");
    Ok(())
}
