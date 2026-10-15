use std::path::PathBuf;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_ALT, VK_F1, VK_F9, VK_F10};
use crate::buffer::chunk::VideoCodec;

fn default_modifiers() -> HOT_KEY_MODIFIERS {
    MOD_ALT
}

fn default_hotkey_vk() -> u32 {
    VK_F10.0 as u32
}

fn default_hotkey_overlay_vk() -> u32 {
    0x5A // VK 'Z'
}

fn default_hotkey_record_vk() -> u32 {
    VK_F9.0 as u32
}

fn default_hotkey_screenshot_vk() -> u32 {
    VK_F1.0 as u32
}

fn default_video_codec() -> VideoCodec {
    VideoCodec::H264
}

fn default_filename_pattern() -> String {
    "Replay_%Y-%m-%d_%H-%M-%S".to_string()
}

fn default_display_index() -> u32 {
    0
}

fn default_language() -> String {
    "auto".to_string()
}

#[link(name = "kernel32")]
extern "system" {
    fn GetUserDefaultUILanguage() -> u16;
}

/// Detects the default UI language of the Windows operating system.
///
/// Returns `"ru"` for Russian, Ukrainian, Belarusian, or Kazakh system locales;
/// returns `"en"` for all other languages.
pub fn detect_system_language() -> &'static str {
    let langid = unsafe { GetUserDefaultUILanguage() };
    let primary_lang = langid & 0x03FF;
    match primary_lang {
        0x19 | 0x22 | 0x23 | 0x3F => "ru",
        _ => "en",
    }
}

/// Global configuration for OmniClip.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Filename pattern for saved clips.
    #[serde(default = "default_filename_pattern")]
    pub filename_pattern: String,
    /// Whether to record desktop and mic into separate audio tracks.
    #[serde(default)]
    pub audio_multi_track: bool,
    /// Display index to capture (0 = primary monitor).
    #[serde(default = "default_display_index")]
    pub display_index: u32,
    /// Active UI language ("auto" | "ru" | "en").
    #[serde(default = "default_language")]
    pub language: String,
    /// Video compression codec (H264 or Hevc).
    #[serde(default = "default_video_codec")]
    pub video_codec: VideoCodec,
    /// Hotkey modifier keys (e.g. Alt).
    #[serde(skip, default = "default_modifiers")]
    pub hotkey_modifiers: HOT_KEY_MODIFIERS,
    /// Hotkey virtual key code for replay clip (e.g. F10 = 0x79).
    #[serde(default = "default_hotkey_vk")]
    pub hotkey_vk: u32,
    /// Hotkey virtual key code for overlay toggle (e.g. Z = 0x5A).
    #[serde(default = "default_hotkey_overlay_vk")]
    pub hotkey_overlay_vk: u32,
    /// Hotkey virtual key code for manual record toggle (e.g. F9 = 0x78).
    #[serde(default = "default_hotkey_record_vk")]
    pub hotkey_record_vk: u32,
    /// Hotkey virtual key code for screenshot capture (e.g. F1 = 0x70).
    #[serde(default = "default_hotkey_screenshot_vk")]
    pub hotkey_screenshot_vk: u32,
    /// Whether to render mouse cursor on captured frames (defaults to false for 0% GPU overhead).
    #[serde(default)]
    pub capture_cursor: bool,
    /// Audio synchronization offset in milliseconds (default: 0, negative values shift audio earlier).
    #[serde(default)]
    pub audio_sync_offset_ms: i32,
}

impl Config {
    /// Resolves the concrete language code ("ru" or "en") considering the "auto" setting.
    pub fn resolved_language(&self) -> &'static str {
        match self.language.trim().to_ascii_lowercase().as_str() {
            "ru" => "ru",
            "en" => "en",
            _ => detect_system_language(),
        }
    }
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
            video_bitrate: 20_000_000,
            gop_size: 60,
            audio_bitrate: 192_000,
            audio_sample_rate: 48_000,
            audio_channels: 2,
            mic_enabled: true,
            mic_volume: 1.0,
            output_dir,
            filename_pattern: default_filename_pattern(),
            audio_multi_track: false,
            display_index: 0,
            language: default_language(),
            video_codec: VideoCodec::H264,
            hotkey_modifiers: MOD_ALT,
            hotkey_vk: VK_F10.0 as u32,
            hotkey_overlay_vk: 0x5A,
            hotkey_record_vk: VK_F9.0 as u32,
            hotkey_screenshot_vk: VK_F1.0 as u32,
            capture_cursor: false,
            audio_sync_offset_ms: 0,
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

    /// Returns the standard configuration file path on Windows (%APPDATA%/OmniClip/config.toml).
    pub fn config_path() -> PathBuf {
        if let Ok(app_data) = std::env::var("APPDATA") {
            PathBuf::from(app_data).join("OmniClip").join("config.toml")
        } else {
            PathBuf::from("omniclip.toml")
        }
    }

    /// Loads the configuration from disk, falling back to defaults if missing or invalid.
    pub fn load() -> Self {
        let path = Self::config_path();
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(mut cfg) = toml::from_str::<Config>(&content) {
                    log::info!("Loaded configuration from '{}'", path.display());
                    // Migrate legacy 35 Mbps default to the new 20 Mbps hardware-efficient profile
                    if cfg.video_bitrate == 35_000_000 {
                        cfg.video_bitrate = 20_000_000;
                        let _ = cfg.save();
                    }
                    return cfg;
                }
            }
        }
        let default_cfg = Self::default();
        let _ = default_cfg.save();
        default_cfg
    }

    /// Persists the current configuration to disk.
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("Failed to create config directory: {}", e))?;
        }
        let toml_str = toml::to_string_pretty(self)
            .map_err(|e| anyhow!("Failed to serialize config to TOML: {}", e))?;
        std::fs::write(&path, toml_str)
            .map_err(|e| anyhow!("Failed to write config file to '{}': {}", path.display(), e))?;
        log::debug!("Saved configuration to '{}'", path.display());
        Ok(())
    }
}

