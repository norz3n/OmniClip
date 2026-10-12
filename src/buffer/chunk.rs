/// A single encoded H.264 video packet.
#[derive(Debug, Clone)]
pub struct VideoChunk {
    /// True if this is an IDR keyframe (contains SPS/PPS).
    pub is_keyframe: bool,
    /// Presentation timestamp in Media Foundation HNS (100 ns).
    pub pts_hns: i64,
    /// Decoding timestamp in Media Foundation HNS (100 ns).
    pub dts_hns: i64,
    /// Sample duration in HNS.
    pub duration_hns: i64,
    /// Compressed NAL unit byte payload.
    pub data: Vec<u8>,
}

impl VideoChunk {
    /// Returns the approximate memory footprint of this chunk.
    #[inline]
    pub fn byte_size(&self) -> usize {
        std::mem::size_of::<Self>() + self.data.len()
    }
}

/// A single encoded AAC audio packet.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// Track index (0 = System / Game Audio, 1 = Microphone).
    pub track_index: u8,
    /// Presentation timestamp in Media Foundation HNS (100 ns).
    pub pts_hns: i64,
    /// Duration of the audio frame in HNS.
    pub duration_hns: i64,
    /// Compressed AAC byte payload.
    pub data: Vec<u8>,
}

impl AudioChunk {
    /// Returns the approximate memory footprint of this chunk.
    #[inline]
    pub fn byte_size(&self) -> usize {
        std::mem::size_of::<Self>() + self.data.len()
    }
}

/// An immutable point-in-time snapshot of the circular buffer ready for disk dumping.
#[derive(Debug, Clone)]
pub struct RingBufferSnapshot {
    /// Ordered video chunks starting strictly with an IDR keyframe.
    pub video_chunks: Vec<VideoChunk>,
    /// Synchronized audio chunks.
    pub audio_chunks: Vec<AudioChunk>,
    /// Encoded video frame width in pixels.
    pub width: u32,
    /// Encoded video frame height in pixels.
    pub height: u32,
    /// Video frame rate.
    pub fps: u32,
    /// Audio sample rate in Hz.
    pub audio_sample_rate: u32,
    /// Audio channel count.
    pub audio_channels: u32,
}

impl RingBufferSnapshot {
    /// Returns total duration of the captured video in HNS.
    pub fn duration_hns(&self) -> i64 {
        if self.video_chunks.is_empty() {
            return 0;
        }
        let first_pts = self.video_chunks.first().unwrap().pts_hns;
        let last = self.video_chunks.last().unwrap();
        (last.pts_hns + last.duration_hns).saturating_sub(first_pts)
    }

    /// Total payload size in bytes.
    pub fn total_payload_bytes(&self) -> usize {
        let v: usize = self.video_chunks.iter().map(|c| c.data.len()).sum();
        let a: usize = self.audio_chunks.iter().map(|c| c.data.len()).sum();
        v + a
    }
}
