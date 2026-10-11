use std::collections::VecDeque;
use std::sync::Arc;
use parking_lot::Mutex;
use log::debug;

use crate::buffer::chunk::{AudioChunk, RingBufferSnapshot, VideoChunk};

/// High-performance circular buffer for compressed video and audio streams.
///
/// Enforces IDR-keyframe alignment: the buffer always starts with a valid IDR keyframe,
/// allowing zero-transcoding instant replay dumping.
pub struct RingBuffer {
    max_duration_hns: i64,
    video_chunks: VecDeque<VideoChunk>,
    audio_chunks: VecDeque<AudioChunk>,
    total_bytes: usize,
    width: u32,
    height: u32,
    fps: u32,
    audio_sample_rate: u32,
    audio_channels: u32,
    has_seen_first_keyframe: bool,
}

impl RingBuffer {
    /// Creates a new RingBuffer with target maximum duration in HNS.
    pub fn new(
        max_duration_hns: i64,
        width: u32,
        height: u32,
        fps: u32,
        audio_sample_rate: u32,
        audio_channels: u32,
    ) -> Self {
        Self {
            max_duration_hns,
            video_chunks: VecDeque::with_capacity(3600), // ~60s at 60fps
            audio_chunks: VecDeque::with_capacity(3000),
            total_bytes: 0,
            width,
            height,
            fps,
            audio_sample_rate,
            audio_channels,
            has_seen_first_keyframe: false,
        }
    }

    /// Updates video dimensions and frame rate dynamically if display mode changes.
    pub fn update_video_format(&mut self, width: u32, height: u32, fps: u32) {
        self.width = width;
        self.height = height;
        self.fps = fps;
    }

    /// Appends an encoded video chunk to the buffer.
    ///
    /// Non-keyframes received before the first keyframe are discarded.
    /// When total buffer duration exceeds `max_duration_hns`, older frames
    /// are pruned strictly up to the next IDR keyframe.
    pub fn push_video(&mut self, chunk: VideoChunk) {
        // Enforce invariant: First frame MUST be an IDR keyframe
        if !self.has_seen_first_keyframe {
            if !chunk.is_keyframe {
                debug!("Dropping non-keyframe before initial IDR keyframe");
                return;
            }
            self.has_seen_first_keyframe = true;
        }

        self.total_bytes += chunk.byte_size();
        self.video_chunks.push_back(chunk);

        self.prune();
    }

    /// Appends an encoded audio chunk to the buffer.
    pub fn push_audio(&mut self, chunk: AudioChunk) {
        if !self.has_seen_first_keyframe {
            // Drop audio prior to video start
            return;
        }

        self.total_bytes += chunk.byte_size();
        self.audio_chunks.push_back(chunk);

        // Prune stale audio if any exists prior to oldest video PTS
        if let Some(first_video) = self.video_chunks.front() {
            let threshold_pts = first_video.pts_hns;
            while let Some(audio) = self.audio_chunks.front() {
                if audio.pts_hns + audio.duration_hns < threshold_pts {
                    let popped = self.audio_chunks.pop_front().unwrap();
                    self.total_bytes = self.total_bytes.saturating_sub(popped.byte_size());
                } else {
                    break;
                }
            }
        }
    }

    /// Prunes old frames preserving IDR-alignment.
    fn prune(&mut self) {
        if self.video_chunks.len() < 2 {
            return;
        }

        let latest_pts = match self.video_chunks.back() {
            Some(v) => v.pts_hns + v.duration_hns,
            None => return,
        };

        let first_pts = match self.video_chunks.front() {
            Some(v) => v.pts_hns,
            None => return,
        };

        let current_duration = latest_pts.saturating_sub(first_pts);
        if current_duration <= self.max_duration_hns {
            return;
        }

        // Search for the furthest keyframe from the start that still retains at least max_duration_hns
        let mut best_prune_idx = None;
        for (idx, chunk) in self.video_chunks.iter().enumerate().skip(1) {
            if chunk.is_keyframe {
                let remaining_duration = latest_pts.saturating_sub(chunk.pts_hns);
                if remaining_duration >= self.max_duration_hns {
                    best_prune_idx = Some(idx);
                } else {
                    // Beyond this keyframe, remaining duration would drop below max_duration_hns
                    break;
                }
            }
        }

        if let Some(prune_count) = best_prune_idx {
            for _ in 0..prune_count {
                if let Some(popped) = self.video_chunks.pop_front() {
                    self.total_bytes = self.total_bytes.saturating_sub(popped.byte_size());
                }
            }

            // Align audio with new video start
            if let Some(new_first_video) = self.video_chunks.front() {
                let threshold_pts = new_first_video.pts_hns;
                while let Some(audio) = self.audio_chunks.front() {
                    if audio.pts_hns + audio.duration_hns < threshold_pts {
                        let popped = self.audio_chunks.pop_front().unwrap();
                        self.total_bytes = self.total_bytes.saturating_sub(popped.byte_size());
                    } else {
                        break;
                    }
                }
            }
        }
    }

    /// Takes an atomic snapshot of the buffer for disk muxing.
    pub fn snapshot(&self) -> Option<RingBufferSnapshot> {
        if self.video_chunks.is_empty() {
            return None;
        }

        // Verify IDR alignment invariant
        assert!(
            self.video_chunks.front().unwrap().is_keyframe,
            "Ring buffer front chunk must be an IDR keyframe"
        );

        Some(RingBufferSnapshot {
            video_chunks: self.video_chunks.iter().cloned().collect(),
            audio_chunks: self.audio_chunks.iter().cloned().collect(),
            width: self.width,
            height: self.height,
            fps: self.fps,
            audio_sample_rate: self.audio_sample_rate,
            audio_channels: self.audio_channels,
        })
    }

    /// Current memory usage in bytes.
    #[inline]
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Number of video chunks currently in buffer.
    #[inline]
    pub fn video_count(&self) -> usize {
        self.video_chunks.len()
    }

    /// Number of audio chunks currently in buffer.
    #[inline]
    pub fn audio_count(&self) -> usize {
        self.audio_chunks.len()
    }

    /// Clears the ring buffer.
    pub fn clear(&mut self) {
        self.video_chunks.clear();
        self.audio_chunks.clear();
        self.total_bytes = 0;
        self.has_seen_first_keyframe = false;
    }
}

/// Thread-safe reference-counted RingBuffer wrapper.
pub type SharedRingBuffer = Arc<Mutex<RingBuffer>>;

pub fn create_shared_ring_buffer(
    max_duration_hns: i64,
    width: u32,
    height: u32,
    fps: u32,
    audio_sample_rate: u32,
    audio_channels: u32,
) -> SharedRingBuffer {
    Arc::new(Mutex::new(RingBuffer::new(
        max_duration_hns,
        width,
        height,
        fps,
        audio_sample_rate,
        audio_channels,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_buffer_discards_pre_keyframe() {
        let mut rb = RingBuffer::new(10_000_000, 1920, 1080, 60, 48000, 2);

        // Send non-keyframe first
        rb.push_video(VideoChunk {
            is_keyframe: false,
            pts_hns: 100,
            dts_hns: 100,
            duration_hns: 166_666,
            data: vec![1, 2, 3],
        });

        assert_eq!(rb.video_count(), 0);

        // Send keyframe
        rb.push_video(VideoChunk {
            is_keyframe: true,
            pts_hns: 200,
            dts_hns: 200,
            duration_hns: 166_666,
            data: vec![4, 5, 6],
        });

        assert_eq!(rb.video_count(), 1);
        assert!(rb.snapshot().unwrap().video_chunks[0].is_keyframe);
    }

    #[test]
    fn test_ring_buffer_idr_pruning() {
        // 2 seconds max duration (20_000_000 HNS)
        let mut rb = RingBuffer::new(20_000_000, 1920, 1080, 60, 48000, 2);

        // Keyframe 1 at 0s
        rb.push_video(VideoChunk {
            is_keyframe: true,
            pts_hns: 0,
            dts_hns: 0,
            duration_hns: 10_000_000,
            data: vec![0; 100],
        });

        // Keyframe 2 at 1s (10_000_000 HNS)
        rb.push_video(VideoChunk {
            is_keyframe: true,
            pts_hns: 10_000_000,
            dts_hns: 10_000_000,
            duration_hns: 10_000_000,
            data: vec![0; 100],
        });

        // Keyframe 3 at 2s (20_000_000 HNS)
        rb.push_video(VideoChunk {
            is_keyframe: true,
            pts_hns: 20_000_000,
            dts_hns: 20_000_000,
            duration_hns: 10_000_000,
            data: vec![0; 100],
        });

        // Keyframe 4 at 3s (30_000_000 HNS)
        // Duration from Keyframe 2 (1s) to Keyframe 4 end (4s) is 30_000_000 HNS >= 20_000_000 HNS.
        // So Keyframe 1 should be pruned!
        rb.push_video(VideoChunk {
            is_keyframe: true,
            pts_hns: 30_000_000,
            dts_hns: 30_000_000,
            duration_hns: 10_000_000,
            data: vec![0; 100],
        });

        let snap = rb.snapshot().expect("snapshot should exist");
        assert!(snap.video_chunks.first().unwrap().is_keyframe);
        // First chunk in buffer should now be Keyframe 3 at 2s (20_000_000)
        // since keeping from 20_000_000 to 40_000_000 is exactly 20_000_000 HNS (2 seconds).
        assert_eq!(snap.video_chunks.first().unwrap().pts_hns, 20_000_000);
    }
}
