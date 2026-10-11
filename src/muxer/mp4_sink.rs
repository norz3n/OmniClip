use std::path::{Path, PathBuf};
use std::time::Instant;
use anyhow::{anyhow, Result};
use log::info;
use windows::core::HSTRING;
use windows::Win32::Media::MediaFoundation::*;

use crate::buffer::chunk::RingBufferSnapshot;
use crate::encoder::h264_mft::pack_u64;

/// Ultra-fast streaming MP4 remuxer using Media Foundation Sink Writer.
///
/// Multi-plexes pre-encoded H.264 video and AAC audio packets directly into an ISO MP4 container
/// with zero re-encoding (< 300 ms for typical 60s replays).
pub struct Mp4Muxer;

impl Mp4Muxer {
    /// Muxes a RingBufferSnapshot into a standalone MP4 file.
    ///
    /// * `snapshot`: Point-in-time snapshot containing aligned video and audio chunks.
    /// * `destination_path`: Target `.mp4` file path.
    pub fn write_replay(snapshot: &RingBufferSnapshot, destination_path: &Path) -> Result<PathBuf> {
        if snapshot.video_chunks.is_empty() {
            return Err(anyhow!("Cannot write empty replay snapshot"));
        }

        let start_time = Instant::now();

        // Ensure parent directory exists
        if let Some(parent) = destination_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("Failed to create replay directory: {}", e))?;
        }

        let path_str = destination_path
            .to_str()
            .ok_or_else(|| anyhow!("Invalid destination path string"))?;
        let url = HSTRING::from(path_str);

        let sink_writer: IMFSinkWriter = unsafe {
            MFCreateSinkWriterFromURL(&url, None, None)
                .map_err(|e| anyhow!("Failed to create SinkWriter for '{}': {}", path_str, e))?
        };

        // Configure Video Stream (H.264 Passthrough)
        let vid_type: IMFMediaType = unsafe { MFCreateMediaType()? };
        unsafe {
            vid_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            vid_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            vid_type.SetUINT32(&MF_MT_AVG_BITRATE, 35_000_000)?;
            vid_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(snapshot.width, snapshot.height))?;
            vid_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(snapshot.fps, 1))?;
            vid_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
            vid_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        }

        let vid_idx = unsafe {
            sink_writer
                .AddStream(&vid_type)
                .map_err(|e| anyhow!("Failed to add video stream to SinkWriter: {}", e))?
        };

        unsafe {
            sink_writer
                .SetInputMediaType(vid_idx, &vid_type, None)
                .map_err(|e| anyhow!("Failed to set video input media type on SinkWriter: {}", e))?;
        }

        // Configure Audio Stream (AAC Passthrough) if audio chunks are present
        let mut aud_stream_idx = None;
        if !snapshot.audio_chunks.is_empty() {
            let aud_type: IMFMediaType = unsafe { MFCreateMediaType()? };
            unsafe {
                aud_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                aud_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                aud_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, snapshot.audio_sample_rate)?;
                aud_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, snapshot.audio_channels)?;
                aud_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 192_000 / 8)?;
                aud_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            }

            let aud_idx = unsafe {
                sink_writer
                    .AddStream(&aud_type)
                    .map_err(|e| anyhow!("Failed to add audio stream to SinkWriter: {}", e))?
            };

            unsafe {
                sink_writer
                    .SetInputMediaType(aud_idx, &aud_type, None)
                    .map_err(|e| anyhow!("Failed to set audio input media type on SinkWriter: {}", e))?;
            }

            aud_stream_idx = Some(aud_idx);
        }

        // Begin Muxing
        unsafe {
            sink_writer
                .BeginWriting()
                .map_err(|e| anyhow!("SinkWriter BeginWriting failed: {}", e))?;
        }

        // Normalize timestamps relative to the first video frame (t = 0)
        let base_pts = snapshot.video_chunks.first().unwrap().pts_hns;

        // Write Video Samples
        for chunk in &snapshot.video_chunks {
            let relative_pts = chunk.pts_hns.saturating_sub(base_pts);

            let media_buf: IMFMediaBuffer = unsafe {
                MFCreateMemoryBuffer(chunk.data.len() as u32)
                    .map_err(|e| anyhow!("Failed to create memory buffer for video sample: {}", e))?
            };

            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut max_len = 0u32;
            let mut cur_len = 0u32;
            unsafe {
                media_buf.Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len))?;
                std::ptr::copy_nonoverlapping(chunk.data.as_ptr(), ptr, chunk.data.len());
                media_buf.SetCurrentLength(chunk.data.len() as u32)?;
                media_buf.Unlock()?;
            }

            let sample: IMFSample = unsafe {
                MFCreateSample().map_err(|e| anyhow!("Failed to create video sample: {}", e))?
            };

            unsafe {
                sample.AddBuffer(&media_buf)?;
                sample.SetSampleTime(relative_pts)?;
                sample.SetSampleDuration(chunk.duration_hns)?;

                if chunk.is_keyframe {
                    sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
                }

                sink_writer
                    .WriteSample(vid_idx, &sample)
                    .map_err(|e| anyhow!("Failed to write video sample: {}", e))?;
            }
        }

        // Write Audio Samples
        if let Some(aud_idx) = aud_stream_idx {
            for chunk in &snapshot.audio_chunks {
                if chunk.pts_hns < base_pts {
                    continue; // Skip audio preceding initial video keyframe
                }

                let relative_pts = chunk.pts_hns.saturating_sub(base_pts);

                let media_buf: IMFMediaBuffer = unsafe {
                    MFCreateMemoryBuffer(chunk.data.len() as u32)
                        .map_err(|e| anyhow!("Failed to create memory buffer for audio sample: {}", e))?
                };

                let mut ptr: *mut u8 = std::ptr::null_mut();
                let mut max_len = 0u32;
                let mut cur_len = 0u32;
                unsafe {
                    media_buf.Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len))?;
                    std::ptr::copy_nonoverlapping(chunk.data.as_ptr(), ptr, chunk.data.len());
                    media_buf.SetCurrentLength(chunk.data.len() as u32)?;
                    media_buf.Unlock()?;
                }

                let sample: IMFSample = unsafe {
                    MFCreateSample().map_err(|e| anyhow!("Failed to create audio sample: {}", e))?
                };

                unsafe {
                    sample.AddBuffer(&media_buf)?;
                    sample.SetSampleTime(relative_pts)?;
                    sample.SetSampleDuration(chunk.duration_hns)?;

                    sink_writer
                        .WriteSample(aud_idx, &sample)
                        .map_err(|e| anyhow!("Failed to write audio sample: {}", e))?;
                }
            }
        }

        // Finalize MP4 File
        unsafe {
            sink_writer
                .Finalize()
                .map_err(|e| anyhow!("SinkWriter Finalize failed: {}", e))?;
        }

        let elapsed = start_time.elapsed();
        let file_size = std::fs::metadata(destination_path)
            .map(|m| m.len())
            .unwrap_or(0);

        info!(
            "Muxed replay to '{}' in {:.2?} ({:.2} MB, {} video chunks, {} audio chunks)",
            destination_path.display(),
            elapsed,
            file_size as f64 / (1024.0 * 1024.0),
            snapshot.video_chunks.len(),
            snapshot.audio_chunks.len()
        );

        Ok(destination_path.to_path_buf())
    }

    /// Generates a standardized timestamp-based replay filename.
    pub fn generate_replay_filename(output_dir: &Path) -> PathBuf {
        let now = std::time::SystemTime::now();
        let datetime = match now.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs(),
            Err(_) => 0,
        };

        // Format: Replay_<unix_timestamp>.mp4
        output_dir.join(format!("Replay_{}.mp4", datetime))
    }
}
