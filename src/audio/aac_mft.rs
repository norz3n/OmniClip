use anyhow::{anyhow, Result};
use log::debug;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

use crate::buffer::chunk::AudioChunk;

/// Media Foundation AAC Encoder MFT.
///
/// Encodes raw 16-bit stereo PCM audio at 48000 Hz into standard AAC audio packets.
pub struct AacEncoder {
    transform: IMFTransform,
    stream_info: MFT_OUTPUT_STREAM_INFO,
    provides_samples: bool,
    sample_rate: u32,
    channels: u32,
    bitrate: u32,
}

impl AacEncoder {
    /// Creates a new AAC encoder transform for the given sample rate, channels, and bitrate.
    pub fn new(sample_rate: u32, channels: u32, bitrate: u32) -> Result<Self> {
        let transform: IMFTransform = unsafe {
            CoCreateInstance(&AACMFTEncoder, None, CLSCTX_ALL)
                .map_err(|e| anyhow!("Failed to instantiate AACMFTEncoder: {}", e))?
        };

        // Output Media Type: AAC
        unsafe {
            let out_type: IMFMediaType = MFCreateMediaType()?;
            out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            out_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            out_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)?;
            out_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
            out_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bitrate / 8)?;
            out_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;

            transform
                .SetOutputType(0, &out_type, 0)
                .map_err(|e| anyhow!("Failed to set AAC output media type: {}", e))?;
        }

        // Input Media Type: 16-bit PCM
        unsafe {
            let bytes_per_sample = 2u32;
            let block_align = channels * bytes_per_sample;
            let avg_bytes_per_sec = sample_rate * block_align;

            let in_type: IMFMediaType = MFCreateMediaType()?;
            in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            in_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            in_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)?;
            in_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
            in_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            in_type.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block_align)?;
            in_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, avg_bytes_per_sec)?;

            transform
                .SetInputType(0, &in_type, 0)
                .map_err(|e| anyhow!("Failed to set AAC PCM16 input media type: {}", e))?;

            transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }

        let stream_info = unsafe {
            transform
                .GetOutputStreamInfo(0)
                .map_err(|e| anyhow!("Failed to get AAC output stream info: {}", e))?
        };

        let provides_samples =
            (stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

        Ok(Self {
            transform,
            stream_info,
            provides_samples,
            sample_rate,
            channels,
            bitrate,
        })
    }

    /// Feeds 16-bit interleaved PCM samples with PTS in HNS and drains any generated AAC packets.
    pub fn encode_pcm(
        &mut self,
        pcm_bytes: &[u8],
        pts_hns: i64,
        duration_hns: i64,
    ) -> Result<Vec<AudioChunk>> {
        self.encode_pcm_with_track(pcm_bytes, pts_hns, duration_hns, 0)
    }

    /// Encodes a raw 16-bit interleaved PCM buffer into zero or more compressed AAC chunks with specified track index.
    pub fn encode_pcm_with_track(
        &mut self,
        pcm_bytes: &[u8],
        pts_hns: i64,
        duration_hns: i64,
        track_index: u8,
    ) -> Result<Vec<AudioChunk>> {
        let media_buf: IMFMediaBuffer = unsafe {
            MFCreateMemoryBuffer(pcm_bytes.len() as u32)
                .map_err(|e| anyhow!("Failed to create audio memory buffer: {}", e))?
        };

        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut max_len = 0u32;
        let mut cur_len = 0u32;
        unsafe {
            media_buf
                .Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len))
                .map_err(|e| anyhow!("Failed to lock audio buffer: {}", e))?;
            std::ptr::copy_nonoverlapping(pcm_bytes.as_ptr(), ptr, pcm_bytes.len());
            media_buf.SetCurrentLength(pcm_bytes.len() as u32)?;
            media_buf.Unlock()?;
        }

        let sample: IMFSample = unsafe {
            MFCreateSample().map_err(|e| anyhow!("Failed to create audio MF sample: {}", e))?
        };

        unsafe {
            sample.AddBuffer(&media_buf)?;
            sample.SetSampleTime(pts_hns)?;
            sample.SetSampleDuration(duration_hns)?;

            self.transform
                .ProcessInput(0, &sample, 0)
                .map_err(|e| anyhow!("AAC encoder ProcessInput failed: {}", e))?;
        }

        // Drain available AAC outputs with target track index
        self.drain_outputs_with_track(track_index)
    }

    /// Drains all available compressed AAC samples from the encoder.
    pub fn drain_outputs(&mut self) -> Result<Vec<AudioChunk>> {
        self.drain_outputs_with_track(0)
    }

    /// Drains all available compressed AAC samples from the encoder with specified track index.
    pub fn drain_outputs_with_track(&mut self, track_index: u8) -> Result<Vec<AudioChunk>> {
        let mut chunks = Vec::new();

        loop {
            let mut out_sample: Option<IMFSample> = None;
            if !self.provides_samples {
                let s = unsafe { MFCreateSample()? };
                let buf = unsafe {
                    MFCreateMemoryBuffer(self.stream_info.cbSize.max(4096))?
                };
                unsafe { s.AddBuffer(&buf)? };
                out_sample = Some(s);
            }

            let out_buffer = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(out_sample.clone()),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            };

            let mut output_buffers = [out_buffer];
            let mut status = 0u32;
            let proc_res = unsafe {
                self.transform
                    .ProcessOutput(0, &mut output_buffers, &mut status)
            };

            match proc_res {
                Ok(()) => {
                    let sample_opt = if self.provides_samples {
                        std::mem::take(&mut *output_buffers[0].pSample)
                    } else {
                        out_sample
                    };
                    let _ = std::mem::take(&mut *output_buffers[0].pEvents);

                    if let Some(s) = sample_opt {
                        let pts = unsafe { s.GetSampleTime().unwrap_or(0) };
                        let duration = unsafe { s.GetSampleDuration().unwrap_or(213_333) };

                        let buf = unsafe { s.ConvertToContiguousBuffer()? };
                        let mut ptr: *mut u8 = std::ptr::null_mut();
                        let mut max_len = 0u32;
                        let mut cur_len = 0u32;

                        unsafe {
                            buf.Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len))?;
                        }

                        if !ptr.is_null() && cur_len > 0 {
                            let data = unsafe {
                                std::slice::from_raw_parts(ptr, cur_len as usize).to_vec()
                            };
                            unsafe {
                                let _ = buf.Unlock();
                            }
                            chunks.push(AudioChunk {
                                track_index,
                                pts_hns: pts,
                                duration_hns: duration,
                                data,
                            });
                        } else {
                            unsafe {
                                let _ = buf.Unlock();
                            }
                        }
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                    let _ = std::mem::take(&mut *output_buffers[0].pSample);
                    let _ = std::mem::take(&mut *output_buffers[0].pEvents);
                    break;
                }
                Err(e) => {
                    let _ = std::mem::take(&mut *output_buffers[0].pSample);
                    let _ = std::mem::take(&mut *output_buffers[0].pEvents);
                    debug!("AAC ProcessOutput returned: {}", e);
                    break;
                }
            }
        }

        Ok(chunks)
    }

    /// Audio sample rate in Hz.
    #[inline]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Number of audio channels.
    #[inline]
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Target bitrate in bits per second.
    #[inline]
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::mf_runtime::MfPlatformGuard;

    #[test]
    fn test_aac_encoder_lifecycle() {
        let _guard = MfPlatformGuard::init().unwrap();
        let mut encoder = AacEncoder::new(48000, 2, 192_000).unwrap();
        assert_eq!(encoder.sample_rate(), 48000);
        assert_eq!(encoder.channels(), 2);

        // Feed 100ms of PCM16 silence (4800 samples * 2 channels * 2 bytes = 19200 bytes)
        let pcm_silence = vec![0u8; 19200];
        let chunks = encoder.encode_pcm(&pcm_silence, 0, 1_000_000).unwrap();
        // AAC transform generates frames for 100ms input
        assert!(!chunks.is_empty(), "AAC encoder must generate chunks for 100ms PCM input");
        for chunk in chunks {
            assert!(!chunk.data.is_empty());
            assert!(chunk.duration_hns > 0);
        }
    }
}

