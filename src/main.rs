#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use anyhow::Result;
use log::info;
use windows::Win32::Foundation::BOOL;

use omni_clip::audio::WasapiLoopbackCapture;
use omni_clip::buffer::{
    create_shared_ring_buffer, ManualRecordSession, RingBufferSnapshot,
};
use omni_clip::capture::{CaptureStatus, D3D11Context, DxgiDuplicator};
use omni_clip::config::Config;
use omni_clip::encoder::{DxgiDeviceManager, H264Encoder, MfPlatformGuard};
use omni_clip::muxer::Mp4Muxer;
use omni_clip::platform::hotkey::HotkeyListener;
use omni_clip::platform::time::QpcTimer;
use omni_clip::ui::{create_shared_ui_state, Language, OmniClipApp, TextKey, t, UiAction};

/// Global shared run flag; also exported to the console handler via a static pointer.
/// Using Arc<AtomicBool> so the eframe app can read it on the main thread without unsafe.
static RUNNING_PTR: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
static EGUI_CTX_PTR: std::sync::OnceLock<Arc<parking_lot::Mutex<Option<eframe::egui::Context>>>> =
    std::sync::OnceLock::new();
static MAIN_THREAD_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static CTRL_C_COUNT: AtomicBool = AtomicBool::new(false);

/// Win32 console control event handler for clean shutdown on Ctrl+C / close.
unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> BOOL {
    match ctrl_type {
        windows::Win32::System::Console::CTRL_C_EVENT
        | windows::Win32::System::Console::CTRL_BREAK_EVENT
        | windows::Win32::System::Console::CTRL_CLOSE_EVENT => {
            if CTRL_C_COUNT.swap(true, Ordering::SeqCst) {
                log::warn!("Repeated termination signal received, forcing immediate exit...");
                std::process::exit(0);
            }

            log::info!("Termination signal received, exiting gracefully...");

            // 1. Signal all worker threads to stop
            if let Some(arc) = RUNNING_PTR.get() {
                arc.store(false, Ordering::SeqCst);
            }

            // 2. Request viewport closure and wake up the winit/egui event loop
            if let Some(ctx_lock) = EGUI_CTX_PTR.get() {
                if let Some(ctx) = ctx_lock.lock().as_ref() {
                    ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
                    ctx.request_repaint();
                }
            }

            // 3. Post WM_CLOSE directly to the overlay window handle
            omni_clip::ui::close_overlay_window();

            // 4. Post WM_QUIT to the main thread's Win32 message loop to instantly break out of eframe
            let main_tid = MAIN_THREAD_ID.load(Ordering::SeqCst);
            if main_tid != 0 {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    main_tid,
                    windows::Win32::UI::WindowsAndMessaging::WM_QUIT,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                );
            }

            // PONYTAIL: 2.5-second watchdog fallback for graceful shutdown. Ceiling: if driver unload or COM release hangs indefinitely, process terminates forcefully after timeout. Upgrade path: asynchronous non-blocking resource cleanup with cooperative driver cancellation.
            thread::spawn(|| {
                thread::sleep(Duration::from_millis(2500));
                log::warn!("Graceful shutdown timed out, forcing process exit...");
                std::process::exit(0);
            });

            BOOL(1)
        }
        _ => BOOL(0),
    }
}

fn main() -> Result<()> {
    // Record main Win32 thread ID for console handler dispatch
    let main_tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    MAIN_THREAD_ID.store(main_tid, Ordering::SeqCst);

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
    let config = Config::load();
    let config_shared = Arc::new(parking_lot::Mutex::new(config.clone()));

    // Shared UI state initialized with detected/configured language
    let initial_lang = Language::from_code(&config.language);
    let ui_state = create_shared_ui_state(
        config.buffer_duration_secs,
        config.fps,
        config.video_bitrate,
        config.mic_enabled,
        config.mic_volume,
        config.output_dir.clone(),
        initial_lang,
    );
    {
        let mut st = ui_state.write();
        st.filename_pattern = config.filename_pattern.clone();
        st.audio_multi_track = config.audio_multi_track;
        st.video_codec = config.video_codec;
        st.selected_display_index = config.display_index;
        st.hotkey_replay_vk = config.hotkey_vk;
        st.hotkey_overlay_vk = config.hotkey_overlay_vk;
        st.hotkey_record_vk = config.hotkey_record_vk;
        st.hotkey_screenshot_vk = config.hotkey_screenshot_vk;
    }

    let ring_buffer = create_shared_ring_buffer(
        config.buffer_duration_hns(),
        1920,
        1080,
        config.fps,
        config.audio_sample_rate,
        config.audio_channels,
    );
    ring_buffer.lock().set_codec(config.video_codec);

    // Audio capture channel
    let timer = QpcTimer::new()?;
    let (audio_tx, audio_rx) = crossbeam_channel::unbounded();
    let audio_capture = Arc::new(parking_lot::Mutex::new(WasapiLoopbackCapture::spawn(
        timer.clone(),
        config.audio_bitrate,
        config.mic_enabled,
        config.mic_volume,
        None,
        None,
        config.audio_multi_track,
        audio_tx,
    )?));
    let audio_capture_worker = audio_capture.clone();

    // Shared egui context handle to wake up the overlay event loop from background threads
    let egui_ctx_shared: Arc<parking_lot::Mutex<Option<eframe::egui::Context>>> =
        Arc::new(parking_lot::Mutex::new(None));
    EGUI_CTX_PTR.set(egui_ctx_shared.clone()).ok();
    let egui_ctx_writer = egui_ctx_shared.clone();
    let egui_ctx_hk = egui_ctx_shared.clone();

    // Channel to notify UI thread when disk writer saves a replay
    let (save_notify_tx, save_notify_rx) = crossbeam_channel::unbounded::<(PathBuf, u32, f64)>();

    // Spawn dedicated on-screen HUD toast overlay thread
    if let Err(e) = omni_clip::ui::spawn_hud_toast_thread() {
        log::warn!("Failed to spawn HUD toast thread: {}", e);
    }

    // Background Disk Writer thread with dynamic user-configured destination path
    let (dump_tx, dump_rx) = crossbeam_channel::unbounded::<(RingBufferSnapshot, PathBuf)>();
    let save_notify_tx_writer = save_notify_tx.clone();
    let ui_state_writer = ui_state.clone();
    let writer_handle = thread::Builder::new()
        .name("omniclip-disk-writer".to_string())
        .spawn(move || {
            while let Ok((snapshot, target_file)) = dump_rx.recv() {
                info!("Disk Writer: Saving replay to '{}'...", target_file.display());
                match Mp4Muxer::write_replay(&snapshot, &target_file) {
                    Ok(path) => {
                        info!(">>> REPLAY SAVED SUCCESSFULLY: {} <<<", path.display());
                        let duration_secs = (snapshot.duration_hns() / 10_000_000) as u32;
                        let size_mb = std::fs::metadata(&path)
                            .map(|m| m.len() as f64 / (1024.0 * 1024.0))
                            .unwrap_or(0.0);
                        let _ = save_notify_tx_writer.send((path.clone(), duration_secs, size_mb));
                        let lang = ui_state_writer.read().language;
                        omni_clip::ui::show_hud_toast(path, duration_secs, size_mb, lang);
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
    let hotkey_listener = match HotkeyListener::spawn(
        config.hotkey_modifiers,
        config.hotkey_vk,
        config.hotkey_overlay_vk,
        config.hotkey_record_vk,
        config.hotkey_screenshot_vk,
        hotkey_tx,
    ) {
        Ok(l) => {
            info!("Hotkeys active: Alt+F10 (Save Replay), Alt+Z (Overlay Hub), Alt+F9 (Manual Record), Alt+F1 (Screenshot)");
            Some(l)
        }
        Err(e) => {
            log::warn!("Hotkey listener registration failed: {}", e);
            None
        }
    };

    // Action channel for communicating UI state changes to capture thread
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
                        let _ = ui_action_tx_hk.send(UiAction::ToggleManualRecord);
                        if let Some(ctx) = egui_ctx_hk.lock().as_ref() {
                            ctx.request_repaint();
                        }
                    }
                    Ok(HotkeyEvent::TakeScreenshot) => {
                        info!("Hotkey dispatch: TakeScreenshot → capture thread");
                        let _ = ui_action_tx_hk.send(UiAction::TakeScreenshot);
                        if let Some(ctx) = egui_ctx_hk.lock().as_ref() {
                            ctx.request_repaint();
                        }
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
    let save_notify_tx_capture = save_notify_tx.clone();
    let config_clone = config.clone();
    let config_capture = config_shared.clone();
    let ui_state_clone = ui_state.clone();
    let running_capture = running.clone();
    let egui_ctx_capture = egui_ctx_shared.clone();

    let capture_handle = thread::Builder::new()
        .name("omniclip-capture".to_string())
        .spawn(move || {
            let res: Result<()> = (|| {
                let _ = capture_start_rx.recv(); // wait for eframe GL context to be ready

                let timer = QpcTimer::new()?;
                let d3d11_context = D3D11Context::new()?;
                let displays = DxgiDuplicator::enumerate_adapter_displays(&d3d11_context);
                info!("DXGI: Found {} connected display outputs", displays.len());
                let mut active_codec = config_clone.video_codec;
                {
                    let mut st = ui_state_clone.write();
                    st.available_displays = displays;
                    st.selected_display_index = config_clone.display_index;
                    st.video_codec = active_codec;
                }

                let mut duplicator = DxgiDuplicator::new(d3d11_context.clone(), config_clone.display_index, timer.clone())?;

                let mut width = duplicator.width();
                let mut height = duplicator.height();
                let mut fps = config_clone.fps;

                info!("Display capture ready: {}x{} @ {} fps (codec: {:?})", width, height, fps, active_codec);
                let dev_manager = DxgiDeviceManager::new(d3d11_context.device())?;
                let mut encoder = H264Encoder::new(
                    &dev_manager,
                    width,
                    height,
                    fps,
                    config_clone.video_bitrate,
                    config_clone.gop_size,
                    active_codec,
                )?;

                let mut frame_duration_hns = config_clone.frame_duration_hns();
                let mut captured_frames = 0u64;
                let mut encoded_chunks = 0u64;
                let mut manual_session: Option<ManualRecordSession> = None;
                let mut last_frame: Option<omni_clip::capture::CapturedFrame> = None;

                while running_capture.load(Ordering::Relaxed) {
                    // Drain incoming audio packets into ring buffer and manual recording session
                    while let Ok(audio_chunk) = audio_rx.try_recv() {
                        if let Some(ref mut session) = manual_session {
                            session.push_audio(audio_chunk.clone());
                        }
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
                                    let (pattern, out_dir) = {
                                        let st = ui_state_clone.read();
                                        (st.filename_pattern.clone(), st.output_dir.clone())
                                    };
                                    let target_file = Mp4Muxer::format_replay_filename(
                                        &pattern,
                                        &out_dir,
                                        snapshot.width,
                                        snapshot.height,
                                        snapshot.fps,
                                    );
                                    let _ = dump_tx_capture.send((snapshot, target_file));
                                    encoder.request_keyframe();
                                } else {
                                    log::warn!("Ring buffer not ready yet (waiting for keyframe)");
                                    ui_state_clone.write().is_saving_disk = false;
                                }
                            }
                            UiAction::ToggleManualRecord => {
                                if manual_session.is_none() {
                                    // Start manual recording
                                    let (pattern, out_dir) = {
                                        let st = ui_state_clone.read();
                                        (st.filename_pattern.clone(), st.output_dir.clone())
                                    };
                                    let rec_pattern = if pattern.starts_with("Replay_") {
                                        pattern.replacen("Replay_", "Record_", 1)
                                    } else {
                                        format!("Record_{}", pattern)
                                    };
                                    let target_file = Mp4Muxer::format_replay_filename(
                                        &rec_pattern,
                                        &out_dir,
                                        width,
                                        height,
                                        fps,
                                    );
                                    info!("Capture thread: Starting manual recording -> '{}'", target_file.display());
                                    manual_session = Some(ManualRecordSession::new(target_file, active_codec));
                                    {
                                        let mut st = ui_state_clone.write();
                                        st.manual_record_active = true;
                                        st.manual_record_start = Some(Instant::now());
                                    }
                                    if let Some(ctx) = egui_ctx_capture.lock().as_ref() {
                                        ctx.request_repaint();
                                    }
                                    encoder.request_keyframe();
                                } else {
                                    // Stop manual recording
                                    info!("Capture thread: Stopping manual recording...");
                                    {
                                        let mut st = ui_state_clone.write();
                                        st.manual_record_active = false;
                                        st.manual_record_start = None;
                                    }
                                    if let Some(ctx) = egui_ctx_capture.lock().as_ref() {
                                        ctx.request_repaint();
                                    }
                                    if let Some(session) = manual_session.take() {
                                        if let Some((snapshot, target_file)) = session.finish(
                                            width,
                                            height,
                                            fps,
                                            config_clone.audio_sample_rate,
                                            config_clone.audio_channels,
                                        ) {
                                            info!(
                                                "Manual recording finished: {} video chunks ({:.1}s), {} audio chunks. Dispatching to disk writer...",
                                                snapshot.video_chunks.len(),
                                                snapshot.duration_hns() as f64 / 10_000_000.0,
                                                snapshot.audio_chunks.len(),
                                            );
                                            let _ = dump_tx_capture.send((snapshot, target_file));
                                        } else {
                                            log::warn!("Manual recording ended without video keyframes");
                                        }
                                    }
                                }
                            }
                            UiAction::SetBufferDuration(secs) => {
                                let new_hns = secs as i64 * 10_000_000i64;
                                ring_buffer_capture.lock().set_max_duration_hns(new_hns);
                                config_capture.lock().buffer_duration_secs = secs;
                                let _ = config_capture.lock().save();
                            }
                            UiAction::SetFps(new_fps) => {
                                fps = new_fps;
                                frame_duration_hns = 10_000_000i64 / new_fps as i64;
                                config_capture.lock().fps = new_fps;
                                let _ = config_capture.lock().save();
                                info!("Dynamic FPS changed to {} (frame duration {} hns)", new_fps, frame_duration_hns);
                            }
                            UiAction::SetBitrate(mbps) => {
                                encoder.set_bitrate(mbps * 1_000_000);
                                config_capture.lock().video_bitrate = mbps * 1_000_000;
                                let _ = config_capture.lock().save();
                                info!("Dynamic video bitrate changed to {} Mbps", mbps);
                            }
                            UiAction::ToggleMic => {
                                let is_enabled = audio_capture_worker.lock().toggle_mic();
                                config_capture.lock().mic_enabled = is_enabled;
                                let _ = config_capture.lock().save();
                                info!("Microphone dynamically toggled: {}", if is_enabled { "UNMUTED" } else { "MUTED" });
                            }
                            UiAction::SetMicVolume(vol) => {
                                audio_capture_worker.lock().set_mic_volume(vol);
                                config_capture.lock().mic_volume = vol;
                                let _ = config_capture.lock().save();
                                info!("Microphone dynamic volume set to: {:.2}", vol);
                            }
                            UiAction::SetOutputDir(dir) => {
                                config_capture.lock().output_dir = dir;
                                let _ = config_capture.lock().save();
                            }
                            UiAction::SetFilenamePattern(pat) => {
                                config_capture.lock().filename_pattern = pat;
                                let _ = config_capture.lock().save();
                            }
                            UiAction::SetAudioMultiTrack(multi) => {
                                config_capture.lock().audio_multi_track = multi;
                                let _ = config_capture.lock().save();
                            }
                            UiAction::SetLanguage(code) => {
                                config_capture.lock().language = code;
                                let _ = config_capture.lock().save();
                            }
                            UiAction::TakeScreenshot => {
                                if let Some(ref frame) = last_frame {
                                    let out_dir = config_capture.lock().output_dir.clone();
                                    let shot_dir = out_dir.join("Screenshots");
                                    let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
                                    let shot_filename = format!(
                                        "Screenshot_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}.bmp",
                                        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
                                    );
                                    let shot_path = shot_dir.join(shot_filename);
                                    match omni_clip::capture::save_screenshot_bmp(&d3d11_context, frame, &shot_path) {
                                        Ok(()) => {
                                            info!(">>> SCREENSHOT SAVED: {} <<<", shot_path.display());
                                            let size_mb = std::fs::metadata(&shot_path)
                                                .map(|m| m.len() as f64 / (1024.0 * 1024.0))
                                                .unwrap_or(0.0);
                                            let _ = save_notify_tx_capture.send((shot_path.clone(), 0, size_mb));
                                            let lang = ui_state_clone.read().language;
                                            omni_clip::ui::show_hud_toast(shot_path.clone(), 0, size_mb, lang);
                                            {
                                                let mut st = ui_state_clone.write();
                                                st.push_toast(t(TextKey::ToastScreenshotSaved, lang), shot_path, 0, size_mb);
                                            }
                                            if let Some(ctx) = egui_ctx_capture.lock().as_ref() {
                                                ctx.request_repaint();
                                            }
                                        }
                                        Err(e) => {
                                            log::error!("Failed to save screenshot: {}", e);
                                        }
                                    }
                                } else {
                                    log::warn!("Screenshot requested, but no desktop frame has been captured yet");
                                }
                            }
                            UiAction::SetVideoCodec(new_codec) => {
                                if new_codec != active_codec {
                                    info!("Capture thread: Switching hardware video codec to {:?}", new_codec);
                                    match H264Encoder::new(
                                        &dev_manager,
                                        width,
                                        height,
                                        fps,
                                        config_capture.lock().video_bitrate,
                                        config_capture.lock().gop_size,
                                        new_codec,
                                    ) {
                                        Ok(new_enc) => {
                                            encoder = new_enc;
                                            active_codec = new_codec;
                                            ring_buffer_capture.lock().set_codec(new_codec);
                                            config_capture.lock().video_codec = new_codec;
                                            let _ = config_capture.lock().save();
                                            ui_state_clone.write().video_codec = new_codec;
                                            encoder.request_keyframe();
                                            info!("Hardware video codec switched successfully to {:?}", new_codec);
                                        }
                                        Err(e) => {
                                            log::error!("Failed to switch video codec to {:?}: {}", new_codec, e);
                                        }
                                    }
                                }
                            }
                            UiAction::SetDisplayIndex(new_idx) => {
                                info!("Capture thread: Switching capture display to output {}", new_idx);
                                match duplicator.switch_display(new_idx) {
                                    Ok(()) => {
                                        let new_w = duplicator.width();
                                        let new_h = duplicator.height();
                                        config_capture.lock().display_index = new_idx;
                                        let _ = config_capture.lock().save();
                                        ui_state_clone.write().selected_display_index = new_idx;
                                        if new_w != width || new_h != height {
                                            width = new_w;
                                            height = new_h;
                                            ring_buffer_capture.lock().update_video_format(width, height, fps);
                                            match H264Encoder::new(
                                                &dev_manager,
                                                width,
                                                height,
                                                fps,
                                                config_capture.lock().video_bitrate,
                                                config_capture.lock().gop_size,
                                                active_codec,
                                            ) {
                                                Ok(new_enc) => {
                                                    encoder = new_enc;
                                                    encoder.request_keyframe();
                                                    info!("Encoder adapted to display resolution: {}x{}", width, height);
                                                }
                                                Err(e) => {
                                                    log::error!("Failed to re-initialize encoder for {}x{}: {}", width, height, e);
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        log::error!("Failed to switch to display {}: {}", new_idx, e);
                                    }
                                }
                            }
                            UiAction::QuitApplication => {
                                running_capture.store(false, Ordering::SeqCst);
                                if let Some(ctx) = egui_ctx_capture.lock().as_ref() {
                                    ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
                                    ctx.request_repaint();
                                }
                                omni_clip::ui::close_overlay_window();
                            }
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
                            last_frame = Some(frame.clone());
                            if let Ok(chunks) = encoder.poll_output() {
                                let mut rb = ring_buffer_capture.lock();
                                for chunk in chunks {
                                    encoded_chunks += 1;
                                    if let Some(ref mut session) = manual_session {
                                        session.push_video(chunk.clone());
                                    }
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
                                    if let Some(ref mut session) = manual_session {
                                        session.push_video(chunk.clone());
                                    }
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

                // If manual recording was active when shutting down, save it
                if let Some(session) = manual_session.take() {
                    {
                        let mut st = ui_state_clone.write();
                        st.manual_record_active = false;
                        st.manual_record_start = None;
                    }
                    if let Some((snapshot, target_file)) = session.finish(
                        width,
                        height,
                        fps,
                        config_clone.audio_sample_rate,
                        config_clone.audio_channels,
                    ) {
                        info!("Saving in-progress manual recording on capture thread exit...");
                        let _ = dump_tx_capture.send((snapshot, target_file));
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

    // Center a spacious 940x620 Material Design 3 Control Hub on the primary monitor.
    let win_w = 940.0;
    let win_h = 620.0;
    let pos_x = ((screen_w - win_w) / 2.0).max(0.0);
    let pos_y = ((screen_h - win_h) / 2.0).max(0.0);

    // Launch egui / eframe Material Design 3 Overlay on Main Thread
    info!("Starting egui Material Design 3 Overlay application...");
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("OmniClip Overlay")
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_visible(false)
            .with_drag_and_drop(false)
            .with_inner_size([win_w, win_h])
            .with_position([pos_x, pos_y])
            .with_icon(omni_clip::ui::app_icon()),
        ..Default::default()
    };

    let app = OmniClipApp::new(
        ui_state.clone(),
        ui_action_tx.clone(),
        overlay_rx,
        save_notify_rx,
        running.clone(),
    );

    let tray_manager = match omni_clip::ui::TrayManager::spawn(
        ui_action_tx,
        egui_ctx_shared.clone(),
        Some(ui_state.clone()),
        running.clone(),
    ) {
        Ok(t) => {
            info!("System tray notification icon initialized successfully");
            Some(t)
        }
        Err(e) => {
            log::warn!("Failed to initialize system tray icon: {}", e);
            None
        }
    };

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
    if let Some(mut tray) = tray_manager {
        tray.stop();
    }
    if let Some(mut h) = hotkey_listener {
        h.stop();
    }
    audio_capture.lock().stop();
    drop(dump_tx);
    let _ = capture_handle.join();
    let _ = writer_handle.join();

    info!("OmniClip shutdown complete.");
    std::process::exit(0);
}
