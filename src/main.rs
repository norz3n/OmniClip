use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use anyhow::Result;
use log::info;

use omni_clip::audio::WasapiLoopbackCapture;
use omni_clip::buffer::{create_shared_ring_buffer, RingBufferSnapshot};
use omni_clip::capture::{CaptureStatus, D3D11Context, DxgiDuplicator};
use omni_clip::config::Config;
use omni_clip::encoder::{DxgiDeviceManager, H264Encoder, MfPlatformGuard};
use omni_clip::muxer::Mp4Muxer;
use omni_clip::platform::hotkey::{HotkeyEvent, HotkeyListener};
use omni_clip::platform::time::QpcTimer;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    info!("=======================================================");
    info!("  OmniClip — Ultra-Lightweight Instant Replay System   ");
    info!("=======================================================");

    let _mf_guard = MfPlatformGuard::init()?;
    let config = Config::default();

    let timer = QpcTimer::new()?;
    let d3d11_context = D3D11Context::new()?;
    let mut duplicator = DxgiDuplicator::new(d3d11_context.clone(), 0, timer.clone())?;

    let width = duplicator.width();
    let height = duplicator.height();
    let fps = config.fps;

    info!("Display: {}x{} @ {} fps", width, height, fps);
    info!(
        "Replay buffer: {}s (target video bitrate: {} Mbps, GOP: {})",
        config.buffer_duration_secs,
        config.video_bitrate / 1_000_000,
        config.gop_size
    );

    let dev_manager = DxgiDeviceManager::new(d3d11_context.device())?;
    let mut encoder = H264Encoder::new(
        &dev_manager,
        width,
        height,
        fps,
        config.video_bitrate,
        config.gop_size,
    )?;

    let ring_buffer = create_shared_ring_buffer(
        config.buffer_duration_hns(),
        width,
        height,
        fps,
        config.audio_sample_rate,
        config.audio_channels,
    );

    // Audio capture channel
    let (audio_tx, audio_rx) = crossbeam_channel::unbounded();
    let mut audio_capture = WasapiLoopbackCapture::spawn(
        timer.clone(),
        config.audio_bitrate,
        audio_tx,
    )?;

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
                    }
                    Err(e) => {
                        log::error!("Failed to save replay: {}", e);
                    }
                }
            }
        })?;

    // Global hotkey listener
    let (hotkey_tx, hotkey_rx) = crossbeam_channel::bounded(4);
    let hotkey_listener = match HotkeyListener::spawn(config.hotkey_modifiers, config.hotkey_vk, hotkey_tx) {
        Ok(l) => {
            info!("Press Alt+F10 anywhere to save instant replay buffer!");
            Some(l)
        }
        Err(e) => {
            log::warn!("Hotkey listener registration failed: {}", e);
            None
        }
    };

    let frame_duration_hns = config.frame_duration_hns();
    let mut captured_frames = 0u64;
    let mut encoded_chunks = 0u64;
    let running = Arc::new(AtomicBool::new(true));

    info!("Streaming screen and audio into zero-copy circular buffer (Ctrl+C to quit)...");

    while running.load(Ordering::SeqCst) {
        // Drain incoming audio packets into ring buffer
        while let Ok(audio_chunk) = audio_rx.try_recv() {
            ring_buffer.lock().push_audio(audio_chunk);
        }

        // Check for hotkey press trigger
        while let Ok(HotkeyEvent::TriggerReplay) = hotkey_rx.try_recv() {
            info!("Hotkey detected! Capturing snapshot for disk writer...");
            if let Some(snapshot) = ring_buffer.lock().snapshot() {
                let _ = dump_tx.send(snapshot);
            } else {
                log::warn!("Ring buffer not ready yet (waiting for initial keyframe)");
            }
        }

        // Capture desktop frame (16 ms timeout ~60 FPS)
        match duplicator.acquire_frame(16)? {
            CaptureStatus::Frame(frame) => {
                captured_frames += 1;

                // Drain encoder events first — this credits NeedInput and collects output.
                if let Ok(chunks) = encoder.poll_output() {
                    let mut rb = ring_buffer.lock();
                    for chunk in chunks {
                        encoded_chunks += 1;
                        rb.push_video(chunk);
                    }
                }

                // Feed frame only when the async encoder has a free slot.
                if encoder.can_feed() {
                    let _ = encoder.feed_frame(&frame, frame_duration_hns);
                }
            }
            CaptureStatus::Timeout => {
                // No desktop change — still drain encoder output to keep credits fresh.
                if let Ok(chunks) = encoder.poll_output() {
                    let mut rb = ring_buffer.lock();
                    for chunk in chunks {
                        encoded_chunks += 1;
                        rb.push_video(chunk);
                    }
                }
            }
            CaptureStatus::AccessLost => {
                info!("DXGI access lost, re-initializing duplicator...");
                thread::sleep(std::time::Duration::from_millis(100));
                let _ = duplicator.init_duplication();
            }
            CaptureStatus::DeviceRemoved => {
                log::error!("D3D11 device removed, terminating pipeline");
                break;
            }
        }
    }

    // Clean shutdown
    if let Some(mut h) = hotkey_listener {
        h.stop();
    }
    audio_capture.stop();
    drop(dump_tx);
    let _ = writer_handle.join();

    info!(
        "Summary: {} frames captured, {} video chunks encoded in buffer ({:.2} MB in RAM)",
        captured_frames,
        encoded_chunks,
        ring_buffer.lock().total_bytes() as f64 / (1024.0 * 1024.0)
    );

    Ok(())
}
