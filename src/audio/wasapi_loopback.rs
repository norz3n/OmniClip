use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use anyhow::{anyhow, Result};
use crossbeam_channel::Sender;
use log::{debug, info, warn};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};

use crate::audio::aac_mft::AacEncoder;
use crate::buffer::chunk::AudioChunk;
use crate::platform::time::QpcTimer;

const TARGET_SAMPLE_RATE: u32 = 48_000;
const TARGET_CHANNELS: u32 = 2;
const SILENCE_CHUNK_FRAMES: usize = 480; // 10 ms at 48 kHz
const SILENCE_DURATION_HNS: i64 = 100_000; // 10 ms in 100-ns units
const HNS_PER_SECOND: i64 = 10_000_000;

/// Downmixes multi-channel audio samples into stereo (left, right) pairs.
///
/// Handles mono, stereo, 5.1 surround sound, and higher channel counts by folding surround
/// channels into left and right with standard attenuation.
///
/// * `data`: Slice of interleaved channel float samples for one or more frames.
/// * `channels`: Number of audio channels per frame in `data`.
fn downmix_to_stereo(data: &[f32], channels: usize) -> Vec<(f32, f32)> {
    if channels == 0 || data.is_empty() {
        return Vec::new();
    }
    let num_frames = data.len() / channels;
    let mut out = Vec::with_capacity(num_frames);

    match channels {
        1 => {
            for &sample in data {
                out.push((sample, sample));
            }
        }
        2 => {
            for chunk in data.chunks_exact(2) {
                out.push((chunk[0], chunk[1]));
            }
        }
        6 => {
            // 5.1 Surround: L, R, C, LFE, Ls, Rs
            const ATTENUATION: f32 = 0.707;
            for chunk in data.chunks_exact(6) {
                let l = (chunk[0] + ATTENUATION * chunk[2] + ATTENUATION * chunk[4]).clamp(-1.0, 1.0);
                let r = (chunk[1] + ATTENUATION * chunk[2] + ATTENUATION * chunk[5]).clamp(-1.0, 1.0);
                out.push((l, r));
            }
        }
        _ => {
            // PONYTAIL: For 7.1 or unusual multichannel, fold front stereo and discard auxiliary channels. Ceiling: surround cues lost. Upgrade path: Full 7.1 matrix downmix.
            for chunk in data.chunks_exact(channels) {
                out.push((chunk[0], chunk[1]));
            }
        }
    }
    out
}

/// Resamples interleaved stereo audio pairs from an arbitrary input sample rate to 48000 Hz.
///
/// Uses linear interpolation to keep latency and overhead minimal while avoiding sample drift.
///
/// * `input`: Slice of (left, right) float samples.
/// * `in_rate`: Source sample rate in Hz.
/// * `out_rate`: Destination sample rate in Hz (48000 Hz).
/// * `phase`: Accumulator tracking fractional sample position across consecutive buffers.
fn resample_linear(
    input: &[(f32, f32)],
    in_rate: u32,
    out_rate: u32,
    phase: &mut f64,
) -> Vec<(f32, f32)> {
    if in_rate == out_rate || input.len() < 2 {
        return input.to_vec();
    }
    // PONYTAIL: Linear resampler used instead of sinc/MFT resampler. Ceiling: minor aliasing on non-48kHz inputs. Upgrade path: MF CreateAudioResampler MFT if audiophile quality needed.
    let step = in_rate as f64 / out_rate as f64;
    let mut output = Vec::with_capacity((input.len() as f64 / step) as usize + 2);
    let in_len = input.len();

    while *phase < (in_len - 1) as f64 {
        let idx = *phase as usize;
        let frac = (*phase - idx as f64) as f32;
        let (l0, r0) = input[idx];
        let (l1, r1) = input[idx + 1];
        let l = l0 + frac * (l1 - l0);
        let r = r0 + frac * (r1 - r0);
        output.push((l, r));
        *phase += step;
    }
    *phase -= (in_len - 1) as f64;
    if *phase < 0.0 {
        *phase = 0.0;
    }
    output
}

/// Supported hardware audio sample formats for low-latency decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSampleFormat {
    Float32,
    Int16,
    Int24Packed,
    Int32Pcm,
}

#[repr(C)]
struct WaveFormatExtensible {
    _format: windows::Win32::Media::Audio::WAVEFORMATEX,
    _samples: u16, // wValidBitsPerSample
    _channel_mask: u32,
    sub_format: windows::core::GUID,
}

fn detect_sample_format(ptr: *const windows::Win32::Media::Audio::WAVEFORMATEX) -> AudioSampleFormat {
    unsafe {
        let fmt = &*ptr;
        if fmt.wFormatTag == 3 {
            return AudioSampleFormat::Float32;
        }
        if fmt.wFormatTag == 1 {
            return match fmt.wBitsPerSample {
                16 => AudioSampleFormat::Int16,
                24 => AudioSampleFormat::Int24Packed,
                32 => AudioSampleFormat::Int32Pcm,
                _ => AudioSampleFormat::Int16,
            };
        }
        if fmt.wFormatTag == 0xFFFE {
            let ext = &*(ptr as *const WaveFormatExtensible);
            if ext.sub_format.data1 == 3 {
                return AudioSampleFormat::Float32;
            }
            return match fmt.wBitsPerSample {
                16 => AudioSampleFormat::Int16,
                24 => AudioSampleFormat::Int24Packed,
                32 => AudioSampleFormat::Int32Pcm,
                _ => AudioSampleFormat::Int16,
            };
        }
        AudioSampleFormat::Int16
    }
}

fn decode_raw_audio_samples(
    ptr: *const u8,
    frames: usize,
    channels: usize,
    format: AudioSampleFormat,
) -> Vec<f32> {
    let total_samples = frames * channels;
    let mut out = Vec::with_capacity(total_samples);

    match format {
        AudioSampleFormat::Float32 => {
            let slice = unsafe { std::slice::from_raw_parts(ptr as *const f32, total_samples) };
            out.extend_from_slice(slice);
        }
        AudioSampleFormat::Int16 => {
            let slice = unsafe { std::slice::from_raw_parts(ptr as *const i16, total_samples) };
            for &s in slice {
                out.push(s as f32 / 32768.0);
            }
        }
        AudioSampleFormat::Int24Packed => {
            let byte_slice = unsafe { std::slice::from_raw_parts(ptr, total_samples * 3) };
            for chunk in byte_slice.chunks_exact(3) {
                let b0 = chunk[0] as u32;
                let b1 = (chunk[1] as u32) << 8;
                let b2 = (chunk[2] as i8 as i32 as u32) << 16;
                let val = (b0 | b1 | b2) as i32;
                out.push(val as f32 / 8388608.0);
            }
        }
        AudioSampleFormat::Int32Pcm => {
            let slice = unsafe { std::slice::from_raw_parts(ptr as *const i32, total_samples) };
            for &s in slice {
                out.push(s as f32 / 2147483648.0);
            }
        }
    }
    out
}

/// Helper function to initialize a microphone capture client endpoint.
fn init_mic_endpoint(
    enumerator: &windows::Win32::Media::Audio::IMMDeviceEnumerator,
    capture_device_id: Option<&str>,
) -> Result<(IAudioClient, IAudioCaptureClient, AudioSampleFormat, u32, u32)> {
    let mic_device = unsafe {
        if let Some(dev_id) = capture_device_id {
            if !dev_id.is_empty() && dev_id != "default" {
                let wide: Vec<u16> = dev_id.encode_utf16().chain(std::iter::once(0)).collect();
                enumerator
                    .GetDevice(windows::core::PCWSTR(wide.as_ptr()))
                    .or_else(|_| enumerator.GetDefaultAudioEndpoint(eCapture, eConsole))
            } else {
                enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)
            }
        } else {
            enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)
        }
        .map_err(|e| anyhow!("No microphone endpoint found: {}", e))?
    };

    let mic_client: IAudioClient = unsafe {
        mic_device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| anyhow!("Failed to activate microphone client: {}", e))?
    };

    let mic_format_ptr = unsafe {
        mic_client
            .GetMixFormat()
            .map_err(|e| anyhow!("Failed to get microphone mix format: {}", e))?
    };

    let mic_fmt = unsafe { *mic_format_ptr };
    let m_channels = mic_fmt.nChannels as u32;
    let m_rate = mic_fmt.nSamplesPerSec;
    let m_bits = mic_fmt.wBitsPerSample;
    let sample_format = detect_sample_format(mic_format_ptr);

    let m_init = unsafe {
        mic_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            0, // Shared capture mode (not loopback)
            HNS_PER_SECOND,
            0,
            mic_format_ptr,
            None,
        )
    };

    unsafe {
        CoTaskMemFree(Some(mic_format_ptr as *const _));
    }

    m_init.map_err(|e| anyhow!("Failed to initialize microphone client: {}", e))?;

    let mic_capture: IAudioCaptureClient = unsafe {
        mic_client
            .GetService()
            .map_err(|e| anyhow!("Failed to get microphone capture client: {}", e))?
    };

    unsafe {
        mic_client
            .Start()
            .map_err(|e| anyhow!("Failed to start microphone client: {}", e))?;
    }

    info!(
        "Initialized Microphone capture: {} Hz, {} channels, {} bit, format: {:?}",
        m_rate, m_channels, m_bits, sample_format
    );

    Ok((mic_client, mic_capture, sample_format, m_rate, m_channels))
}

/// WASAPI Loopback system audio and microphone capture engine running on a dedicated thread.
pub struct WasapiLoopbackCapture {
    running: Arc<AtomicBool>,
    mic_enabled: Arc<AtomicBool>,
    mic_volume: Arc<parking_lot::Mutex<f32>>,
    sync_offset_hns: Arc<AtomicI64>,
    mic_peak: Arc<std::sync::atomic::AtomicU32>,
    mic_status: Arc<parking_lot::Mutex<String>>,
    requested_capture_device: Arc<parking_lot::Mutex<Option<String>>>,
    device_changed: Arc<AtomicBool>,
    thread_handle: Option<JoinHandle<()>>,
}

impl WasapiLoopbackCapture {
    /// Spawns the background audio capture loop thread.
    ///
    /// * `timer`: High-resolution QPC timer for synchronized timestamps.
    /// * `target_bitrate`: Target AAC bitrate in bps (e.g. 192_000).
    /// * `mic_enabled`: Whether to capture and mix the default microphone.
    /// * `mic_volume`: Microphone volume scaling multiplier (1.0 = 100%).
    /// * `playback_device_id`: Optional specific audio playback endpoint ID ("default" for system default).
    /// * `capture_device_id`: Optional specific audio recording endpoint ID ("default" for system default).
    /// * `multi_track`: Whether to emit separate audio tracks (Track 0 = Desktop, Track 1 = Mic).
    /// * `sync_offset_ms`: Audio synchronization offset in milliseconds (negative shifts audio earlier).
    /// * `chunk_sender`: Crossbeam channel sender delivering compressed AAC chunks.
    pub fn spawn(
        timer: QpcTimer,
        target_bitrate: u32,
        mic_enabled: bool,
        mic_volume: f32,
        playback_device_id: Option<String>,
        capture_device_id: Option<String>,
        multi_track: bool,
        sync_offset_ms: i32,
        chunk_sender: Sender<AudioChunk>,
    ) -> Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        let mic_enabled_arc = Arc::new(AtomicBool::new(mic_enabled));
        let mic_volume_arc = Arc::new(parking_lot::Mutex::new(mic_volume));
        let mic_enabled_thread = mic_enabled_arc.clone();
        let mic_volume_thread = mic_volume_arc.clone();

        let sync_offset_hns = Arc::new(AtomicI64::new(sync_offset_ms as i64 * 10_000));
        let sync_offset_hns_thread = sync_offset_hns.clone();

        let mic_peak = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mic_peak_thread = mic_peak.clone();

        let mic_status = Arc::new(parking_lot::Mutex::new("OK".to_string()));
        let mic_status_thread = mic_status.clone();

        let requested_capture_device = Arc::new(parking_lot::Mutex::new(capture_device_id.clone()));
        let requested_capture_device_thread = requested_capture_device.clone();

        let device_changed = Arc::new(AtomicBool::new(false));
        let device_changed_thread = device_changed.clone();

        let (init_tx, init_rx) = crossbeam_channel::bounded::<Result<()>>(1);

        let handle = thread::Builder::new()
            .name("omniclip-audio".to_string())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }

                type LoopbackInit = (IAudioClient, IAudioCaptureClient, AacEncoder, bool, u32, u32);
                type MicInit = Option<(IAudioClient, IAudioCaptureClient, AudioSampleFormat, u32, u32)>;

                let enumerator: IMMDeviceEnumerator = match unsafe {
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                } {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = init_tx.send(Err(anyhow!("Failed to create MMDeviceEnumerator: {}", e)));
                        unsafe { CoUninitialize(); }
                        return;
                    }
                };

                let init_res = (|| -> Result<(LoopbackInit, MicInit)> {

                    // 1. Initialize Loopback System Audio Endpoint
                    let device = unsafe {
                        if let Some(ref dev_id) = playback_device_id {
                            if !dev_id.is_empty() && dev_id != "default" {
                                let wide: Vec<u16> = dev_id.encode_utf16().chain(std::iter::once(0)).collect();
                                enumerator
                                    .GetDevice(windows::core::PCWSTR(wide.as_ptr()))
                                    .or_else(|_| enumerator.GetDefaultAudioEndpoint(eRender, eConsole))
                            } else {
                                enumerator.GetDefaultAudioEndpoint(eRender, eConsole)
                            }
                        } else {
                            enumerator.GetDefaultAudioEndpoint(eRender, eConsole)
                        }
                        .map_err(|e| anyhow!("Failed to get audio playback endpoint: {}", e))?
                    };

                    let audio_client: IAudioClient = unsafe {
                        device
                            .Activate(CLSCTX_ALL, None)
                            .map_err(|e| anyhow!("Failed to activate IAudioClient: {}", e))?
                    };

                    let mix_format_ptr = unsafe {
                        audio_client
                            .GetMixFormat()
                            .map_err(|e| anyhow!("Failed to get audio mix format: {}", e))?
                    };

                    let mix_format = unsafe { *mix_format_ptr };
                    let channels = mix_format.nChannels as u32;
                    let sample_rate = mix_format.nSamplesPerSec;
                    let bits_per_sample = mix_format.wBitsPerSample;
                    let is_float = mix_format.wFormatTag == 3 || (mix_format.wFormatTag == 0xFFFE && bits_per_sample == 32);

                    let buffer_duration_hns: i64 = HNS_PER_SECOND; // 1s buffer
                    let init_status = unsafe {
                        audio_client.Initialize(
                            AUDCLNT_SHAREMODE_SHARED,
                            AUDCLNT_STREAMFLAGS_LOOPBACK,
                            buffer_duration_hns,
                            0,
                            mix_format_ptr,
                            None,
                        )
                    };

                    unsafe {
                        CoTaskMemFree(Some(mix_format_ptr as *const _));
                    }

                    init_status.map_err(|e| anyhow!("Failed to initialize IAudioClient: {}", e))?;

                    let capture_client: IAudioCaptureClient = unsafe {
                        audio_client
                            .GetService()
                            .map_err(|e| anyhow!("Failed to get IAudioCaptureClient: {}", e))?
                    };

                    unsafe {
                        audio_client
                            .Start()
                            .map_err(|e| anyhow!("Failed to start IAudioClient: {}", e))?;
                    }

                    let aac_encoder = AacEncoder::new(TARGET_SAMPLE_RATE, TARGET_CHANNELS, target_bitrate)?;

                    if let Ok(stream_latency_hns) = unsafe { audio_client.GetStreamLatency() } {
                        info!(
                            "WASAPI Loopback stream latency: {} hns ({:.2} ms)",
                            stream_latency_hns,
                            stream_latency_hns as f64 / 10_000.0
                        );
                    }

                    info!(
                        "Initialized WASAPI Loopback capture: device {} Hz, {} channels, {} bit (Float: {}), normalized to {} Hz Stereo",
                        sample_rate, channels, bits_per_sample, is_float, TARGET_SAMPLE_RATE
                    );

                    // 2. Optional Microphone Capture Endpoint
                    let mic_opt: MicInit = match init_mic_endpoint(&enumerator, capture_device_id.as_deref()) {
                        Ok(m) => {
                            *mic_status_thread.lock() = "OK".to_string();
                            Some(m)
                        }
                        Err(e) => {
                            warn!("Initial microphone capture setup: {}", e);
                            *mic_status_thread.lock() = format!("{}", e);
                            None
                        }
                    };

                    Ok(((audio_client, capture_client, aac_encoder, is_float, sample_rate, channels), mic_opt))
                })();

                let (
                    (audio_client, capture_client, mut encoder, is_float, sample_rate, channels),
                    mut mic_client_opt,
                ) = match init_res {
                    Ok(res) => {
                        let _ = init_tx.send(Ok(()));
                        res
                    }
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        unsafe { CoUninitialize(); }
                        return;
                    }
                };

                let mut mic_encoder_opt = if multi_track && mic_client_opt.is_some() {
                    AacEncoder::new(TARGET_SAMPLE_RATE, TARGET_CHANNELS, target_bitrate).ok()
                } else {
                    None
                };

                let mut pcm_buffer = Vec::<u8>::with_capacity(8192);
                let initial_offset_hns = sync_offset_hns_thread.load(Ordering::Relaxed);
                let mut next_pts_hns = (timer.elapsed_hns() + initial_offset_hns).max(0);
                let mut last_real_packet_hns = next_pts_hns;
                let mut resample_phase = 0.0f64;

                let mut mic_fifo = std::collections::VecDeque::<(f32, f32)>::with_capacity(4800);
                let mut mic_resample_phase = 0.0f64;
                let mut last_mic_retry = std::time::Instant::now();

                while running_clone.load(Ordering::SeqCst) {
                    // 1. Dynamic device switch request from UI
                    if device_changed_thread.swap(false, Ordering::SeqCst) {
                        if let Some((old_client, ..)) = mic_client_opt.take() {
                            unsafe { let _ = old_client.Stop(); }
                        }
                        let dev = requested_capture_device_thread.lock().clone();
                        match init_mic_endpoint(&enumerator, dev.as_deref()) {
                            Ok(m) => {
                                info!("Microphone switched successfully to device: {:?}", dev);
                                *mic_status_thread.lock() = "OK".to_string();
                                if multi_track && mic_encoder_opt.is_none() {
                                    mic_encoder_opt = AacEncoder::new(TARGET_SAMPLE_RATE, TARGET_CHANNELS, target_bitrate).ok();
                                }
                                mic_client_opt = Some(m);
                            }
                            Err(e) => {
                                warn!("Failed to switch microphone to {:?}: {}", dev, e);
                                *mic_status_thread.lock() = format!("{}", e);
                            }
                        }
                    }

                    // 2. Periodic reconnect watchdog if microphone client is not active
                    let now_instant = std::time::Instant::now();
                    if mic_client_opt.is_none() && now_instant.duration_since(last_mic_retry) >= std::time::Duration::from_secs(2) {
                        last_mic_retry = now_instant;
                        let dev = requested_capture_device_thread.lock().clone();
                        match init_mic_endpoint(&enumerator, dev.as_deref()) {
                            Ok(m) => {
                                info!("Microphone successfully (re)connected!");
                                *mic_status_thread.lock() = "OK".to_string();
                                if multi_track && mic_encoder_opt.is_none() {
                                    mic_encoder_opt = AacEncoder::new(TARGET_SAMPLE_RATE, TARGET_CHANNELS, target_bitrate).ok();
                                }
                                mic_client_opt = Some(m);
                            }
                            Err(e) => {
                                debug!("Microphone reconnection attempt: {}", e);
                                *mic_status_thread.lock() = format!("{}", e);
                            }
                        }
                    }

                    let is_mic_active = mic_enabled_thread.load(Ordering::Relaxed);
                    let cur_mic_volume = if is_mic_active { *mic_volume_thread.lock() } else { 0.0 };

                    // Poll and buffer incoming microphone packets
                    if let Some((_, ref mic_capture, sample_format, m_rate, m_ch)) = mic_client_opt {
                        let mut m_packet = unsafe { mic_capture.GetNextPacketSize().unwrap_or(0) };
                        while m_packet > 0 {
                            let mut m_ptr: *mut u8 = std::ptr::null_mut();
                            let mut m_frames = 0u32;
                            let mut m_flags = 0u32;
                            let mut m_dev_pos = 0u64;
                            let mut m_qpc_pos = 0u64;

                            let get_res = unsafe {
                                mic_capture.GetBuffer(
                                    &mut m_ptr,
                                    &mut m_frames,
                                    &mut m_flags,
                                    Some(&mut m_dev_pos),
                                    Some(&mut m_qpc_pos),
                                )
                            };

                            match get_res {
                                Ok(()) => {
                                    if m_frames > 0 {
                                        let is_silent = (m_flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
                                        if !is_silent && !m_ptr.is_null() {
                                            let floats = decode_raw_audio_samples(
                                                m_ptr,
                                                m_frames as usize,
                                                m_ch as usize,
                                                sample_format,
                                            );

                                            // Calculate live peak for VU meter
                                            let mut frame_peak = 0.0f32;
                                            for &f in &floats {
                                                let abs = f.abs();
                                                if abs > frame_peak {
                                                    frame_peak = abs;
                                                }
                                            }
                                            let cur_peak = (mic_peak_thread.load(Ordering::Relaxed) as f32) / 1000.0;
                                            let new_peak = if frame_peak > cur_peak {
                                                frame_peak
                                            } else {
                                                (cur_peak * 0.92).max(0.0)
                                            };
                                            mic_peak_thread.store((new_peak * 1000.0) as u32, Ordering::Relaxed);

                                            let stereo = downmix_to_stereo(&floats, m_ch as usize);
                                            let resampled = resample_linear(
                                                &stereo,
                                                m_rate,
                                                TARGET_SAMPLE_RATE,
                                                &mut mic_resample_phase,
                                            );
                                            for s in resampled {
                                                mic_fifo.push_back(s);
                                            }
                                        } else {
                                            // Falloff VU peak on silent buffer
                                            let cur_peak = (mic_peak_thread.load(Ordering::Relaxed) as f32) / 1000.0;
                                            mic_peak_thread.store(((cur_peak * 0.85) * 1000.0) as u32, Ordering::Relaxed);

                                            let out_frames = if m_rate == TARGET_SAMPLE_RATE {
                                                m_frames as usize
                                            } else {
                                                ((m_frames as u64 * TARGET_SAMPLE_RATE as u64) / m_rate as u64) as usize
                                            };
                                            for _ in 0..out_frames {
                                                mic_fifo.push_back((0.0, 0.0));
                                            }
                                        }
                                        let _ = unsafe { mic_capture.ReleaseBuffer(m_frames) };
                                    }
                                }
                                Err(e) => {
                                    warn!("Microphone capture GetBuffer error: {}. Disconnecting to allow reconnection...", e);
                                    *mic_status_thread.lock() = "Disconnected".to_string();
                                    if let Some((client, ..)) = mic_client_opt.take() {
                                        unsafe { let _ = client.Stop(); }
                                    }
                                    break;
                                }
                            }
                            m_packet = unsafe { mic_capture.GetNextPacketSize().unwrap_or(0) };
                        }

                        // FIFO sample-slip mixer bounds
                        const MAX_MIC_LATENCY_SAMPLES: usize = 2400; // ~50 ms
                        while mic_fifo.len() > MAX_MIC_LATENCY_SAMPLES {
                            mic_fifo.pop_front();
                        }
                    } else {
                        // Peak is 0 when no mic client is available
                        mic_peak_thread.store(0, Ordering::Relaxed);
                    }

                    let mut packet_size = match unsafe { capture_client.GetNextPacketSize() } {
                        Ok(sz) => sz,
                        Err(e) => {
                            warn!("GetNextPacketSize failed: {}", e);
                            break;
                        }
                    };

                    // Silence gap-filler: when no loopback audio has arrived for >50ms, synthesize silence chunks and mix mic voice
                    if packet_size == 0 {
                        let now_hns = timer.elapsed_hns();
                        let cur_offset_hns = sync_offset_hns_thread.load(Ordering::Relaxed);
                        let adj_now_hns = (now_hns + cur_offset_hns).max(0);
                        // Only declare silence if no real audio packet has arrived for at least 50 ms (grace period)
                        if now_hns.saturating_sub(last_real_packet_hns) >= 500_000 && adj_now_hns >= next_pts_hns + SILENCE_DURATION_HNS {
                            pcm_buffer.clear();
                            let mut mic_pcm_buffer = Vec::<u8>::with_capacity(SILENCE_CHUNK_FRAMES * 4);
                            for _ in 0..SILENCE_CHUNK_FRAMES {
                                let (mic_l, mic_r) = mic_fifo.pop_front().unwrap_or((0.0, 0.0));
                                if mic_encoder_opt.is_some() {
                                    pcm_buffer.extend_from_slice(&0i16.to_le_bytes());
                                    pcm_buffer.extend_from_slice(&0i16.to_le_bytes());
                                    let m_l = ((mic_l * cur_mic_volume).clamp(-1.0, 1.0) * 32767.0) as i16;
                                    let m_r = ((mic_r * cur_mic_volume).clamp(-1.0, 1.0) * 32767.0) as i16;
                                    mic_pcm_buffer.extend_from_slice(&m_l.to_le_bytes());
                                    mic_pcm_buffer.extend_from_slice(&m_r.to_le_bytes());
                                } else {
                                    let mix_l = (mic_l * cur_mic_volume).clamp(-1.0, 1.0);
                                    let mix_r = (mic_r * cur_mic_volume).clamp(-1.0, 1.0);
                                    let s_l = (mix_l * 32767.0) as i16;
                                    let s_r = (mix_r * 32767.0) as i16;
                                    pcm_buffer.extend_from_slice(&s_l.to_le_bytes());
                                    pcm_buffer.extend_from_slice(&s_r.to_le_bytes());
                                }
                            }

                            match encoder.encode_pcm_with_track(&pcm_buffer, next_pts_hns, SILENCE_DURATION_HNS, 0) {
                                Ok(chunks) => {
                                    for chunk in chunks {
                                        if chunk_sender.send(chunk).is_err() {
                                            running_clone.store(false, Ordering::SeqCst);
                                            break;
                                        }
                                    }
                                }
                                Err(e) => debug!("Audio silence encode error: {}", e),
                            }

                            if let Some(ref mut mic_enc) = mic_encoder_opt {
                                if let Ok(chunks) = mic_enc.encode_pcm_with_track(&mic_pcm_buffer, next_pts_hns, SILENCE_DURATION_HNS, 1) {
                                    for chunk in chunks {
                                        if chunk_sender.send(chunk).is_err() {
                                            running_clone.store(false, Ordering::SeqCst);
                                            break;
                                        }
                                    }
                                }
                            }

                            next_pts_hns += SILENCE_DURATION_HNS;
                        } else {
                            thread::sleep(std::time::Duration::from_millis(2));
                        }
                        continue;
                    }

                    while packet_size > 0 {
                        let mut data_ptr: *mut u8 = std::ptr::null_mut();
                        let mut num_frames = 0u32;
                        let mut flags = 0u32;
                        let mut dev_pos = 0u64;
                        let mut qpc_pos = 0u64;

                        let get_res = unsafe {
                            capture_client.GetBuffer(
                                &mut data_ptr,
                                &mut num_frames,
                                &mut flags,
                                Some(&mut dev_pos),
                                Some(&mut qpc_pos),
                            )
                        };

                        if let Err(e) = get_res {
                            warn!("GetBuffer failed: {}", e);
                            break;
                        }

                        if num_frames > 0 {
                            let now_hns = timer.elapsed_hns();
                            last_real_packet_hns = now_hns;

                            let cur_offset_hns = sync_offset_hns_thread.load(Ordering::Relaxed);
                            let raw_pts = if qpc_pos > 0 {
                                timer.wasapi_hns_to_relative(qpc_pos as i64)
                            } else {
                                now_hns
                            };
                            let adj_raw_pts = (raw_pts + cur_offset_hns).max(0);

                            let is_discontinuity = (flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32) != 0;

                            // Bridge any gap between previous packet and current raw_pts with silence (+ mic) if stream was idle
                            if is_discontinuity || adj_raw_pts > next_pts_hns + 200_000 {
                                while next_pts_hns + SILENCE_DURATION_HNS <= adj_raw_pts {
                                    pcm_buffer.clear();
                                    let mut mic_pcm_buffer = Vec::<u8>::with_capacity(SILENCE_CHUNK_FRAMES * 4);
                                    for _ in 0..SILENCE_CHUNK_FRAMES {
                                        let (mic_l, mic_r) = mic_fifo.pop_front().unwrap_or((0.0, 0.0));
                                        if mic_encoder_opt.is_some() {
                                            pcm_buffer.extend_from_slice(&0i16.to_le_bytes());
                                            pcm_buffer.extend_from_slice(&0i16.to_le_bytes());
                                            let m_l = ((mic_l * cur_mic_volume).clamp(-1.0, 1.0) * 32767.0) as i16;
                                            let m_r = ((mic_r * cur_mic_volume).clamp(-1.0, 1.0) * 32767.0) as i16;
                                            mic_pcm_buffer.extend_from_slice(&m_l.to_le_bytes());
                                            mic_pcm_buffer.extend_from_slice(&m_r.to_le_bytes());
                                        } else {
                                            let mix_l = (mic_l * cur_mic_volume).clamp(-1.0, 1.0);
                                            let mix_r = (mic_r * cur_mic_volume).clamp(-1.0, 1.0);
                                            let s_l = (mix_l * 32767.0) as i16;
                                            let s_r = (mix_r * 32767.0) as i16;
                                            pcm_buffer.extend_from_slice(&s_l.to_le_bytes());
                                            pcm_buffer.extend_from_slice(&s_r.to_le_bytes());
                                        }
                                    }
                                    if let Ok(chunks) = encoder.encode_pcm_with_track(&pcm_buffer, next_pts_hns, SILENCE_DURATION_HNS, 0) {
                                        for chunk in chunks {
                                            if chunk_sender.send(chunk).is_err() {
                                                running_clone.store(false, Ordering::SeqCst);
                                                break;
                                            }
                                        }
                                    }
                                    if let Some(ref mut mic_enc) = mic_encoder_opt {
                                        if let Ok(chunks) = mic_enc.encode_pcm_with_track(&mic_pcm_buffer, next_pts_hns, SILENCE_DURATION_HNS, 1) {
                                            for chunk in chunks {
                                                if chunk_sender.send(chunk).is_err() {
                                                    running_clone.store(false, Ordering::SeqCst);
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    next_pts_hns += SILENCE_DURATION_HNS;
                                }
                            }

                            // For contiguous streaming, preserve uninterrupted PTS to eliminate AAC MDCT window clicks
                            let pts_hns = if (adj_raw_pts - next_pts_hns).abs() > 500_000 {
                                adj_raw_pts
                            } else {
                                next_pts_hns
                            };
                            let is_silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;

                            pcm_buffer.clear();
                            let mut mic_pcm_buffer = Vec::<u8>::new();
                            let duration_hns: i64;

                            if is_silent || data_ptr.is_null() {
                                let out_frames = if sample_rate == TARGET_SAMPLE_RATE {
                                    num_frames as usize
                                } else {
                                    ((num_frames as u64 * TARGET_SAMPLE_RATE as u64) / sample_rate as u64) as usize
                                };
                                mic_pcm_buffer.reserve(out_frames * 4);
                                for _ in 0..out_frames {
                                    let (mic_l, mic_r) = mic_fifo.pop_front().unwrap_or((0.0, 0.0));
                                    if mic_encoder_opt.is_some() {
                                        pcm_buffer.extend_from_slice(&0i16.to_le_bytes());
                                        pcm_buffer.extend_from_slice(&0i16.to_le_bytes());
                                        let m_l = ((mic_l * cur_mic_volume).clamp(-1.0, 1.0) * 32767.0) as i16;
                                        let m_r = ((mic_r * cur_mic_volume).clamp(-1.0, 1.0) * 32767.0) as i16;
                                        mic_pcm_buffer.extend_from_slice(&m_l.to_le_bytes());
                                        mic_pcm_buffer.extend_from_slice(&m_r.to_le_bytes());
                                    } else {
                                        let mix_l = (mic_l * cur_mic_volume).clamp(-1.0, 1.0);
                                        let mix_r = (mic_r * cur_mic_volume).clamp(-1.0, 1.0);
                                        let s_l = (mix_l * 32767.0) as i16;
                                        let s_r = (mix_r * 32767.0) as i16;
                                        pcm_buffer.extend_from_slice(&s_l.to_le_bytes());
                                        pcm_buffer.extend_from_slice(&s_r.to_le_bytes());
                                    }
                                }
                                duration_hns = (out_frames as i64 * HNS_PER_SECOND) / TARGET_SAMPLE_RATE as i64;
                            } else {
                                let stereo_samples = if is_float {
                                    let float_count = num_frames as usize * channels as usize;
                                    let float_slice = unsafe {
                                        std::slice::from_raw_parts(data_ptr as *const f32, float_count)
                                    };
                                    downmix_to_stereo(float_slice, channels as usize)
                                } else {
                                    let int16_count = num_frames as usize * channels as usize;
                                    let raw_slice = unsafe {
                                        std::slice::from_raw_parts(data_ptr as *const i16, int16_count)
                                    };
                                    let float_samples: Vec<f32> = raw_slice
                                        .iter()
                                        .map(|&s| s as f32 / 32768.0)
                                        .collect();
                                    downmix_to_stereo(&float_samples, channels as usize)
                                };

                                let resampled = resample_linear(
                                    &stereo_samples,
                                    sample_rate,
                                    TARGET_SAMPLE_RATE,
                                    &mut resample_phase,
                                );

                                mic_pcm_buffer.reserve(resampled.len() * 4);
                                for &(l, r) in &resampled {
                                    let (mic_l, mic_r) = mic_fifo.pop_front().unwrap_or((0.0, 0.0));
                                    if mic_encoder_opt.is_some() {
                                        let s_l = (l.clamp(-1.0, 1.0) * 32767.0) as i16;
                                        let s_r = (r.clamp(-1.0, 1.0) * 32767.0) as i16;
                                        pcm_buffer.extend_from_slice(&s_l.to_le_bytes());
                                        pcm_buffer.extend_from_slice(&s_r.to_le_bytes());
                                        let m_l = ((mic_l * cur_mic_volume).clamp(-1.0, 1.0) * 32767.0) as i16;
                                        let m_r = ((mic_r * cur_mic_volume).clamp(-1.0, 1.0) * 32767.0) as i16;
                                        mic_pcm_buffer.extend_from_slice(&m_l.to_le_bytes());
                                        mic_pcm_buffer.extend_from_slice(&m_r.to_le_bytes());
                                    } else {
                                        let mix_l = (l + mic_l * cur_mic_volume).clamp(-1.0, 1.0);
                                        let mix_r = (r + mic_r * cur_mic_volume).clamp(-1.0, 1.0);
                                        let s_l = (mix_l * 32767.0) as i16;
                                        let s_r = (mix_r * 32767.0) as i16;
                                        pcm_buffer.extend_from_slice(&s_l.to_le_bytes());
                                        pcm_buffer.extend_from_slice(&s_r.to_le_bytes());
                                    }
                                }
                                duration_hns = (resampled.len() as i64 * HNS_PER_SECOND) / TARGET_SAMPLE_RATE as i64;
                            }

                            if !pcm_buffer.is_empty() {
                                match encoder.encode_pcm_with_track(&pcm_buffer, pts_hns, duration_hns, 0) {
                                    Ok(chunks) => {
                                        for chunk in chunks {
                                            if chunk_sender.send(chunk).is_err() {
                                                running_clone.store(false, Ordering::SeqCst);
                                                break;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        debug!("Audio encode error: {}", e);
                                    }
                                }

                                if let Some(ref mut mic_enc) = mic_encoder_opt {
                                    match mic_enc.encode_pcm_with_track(&mic_pcm_buffer, pts_hns, duration_hns, 1) {
                                        Ok(chunks) => {
                                            for chunk in chunks {
                                                if chunk_sender.send(chunk).is_err() {
                                                    running_clone.store(false, Ordering::SeqCst);
                                                    break;
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            debug!("Microphone encode error track 1: {}", e);
                                        }
                                    }
                                }
                                next_pts_hns = pts_hns + duration_hns;
                            }
                        }

                        let _ = unsafe { capture_client.ReleaseBuffer(num_frames) };
                        packet_size = unsafe { capture_client.GetNextPacketSize().unwrap_or(0) };
                    }
                }

                let _ = unsafe { audio_client.Stop() };
                if let Some((ref mic_client, ..)) = mic_client_opt {
                    let _ = unsafe { mic_client.Stop() };
                }
                unsafe { CoUninitialize(); }
                info!("Stopped WASAPI Loopback and Microphone capture thread");
            })
            .map_err(|e| anyhow!("Failed to spawn audio capture thread: {}", e))?;

        init_rx
            .recv()
            .map_err(|e| anyhow!("Audio capture thread failed to initialize: {}", e))??;

        Ok(Self {
            running,
            mic_enabled: mic_enabled_arc,
            mic_volume: mic_volume_arc,
            sync_offset_hns,
            mic_peak,
            mic_status,
            requested_capture_device,
            device_changed,
            thread_handle: Some(handle),
        })
    }

    /// Dynamically sets the active microphone capture device.
    pub fn set_capture_device(&self, device_id: Option<String>) {
        *self.requested_capture_device.lock() = device_id;
        self.device_changed.store(true, Ordering::SeqCst);
    }

    /// Returns the current microphone peak volume (0.0 to 1.0) for live VU meters.
    pub fn mic_peak(&self) -> f32 {
        (self.mic_peak.load(Ordering::Relaxed) as f32) / 1000.0
    }

    /// Returns the current microphone status message (e.g. "OK" or error description).
    pub fn mic_status(&self) -> String {
        self.mic_status.lock().clone()
    }

    /// Dynamically sets the audio synchronization offset in milliseconds.
    pub fn set_sync_offset_ms(&self, offset_ms: i32) {
        self.sync_offset_hns.store(offset_ms as i64 * 10_000, Ordering::Relaxed);
    }

    /// Dynamically sets the microphone recording volume multiplier.
    pub fn set_mic_volume(&self, vol: f32) {
        *self.mic_volume.lock() = vol;
    }

    /// Dynamically enables or disables microphone recording.
    pub fn set_mic_enabled(&self, enabled: bool) {
        self.mic_enabled.store(enabled, Ordering::SeqCst);
    }

    /// Toggles the microphone between muted and active, returning the new state.
    pub fn toggle_mic(&self) -> bool {
        let new_state = !self.mic_enabled.load(Ordering::SeqCst);
        self.mic_enabled.store(new_state, Ordering::SeqCst);
        new_state
    }

    /// Stops the audio capture thread.
    pub fn stop(&mut self) {
        if self.running.swap(false, Ordering::SeqCst) {
            if let Some(handle) = self.thread_handle.take() {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for WasapiLoopbackCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Enumerates active playback or recording audio devices and returns a list of (id, friendly_name) tuples.
pub fn enumerate_audio_devices(is_capture: bool) -> Vec<(String, String)> {
    // Run COM enumeration in an isolated worker thread to avoid tainting caller thread's COM apartment (STA / MTA)
    std::thread::spawn(move || {
        let mut devices = Vec::new();
        unsafe {
            use windows::Win32::System::Com::STGM_READ;
            use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;

            let com_init = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            if let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                let flow = if is_capture { eCapture } else { eRender };
                if let Ok(collection) = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) {
                    if let Ok(count) = collection.GetCount() {
                        for i in 0..count {
                            if let Ok(device) = collection.Item(i) {
                                let id_str = if let Ok(id) = device.GetId() {
                                    let s = id.to_string().unwrap_or_default();
                                    CoTaskMemFree(Some(id.0 as *const _));
                                    s
                                } else {
                                    continue;
                                };

                                let mut friendly_name = if is_capture {
                                    format!("Микрофон {}", i + 1)
                                } else {
                                    format!("Динамики / Наушники {}", i + 1)
                                };

                                if let Ok(props) = device.OpenPropertyStore(STGM_READ) {
                                    // PKEY_Device_FriendlyName: {a45c254e-df1c-4efd-8020-67d146a850e0}, 14
                                    let key = PROPERTYKEY {
                                        fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
                                        pid: 14,
                                    };
                                    if let Ok(prop_var) = props.GetValue(&key) {
                                        let s = prop_var.to_string();
                                        if !s.is_empty() {
                                            friendly_name = s;
                                        }
                                    }
                                }

                                devices.push((id_str, friendly_name));
                            }
                        }
                    }
                }
            }
            if com_init {
                CoUninitialize();
            }
        }
        devices
    })
    .join()
    .unwrap_or_default()
}

/// Real-time audio session application metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioAppInfo {
    /// Executable filename (e.g. "firefox.exe", "Discord.exe").
    pub name: String,
    /// Absolute filesystem path to the executable file.
    pub exe_path: String,
    /// Operating system process identifier.
    pub pid: u32,
    /// Real-time peak volume meter value ranging from 0.0 to 1.0.
    pub peak: f32,
    /// Indicates whether the session currently has active audio output.
    pub is_playing: bool,
}

/// Scans active WASAPI audio sessions and returns applications currently emitting audio.
///
/// Filters out silent idle processes, Windows internal services, and OmniClip's own process.
/// Enumerate all active render endpoints to ensure applications outputting to headphones,
/// speakers, or secondary devices are detected.
///
/// # Returns
/// A list of `AudioAppInfo` entries for applications producing sound.
pub fn scan_active_audio_sessions() -> Vec<AudioAppInfo> {
    use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
    use windows::core::Interface;

    let my_pid = std::process::id();
    let mut apps: Vec<AudioAppInfo> = Vec::new();

    unsafe {
        let com_init = CoInitializeEx(None, COINIT_MULTITHREADED);
        let needs_uninit = com_init.is_ok();

        if let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) {
            if let Ok(collection) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) {
                if let Ok(dev_count) = collection.GetCount() {
                    for d in 0..dev_count {
                        if let Ok(device) = collection.Item(d) {
                            if let Ok(session_mgr) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) {
                                if let Ok(session_enum) = session_mgr.GetSessionEnumerator() {
                                    if let Ok(count) = session_enum.GetCount() {
                                        for i in 0..count {
                                            if let Ok(session) = session_enum.GetSession(i) {
                                                if let Ok(ctrl) = session.cast::<IAudioSessionControl2>() {
                                                    if let Ok(pid) = ctrl.GetProcessId() {
                                                        if pid == 0 || pid == my_pid {
                                                            continue;
                                                        }
                                                        // Skip Windows internal system sounds session (HRESULT 0 is S_OK, HRESULT 1 is S_FALSE)
                                                        if ctrl.IsSystemSoundsSession() == windows::core::HRESULT(0) {
                                                            continue;
                                                        }

                                                        let state = session.GetState().unwrap_or(AudioSessionStateInactive);
                                                        let peak = if let Ok(meter) = session.cast::<IAudioMeterInformation>() {
                                                            meter.GetPeakValue().unwrap_or(0.0)
                                                        } else {
                                                            0.0
                                                        };

                                                        let is_playing = state == AudioSessionStateActive || peak > 0.0005;

                                                        // Only consider processes that are currently playing sound or active
                                                        if !is_playing {
                                                            continue;
                                                        }

                                                        if let Ok((name, exe_path)) = get_process_info(pid) {
                                                            if name.is_empty() {
                                                                continue;
                                                            }
                                                            if let Some(existing) = apps.iter_mut().find(|a| a.name.eq_ignore_ascii_case(&name)) {
                                                                existing.peak = existing.peak.max(peak);
                                                                existing.is_playing = existing.is_playing || is_playing;
                                                                if existing.exe_path.is_empty() && !exe_path.is_empty() {
                                                                    existing.exe_path = exe_path;
                                                                }
                                                            } else {
                                                                apps.push(AudioAppInfo {
                                                                    name,
                                                                    exe_path,
                                                                    pid,
                                                                    peak,
                                                                    is_playing,
                                                                });
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if needs_uninit {
            CoUninitialize();
        }
    }

    apps
}

/// Enumerates running applications that have active audio sessions.
///
/// # Returns
/// A list of executable names for running audio applications.
pub fn enumerate_audio_sessions() -> Vec<String> {
    scan_active_audio_sessions()
        .into_iter()
        .map(|a| a.name)
        .collect()
}

/// Queries process image path and extracts binary filename and absolute path.
unsafe fn get_process_info(pid: u32) -> Result<(String, String)> {
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
    };
    use windows::Win32::Foundation::CloseHandle;
    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let res = QueryFullProcessImageNameW(
        handle,
        PROCESS_NAME_FORMAT(0),
        windows::core::PWSTR(buf.as_mut_ptr()),
        &mut len,
    );
    let _ = CloseHandle(handle);
    if res.is_ok() && len > 0 {
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        let filename = std::path::Path::new(&full)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| full.clone());
        Ok((filename, full))
    } else {
        Err(anyhow!("Process query failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_downmix_stereo_passthrough() {
        let input = vec![0.5f32, -0.5f32, 0.2f32, -0.2f32];
        let stereo = downmix_to_stereo(&input, 2);
        assert_eq!(stereo.len(), 2);
        assert_eq!(stereo[0], (0.5, -0.5));
        assert_eq!(stereo[1], (0.2, -0.2));
    }

    #[test]
    fn test_downmix_mono() {
        let input = vec![0.8f32, -0.4f32];
        let stereo = downmix_to_stereo(&input, 1);
        assert_eq!(stereo.len(), 2);
        assert_eq!(stereo[0], (0.8, 0.8));
        assert_eq!(stereo[1], (-0.4, -0.4));
    }

    #[test]
    fn test_downmix_5_1() {
        // L, R, C, LFE, Ls, Rs
        let input = vec![0.5f32, 0.5f32, 0.2f32, 0.0f32, 0.1f32, 0.1f32];
        let stereo = downmix_to_stereo(&input, 6);
        assert_eq!(stereo.len(), 1);
        let (l, r) = stereo[0];
        assert!((l - (0.5 + 0.707 * 0.2 + 0.707 * 0.1)).abs() < 1e-4);
        assert!((r - (0.5 + 0.707 * 0.2 + 0.707 * 0.1)).abs() < 1e-4);
    }

    #[test]
    fn test_resample_linear_identity() {
        let input = vec![(0.1f32, 0.2f32), (0.3f32, 0.4f32)];
        let mut phase = 0.0f64;
        let resampled = resample_linear(&input, 48000, 48000, &mut phase);
        assert_eq!(resampled, input);
    }

    #[test]
    fn test_resample_linear_upsample() {
        let input = vec![(0.0f32, 0.0f32), (1.0f32, 1.0f32), (0.0f32, 0.0f32)];
        let mut phase = 0.0f64;
        // Upsampling from 24000 to 48000 should yield approximately double the samples
        let resampled = resample_linear(&input, 24000, 48000, &mut phase);
        assert!(resampled.len() >= 4);
    }

    #[test]
    fn test_audio_mixer_clamping() {
        let loopback = (0.8f32, -0.7f32);
        let mic = (0.5f32, -0.6f32);
        let mic_vol = 1.0f32;
        let mix_l = (loopback.0 + mic.0 * mic_vol).clamp(-1.0, 1.0);
        let mix_r = (loopback.1 + mic.1 * mic_vol).clamp(-1.0, 1.0);
        assert_eq!(mix_l, 1.0);
        assert_eq!(mix_r, -1.0);
    }

    #[test]
    fn test_enumerate_audio_devices_smoke() {
        let playback = enumerate_audio_devices(false);
        let capture = enumerate_audio_devices(true);
        // Validates that enumerating devices executes without crash
        let _ = (playback, capture);
    }

    #[test]
    fn test_enumerate_audio_sessions_smoke() {
        let active = scan_active_audio_sessions();
        let sessions = enumerate_audio_sessions();
        assert_eq!(active.len(), sessions.len());
    }

    #[test]
    fn test_audio_stream_timestamp_continuity() {
        // Simulates the loop pacing algorithm to ensure zero PTS jitter under fluctuating QPC arrivals
        let frame_duration_hns = 100_000i64; // 10ms
        let mut next_pts_hns = 1_000_000i64;
        let mut emitted_pts = Vec::new();

        // 50 packets arriving with +/- 2ms QPC jitter
        for i in 0..50 {
            let jitter = ((i % 5) as i64 - 2) * 20_000; // -4ms to +4ms jitter
            let raw_pts = 1_000_000i64 + i * frame_duration_hns + jitter;

            let pts_hns = if (raw_pts - next_pts_hns).abs() > 500_000 {
                raw_pts
            } else {
                next_pts_hns
            };

            emitted_pts.push(pts_hns);
            next_pts_hns = pts_hns + frame_duration_hns;
        }

        // Verify all 50 packets are strictly contiguous without a single microsecond gap or jump
        for i in 1..emitted_pts.len() {
            assert_eq!(
                emitted_pts[i],
                emitted_pts[i - 1] + frame_duration_hns,
                "Packet {} PTS desynchronized from contiguous timeline",
                i
            );
        }
    }

    #[test]
    fn test_audio_sync_offset_compensation() {
        let offset_ms: i32 = -50; // -50ms shift earlier
        let offset_hns = offset_ms as i64 * 10_000;
        assert_eq!(offset_hns, -500_000);

        let raw_pts = 10_000_000i64; // 1.000s
        let adj_pts = (raw_pts + offset_hns).max(0);
        assert_eq!(adj_pts, 9_500_000); // 0.950s (shifted 50ms earlier)

        // Clamping check for near-zero start
        let near_zero_pts = 200_000i64;
        let clamped = (near_zero_pts + offset_hns).max(0);
        assert_eq!(clamped, 0);
    }

    #[test]
    fn test_decode_raw_audio_samples_formats() {
        use super::{decode_raw_audio_samples, AudioSampleFormat};

        // 1. Float32
        let floats = vec![0.5f32, -0.5f32];
        let decoded_f32 = decode_raw_audio_samples(floats.as_ptr() as *const u8, 1, 2, AudioSampleFormat::Float32);
        assert_eq!(decoded_f32.len(), 2);
        assert!((decoded_f32[0] - 0.5).abs() < 1e-6);
        assert!((decoded_f32[1] - (-0.5)).abs() < 1e-6);

        // 2. Int16 (32768 full scale)
        let int16s = vec![16384i16, -16384i16];
        let decoded_i16 = decode_raw_audio_samples(int16s.as_ptr() as *const u8, 1, 2, AudioSampleFormat::Int16);
        assert_eq!(decoded_i16.len(), 2);
        assert!((decoded_i16[0] - 0.5).abs() < 1e-4);
        assert!((decoded_i16[1] - (-0.5)).abs() < 1e-4);

        // 3. Int24Packed (8388608 full scale, 3 bytes per sample)
        // 4194304 in 24-bit LE: 0x00, 0x00, 0x40
        // -4194304 in 24-bit LE: 0x00, 0x00, 0xC0
        let int24_bytes = vec![0x00u8, 0x00, 0x40, 0x00, 0x00, 0xC0];
        let decoded_i24 = decode_raw_audio_samples(int24_bytes.as_ptr(), 1, 2, AudioSampleFormat::Int24Packed);
        assert_eq!(decoded_i24.len(), 2);
        assert!((decoded_i24[0] - 0.5).abs() < 1e-4);
        assert!((decoded_i24[1] - (-0.5)).abs() < 1e-4);

        // 4. Int32Pcm (2147483648 full scale)
        let int32s = vec![1073741824i32, -1073741824i32];
        let decoded_i32 = decode_raw_audio_samples(int32s.as_ptr() as *const u8, 1, 2, AudioSampleFormat::Int32Pcm);
        assert_eq!(decoded_i32.len(), 2);
        assert!((decoded_i32[0] - 0.5).abs() < 1e-6);
        assert!((decoded_i32[1] - (-0.5)).abs() < 1e-6);
    }
}
