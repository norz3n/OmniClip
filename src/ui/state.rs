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

/// Global UI and interactive runtime state for OmniClip.
#[derive(Debug, Clone)]
pub struct UiState {
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

    /// Whether microphone capture is enabled.
    pub mic_enabled: bool,
    /// Microphone volume level (0.0 to 1.0).
    pub mic_volume: f32,

    /// Directory where saved videos are stored.
    pub output_dir: PathBuf,

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
    ) -> Self {
        let bitrate_mbps = video_bitrate_bps / 1_000_000;
        let bytes_per_sec = (bitrate_mbps as u64 * 1_000_000) / 8;
        let estimated_ram_mb = (bytes_per_sec * buffer_duration_secs as u64) / (1024 * 1024);

        Self {
            replay_buffer_active: true,
            buffer_duration_secs,
            estimated_ram_mb: estimated_ram_mb as usize,
            current_ram_bytes: 0,
            manual_record_active: false,
            manual_record_start: None,
            target_fps,
            video_bitrate_mbps: bitrate_mbps,
            mic_enabled,
            mic_volume,
            output_dir,
            hud_visible: true,
            hub_visible: false,
            is_saving_disk: false,
            active_toast: None,
            toast_id_counter: 0,
        }
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

    /// Triggers display of a new toast notification.
    pub fn push_toast(&mut self, title: impl Into<String>, filepath: PathBuf, duration_secs: u32, size_mb: f64) {
        self.toast_id_counter += 1;
        self.active_toast = Some(ToastNotification::new(
            self.toast_id_counter,
            title,
            filepath,
            duration_secs,
            size_mb,
        ));
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
) -> SharedUiState {
    Arc::new(RwLock::new(UiState::new(
        buffer_duration_secs,
        target_fps,
        video_bitrate_bps,
        mic_enabled,
        mic_volume,
        output_dir,
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
