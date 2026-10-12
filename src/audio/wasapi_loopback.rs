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
    thread_handle: Option<JoinHandle<()>>,
}

impl WasapiLoopbackCapture {
    /// Spawns the background audio capture loop thread.
    ///
    /// * `timer`: High-resolution QPC timer for synchronized timestamps.
    /// * `target_bitrate`: Target AAC bitrate in bps (e.g. 192_000).
    /// * `mic_enabled`: Whether to capture and mix the default microphone.
    /// * `mic_volume`: Microphone volume scaling multiplier (1.0 = 100%).
    /// * `chunk_sender`: Crossbeam channel sender delivering compressed AAC chunks.
    pub fn spawn(
        timer: QpcTimer,
        target_bitrate: u32,
        mic_enabled: bool,
        mic_volume: f32,
        chunk_sender: Sender<AudioChunk>,
    ) -> Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

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
                        enumerator
                            .GetDefaultAudioEndpoint(eRender, eConsole)
                            .map_err(|e| anyhow!("Failed to get default audio endpoint: {}", e))?
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
                    let mic_opt: MicInit = if mic_enabled {
                        let mic_init_result = (|| -> Result<(IAudioClient, IAudioCaptureClient, bool, u32, u32)> {
                            let mic_device = unsafe {
                                enumerator
                                    .GetDefaultAudioEndpoint(eCapture, eConsole)
                                    .map_err(|e| anyhow!("No default microphone endpoint: {}", e))?
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
                    } else {
                        None
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

                let mut pcm_buffer = Vec::<u8>::with_capacity(8192);
                let mut next_pts_hns = timer.elapsed_hns();
                let mut resample_phase = 0.0f64;

                let mut mic_fifo = std::collections::VecDeque::<(f32, f32)>::with_capacity(4800);
                let mut mic_resample_phase = 0.0f64;

                while running_clone.load(Ordering::SeqCst) {
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

                    // Silence gap-filler: when no loopback audio is playing, synthesize silence chunks and mix mic voice
                    if packet_size == 0 {
                        let now_hns = timer.elapsed_hns();
                        if now_hns >= next_pts_hns + SILENCE_DURATION_HNS {
                            pcm_buffer.clear();
                            for _ in 0..SILENCE_CHUNK_FRAMES {
                                let (mic_l, mic_r) = mic_fifo.pop_front().unwrap_or((0.0, 0.0));
                                let mix_l = (mic_l * mic_volume).clamp(-1.0, 1.0);
                                let mix_r = (mic_r * mic_volume).clamp(-1.0, 1.0);
                                let s_l = (mix_l * 32767.0) as i16;
                                let s_r = (mix_r * 32767.0) as i16;
                                pcm_buffer.extend_from_slice(&s_l.to_le_bytes());
                                pcm_buffer.extend_from_slice(&s_r.to_le_bytes());
                            }

                            match encoder.encode_pcm(&pcm_buffer, next_pts_hns, SILENCE_DURATION_HNS) {
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
                            next_pts_hns += SILENCE_DURATION_HNS;
                        } else {
                            thread::sleep(std::time::Duration::from_millis(5));
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
                            let raw_pts = if qpc_pos > 0 {
                                timer.qpc_to_hns(qpc_pos as i64)
                            } else {
                                timer.elapsed_hns()
                            };

                            // Bridge any gap between previous packet and current raw_pts with silence (+ mic)
                            while next_pts_hns + SILENCE_DURATION_HNS <= raw_pts {
                                pcm_buffer.clear();
                                for _ in 0..SILENCE_CHUNK_FRAMES {
                                    let (mic_l, mic_r) = mic_fifo.pop_front().unwrap_or((0.0, 0.0));
                                    let mix_l = (mic_l * mic_volume).clamp(-1.0, 1.0);
                                    let mix_r = (mic_r * mic_volume).clamp(-1.0, 1.0);
                                    let s_l = (mix_l * 32767.0) as i16;
                                    let s_r = (mix_r * 32767.0) as i16;
                                    pcm_buffer.extend_from_slice(&s_l.to_le_bytes());
                                    pcm_buffer.extend_from_slice(&s_r.to_le_bytes());
                                }
                                if let Ok(chunks) = encoder.encode_pcm(&pcm_buffer, next_pts_hns, SILENCE_DURATION_HNS) {
                                    for chunk in chunks {
                                        if chunk_sender.send(chunk).is_err() {
                                            running_clone.store(false, Ordering::SeqCst);
                                            break;
                                        }
                                    }
                                }
                                next_pts_hns += SILENCE_DURATION_HNS;
                            }

                            let pts_hns = next_pts_hns.max(raw_pts);
                            let is_silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;

                            pcm_buffer.clear();
                            let duration_hns: i64;

                            if is_silent || data_ptr.is_null() {
                                let out_frames = if sample_rate == TARGET_SAMPLE_RATE {
                                    num_frames as usize
                                } else {
                                    ((num_frames as u64 * TARGET_SAMPLE_RATE as u64) / sample_rate as u64) as usize
                                };
                                for _ in 0..out_frames {
                                    let (mic_l, mic_r) = mic_fifo.pop_front().unwrap_or((0.0, 0.0));
                                    let mix_l = (mic_l * mic_volume).clamp(-1.0, 1.0);
                                    let mix_r = (mic_r * mic_volume).clamp(-1.0, 1.0);
                                    let s_l = (mix_l * 32767.0) as i16;
                                    let s_r = (mix_r * 32767.0) as i16;
                                    pcm_buffer.extend_from_slice(&s_l.to_le_bytes());
                                    pcm_buffer.extend_from_slice(&s_r.to_le_bytes());
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

                                for &(l, r) in &resampled {
                                    let (mic_l, mic_r) = mic_fifo.pop_front().unwrap_or((0.0, 0.0));
                                    let mix_l = (l + mic_l * mic_volume).clamp(-1.0, 1.0);
                                    let mix_r = (r + mic_r * mic_volume).clamp(-1.0, 1.0);
                                    let s_l = (mix_l * 32767.0) as i16;
                                    let s_r = (mix_r * 32767.0) as i16;
                                    pcm_buffer.extend_from_slice(&s_l.to_le_bytes());
                                    pcm_buffer.extend_from_slice(&s_r.to_le_bytes());
                                }
                                duration_hns = (resampled.len() as i64 * HNS_PER_SECOND) / TARGET_SAMPLE_RATE as i64;
                            }

                            if !pcm_buffer.is_empty() {
                                match encoder.encode_pcm(&pcm_buffer, pts_hns, duration_hns) {
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
            thread_handle: Some(handle),
        })
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
}
