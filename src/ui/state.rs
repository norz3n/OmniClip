use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::RwLock;

/// Toast notification message displayed when a replay or recording is saved.
#[derive(Debug, Clone)]
pub struct ToastNotification {
    /// Unique identifier for this notification.
    pub id: u64,
    /// Header title (e.g. "Повтор сохранен").
    pub title: String,
    /// Target file name.
    pub filename: String,
    /// Absolute target file path for Explorer reveal.
    pub filepath: PathBuf,
    /// Recorded clip duration in seconds.
    pub duration_secs: u32,
    /// File size in megabytes.
    pub size_mb: f64,
    /// Expiration timestamp (auto-dismiss after 4 seconds).
    pub expires_at: Instant,
}

impl ToastNotification {
    /// Creates a new toast notification with standard 4-second timeout.
    pub fn new(
        id: u64,
        title: impl Into<String>,
        filepath: PathBuf,
        duration_secs: u32,
        size_mb: f64,
    ) -> Self {
        let filename = filepath
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "replay.mp4".to_string());

        Self {
            id,
            title: title.into(),
            filename,
            filepath,
            duration_secs,
            size_mb,
            expires_at: Instant::now() + Duration::from_secs(4),
        }
    }

    /// Checks if the notification has timed out.
    #[inline]
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

/// Active navigation tab in the Material Design 3 Control Hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubTab {
    /// Dashboard / Quick Action Hub (Replay + Record).
    Dashboard,
    /// Video Capture settings (FPS, Bitrate, Resolution).
    Video,
    /// Ring Buffer & RAM allocation settings.
    Buffer,
    /// Audio input & playback levels.
    Audio,
    /// Global hotkey shortcuts overview.
    Hotkeys,
    /// Storage folder & recent replays browser.
    Storage,
}

/// Saved replay entry for session history.
#[derive(Debug, Clone)]
pub struct SavedReplayEntry {
    pub filepath: PathBuf,
    pub filename: String,
    pub duration_secs: u32,
    pub size_mb: f64,
}

/// Active audio application detected by real-time WASAPI monitor.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioAppEntry {
    /// Process executable name (e.g. "Discord.exe").
    pub name: String,
    /// Absolute filesystem path to the executable file.
    pub exe_path: String,
    /// Operating system process ID.
    pub pid: u32,
    /// Real-time peak volume level (0.0 ..= 1.0).
    pub peak: f32,
    /// Whether actively emitting audio.
    pub is_playing: bool,
    /// Timestamp when audio output was last detected.
    pub last_active: Instant,
}

/// Global UI and interactive runtime state for OmniClip.
#[derive(Debug, Clone)]
pub struct UiState {
    /// Currently selected navigation tab.
    pub active_tab: HubTab,
    /// Whether circular instant replay buffering is active.
    pub replay_buffer_active: bool,
    /// Configured circular replay buffer length in seconds (e.g. 15, 30, 60, 120, 300).
    pub buffer_duration_secs: u32,
    /// Estimated RAM usage in megabytes based on configured bitrate and duration.
    pub estimated_ram_mb: usize,
    /// Current live RAM usage in bytes from the actual ring buffer.
    pub current_ram_bytes: usize,

    /// Whether manual recording mode is running.
    pub manual_record_active: bool,
    /// Timestamp when manual recording began.
    pub manual_record_start: Option<Instant>,

    /// Target capture and encoding frame rate (e.g. 60 or 120 FPS).
    pub target_fps: u32,
    /// Target video bitrate in Mbps (e.g. 15, 35, 50, 80).
    pub video_bitrate_mbps: u32,
    /// Video capture resolution preset description.
    pub resolution_label: String,
    /// Video encoder rate control mode (CBR, VBR, CQP).
    pub video_rate_control: String,
    /// Video peak / maximum bitrate in Mbps (for VBR).
    pub video_peak_bitrate_mbps: u32,
    /// Hardware encoder latency/quality preset.
    pub video_encoder_preset: String,
    /// H.264 encoding profile (High, Main, Baseline).
    pub video_profile: String,
    /// Keyframe interval (GOP) in seconds.
    pub video_gop_secs: u32,
    /// Number of consecutive B-frames (0 for lowest latency).
    pub video_b_frames: u32,
    /// Texture downscaling filter (Bilinear, Bicubic, Lanczos).
    pub video_downscale_filter: String,
    /// Video color range (Full 0-255 vs Limited 16-235).
    pub video_color_range: String,

    /// Whether microphone capture is enabled.
    pub mic_enabled: bool,
    /// Microphone volume level (0.0 to 2.0).
    pub mic_volume: f32,
    /// Whether Push-to-Talk activation mode is enabled for microphone.
    pub mic_mode_ptt: bool,
    /// Push-to-Talk keyboard trigger key name.
    pub mic_ptt_key: String,
    /// Microphone noise gate threshold in decibels (-60 dB to 0 dB).
    pub mic_noise_gate_db: f32,
    /// Live microphone volume peak (0.0 to 1.0) for the UI VU meter.
    pub mic_live_peak: f32,
    /// Live microphone status message (e.g. "OK", "Disconnected", "Access Denied").
    pub mic_status_msg: String,

    /// Master game/desktop audio loopback enabled.
    pub game_audio_enabled: bool,
    /// Master game/desktop audio volume (0.0 to 1.0).
    pub game_audio_volume: f32,
    /// Audio encoding bitrate in kbps (e.g. 128, 192, 256, 320).
    pub audio_bitrate_kbps: u32,
    /// Audio sampling rate in Hz (44100 or 48000).
    pub audio_sample_rate_hz: u32,
    /// Audio channels configuration ("Стерео (2.0)" or "Моно (1.0)").
    pub audio_channels: String,
    /// Whether to record game and microphone into separate audio tracks.
    pub audio_multi_track: bool,
    /// Audio synchronization offset in milliseconds.
    pub audio_sync_offset_ms: i32,
    /// Selected audio playback endpoint ID ("default" for system default).
    pub selected_playback_device: String,
    /// Selected audio recording endpoint ID ("default" for system default).
    pub selected_capture_device: String,
    /// Enumerated audio playback devices: (device_id, friendly_name).
    pub available_playback_devices: Vec<(String, String)>,
    /// Enumerated audio capture/microphone devices: (device_id, friendly_name).
    pub available_capture_devices: Vec<(String, String)>,

    /// Application audio capture mode: "all" (All system), "include" (Selected only), "exclude" (Exclude selected).
    pub audio_app_filter_mode: String,
    /// List of detected active audio applications currently emitting sound.
    pub detected_audio_apps: Vec<AudioAppEntry>,
    /// Set of application executable names selected for include/exclude filter.
    pub selected_audio_apps: Vec<String>,

    /// Hard RAM memory limit safety cap for circular buffer in Gigabytes.
    pub buffer_ram_limit_gb: u32,
    /// Whether circular replay buffer starts automatically on app launch.
    pub buffer_autostart: bool,

    /// File naming template pattern (e.g. "Replay_%Y-%m-%d_%H-%M-%S").
    pub filename_pattern: String,
    /// In-game HUD corner indicator position ("Верхний правый", "Нижний правый", etc.).
    pub hud_position: String,
    /// Whether to display live FPS in in-game HUD.
    pub hud_show_fps: bool,
    /// Whether to play subtle audio notification when replay is saved.
    pub sound_notify_on_save: bool,

    /// Directory where saved videos are stored.
    pub output_dir: PathBuf,
    /// Session list of recent saved replay captures.
    pub recent_replays: Vec<SavedReplayEntry>,
    /// Session list of recent saved screenshots.
    pub recent_screenshots: Vec<SavedReplayEntry>,

    /// In-game HUD corner indicator visibility.
    pub hud_visible: bool,
    /// Main Alt+Z overlay hub visibility.
    pub hub_visible: bool,
    /// Indicator showing whether a replay write operation is currently in progress.
    pub is_saving_disk: bool,

    /// Active toast notification popup, if any.
    pub active_toast: Option<ToastNotification>,
    /// Incremental ID counter for toasts.
    toast_id_counter: u64,
    /// Active UI language (Russian or English).
    pub language: crate::ui::i18n::Language,
    /// Hardware video compression codec (H.264 or HEVC).
    pub video_codec: crate::buffer::chunk::VideoCodec,
    /// Currently selected display monitor index.
    pub selected_display_index: u32,
    /// Available displays enumerated by DXGI.
    pub available_displays: Vec<crate::capture::DisplayInfo>,
    /// Hotkey virtual key code for replay clip (e.g. F10 = 0x79).
    pub hotkey_replay_vk: u32,
    /// Hotkey virtual key code for overlay toggle (e.g. Z = 0x5A).
    pub hotkey_overlay_vk: u32,
    /// Hotkey virtual key code for manual record toggle (e.g. F9 = 0x78).
    pub hotkey_record_vk: u32,
    /// Hotkey virtual key code for screenshot capture (e.g. F1 = 0x70).
    pub hotkey_screenshot_vk: u32,
    /// Whether mouse cursor capture is enabled.
    pub capture_cursor: bool,
}

impl UiState {
    /// Creates a new default UI state instance.
    pub fn new(
        buffer_duration_secs: u32,
        target_fps: u32,
        video_bitrate_bps: u32,
        mic_enabled: bool,
        mic_volume: f32,
        output_dir: PathBuf,
        language: crate::ui::i18n::Language,
    ) -> Self {
        let bitrate_mbps = video_bitrate_bps / 1_000_000;
        let bytes_per_sec = (bitrate_mbps as u64 * 1_000_000) / 8;
        let estimated_ram_mb = (bytes_per_sec * buffer_duration_secs as u64) / (1024 * 1024);

        Self {
            active_tab: HubTab::Dashboard,
            replay_buffer_active: true,
            buffer_duration_secs,
            estimated_ram_mb: estimated_ram_mb as usize,
            current_ram_bytes: 0,
            manual_record_active: false,
            manual_record_start: None,
            target_fps,
            video_bitrate_mbps: bitrate_mbps,
            resolution_label: "Исходное (Native)".to_string(),
            video_rate_control: "CBR (Постоянный)".to_string(),
            video_peak_bitrate_mbps: (bitrate_mbps * 3) / 2,
            video_encoder_preset: "Ultra Low Latency (Минимум задержки)".to_string(),
            video_profile: "High (Рекомендуется)".to_string(),
            video_gop_secs: 1,
            video_b_frames: 0,
            video_downscale_filter: "Билинейный (Быстрый)".to_string(),
            video_color_range: "Полный (Full 0-255)".to_string(),
            mic_enabled,
            mic_volume,
            mic_mode_ptt: false,
            mic_ptt_key: "V".to_string(),
            mic_noise_gate_db: -45.0,
            mic_live_peak: 0.0,
            mic_status_msg: "OK".to_string(),
            game_audio_enabled: true,
            game_audio_volume: 1.0,
            audio_bitrate_kbps: 192,
            audio_sample_rate_hz: 48000,
            audio_channels: "Стерео (2.0)".to_string(),
            audio_multi_track: false,
            audio_sync_offset_ms: 0,
            selected_playback_device: "default".to_string(),
            selected_capture_device: "default".to_string(),
            available_playback_devices: Vec::new(),
            available_capture_devices: Vec::new(),
            audio_app_filter_mode: "all".to_string(),
            detected_audio_apps: Vec::new(),
            selected_audio_apps: Vec::new(),
            buffer_ram_limit_gb: 4,
            buffer_autostart: true,
            filename_pattern: "Replay_%Y-%m-%d_%H-%M-%S".to_string(),
            hud_position: "Верхний правый".to_string(),
            hud_show_fps: true,
            sound_notify_on_save: true,
            output_dir,
            recent_replays: Vec::new(),
            recent_screenshots: Vec::new(),
            hud_visible: true,
            hub_visible: false,
            is_saving_disk: false,
            active_toast: None,
            toast_id_counter: 0,
            language,
            video_codec: crate::buffer::chunk::VideoCodec::H264,
            selected_display_index: 0,
            available_displays: Vec::new(),
            hotkey_replay_vk: windows::Win32::UI::Input::KeyboardAndMouse::VK_F10.0 as u32,
            hotkey_overlay_vk: 0x5A,
            hotkey_record_vk: windows::Win32::UI::Input::KeyboardAndMouse::VK_F9.0 as u32,
            hotkey_screenshot_vk: windows::Win32::UI::Input::KeyboardAndMouse::VK_F1.0 as u32,
            capture_cursor: false,
        }
    }

    /// Sets the active UI language.
    pub fn set_language(&mut self, language: crate::ui::i18n::Language) {
        self.language = language;
    }

    /// Updates dynamic RAM consumption from the live ring buffer.
    pub fn set_current_ram_bytes(&mut self, bytes: usize) {
        self.current_ram_bytes = bytes;
    }

    /// Sets disk saving indicator state.
    pub fn set_saving_state(&mut self, saving: bool) {
        self.is_saving_disk = saving;
    }

    /// Sets circular buffer duration and recalculates RAM requirement estimation.
    pub fn set_buffer_duration(&mut self, secs: u32) {
        self.buffer_duration_secs = secs;
        let bytes_per_sec = (self.video_bitrate_mbps as usize * 1_000_000) / 8;
        self.estimated_ram_mb = (bytes_per_sec * secs as usize) / (1024 * 1024);
    }

    /// Triggers display of a new toast notification and records entry in recent history.
    pub fn push_toast(&mut self, title: impl Into<String>, filepath: PathBuf, duration_secs: u32, size_mb: f64) {
        self.toast_id_counter += 1;
        let toast = ToastNotification::new(
            self.toast_id_counter,
            title,
            filepath.clone(),
            duration_secs,
            size_mb,
        );
        if duration_secs == 0 {
            self.recent_screenshots.insert(
                0,
                SavedReplayEntry {
                    filepath,
                    filename: toast.filename.clone(),
                    duration_secs: 0,
                    size_mb,
                },
            );
            if self.recent_screenshots.len() > 10 {
                self.recent_screenshots.truncate(10);
            }
        } else {
            self.recent_replays.insert(
                0,
                SavedReplayEntry {
                    filepath,
                    filename: toast.filename.clone(),
                    duration_secs,
                    size_mb,
                },
            );
            if self.recent_replays.len() > 10 {
                self.recent_replays.truncate(10);
            }
        }
        self.active_toast = Some(toast);
    }

    /// Clears expired toast notification.
    pub fn dismiss_expired_toast(&mut self) -> bool {
        if let Some(toast) = &self.active_toast {
            if toast.is_expired() {
                self.active_toast = None;
                return true;
            }
        }
        false
    }
}

/// Shared thread-safe handle to the UI state.
pub type SharedUiState = Arc<RwLock<UiState>>;

/// Creates a new shared thread-safe UI state container.
pub fn create_shared_ui_state(
    buffer_duration_secs: u32,
    target_fps: u32,
    video_bitrate_bps: u32,
    mic_enabled: bool,
    mic_volume: f32,
    output_dir: PathBuf,
    language: crate::ui::i18n::Language,
) -> SharedUiState {
    Arc::new(RwLock::new(UiState::new(
        buffer_duration_secs,
        target_fps,
        video_bitrate_bps,
        mic_enabled,
        mic_volume,
        output_dir,
        language,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ui_state_initialization_and_ram_estimate() {
        let state = UiState::new(
            60,
            60,
            35_000_000,
            true,
            1.0,
            PathBuf::from("replays"),
            crate::ui::i18n::Language::Ru,
        );

        assert!(state.replay_buffer_active);
        assert_eq!(state.buffer_duration_secs, 60);
        assert_eq!(state.video_bitrate_mbps, 35);
        assert!(state.estimated_ram_mb > 0);
        assert!(!state.manual_record_active);
        assert!(state.active_toast.is_none());
    }

    #[test]
    fn test_ui_state_buffer_duration_recalc() {
        let mut state = UiState::new(
            60,
            60,
            40_000_000,
            true,
            1.0,
            PathBuf::from("replays"),
            crate::ui::i18n::Language::Ru,
        );

        let initial_ram = state.estimated_ram_mb;
        state.set_buffer_duration(120);
        assert_eq!(state.buffer_duration_secs, 120);
        // Double duration should double estimated RAM
        assert_eq!(state.estimated_ram_mb, initial_ram * 2);
    }

    #[test]
    fn test_toast_notification_lifecycle() {
        let mut state = UiState::new(
            60,
            60,
            35_000_000,
            true,
            1.0,
            PathBuf::from("replays"),
            crate::ui::i18n::Language::Ru,
        );

        assert!(state.active_toast.is_none());
        state.push_toast(
            "Повтор сохранен",
            PathBuf::from("replays/Replay_123.mp4"),
            60,
            34.5,
        );

        assert!(state.active_toast.is_some());
        let toast = state.active_toast.as_ref().unwrap();
        assert_eq!(toast.title, "Повтор сохранен");
        assert_eq!(toast.filename, "Replay_123.mp4");
        assert_eq!(toast.duration_secs, 60);
        assert!(!toast.is_expired());

        // Dismissing non-expired toast should return false
        assert!(!state.dismiss_expired_toast());
        assert!(state.active_toast.is_some());
    }
}
