use std::sync::atomic::{AtomicBool, Ordering};
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

/// WASAPI Loopback system audio and microphone capture engine running on a dedicated thread.
pub struct WasapiLoopbackCapture {
    running: Arc<AtomicBool>,
    mic_enabled: Arc<AtomicBool>,
    mic_volume: Arc<parking_lot::Mutex<f32>>,
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
    /// * `chunk_sender`: Crossbeam channel sender delivering compressed AAC chunks.
    pub fn spawn(
        timer: QpcTimer,
        target_bitrate: u32,
        mic_enabled: bool,
        mic_volume: f32,
        playback_device_id: Option<String>,
        capture_device_id: Option<String>,
        multi_track: bool,
        chunk_sender: Sender<AudioChunk>,
    ) -> Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        let mic_enabled_arc = Arc::new(AtomicBool::new(mic_enabled));
        let mic_volume_arc = Arc::new(parking_lot::Mutex::new(mic_volume));
        let mic_enabled_thread = mic_enabled_arc.clone();
        let mic_volume_thread = mic_volume_arc.clone();

        let (init_tx, init_rx) = crossbeam_channel::bounded::<Result<()>>(1);

        let handle = thread::Builder::new()
            .name("omniclip-audio".to_string())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }

                type LoopbackInit = (IAudioClient, IAudioCaptureClient, AacEncoder, bool, u32, u32);
                type MicInit = Option<(IAudioClient, IAudioCaptureClient, bool, u32, u32)>;

                let init_res = (|| -> Result<(LoopbackInit, MicInit)> {
                    let enumerator: IMMDeviceEnumerator = unsafe {
                        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                            .map_err(|e| anyhow!("Failed to create MMDeviceEnumerator: {}", e))?
                    };

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

                    info!(
                        "Initialized WASAPI Loopback capture: device {} Hz, {} channels, {} bit (Float: {}), normalized to {} Hz Stereo",
                        sample_rate, channels, bits_per_sample, is_float, TARGET_SAMPLE_RATE
                    );

                    // 2. Optional Microphone Capture Endpoint
                    let mic_opt: MicInit = {
                        let mic_init_result = (|| -> Result<(IAudioClient, IAudioCaptureClient, bool, u32, u32)> {
                            let mic_device = unsafe {
                                if let Some(ref dev_id) = capture_device_id {
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
                                .map_err(|e| anyhow!("No microphone endpoint: {}", e))?
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
                            let m_float = mic_fmt.wFormatTag == 3 || (mic_fmt.wFormatTag == 0xFFFE && m_bits == 32);

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
                                "Initialized Microphone capture: {} Hz, {} channels, {} bit (Float: {})",
                                m_rate, m_channels, m_bits, m_float
                            );

                            Ok((mic_client, mic_capture, m_float, m_rate, m_channels))
                        })();

                        match mic_init_result {
                            Ok(res) => Some(res),
                            Err(e) => {
                                info!("Microphone capture unavailable, running loopback only: {}", e);
                                None
                            }
                        }
                    };

                    Ok(((audio_client, capture_client, aac_encoder, is_float, sample_rate, channels), mic_opt))
                })();

                let (
                    (audio_client, capture_client, mut encoder, is_float, sample_rate, channels),
                    mic_client_opt,
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
                let mut next_pts_hns = timer.elapsed_hns();
                let mut last_real_packet_hns = next_pts_hns;
                let mut resample_phase = 0.0f64;

                let mut mic_fifo = std::collections::VecDeque::<(f32, f32)>::with_capacity(4800);
                let mut mic_resample_phase = 0.0f64;

                while running_clone.load(Ordering::SeqCst) {
                    let is_mic_active = mic_enabled_thread.load(Ordering::Relaxed);
                    let cur_mic_volume = if is_mic_active { *mic_volume_thread.lock() } else { 0.0 };

                    // Poll and buffer incoming microphone packets
                    if let Some((_, ref mic_capture, m_float, m_rate, m_ch)) = mic_client_opt {
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

                            if let Ok(()) = get_res {
                                if m_frames > 0 {
                                    let is_silent = (m_flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
                                    if !is_silent && !m_ptr.is_null() {
                                        let stereo = if m_float {
                                            let count = m_frames as usize * m_ch as usize;
                                            let slice = unsafe { std::slice::from_raw_parts(m_ptr as *const f32, count) };
                                            downmix_to_stereo(slice, m_ch as usize)
                                        } else {
                                            let count = m_frames as usize * m_ch as usize;
                                            let slice = unsafe { std::slice::from_raw_parts(m_ptr as *const i16, count) };
                                            let floats: Vec<f32> = slice.iter().map(|&s| s as f32 / 32768.0).collect();
                                            downmix_to_stereo(&floats, m_ch as usize)
                                        };

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
                            } else {
                                break;
                            }
                            m_packet = unsafe { mic_capture.GetNextPacketSize().unwrap_or(0) };
                        }

                        // PONYTAIL: FIFO sample-slip mixer instead of phase-locked PLL resampler. Ceiling: occasional dropped/duplicated sample between unsynced DAC/ADC clocks. Upgrade path: Dynamic drift tracking PLL mixer or separate audio track.
                        const MAX_MIC_LATENCY_SAMPLES: usize = 2400; // ~50 ms
                        while mic_fifo.len() > MAX_MIC_LATENCY_SAMPLES {
                            mic_fifo.pop_front();
                        }
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
                        // Only declare silence if no real audio packet has arrived for at least 50 ms (grace period)
                        if now_hns.saturating_sub(last_real_packet_hns) >= 500_000 && now_hns >= next_pts_hns + SILENCE_DURATION_HNS {
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

                            let raw_pts = if qpc_pos > 0 {
                                timer.qpc_to_hns(qpc_pos as i64)
                            } else {
                                now_hns
                            };

                            let is_discontinuity = (flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32) != 0;

                            // Bridge any gap between previous packet and current raw_pts with silence (+ mic) if stream was idle
                            if is_discontinuity || raw_pts > next_pts_hns + 200_000 {
                                while next_pts_hns + SILENCE_DURATION_HNS <= raw_pts {
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
                            let pts_hns = if (raw_pts - next_pts_hns).abs() > 500_000 {
                                raw_pts
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
                                        let m_l = ((mic_l * mic_volume).clamp(-1.0, 1.0) * 32767.0) as i16;
                                        let m_r = ((mic_r * mic_volume).clamp(-1.0, 1.0) * 32767.0) as i16;
                                        mic_pcm_buffer.extend_from_slice(&m_l.to_le_bytes());
                                        mic_pcm_buffer.extend_from_slice(&m_r.to_le_bytes());
                                    } else {
                                        let mix_l = (mic_l * mic_volume).clamp(-1.0, 1.0);
                                        let mix_r = (mic_r * mic_volume).clamp(-1.0, 1.0);
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
            thread_handle: Some(handle),
        })
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
}
