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

        let attrs: IMFAttributes = unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 3)?;
            let attrs = attrs.unwrap();
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            attrs.SetUINT32(&MF_LOW_LATENCY, 1)?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attrs
        };

        let sink_writer: IMFSinkWriter = unsafe {
            MFCreateSinkWriterFromURL(&url, None, Some(&attrs))
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

        // Configure Audio Stream (AAC Passthrough) with multi-track support
        let has_multi_track = snapshot.audio_chunks.iter().any(|c| c.track_index == 1);
        let mut aud_stream_0_idx = None;
        let mut aud_stream_1_idx = None;

        if !snapshot.audio_chunks.is_empty() {
            // Track 0: System / Game Audio
            let aud_type: IMFMediaType = unsafe { MFCreateMediaType()? };
            unsafe {
                aud_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                aud_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                aud_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, snapshot.audio_sample_rate)?;
                aud_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, snapshot.audio_channels)?;
                aud_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 192_000 / 8)?;
                aud_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            }

            let aud_idx_0 = unsafe {
                sink_writer
                    .AddStream(&aud_type)
                    .map_err(|e| anyhow!("Failed to add audio stream 0 to SinkWriter: {}", e))?
            };

            unsafe {
                sink_writer
                    .SetInputMediaType(aud_idx_0, &aud_type, None)
                    .map_err(|e| anyhow!("Failed to set audio input media type 0 on SinkWriter: {}", e))?;
            }

            aud_stream_0_idx = Some(aud_idx_0);

            // Track 1: Microphone Audio if separate tracks are present
            if has_multi_track {
                let aud_type_mic: IMFMediaType = unsafe { MFCreateMediaType()? };
                unsafe {
                    aud_type_mic.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                    aud_type_mic.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                    aud_type_mic.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, snapshot.audio_sample_rate)?;
                    aud_type_mic.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, snapshot.audio_channels)?;
                    aud_type_mic.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 192_000 / 8)?;
                    aud_type_mic.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                }

                let aud_idx_1 = unsafe {
                    sink_writer
                        .AddStream(&aud_type_mic)
                        .map_err(|e| anyhow!("Failed to add audio stream 1 (Mic) to SinkWriter: {}", e))?
                };

                unsafe {
                    sink_writer
                        .SetInputMediaType(aud_idx_1, &aud_type_mic, None)
                        .map_err(|e| anyhow!("Failed to set audio input media type 1 on SinkWriter: {}", e))?;
                }

                aud_stream_1_idx = Some(aud_idx_1);
            }
        }

        // Begin Muxing
        unsafe {
            sink_writer
                .BeginWriting()
                .map_err(|e| anyhow!("SinkWriter BeginWriting failed: {}", e))?;
        }

        // Normalize timestamps relative to the first video frame (t = 0)
        let base_pts = snapshot.video_chunks.first().unwrap().pts_hns;

        // Chronologically interleaved streaming:
        // Feeding video and audio interleaved by PTS ensures the MP4 container never has
        // to buffer huge streams in memory or back-seek on disk during Finalize().
        let mut v_idx = 0;
        let mut a_idx = 0;
        let v_len = snapshot.video_chunks.len();
        let a_len = snapshot.audio_chunks.len();

        // Skip any audio preceding base_pts
        while a_idx < a_len && snapshot.audio_chunks[a_idx].pts_hns < base_pts {
            a_idx += 1;
        }

        while v_idx < v_len || a_idx < a_len {
            let next_is_video = if v_idx >= v_len {
                false
            } else if a_idx >= a_len {
                true
            } else {
                snapshot.video_chunks[v_idx].pts_hns <= snapshot.audio_chunks[a_idx].pts_hns
            };

            if next_is_video {
                let chunk = &snapshot.video_chunks[v_idx];
                v_idx += 1;
                let rel_pts = chunk.pts_hns.saturating_sub(base_pts);
                unsafe {
                    Self::write_chunk_sample(
                        &sink_writer,
                        vid_idx,
                        &chunk.data,
                        rel_pts,
                        chunk.duration_hns,
                        chunk.is_keyframe,
                    )?;
                }
            } else if let Some(aud_idx_0) = aud_stream_0_idx {
                let chunk = &snapshot.audio_chunks[a_idx];
                a_idx += 1;
                let target_stream = if chunk.track_index == 1 {
                    aud_stream_1_idx.unwrap_or(aud_idx_0)
                } else {
                    aud_idx_0
                };
                let rel_pts = chunk.pts_hns.saturating_sub(base_pts);
                unsafe {
                    Self::write_chunk_sample(
                        &sink_writer,
                        target_stream,
                        &chunk.data,
                        rel_pts,
                        chunk.duration_hns,
                        false,
                    )?;
                }
            } else {
                a_idx += 1;
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
            "Muxed replay to '{}' in {:.2?} ({:.2} MB, {} video chunks, {} audio chunks{})",
            destination_path.display(),
            elapsed,
            file_size as f64 / (1024.0 * 1024.0),
            snapshot.video_chunks.len(),
            snapshot.audio_chunks.len(),
            if has_multi_track { ", multi-track enabled" } else { "" }
        );

        Ok(destination_path.to_path_buf())
    }

    /// Expands a filename pattern into a sanitized `.mp4` file path inside `output_dir`.
    pub fn format_replay_filename(
        pattern: &str,
        output_dir: &Path,
        width: u32,
        height: u32,
        fps: u32,
    ) -> PathBuf {
        let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };

        let year = format!("{:04}", st.wYear);
        let month = format!("{:02}", st.wMonth);
        let day = format!("{:02}", st.wDay);
        let hour = format!("{:02}", st.wHour);
        let minute = format!("{:02}", st.wMinute);
        let second = format!("{:02}", st.wSecond);
        let date_str = format!("{}-{}-{}", year, month, day);
        let time_str = format!("{}-{}-{}", hour, minute, second);
        let res_str = format!("{}x{}", width, height);
        let fps_str = format!("{}fps", fps);

        let now_unix = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs().to_string(),
            Err(_) => "0".to_string(),
        };

        let mut name = if pattern.trim().is_empty() {
            "Replay_%Y-%m-%d_%H-%M-%S".to_string()
        } else {
            pattern.to_string()
        };

        name = name
            .replace("%Y", &year)
            .replace("%m", &month)
            .replace("%d", &day)
            .replace("%H", &hour)
            .replace("%M", &minute)
            .replace("%S", &second)
            .replace("{date}", &date_str)
            .replace("{time}", &time_str)
            .replace("{year}", &year)
            .replace("{month}", &month)
            .replace("{day}", &day)
            .replace("{hour}", &hour)
            .replace("{minute}", &minute)
            .replace("{second}", &second)
            .replace("{resolution}", &res_str)
            .replace("{fps}", &fps_str)
            .replace("{id}", &now_unix);

        // Sanitize illegal Windows filename characters (< > : " / \ | ? *)
        let sanitized: String = name
            .chars()
            .map(|c| match c {
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
                c if c.is_control() => '_',
                c => c,
            })
            .collect();

        let final_filename = if sanitized.to_lowercase().ends_with(".mp4") {
            sanitized
        } else {
            format!("{}.mp4", sanitized)
        };

        output_dir.join(final_filename)
    }

    /// Generates a standardized timestamp-based replay filename.
    pub fn generate_replay_filename(output_dir: &Path) -> PathBuf {
        Self::format_replay_filename("Replay_%Y-%m-%d_%H-%M-%S", output_dir, 1920, 1080, 60)
    }

    /// Writes a single encoded chunk as a Media Foundation sample.
    #[inline]
    unsafe fn write_chunk_sample(
        sink_writer: &IMFSinkWriter,
        stream_idx: u32,
        data: &[u8],
        relative_pts: i64,
        duration_hns: i64,
        is_keyframe: bool,
    ) -> Result<()> {
        let media_buf: IMFMediaBuffer = MFCreateMemoryBuffer(data.len() as u32)
            .map_err(|e| anyhow!("Failed to create memory buffer: {}", e))?;

        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut max_len = 0u32;
        let mut cur_len = 0u32;
        media_buf.Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len))?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        media_buf.SetCurrentLength(data.len() as u32)?;
        media_buf.Unlock()?;

        let sample: IMFSample = MFCreateSample()
            .map_err(|e| anyhow!("Failed to create sample: {}", e))?;

        sample.AddBuffer(&media_buf)?;
        sample.SetSampleTime(relative_pts)?;
        sample.SetSampleDuration(duration_hns)?;

        if is_keyframe {
            sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
        }

        sink_writer
            .WriteSample(stream_idx, &sample)
            .map_err(|e| anyhow!("Failed to write sample: {}", e))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_replay_filename_tokens() {
        let dir = Path::new("C:/Videos");
        let path = Mp4Muxer::format_replay_filename("Replay_{date}_{resolution}_{fps}", dir, 1920, 1080, 60);
        let filename = path.file_name().unwrap().to_string_lossy();
        assert!(filename.starts_with("Replay_"));
        assert!(filename.contains("1920x1080"));
        assert!(filename.contains("60fps"));
        assert!(filename.ends_with(".mp4"));
    }

    #[test]
    fn test_format_replay_filename_sanitization() {
        let dir = Path::new("C:/Videos");
        let path = Mp4Muxer::format_replay_filename("Clip:<invalid>*?|name", dir, 1920, 1080, 60);
        let filename = path.file_name().unwrap().to_string_lossy();
        assert!(!filename.contains(':'));
        assert!(!filename.contains('*'));
        assert!(!filename.contains('?'));
        assert!(!filename.contains('|'));
        assert!(filename.ends_with(".mp4"));
    }
}
