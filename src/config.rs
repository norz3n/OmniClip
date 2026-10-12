use std::path::PathBuf;
use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_ALT, VK_F10};

/// Global configuration for OmniClip.
#[derive(Debug, Clone)]
pub struct Config {
    /// Replay ring buffer duration in seconds (e.g. 60s).
    pub buffer_duration_secs: u32,
    /// Target capture and encoding frame rate (e.g. 60 FPS).
    pub fps: u32,
    /// Video bitrate in bits per second (e.g. 35 Mbps).
    pub video_bitrate: u32,
    /// Keyframe interval (GOP size in frames, e.g. 60 frames = 1 sec).
    pub gop_size: u32,
    /// Audio bitrate in bits per second (e.g. 192 kbps).
    pub audio_bitrate: u32,
    /// Audio sampling rate in Hz (e.g. 48000 Hz).
    pub audio_sample_rate: u32,
    /// Audio channel count (2 for Stereo).
    pub audio_channels: u32,
    /// Whether microphone capture is enabled (defaults to true).
    pub mic_enabled: bool,
    /// Microphone volume multiplier (1.0 = 100%).
    pub mic_volume: f32,
    /// Directory where MP4 replay files are saved.
    pub output_dir: PathBuf,
    /// Hotkey modifier keys (e.g. Alt).
    pub hotkey_modifiers: HOT_KEY_MODIFIERS,
    /// Hotkey virtual key code (e.g. F10 = 0x79).
    pub hotkey_vk: u32,
}

impl Default for Config {
    fn default() -> Self {
        let output_dir = match std::env::var("USERPROFILE") {
            Ok(user_dir) => PathBuf::from(user_dir).join("Videos").join("OmniClip"),
            Err(_) => PathBuf::from("replays"),
        };

        Self {
            buffer_duration_secs: 60,
            fps: 60,
            video_bitrate: 35_000_000,
            gop_size: 60,
            audio_bitrate: 192_000,
            audio_sample_rate: 48_000,
            audio_channels: 2,
            mic_enabled: true,
            mic_volume: 1.0,
            output_dir,
            hotkey_modifiers: MOD_ALT,
            hotkey_vk: VK_F10.0 as u32,
        }
    }
}

impl Config {
    /// Returns replay buffer duration in Media Foundation Hundred-Nanosecond (HNS) units.
    #[inline]
    pub fn buffer_duration_hns(&self) -> i64 {
        self.buffer_duration_secs as i64 * 10_000_000i64
    }

    /// Returns the ideal frame duration in HNS for the configured target FPS.
    #[inline]
    pub fn frame_duration_hns(&self) -> i64 {
        if self.fps == 0 {
            166_666 // default ~60fps
        } else {
            10_000_000i64 / self.fps as i64
        }
    }
}
