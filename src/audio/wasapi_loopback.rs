use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use anyhow::{anyhow, Result};
use crossbeam_channel::Sender;
use log::{debug, info, warn};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};

use crate::audio::aac_mft::AacEncoder;
use crate::buffer::chunk::AudioChunk;
use crate::platform::time::QpcTimer;

/// WASAPI Loopback system audio capture engine running on a dedicated thread.
pub struct WasapiLoopbackCapture {
    running: Arc<AtomicBool>,
    thread_handle: Option<JoinHandle<()>>,
}

impl WasapiLoopbackCapture {
    /// Spawns the background audio capture loop thread.
    ///
    /// * `timer`: High-resolution QPC timer for synchronized timestamps.
    /// * `target_bitrate`: Target AAC bitrate in bps (e.g. 192_000).
    /// * `chunk_sender`: Crossbeam channel sender delivering compressed AAC chunks.
    pub fn spawn(
        timer: QpcTimer,
        target_bitrate: u32,
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

                let init_res = (|| -> Result<(IAudioClient, IAudioCaptureClient, AacEncoder, bool, u32, u32)> {
                    let enumerator: IMMDeviceEnumerator = unsafe {
                        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                            .map_err(|e| anyhow!("Failed to create MMDeviceEnumerator: {}", e))?
                    };

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

                    let buffer_duration_hns: i64 = 10_000_000; // 1s buffer
                    unsafe {
                        audio_client
                            .Initialize(
                                AUDCLNT_SHAREMODE_SHARED,
                                AUDCLNT_STREAMFLAGS_LOOPBACK,
                                buffer_duration_hns,
                                0,
                                mix_format_ptr,
                                None,
                            )
                            .map_err(|e| anyhow!("Failed to initialize IAudioClient: {}", e))?;
                    }

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

                    let aac_encoder = AacEncoder::new(sample_rate, channels, target_bitrate)?;

                    info!(
                        "Initialized WASAPI Loopback capture: {} Hz, {} channels, {} bit (Float: {})",
                        sample_rate, channels, bits_per_sample, is_float
                    );

                    Ok((audio_client, capture_client, aac_encoder, is_float, sample_rate, channels))
                })();

                let (audio_client, capture_client, mut encoder, is_float, sample_rate, channels) = match init_res {
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

                while running_clone.load(Ordering::SeqCst) {
                    let mut packet_size = match unsafe { capture_client.GetNextPacketSize() } {
                        Ok(sz) => sz,
                        Err(e) => {
                            warn!("GetNextPacketSize failed: {}", e);
                            break;
                        }
                    };

                    if packet_size == 0 {
                        // Sleep briefly between polling intervals
                        thread::sleep(std::time::Duration::from_millis(5));
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
                            pcm_buffer.clear();
                            let is_silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;

                            if is_silent || data_ptr.is_null() {
                                // Fill with PCM16 zeroes
                                let zero_bytes = (num_frames * channels * 2) as usize;
                                pcm_buffer.resize(zero_bytes, 0);
                            } else if is_float {
                                // Convert 32-bit IEEE float to 16-bit signed PCM
                                let float_count = (num_frames * channels) as usize;
                                let float_slice = unsafe {
                                    std::slice::from_raw_parts(data_ptr as *const f32, float_count)
                                };
                                for &val in float_slice {
                                    let sample_i16 = (val.clamp(-1.0, 1.0) * 32767.0) as i16;
                                    pcm_buffer.extend_from_slice(&sample_i16.to_le_bytes());
                                }
                            } else {
                                // Raw 16-bit PCM bytes
                                let byte_count = (num_frames * channels * 2) as usize;
                                let raw_slice = unsafe {
                                    std::slice::from_raw_parts(data_ptr, byte_count)
                                };
                                pcm_buffer.extend_from_slice(raw_slice);
                            }

                            // Calculate PTS based on QPC timestamp
                            let pts_hns = if qpc_pos > 0 {
                                timer.qpc_to_hns(qpc_pos as i64)
                            } else {
                                timer.elapsed_hns()
                            };

                            let duration_hns = (num_frames as i64 * 10_000_000i64) / sample_rate as i64;

                            match encoder.encode_pcm(&pcm_buffer, pts_hns, duration_hns) {
                                Ok(chunks) => {
                                    for chunk in chunks {
                                        if let Err(_) = chunk_sender.send(chunk) {
                                            // Channel closed, terminate capture thread
                                            running_clone.store(false, Ordering::SeqCst);
                                            break;
                                        }
                                    }
                                }
                                Err(e) => {
                                    debug!("Audio encode error: {}", e);
                                }
                            }
                        }

                        let _ = unsafe { capture_client.ReleaseBuffer(num_frames) };
                        packet_size = unsafe { capture_client.GetNextPacketSize().unwrap_or(0) };
                    }
                }

                let _ = unsafe { audio_client.Stop() };
                unsafe { CoUninitialize(); }
                info!("Stopped WASAPI Loopback capture thread");
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
