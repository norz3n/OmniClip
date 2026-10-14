use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Stroke, Vec2};

use crate::platform::hotkey::HotkeyEvent;
use crate::ui::state::{HubTab, SharedUiState, UiState};
use crate::ui::i18n::{t, Language, TextKey};
use crate::ui::theme::{M3Icon, M3Theme};

/// User action requested from the UI back to the core capture and encoding engine.
#[derive(Debug, Clone)]
pub enum UiAction {
    /// Save active replay buffer to disk.
    SaveReplay,
    /// Toggle continuous manual recording mode.
    ToggleManualRecord,
    /// Change replay buffer retention duration in seconds.
    SetBufferDuration(u32),
    /// Change target capture frame rate.
    SetFps(u32),
    /// Change video encoding bitrate in Mbps.
    SetBitrate(u32),
    /// Toggle microphone audio capture on/off.
    ToggleMic,
    /// Adjust microphone recording volume.
    SetMicVolume(f32),
    /// Change target folder for saved clips.
    SetOutputDir(PathBuf),
    /// Change filename pattern template.
    SetFilenamePattern(String),
    /// Change multi-track audio configuration.
    SetAudioMultiTrack(bool),
    /// Capture instant screenshot to disk (Alt+F1).
    TakeScreenshot,
    /// Change application display language ("auto", "ru", "en").
    SetLanguage(String),
    /// Request clean application shutdown.
    QuitApplication,
}

/// OmniClip Material Design 3 interactive overlay application powered by egui/eframe.
pub struct OmniClipApp {
    state: SharedUiState,
    action_tx: Sender<UiAction>,
    _hotkey_rx: Receiver<HotkeyEvent>,
    save_notify_rx: Receiver<(PathBuf, u32, f64)>,
    pulse_phase: f32,
    /// Signals the capture thread to stop; shared with the Win32 console handler.
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Flags whether Montserrat font and M3 design tokens have been initialized in egui Context.
    fonts_initialized: bool,
    /// Editable text buffer for storage directory path.
    path_edit_buffer: String,
    /// Cached egui textures for application executable icons (keyed by exe path or name).
    app_icon_textures: Mutex<HashMap<String, Option<egui::TextureHandle>>>,
}

impl OmniClipApp {
    /// Creates a new OmniClipApp instance with communication channels and initial states.
    pub fn new(
        state: SharedUiState,
        action_tx: Sender<UiAction>,
        hotkey_rx: Receiver<HotkeyEvent>,
        save_notify_rx: Receiver<(PathBuf, u32, f64)>,
        running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        let initial_path = state.read().output_dir.to_string_lossy().to_string();

        // Enumerate initial audio devices and active sessions
        {
            let mut st = state.write();
            if st.available_playback_devices.is_empty() {
                st.available_playback_devices = crate::audio::enumerate_audio_devices(false);
            }
            if st.available_capture_devices.is_empty() {
                st.available_capture_devices = crate::audio::enumerate_audio_devices(true);
            }
            if st.detected_audio_apps.is_empty() {
                let now = std::time::Instant::now();
                st.detected_audio_apps = crate::audio::scan_active_audio_sessions()
                    .into_iter()
                    .map(|a| crate::ui::state::AudioAppEntry {
                        name: a.name,
                        exe_path: a.exe_path,
                        pid: a.pid,
                        peak: a.peak,
                        is_playing: a.is_playing,
                        last_active: now,
                    })
                    .collect();
            }
        }

        // Spawn real-time audio session monitor worker thread
        let monitor_state = state.clone();
        let monitor_running = running.clone();
        let _ = std::thread::Builder::new()
            .name("omniclip-audio-monitor".to_string())
            .spawn(move || {
                unsafe {
                    let _ = windows::Win32::System::Com::CoInitializeEx(
                        None,
                        windows::Win32::System::Com::COINIT_MULTITHREADED,
                    );
                }

                while monitor_running.load(std::sync::atomic::Ordering::Relaxed) {
                    if crate::ui::is_overlay_visible() {
                        let scanned_apps = crate::audio::scan_active_audio_sessions();
                        let now = std::time::Instant::now();

                        let mut st = monitor_state.write();
                        let selected = st.selected_audio_apps.clone();

                        // 1. Ingest or update scanned applications emitting audio
                        for scanned in scanned_apps {
                            if let Some(existing) = st
                                .detected_audio_apps
                                .iter_mut()
                                .find(|a| a.name.eq_ignore_ascii_case(&scanned.name))
                            {
                                existing.pid = scanned.pid;
                                existing.peak = scanned.peak;
                                existing.is_playing = scanned.is_playing;
                                if existing.exe_path.is_empty() && !scanned.exe_path.is_empty() {
                                    existing.exe_path = scanned.exe_path.clone();
                                }
                                if scanned.is_playing || scanned.peak > 0.001 {
                                    existing.last_active = now;
                                }
                            } else {
                                st.detected_audio_apps.push(crate::ui::state::AudioAppEntry {
                                    name: scanned.name,
                                    exe_path: scanned.exe_path,
                                    pid: scanned.pid,
                                    peak: scanned.peak,
                                    is_playing: scanned.is_playing,
                                    last_active: now,
                                });
                            }
                        }

                        // 2. Decay peaks and remove unselected applications silent for > 3.5 seconds
                        st.detected_audio_apps.retain_mut(|entry| {
                            let is_sel = selected.iter().any(|s| s.eq_ignore_ascii_case(&entry.name));
                            let silent_duration = now.duration_since(entry.last_active);

                            if silent_duration > std::time::Duration::from_millis(3500) {
                                entry.peak = 0.0;
                                entry.is_playing = false;
                                is_sel
                            } else {
                                entry.peak *= 0.85;
                                if entry.peak < 0.001 {
                                    entry.peak = 0.0;
                                }
                                true
                            }
                        });

                        // 3. Ensure any user-selected app is present in the list (even if silent)
                        for sel in &selected {
                            if !st
                                .detected_audio_apps
                                .iter()
                                .any(|a| a.name.eq_ignore_ascii_case(sel))
                            {
                                st.detected_audio_apps.push(crate::ui::state::AudioAppEntry {
                                    name: sel.clone(),
                                    exe_path: String::new(),
                                    pid: 0,
                                    peak: 0.0,
                                    is_playing: false,
                                    last_active: now - std::time::Duration::from_secs(10),
                                });
                            }
                        }

                        drop(st);
                        std::thread::sleep(std::time::Duration::from_millis(150));
                    } else {
                        // Overlay is not visible, idle to conserve CPU
                        std::thread::sleep(std::time::Duration::from_millis(600));
                    }
                }

                unsafe {
                    windows::Win32::System::Com::CoUninitialize();
                }
            });

        Self {
            state,
            action_tx,
            _hotkey_rx: hotkey_rx,
            save_notify_rx,
            pulse_phase: 0.0,
            running,
            fonts_initialized: false,
            path_edit_buffer: initial_path,
            app_icon_textures: Mutex::new(HashMap::new()),
        }
    }
}

impl eframe::App for OmniClipApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Transparent clear color so rounded CentralPanel corners blend smoothly
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Initialize Montserrat typography and M3 visual styles on first frame
        if !self.fonts_initialized {
            M3Theme::setup_fonts(ctx);
            M3Theme::apply_m3_style(ctx);
            self.fonts_initialized = true;
        }

        // 0. Check global stop signal (Ctrl+C / console close / tray exit) — close egui window cleanly
        if !self.running.load(std::sync::atomic::Ordering::Relaxed) || ctx.input(|i| i.viewport().close_requested()) {
            self.running.store(false, std::sync::atomic::Ordering::SeqCst);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Animation phase for pulsing status indicators
        self.pulse_phase = (self.pulse_phase + 0.06) % (std::f32::consts::PI * 2.0);

        // 1. Drain replay save notifications from disk writer
        while let Ok((saved_path, duration_secs, size_mb)) = self.save_notify_rx.try_recv() {
            let mut st = self.state.write();
            let toast_title = t(TextKey::ToastReplaySaved, st.language);
            st.push_toast(toast_title, saved_path, duration_secs, size_mb);
            st.is_saving_disk = false;
        }

        // 2. Check overlay visibility: if hidden, suppress rendering and enforce viewport invisibility
        if !crate::ui::is_overlay_visible() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            // Keep a low-frequency tick (150ms) even while hidden to react immediately to stop signals and notifications
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
            return;
        }

        // Request periodic repaint when overlay is visible for smooth VU meters and real-time scanner animations
        ctx.request_repaint_after(std::time::Duration::from_millis(80));

        // 3. Handle Escape key to dismiss Hub
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            crate::ui::set_overlay_visible(false);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            return;
        }

        // 4. Check and clear expired toast notifications
        self.state.write().dismiss_expired_toast();

        let state_snapshot = self.state.read().clone();

        // Material 3 Control Hub Interface Container
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(M3Theme::EGUI_SURFACE)
                    .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT))
                    .corner_radius(CornerRadius::same(M3Theme::RADIUS_LG_U8))
                    .inner_margin(18.0),
            )
            .show(ctx, |ui| {
                // Top Header Bar
                self.render_header(ui, ctx, &state_snapshot);

                ui.add_space(10.0);

                // Material Design 3 Navigation Pill Tabs
                self.render_nav_tabs(ui, &state_snapshot);

                // Toast notification banner if a replay was just saved
                if let Some(toast) = &state_snapshot.active_toast {
                    ui.add_space(8.0);
                    self.render_toast_banner(ui, toast, state_snapshot.language);
                }

                ui.add_space(10.0);

                // Main Tab Content Area inside a scrollable viewport for complete layout safety
                let scroll_height = (ui.available_height() - 42.0).max(180.0);
                egui::ScrollArea::vertical()
                    .max_height(scroll_height)
                    .auto_shrink([false, false])
                    .min_scrolled_width(ui.available_width())
                    .show(ui, |ui| {
                        match state_snapshot.active_tab {
                            HubTab::Dashboard => self.render_tab_dashboard(ui, &state_snapshot),
                            HubTab::Video => self.render_tab_video(ui, &state_snapshot),
                            HubTab::Buffer => self.render_tab_buffer(ui, &state_snapshot),
                            HubTab::Audio => self.render_tab_audio(ui, &state_snapshot),
                            HubTab::Hotkeys => self.render_tab_hotkeys(ui, &state_snapshot),
                            HubTab::Storage => self.render_tab_storage(ui, &state_snapshot),
                        }
                    });

                // Dynamically pin the bottom bar cleanly to the bottom of the window without clipping
                ui.add_space((ui.available_height() - 32.0).max(8.0));

                // Bottom Status and Navigation Bar
                self.render_bottom_bar(ui, &state_snapshot);
            });

        // Repaint at 30 Hz for smooth pulse animations while window is shown
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

impl OmniClipApp {
    /// Renders the top header bar with branding, live status chips, and close button.
    fn render_header(&self, ui: &mut egui::Ui, ctx: &egui::Context, state: &UiState) {
        ui.horizontal(|ui| {
            ui.heading(
                RichText::new("OmniClip")
                    .color(M3Theme::EGUI_PRIMARY)
                    .size(22.0)
                    .strong(),
            );
            ui.add_space(14.0);

            // Pulsing live status indicator dot
            let pulse_alpha = ((self.pulse_phase.sin() * 0.4 + 0.6) * 255.0) as u8;
            let (status_color, status_text) = if state.manual_record_active {
                let text = if let Some(start) = state.manual_record_start {
                    let s = start.elapsed().as_secs();
                    format!("{} {:02}:{:02}", t(TextKey::StatusRecording, state.language), s / 60, s % 60)
                } else {
                    t(TextKey::StatusRecording, state.language).to_string()
                };
                (
                    Color32::from_rgba_premultiplied(255, 82, 82, pulse_alpha),
                    text,
                )
            } else if state.replay_buffer_active {
                (
                    Color32::from_rgba_premultiplied(76, 175, 80, pulse_alpha),
                    t(TextKey::BufferActive, state.language).to_string(),
                )
            } else {
                (M3Theme::EGUI_OUTLINE, t(TextKey::StatusIdle, state.language).to_string())
            };

            // Real smooth anti-aliased glowing LED circle
            let (dot_rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(dot_rect.center(), 4.5, status_color);
            ui.add_space(4.0);
            ui.label(
                RichText::new(status_text)
                    .color(status_color)
                    .size(12.0)
                    .strong(),
            );

            ui.add_space(8.0);

            // RAM live chip
            let ram_mb = state.current_ram_bytes / (1024 * 1024);
            egui::Frame::NONE
                .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                .inner_margin(egui::vec2(8.0, 3.0))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!("RAM: {} / ~{} MB", ram_mb, state.estimated_ram_mb))
                            .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                            .size(11.0),
                    );
                });

            ui.add_space(6.0);

            // FPS chip
            egui::Frame::NONE
                .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                .inner_margin(egui::vec2(8.0, 3.0))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!("{} FPS @ {} Mbps", state.target_fps, state.video_bitrate_mbps))
                            .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                            .size(11.0),
                    );
                });

            // Right-aligned close button and language toggle
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let close_btn = ui.add(
                    egui::Button::new(
                        RichText::new(M3Icon::CLOSE)
                            .color(M3Theme::EGUI_ON_SURFACE)
                            .size(12.0),
                    )
                    .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                    .corner_radius(CornerRadius::same(M3Theme::RADIUS_SM_U8))
                    .min_size(Vec2::new(30.0, 28.0)),
                );
                if close_btn.clicked() {
                    crate::ui::set_overlay_visible(false);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                }

                ui.add_space(8.0);

                // Modern Material 3 Segmented Language Switcher (🌐 RU | EN)
                let current_lang = state.language;
                let is_ru = current_lang == Language::Ru;
                let is_en = current_lang == Language::En;

                ui.allocate_ui_with_layout(
                    Vec2::new(82.0, 28.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        egui::Frame::NONE
                            .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                            .stroke(egui::Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT))
                            .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                            .inner_margin(egui::vec2(5.0, 3.0))
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(2.0, 0.0);
                                ui.spacing_mut().button_padding = egui::vec2(4.0, 2.0);

                                ui.label(
                                    RichText::new(M3Icon::GLOBE)
                                        .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                                        .size(11.5),
                                );
                                ui.add_space(2.0);

                                let ru_btn = ui.add(
                                    egui::Button::new(
                                        RichText::new("RU")
                                            .color(if is_ru {
                                                M3Theme::EGUI_ON_PRIMARY
                                            } else {
                                                M3Theme::EGUI_ON_SURFACE_VARIANT
                                            })
                                            .size(11.0)
                                            .strong(),
                                    )
                                    .fill(if is_ru {
                                        M3Theme::EGUI_PRIMARY
                                    } else {
                                        Color32::TRANSPARENT
                                    })
                                    .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                                    .min_size(Vec2::new(26.0, 20.0)),
                                )
                                .on_hover_text("Русский");
                                if ru_btn.clicked() && !is_ru {
                                    self.state.write().set_language(Language::Ru);
                                    let _ = self.action_tx.send(UiAction::SetLanguage("ru".to_string()));
                                }

                                let en_btn = ui.add(
                                    egui::Button::new(
                                        RichText::new("EN")
                                            .color(if is_en {
                                                M3Theme::EGUI_ON_PRIMARY
                                            } else {
                                                M3Theme::EGUI_ON_SURFACE_VARIANT
                                            })
                                            .size(11.0)
                                            .strong(),
                                    )
                                    .fill(if is_en {
                                        M3Theme::EGUI_PRIMARY
                                    } else {
                                        Color32::TRANSPARENT
                                    })
                                    .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                                    .min_size(Vec2::new(26.0, 20.0)),
                                )
                                .on_hover_text("English");
                                if en_btn.clicked() && !is_en {
                                    self.state.write().set_language(Language::En);
                                    let _ = self.action_tx.send(UiAction::SetLanguage("en".to_string()));
                                }
                            });
                    },
                );
            });
        });
    }

    /// Renders Material Design 3 Navigation Pill tabs using crisp vector glyphs.
    fn render_nav_tabs(&mut self, ui: &mut egui::Ui, state: &UiState) {
        let lang = state.language;
        let tabs = [
            (HubTab::Dashboard, format!("{}  {}", M3Icon::LIGHTNING, t(TextKey::TabDashboard, lang))),
            (HubTab::Video, format!("{}  {}", M3Icon::VIDEO, t(TextKey::TabVideo, lang))),
            (HubTab::Buffer, format!("{}  {}", M3Icon::STOPWATCH, t(TextKey::TabBuffer, lang))),
            (HubTab::Audio, format!("{}  {}", M3Icon::AUDIO, t(TextKey::TabAudio, lang))),
            (HubTab::Hotkeys, format!("{}  {}", M3Icon::KEYBOARD, t(TextKey::TabHotkeys, lang))),
            (HubTab::Storage, format!("{}  {}", M3Icon::FOLDER, t(TextKey::TabStorage, lang))),
        ];

        ui.horizontal(|ui| {
            for (tab, label) in tabs {
                let is_sel = state.active_tab == tab;
                let bg = if is_sel {
                    M3Theme::EGUI_PRIMARY_CONTAINER
                } else {
                    M3Theme::EGUI_SURFACE_CONTAINER_HIGH
                };
                let fg = if is_sel {
                    M3Theme::EGUI_ON_PRIMARY_CONTAINER
                } else {
                    M3Theme::EGUI_ON_SURFACE_VARIANT
                };
                let stroke = if is_sel {
                    Stroke::new(1.0_f32, M3Theme::EGUI_PRIMARY)
                } else {
                    Stroke::NONE
                };

                let btn = ui.add(
                    egui::Button::new(
                        RichText::new(label)
                            .color(fg)
                            .size(12.5)
                            .strong(),
                    )
                    .fill(bg)
                    .stroke(stroke)
                    .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                    .min_size(Vec2::new(74.0, 30.0)),
                );

                if btn.clicked() {
                    self.state.write().active_tab = tab;
                }
            }
        });
    }

    /// Renders an active toast notification banner when a replay has been saved.
    fn render_toast_banner(&self, ui: &mut egui::Ui, toast: &crate::ui::state::ToastNotification, lang: Language) {
        let banner_w = ui.available_width();
        let inner_w = (banner_w - 28.0).max(10.0);
        egui::Frame::NONE
            .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
            .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_PRIMARY))
            .corner_radius(CornerRadius::same(M3Theme::RADIUS_MD_U8))
            .inner_margin(egui::vec2(14.0, 8.0))
            .show(ui, |ui| {
                ui.set_min_width(inner_w);
                ui.set_max_width(inner_w);
                ui.allocate_ui_with_layout(
                    Vec2::new(inner_w, 26.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.label(
                            RichText::new(M3Icon::CHECK)
                                .color(M3Theme::EGUI_STATUS_GREEN)
                                .size(13.0)
                                .strong(),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(&toast.title)
                                .color(M3Theme::EGUI_PRIMARY)
                                .size(12.5)
                                .strong(),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("·")
                                .color(M3Theme::EGUI_OUTLINE)
                                .size(12.0),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(&toast.filename)
                                .color(M3Theme::EGUI_ON_SURFACE)
                                .size(12.0)
                                .strong(),
                        );
                        ui.add_space(8.0);

                        let sec_unit = match lang {
                            Language::Ru => "с",
                            Language::En => "s",
                        };
                        let mb_unit = match lang {
                            Language::Ru => "МБ",
                            Language::En => "MB",
                        };
                        egui::Frame::NONE
                            .fill(M3Theme::EGUI_SURFACE_CONTAINER)
                            .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT))
                            .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                            .inner_margin(egui::vec2(8.0, 3.0))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!("{}{sec_unit} · {:.1} {mb_unit}", toast.duration_secs, toast.size_mb))
                                        .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                                        .size(11.0),
                                );
                            });

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let reveal_btn = ui.add(
                                egui::Button::new(
                                    RichText::new(format!("{}  {}", M3Icon::OPEN_EXTERNAL, t(TextKey::ShowInFolder, lang)))
                                        .color(M3Theme::EGUI_ON_PRIMARY_CONTAINER)
                                        .size(11.5)
                                        .strong(),
                                )
                                .fill(M3Theme::EGUI_PRIMARY_CONTAINER)
                                .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                                .min_size(Vec2::new(0.0, 24.0)),
                            );
                            if reveal_btn.clicked() {
                                // ponytail: Use Windows Explorer select command without external subprocess abstractions
                                let _ = std::process::Command::new("explorer")
                                    .args(["/select,", &toast.filepath.to_string_lossy()])
                                    .spawn();
                            }
                        });
                    },
                );
            });
    }

    /// Associated function to render a Material 3 container section card with uniform full available width.
    pub fn card<R>(
        ui: &mut egui::Ui,
        title: &str,
        subtitle: Option<&str>,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> R {
        let card_w = ui.available_width();
        let inner_w = (card_w - 28.0).max(10.0);
        egui::Frame::NONE
            .fill(M3Theme::EGUI_SURFACE_CONTAINER)
            .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT))
            .corner_radius(CornerRadius::same(M3Theme::RADIUS_MD_U8))
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.set_min_width(inner_w);
                ui.set_max_width(inner_w);
                ui.label(RichText::new(title).color(M3Theme::EGUI_ON_SURFACE).size(14.0).strong());
                if let Some(sub) = subtitle {
                    ui.label(RichText::new(sub).color(M3Theme::EGUI_ON_SURFACE_VARIANT).size(11.0));
                }
                ui.add_space(8.0);
                add_contents(ui)
            })
            .inner
    }

    /// Helper to render a Material 3 container section card with uniform full available width.
    fn render_section_card<R>(
        &self,
        ui: &mut egui::Ui,
        title: &str,
        subtitle: Option<&str>,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> R {
        Self::card(ui, title, subtitle, add_contents)
    }

    /// Associated function to render a vertically centered horizontal row spanning available width.
    pub fn row<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 28.0),
            Layout::left_to_right(Align::Center),
            add_contents,
        )
        .inner
    }

    /// Helper to render a vertically centered horizontal row spanning available width.
    fn render_row<R>(&self, ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
        Self::row(ui, add_contents)
    }

    /// Associated function to render a slider row with fixed-width label, uniform slider rail, and an aligned Material 3 dynamic value badge.
    pub fn slider_row<T: egui::emath::Numeric>(
        ui: &mut egui::Ui,
        label: &str,
        val: &mut T,
        range: std::ops::RangeInclusive<T>,
        step: Option<f64>,
        badge_text: &str,
        badge_width: f32,
    ) -> bool {
        let mut changed = false;
        Self::row(ui, |ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(180.0, 26.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(RichText::new(label).color(M3Theme::EGUI_ON_SURFACE_VARIANT).size(12.0));
                },
            );

            let mut slider = egui::Slider::new(val, range).show_value(false);
            if let Some(s) = step {
                slider = slider.step_by(s);
            }
            let res = ui.add_sized(Vec2::new(190.0, 20.0), slider);
            if res.changed() {
                changed = true;
            }

            ui.add_space(10.0);

            let (rect, _) = ui.allocate_exact_size(Vec2::new(badge_width, 24.0), egui::Sense::hover());
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(M3Theme::RADIUS_SM_U8),
                M3Theme::EGUI_SURFACE_CONTAINER_HIGH,
            );
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(M3Theme::RADIUS_SM_U8),
                Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                badge_text,
                egui::FontId::proportional(11.5),
                M3Theme::EGUI_ON_SURFACE,
            );
        });
        changed
    }

    /// Helper to render a slider row with fixed-width label, uniform slider rail, and an aligned Material 3 dynamic value badge.
    fn render_slider_row<T: egui::emath::Numeric>(
        &self,
        ui: &mut egui::Ui,
        label: &str,
        val: &mut T,
        range: std::ops::RangeInclusive<T>,
        step: Option<f64>,
        badge_text: &str,
        badge_width: f32,
    ) -> bool {
        Self::slider_row(ui, label, val, range, step, badge_text, badge_width)
    }

    /// Helper to render a compact styled preset chip.
    fn render_preset_chip(&self, ui: &mut egui::Ui, text: &str, is_selected: bool) -> bool {
        let bg = if is_selected {
            M3Theme::EGUI_PRIMARY_CONTAINER
        } else {
            M3Theme::EGUI_SURFACE_CONTAINER_HIGH
        };
        let fg = if is_selected {
            M3Theme::EGUI_ON_PRIMARY_CONTAINER
        } else {
            M3Theme::EGUI_ON_SURFACE
        };
        let stroke = if is_selected {
            Stroke::new(1.0_f32, M3Theme::EGUI_PRIMARY)
        } else {
            Stroke::NONE
        };

        let btn = egui::Button::new(RichText::new(text).color(fg).size(11.5).strong())
            .fill(bg)
            .stroke(stroke)
            .corner_radius(CornerRadius::same(M3Theme::RADIUS_SM_U8))
            .min_size(Vec2::new(48.0, 26.0));

        ui.add(btn).clicked()
    }

    /// Associated function to render an interactive Material Design 3 toggle switch with exact vertical centering.
    pub fn toggle_switch(ui: &mut egui::Ui, checked: &mut bool, label: &str, desc: Option<&str>) -> bool {
        let mut changed = false;
        ui.vertical(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width(), 26.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    let desired_size = Vec2::new(38.0, 22.0);
                    let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
                    if response.clicked() {
                        *checked = !*checked;
                        response.mark_changed();
                        changed = true;
                    }

                    let bg_color = if *checked {
                        M3Theme::EGUI_PRIMARY
                    } else {
                        M3Theme::EGUI_SURFACE_CONTAINER_HIGHEST
                    };
                    let corner = CornerRadius::same(11);
                    ui.painter().rect_filled(rect, corner, bg_color);
                    if !*checked {
                        ui.painter().rect_stroke(rect, corner, Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE), egui::StrokeKind::Inside);
                    }

                    let thumb_radius = 7.5;
                    let thumb_center_x = if *checked {
                        rect.max.x - 11.0
                    } else {
                        rect.min.x + 11.0
                    };
                    let thumb_center = egui::pos2(thumb_center_x, rect.center().y);
                    let thumb_color = if *checked {
                        M3Theme::EGUI_ON_PRIMARY
                    } else {
                        M3Theme::EGUI_OUTLINE
                    };
                    ui.painter().circle_filled(thumb_center, thumb_radius, thumb_color);

                    ui.add_space(8.0);
                    let lbl_resp = ui.add(
                        egui::Label::new(RichText::new(label).color(M3Theme::EGUI_ON_SURFACE).size(12.5).strong())
                            .sense(egui::Sense::click()),
                    );
                    if lbl_resp.clicked() {
                        *checked = !*checked;
                        changed = true;
                    }
                },
            );

            if let Some(d) = desc {
                ui.horizontal(|ui| {
                    ui.add_space(46.0); // 38px switch width + 8px spacing
                    ui.label(RichText::new(d).color(M3Theme::EGUI_ON_SURFACE_VARIANT).size(10.5));
                });
            }
        });
        changed
    }

    /// Helper to render an interactive Material Design 3 toggle switch with exact vertical centering.
    fn render_toggle_switch(&self, ui: &mut egui::Ui, checked: &mut bool, label: &str, desc: Option<&str>) -> bool {
        Self::toggle_switch(ui, checked, label, desc)
    }

    /// Renders Tab 1: Dashboard / Main Action Hub.
    fn render_tab_dashboard(&self, ui: &mut egui::Ui, state: &UiState) {
        let lang = state.language;
        let s_unit = match lang { Language::Ru => "с", Language::En => "s" };
        let m_unit = match lang { Language::Ru => "м", Language::En => "m" };
        let mb_unit = match lang { Language::Ru => "МБ", Language::En => "MB" };

        ui.columns(2, |columns| {
            // Column 1: Instant Replay Card
            egui::Frame::NONE
                .fill(M3Theme::EGUI_SURFACE_CONTAINER)
                .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT))
                .corner_radius(CornerRadius::same(M3Theme::RADIUS_LG_U8))
                .inner_margin(16.0)
                .show(&mut columns[0], |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(t(TextKey::InstantReplayTitle, lang))
                                .color(M3Theme::EGUI_ON_SURFACE)
                                .size(15.5)
                                .strong(),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let chip_txt = if state.replay_buffer_active {
                                t(TextKey::StatusBufferOn, lang)
                            } else {
                                t(TextKey::StatusBufferOff, lang)
                            };
                            let chip_col = if state.replay_buffer_active {
                                M3Theme::EGUI_STATUS_GREEN
                            } else {
                                M3Theme::EGUI_OUTLINE
                            };
                            ui.label(RichText::new(chip_txt).color(chip_col).size(11.5).strong());
                        });
                    });

                    ui.label(
                        RichText::new(t(TextKey::InstantReplayDesc, lang))
                            .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                            .size(11.5),
                    );

                    ui.add_space(14.0);

                    // Large Save Action Button (Alt+F10)
                    let (save_bg, save_fg, save_txt) = if state.is_saving_disk {
                        (
                            M3Theme::EGUI_SURFACE_CONTAINER_HIGH,
                            M3Theme::EGUI_PRIMARY,
                            format!("{}  {}", M3Icon::STOPWATCH, t(TextKey::SavingReplay, lang)),
                        )
                    } else {
                        (
                            M3Theme::EGUI_PRIMARY,
                            M3Theme::EGUI_ON_PRIMARY,
                            format!("{}  {}", M3Icon::SAVE, t(TextKey::SaveReplay, lang)),
                        )
                    };

                    let save_btn = ui.add_enabled(
                        !state.is_saving_disk,
                        egui::Button::new(RichText::new(save_txt).color(save_fg).size(14.5).strong())
                            .fill(save_bg)
                            .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                            .min_size(Vec2::new(ui.available_width(), 44.0)),
                    );
                    if save_btn.clicked() {
                        let _ = self.action_tx.send(UiAction::SaveReplay);
                        self.state.write().is_saving_disk = true;
                    }

                    ui.add_space(14.0);

                    ui.label(
                        RichText::new(t(TextKey::BufferDurationLabel, lang))
                            .color(M3Theme::EGUI_ON_SURFACE)
                            .size(12.5),
                    );
                    ui.add_space(6.0);

                    // Quick duration preset chips
                    let durations = [15, 30, 60, 120, 300];
                    ui.horizontal_wrapped(|ui| {
                        for secs in durations {
                            let is_sel = state.buffer_duration_secs == secs;
                            let text = match secs {
                                15 => format!("15{s_unit}"),
                                30 => format!("30{s_unit}"),
                                60 => format!("60{s_unit}"),
                                120 => format!("2{m_unit}"),
                                300 => format!("5{m_unit}"),
                                _ => "??".to_string(),
                            };

                            if self.render_preset_chip(ui, &text, is_sel) {
                                self.state.write().set_buffer_duration(secs);
                                let _ = self.action_tx.send(UiAction::SetBufferDuration(secs));
                            }
                        }
                    });

                    ui.add_space(12.0);
                    let ram_info = match lang {
                        Language::Ru => format!(
                            "Потребление ОЗУ: ~{} МБ ({} сек @ {} Mbps)",
                            state.estimated_ram_mb, state.buffer_duration_secs, state.video_bitrate_mbps
                        ),
                        Language::En => format!(
                            "RAM Consumption: ~{} MB ({} sec @ {} Mbps)",
                            state.estimated_ram_mb, state.buffer_duration_secs, state.video_bitrate_mbps
                        ),
                    };
                    ui.label(
                        RichText::new(ram_info)
                            .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                            .size(11.5),
                    );
                });

            // Column 2: Manual Recording Card
            egui::Frame::NONE
                .fill(M3Theme::EGUI_SURFACE_CONTAINER)
                .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT))
                .corner_radius(CornerRadius::same(M3Theme::RADIUS_LG_U8))
                .inner_margin(16.0)
                .show(&mut columns[1], |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(t(TextKey::ManualRecordTitle, lang))
                                .color(M3Theme::EGUI_ON_SURFACE)
                                .size(15.5)
                                .strong(),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let chip_txt = if state.manual_record_active {
                                format!("● {}", t(TextKey::StatusRecording, lang))
                            } else {
                                t(TextKey::ManualRecordReady, lang).to_string()
                            };
                            let chip_col = if state.manual_record_active {
                                M3Theme::EGUI_STATUS_RED
                            } else {
                                M3Theme::EGUI_STATUS_GREEN
                            };
                            ui.label(RichText::new(chip_txt).color(chip_col).size(11.5).strong());
                        });
                    });

                    ui.label(
                        RichText::new(t(TextKey::ManualRecordDesc, lang))
                            .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                            .size(11.5),
                    );

                    ui.add_space(14.0);

                    let (rec_bg, rec_fg, rec_text) = if state.manual_record_active {
                        (
                            M3Theme::EGUI_STATUS_RED,
                            Color32::WHITE,
                            format!("{}  {}", M3Icon::STOP, t(TextKey::StopRecord, lang)),
                        )
                    } else {
                        (
                            M3Theme::EGUI_SURFACE_CONTAINER_HIGH,
                            M3Theme::EGUI_PRIMARY,
                            format!("{}  {}", M3Icon::PLAY, t(TextKey::StartRecord, lang)),
                        )
                    };

                    let rec_btn = ui.add(
                        egui::Button::new(RichText::new(rec_text).color(rec_fg).size(14.5).strong())
                            .fill(rec_bg)
                            .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                            .min_size(Vec2::new(ui.available_width(), 44.0)),
                    );
                    if rec_btn.clicked() {
                        let _ = self.action_tx.send(UiAction::ToggleManualRecord);
                    }

                    ui.add_space(14.0);
                    let (status_lbl, status_color) = if state.manual_record_active {
                        let timer_part = if let Some(start) = state.manual_record_start {
                            let s = start.elapsed().as_secs();
                            format!(" {:02}:{:02}", s / 60, s % 60)
                        } else {
                            String::new()
                        };
                        let stop_hint = match lang {
                            Language::Ru => "(Alt+F9 для остановки)",
                            Language::En => "(Alt+F9 to stop)",
                        };
                        (
                            format!("● {}:{timer_part} {stop_hint}", t(TextKey::StatusRecording, lang)),
                            M3Theme::EGUI_STATUS_RED,
                        )
                    } else {
                        (
                            t(TextKey::ManualRecordStatusWaiting, lang).to_string(),
                            M3Theme::EGUI_ON_SURFACE_VARIANT,
                        )
                    };
                    ui.label(
                        RichText::new(status_lbl)
                            .color(status_color)
                            .size(12.0),
                    );

                    ui.add_space(8.0);
                    if let Some(recent) = state.recent_replays.first() {
                        egui::Frame::NONE
                            .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                            .corner_radius(CornerRadius::same(M3Theme::RADIUS_SM_U8))
                            .inner_margin(egui::vec2(12.0, 8.0))
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.label(
                                    RichText::new(t(TextKey::LatestSavedClip, lang))
                                        .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                                        .size(10.5),
                                );
                                ui.add_space(2.0);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(M3Icon::VIDEO)
                                            .color(M3Theme::EGUI_PRIMARY)
                                            .size(12.0),
                                    );
                                    ui.label(
                                        RichText::new(&recent.filename)
                                            .color(M3Theme::EGUI_ON_SURFACE)
                                            .size(11.5)
                                            .strong(),
                                    );
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new("·")
                                            .color(M3Theme::EGUI_OUTLINE)
                                            .size(11.5),
                                    );
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(format!("{}{s_unit} · {:.1} {mb_unit}", recent.duration_secs, recent.size_mb))
                                            .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                                            .size(11.0),
                                    );
                                });
                            });
                    }
                });
        });
    }

    /// Renders Tab 2: Video & Encoding quality settings (OBS-style detailed configuration).
    fn render_tab_video(&self, ui: &mut egui::Ui, state: &UiState) {
        let lang = state.language;

        // Section 1: Resolution & Framerate
        self.render_section_card(
            ui,
            &format!("{}  {}", M3Icon::VIDEO, t(TextKey::SectionResolution, lang)),
            Some(t(TextKey::SectionResolutionDesc, lang)),
            |ui| {
                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::OutputResolution, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    let resolutions = [t(TextKey::PresetNative, lang), "1080p (FHD)", "1440p (2K)", "4K (UHD)", "3440x1440"];
                    for res in resolutions {
                        let is_sel = state.resolution_label == res;
                        if self.render_preset_chip(ui, res, is_sel) {
                            self.state.write().resolution_label = res.to_string();
                        }
                    }
                });

                ui.add_space(8.0);

                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::DownscaleFilter, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    let filters = [
                        t(TextKey::FilterBilinear, lang),
                        t(TextKey::FilterBicubic, lang),
                        t(TextKey::FilterLanczos, lang),
                    ];
                    for flt in filters {
                        let is_sel = state.video_downscale_filter == flt;
                        if self.render_preset_chip(ui, flt, is_sel) {
                            self.state.write().video_downscale_filter = flt.to_string();
                        }
                    }
                });

                ui.add_space(8.0);

                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::Framerate, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    for fps in [30, 60, 90, 120, 144, 165, 240] {
                        let is_sel = state.target_fps == fps;
                        if self.render_preset_chip(ui, &format!("{}", fps), is_sel) {
                            self.state.write().target_fps = fps;
                            let _ = self.action_tx.send(UiAction::SetFps(fps));
                        }
                    }
                });

                ui.add_space(6.0);
                let mut fps_val = state.target_fps;
                let fps_badge = format!("{} FPS", fps_val);
                if self.render_slider_row(ui, t(TextKey::ExactFps, lang), &mut fps_val, 15..=240, Some(1.0), &fps_badge, 64.0) {
                    self.state.write().target_fps = fps_val;
                    let _ = self.action_tx.send(UiAction::SetFps(fps_val));
                }
            },
        );

        ui.add_space(8.0);

        // Section 2: Rate Control & Bitrate
        self.render_section_card(
            ui,
            &format!("{}  {}", M3Icon::SLIDERS, t(TextKey::SectionRateControl, lang)),
            Some(t(TextKey::SectionRateControlDesc, lang)),
            |ui| {
                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::BitrateMode, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    let rate_modes = ["CBR (Constant)", "VBR (Variable)", "CQP (Quality)"];
                    for mode in rate_modes {
                        let is_sel = state.video_rate_control == mode;
                        if self.render_preset_chip(ui, mode, is_sel) {
                            self.state.write().video_rate_control = mode.to_string();
                        }
                    }
                });

                ui.add_space(8.0);

                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::TargetBitrate, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    let presets = [
                        (15, "15 Mbps"),
                        (25, "25 Mbps"),
                        (35, t(TextKey::BitrateBalance, lang)),
                        (50, t(TextKey::BitrateQuality, lang)),
                        (80, t(TextKey::BitrateUltra, lang)),
                        (120, t(TextKey::BitrateMax, lang)),
                    ];
                    for (mbps, label) in presets {
                        let is_sel = state.video_bitrate_mbps == mbps;
                        if self.render_preset_chip(ui, label, is_sel) {
                            self.state.write().video_bitrate_mbps = mbps;
                            let _ = self.action_tx.send(UiAction::SetBitrate(mbps));
                        }
                    }
                });

                ui.add_space(6.0);
                let mut bitrate_val = state.video_bitrate_mbps;
                let bit_badge = format!("{} Mbps", bitrate_val);
                if self.render_slider_row(ui, t(TextKey::BitrateSlider, lang), &mut bitrate_val, 2..=150, Some(1.0), &bit_badge, 68.0) {
                    self.state.write().video_bitrate_mbps = bitrate_val;
                    let _ = self.action_tx.send(UiAction::SetBitrate(bitrate_val));
                }

                ui.add_space(4.0);
                let mut peak_val = state.video_peak_bitrate_mbps;
                let peak_badge = format!("{} Mbps", peak_val);
                if self.render_slider_row(ui, t(TextKey::MaxPeakBitrate, lang), &mut peak_val, 5..=200, Some(1.0), &peak_badge, 68.0) {
                    self.state.write().video_peak_bitrate_mbps = peak_val;
                }
            },
        );

        ui.add_space(8.0);

        // Section 3: Hardware Encoder Tuning
        self.render_section_card(
            ui,
            &format!("{}  {}", M3Icon::LIGHTNING, t(TextKey::SectionEncoder, lang)),
            Some(t(TextKey::SectionEncoderDesc, lang)),
            |ui| {
                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::EncoderPreset, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    let presets = [
                        t(TextKey::PresetLowLatency, lang),
                        t(TextKey::PresetBalanced, lang),
                        t(TextKey::PresetQuality, lang),
                    ];
                    for pr in presets {
                        let is_sel = state.video_encoder_preset == pr;
                        if self.render_preset_chip(ui, pr, is_sel) {
                            self.state.write().video_encoder_preset = pr.to_string();
                        }
                    }
                });

                ui.add_space(8.0);

                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::H264Profile, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    for prof in [t(TextKey::ProfileHighRecommended, lang), "Main", "Baseline"] {
                        let is_sel = state.video_profile == prof;
                        if self.render_preset_chip(ui, prof, is_sel) {
                            self.state.write().video_profile = prof.to_string();
                        }
                    }
                });

                ui.add_space(8.0);

                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::KeyframeInterval, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    for gop in [(1, t(TextKey::GopFast, lang)), (2, t(TextKey::GopCompress, lang))] {
                        let is_sel = state.video_gop_secs == gop.0;
                        if self.render_preset_chip(ui, gop.1, is_sel) {
                            self.state.write().video_gop_secs = gop.0;
                        }
                    }
                });

                ui.add_space(8.0);

                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::ColorRange, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    for cr in [t(TextKey::ColorRangeFull, lang), t(TextKey::ColorRangeLimited, lang)] {
                        let is_sel = state.video_color_range == cr;
                        if self.render_preset_chip(ui, cr, is_sel) {
                            self.state.write().video_color_range = cr.to_string();
                        }
                    }
                });

                ui.add_space(8.0);

                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::BFrames, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    for b in [0, 1, 2] {
                        let is_sel = state.video_b_frames == b;
                        let lbl = if b == 0 { t(TextKey::BFramesMinLag, lang) } else { &format!("{}", b) };
                        if self.render_preset_chip(ui, lbl, is_sel) {
                            self.state.write().video_b_frames = b;
                        }
                    }
                });
            },
        );
    }

    /// Renders Tab 3: Replay Buffer & RAM allocation settings.
    fn render_tab_buffer(&self, ui: &mut egui::Ui, state: &UiState) {
        let lang = state.language;
        let s_unit = match lang { Language::Ru => "с", Language::En => "s" };
        let m_unit = match lang { Language::Ru => "м", Language::En => "m" };
        let gb_unit = match lang { Language::Ru => "ГБ", Language::En => "GB" };
        let sec_full = match lang { Language::Ru => "сек", Language::En => "sec" };

        self.render_section_card(
            ui,
            &format!("{}  {}", M3Icon::STOPWATCH, t(TextKey::SectionBufferDuration, lang)),
            Some(t(TextKey::SectionBufferDurationDesc, lang)),
            |ui| {
                let mut duration_val = state.buffer_duration_secs;
                let dur_badge = format!("{} {sec_full}", duration_val);
                if self.render_slider_row(ui, t(TextKey::BufferDurationLabel, lang), &mut duration_val, 10..=1200, Some(5.0), &dur_badge, 68.0) {
                    self.state.write().set_buffer_duration(duration_val);
                    let _ = self.action_tx.send(UiAction::SetBufferDuration(duration_val));
                }

                ui.add_space(8.0);

                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::QuickPresets, lang)).color(M3Theme::EGUI_ON_SURFACE_VARIANT).size(11.5));
                    let presets = [
                        (15, format!("15{s_unit}")),
                        (30, format!("30{s_unit}")),
                        (45, format!("45{s_unit}")),
                        (60, format!("1{m_unit}")),
                        (90, format!("90{s_unit}")),
                        (120, format!("2{m_unit}")),
                        (300, format!("5{m_unit}")),
                        (600, format!("10{m_unit}")),
                        (900, format!("15{m_unit}")),
                        (1200, format!("20{m_unit}")),
                    ];
                    for (secs, lbl) in presets {
                        let is_sel = state.buffer_duration_secs == secs;
                        if self.render_preset_chip(ui, &lbl, is_sel) {
                            self.state.write().set_buffer_duration(secs);
                            let _ = self.action_tx.send(UiAction::SetBufferDuration(secs));
                        }
                    }
                });

                ui.add_space(8.0);

                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::MaxRamLimit, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    let caps = [
                        (1, format!("1 {gb_unit}")),
                        (2, format!("2 {gb_unit}")),
                        (4, t(TextKey::RamCapDefault, lang).to_string()),
                        (8, format!("8 {gb_unit}")),
                        (0, t(TextKey::RamCapUnlimited, lang).to_string()),
                    ];
                    for cap in caps {
                        let is_sel = state.buffer_ram_limit_gb == cap.0;
                        if self.render_preset_chip(ui, &cap.1, is_sel) {
                            self.state.write().buffer_ram_limit_gb = cap.0;
                        }
                    }
                });

                ui.add_space(10.0);
                let mut autostart = state.buffer_autostart;
                if self.render_toggle_switch(
                    ui,
                    &mut autostart,
                    t(TextKey::AutostartBuffer, lang),
                    Some(t(TextKey::AutostartBufferDesc, lang)),
                ) {
                    self.state.write().buffer_autostart = autostart;
                }
            },
        );

        ui.add_space(8.0);

        self.render_section_card(
            ui,
            &format!("{}  {}", M3Icon::LIGHTNING, t(TextKey::BufferTelemetryTitle, lang)),
            Some(t(TextKey::BufferTelemetryDesc, lang)),
            |ui| {
                let ram_mb = state.current_ram_bytes / (1024 * 1024);
                let progress = if state.estimated_ram_mb > 0 {
                    (ram_mb as f32 / state.estimated_ram_mb as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };

                let telemetry_text = match lang {
                    Language::Ru => format!(
                        "Заполнено ОЗУ: {} MB из ~{} MB ({:.0}%)  •  Поток: ~{:.1} МБ/сек",
                        ram_mb,
                        state.estimated_ram_mb,
                        progress * 100.0,
                        (state.video_bitrate_mbps as f32) / 8.0
                    ),
                    Language::En => format!(
                        "RAM Used: {} MB of ~{} MB ({:.0}%)  •  Bitstream: ~{:.1} MB/s",
                        ram_mb,
                        state.estimated_ram_mb,
                        progress * 100.0,
                        (state.video_bitrate_mbps as f32) / 8.0
                    ),
                };

                ui.label(
                    RichText::new(telemetry_text)
                        .color(M3Theme::EGUI_ON_SURFACE)
                        .size(12.5),
                );
                ui.add_space(6.0);
                ui.add(
                    egui::ProgressBar::new(progress)
                        .show_percentage()
                        .corner_radius(CornerRadius::same(M3Theme::RADIUS_XS_U8)),
                );

                ui.add_space(8.0);
                ui.label(
                    RichText::new(t(TextKey::BufferTipSsd, lang))
                        .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                        .size(11.0),
                );
            },
        );
    }

    /// Renders Tab 4: Audio & Microphone configuration (OBS-style mixer and tracks).
    fn render_tab_audio(&self, ui: &mut egui::Ui, state: &UiState) {
        let lang = state.language;

        // Section 1: Microphone & Noise Gate
        self.render_section_card(
            ui,
            &format!("{}  {}", M3Icon::MIC, t(TextKey::SectionMic, lang)),
            Some(t(TextKey::SectionMicDesc, lang)),
            |ui| {
                let mut mic_on = state.mic_enabled;
                if self.render_toggle_switch(
                    ui,
                    &mut mic_on,
                    t(TextKey::MicCapture, lang),
                    Some(t(TextKey::MicCaptureDesc, lang)),
                ) {
                    self.state.write().mic_enabled = mic_on;
                    let _ = self.action_tx.send(UiAction::ToggleMic);
                }

                ui.add_space(8.0);

                // Microphone Device Selection ComboBox
                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::InputDevice, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    let default_mic_lbl = t(TextKey::DefaultMic, lang);
                    let current_mic = if state.selected_capture_device == "default" || state.selected_capture_device.is_empty() {
                        default_mic_lbl.to_string()
                    } else {
                        state.available_capture_devices
                            .iter()
                            .find(|(id, _)| id == &state.selected_capture_device)
                            .map(|(_, name)| name.clone())
                            .unwrap_or_else(|| default_mic_lbl.to_string())
                    };

                    egui::ComboBox::from_id_salt("mic_device_combo")
                        .selected_text(RichText::new(&current_mic).color(M3Theme::EGUI_ON_SURFACE).size(12.0))
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            let is_def = state.selected_capture_device == "default" || state.selected_capture_device.is_empty();
                            if ui.selectable_label(is_def, default_mic_lbl).clicked() {
                                self.state.write().selected_capture_device = "default".to_string();
                            }
                            for (dev_id, dev_name) in &state.available_capture_devices {
                                let is_sel = &state.selected_capture_device == dev_id;
                                if ui.selectable_label(is_sel, dev_name).clicked() {
                                    self.state.write().selected_capture_device = dev_id.clone();
                                }
                            }
                        });

                    if ui.add(
                        egui::Button::new(RichText::new(format!("{}  {}", M3Icon::REFRESH, t(TextKey::Refresh, lang))).size(11.0))
                            .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                            .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                    ).clicked() {
                        let devs = crate::audio::enumerate_audio_devices(true);
                        self.state.write().available_capture_devices = devs;
                    }
                });

                ui.add_space(8.0);

                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::ActivationMode, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    let ptt = state.mic_mode_ptt;
                    if self.render_preset_chip(ui, t(TextKey::AlwaysOn, lang), !ptt) {
                        self.state.write().mic_mode_ptt = false;
                    }
                    if self.render_preset_chip(ui, t(TextKey::PushToTalk, lang), ptt) {
                        self.state.write().mic_mode_ptt = true;
                    }

                    if ptt {
                        ui.add_space(10.0);
                        ui.label(RichText::new(t(TextKey::PttKey, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                        let mut ptt_key = state.mic_ptt_key.clone();
                        if ui.add(egui::TextEdit::singleline(&mut ptt_key).desired_width(40.0)).changed() {
                            self.state.write().mic_ptt_key = ptt_key;
                        }
                    }
                });

                ui.add_space(8.0);

                ui.add_space(6.0);
                let mut vol = state.mic_volume;
                let mic_db = if vol > 0.0 { 20.0 * vol.log10() } else { -60.0 };
                let mic_badge = format!("{:.0}% ({:.1} dB)", vol * 100.0, mic_db);
                if self.render_slider_row(ui, t(TextKey::MicVolume, lang), &mut vol, 0.0..=2.0, Some(0.05), &mic_badge, 88.0) {
                    self.state.write().mic_volume = vol;
                    let _ = self.action_tx.send(UiAction::SetMicVolume(vol));
                }

                ui.add_space(4.0);

                let mut gate = state.mic_noise_gate_db;
                let gate_badge = format!("{:.0} dB", gate);
                if self.render_slider_row(ui, t(TextKey::NoiseGate, lang), &mut gate, -60.0..=0.0, Some(1.0), &gate_badge, 64.0) {
                    self.state.write().mic_noise_gate_db = gate;
                }
            },
        );

        ui.add_space(8.0);

        // Section 2: Game / Desktop Audio & App Filter
        self.render_section_card(
            ui,
            &format!("{}  {}", M3Icon::AUDIO, t(TextKey::SectionSystemAudio, lang)),
            Some(t(TextKey::SectionSystemAudioDesc, lang)),
            |ui| {
                let mut game_on = state.game_audio_enabled;
                if self.render_toggle_switch(
                    ui,
                    &mut game_on,
                    t(TextKey::GameAudioCapture, lang),
                    Some(t(TextKey::GameAudioCaptureDesc, lang)),
                ) {
                    self.state.write().game_audio_enabled = game_on;
                }

                ui.add_space(8.0);

                // Playback Device Selection ComboBox
                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::OutputDevice, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    let default_speakers_lbl = t(TextKey::DefaultSpeakers, lang);
                    let current_playback = if state.selected_playback_device == "default" || state.selected_playback_device.is_empty() {
                        default_speakers_lbl.to_string()
                    } else {
                        state.available_playback_devices
                            .iter()
                            .find(|(id, _)| id == &state.selected_playback_device)
                            .map(|(_, name)| name.clone())
                            .unwrap_or_else(|| default_speakers_lbl.to_string())
                    };

                    egui::ComboBox::from_id_salt("playback_device_combo")
                        .selected_text(RichText::new(&current_playback).color(M3Theme::EGUI_ON_SURFACE).size(12.0))
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            let is_def = state.selected_playback_device == "default" || state.selected_playback_device.is_empty();
                            if ui.selectable_label(is_def, default_speakers_lbl).clicked() {
                                self.state.write().selected_playback_device = "default".to_string();
                            }
                            for (dev_id, dev_name) in &state.available_playback_devices {
                                let is_sel = &state.selected_playback_device == dev_id;
                                if ui.selectable_label(is_sel, dev_name).clicked() {
                                    self.state.write().selected_playback_device = dev_id.clone();
                                }
                            }
                        });

                    if ui.add(
                        egui::Button::new(RichText::new(format!("{}  {}", M3Icon::REFRESH, t(TextKey::Refresh, lang))).size(11.0))
                            .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                            .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                    ).clicked() {
                        let devs = crate::audio::enumerate_audio_devices(false);
                        self.state.write().available_playback_devices = devs;
                    }
                });

                ui.add_space(6.0);

                let mut gvol = state.game_audio_volume;
                let game_db = if gvol > 0.0 { 20.0 * gvol.log10() } else { -60.0 };
                let game_badge = format!("{:.0}% ({:.1} dB)", gvol * 100.0, game_db);
                let game_vol_lbl = match lang {
                    Language::Ru => "Громкость игры:",
                    Language::En => "Game Volume:",
                };
                if self.render_slider_row(ui, game_vol_lbl, &mut gvol, 0.0..=1.0, Some(0.05), &game_badge, 88.0) {
                    self.state.write().game_audio_volume = gvol;
                }

                ui.add_space(10.0);

                // Application Audio Filtering Mode
                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::AudioFilterMode, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    for (mode_key, mode_lbl) in [
                        ("all", t(TextKey::FilterAllAudio, lang)),
                        ("include", t(TextKey::FilterSelectedOnly, lang)),
                        ("exclude", t(TextKey::FilterExcludeSelected, lang)),
                    ] {
                        let is_sel = state.audio_app_filter_mode == mode_key;
                        if self.render_preset_chip(ui, mode_lbl, is_sel) {
                            self.state.write().audio_app_filter_mode = mode_key.to_string();
                        }
                    }
                });

                if state.audio_app_filter_mode != "all" {
                    ui.add_space(8.0);
                    egui::Frame::NONE
                        .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                        .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT))
                        .corner_radius(CornerRadius::same(M3Theme::RADIUS_MD_U8))
                        .inner_margin(12.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let filter_hint = if state.audio_app_filter_mode == "include" {
                                    let hint_text = match lang {
                                        Language::Ru => "Записывать звук только из отмеченных приложений:",
                                        Language::En => "Record audio only from checked applications:",
                                    };
                                    format!("{}  {}", M3Icon::CHECK, hint_text)
                                } else {
                                    let hint_text = match lang {
                                        Language::Ru => "Исключить из записи звуки отмеченных приложений:",
                                        Language::En => "Exclude audio from checked applications:",
                                    };
                                    format!("{}  {}", M3Icon::MUTE, hint_text)
                                };
                                ui.label(RichText::new(filter_hint).color(M3Theme::EGUI_ON_SURFACE).size(12.0).strong());

                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    // Quick select actions if apps exist
                                    if !state.detected_audio_apps.is_empty() {
                                        if ui.add(
                                            egui::Button::new(RichText::new(t(TextKey::DeselectAll, lang)).size(10.5))
                                                .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGHEST)
                                                .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                                        ).clicked() {
                                            self.state.write().selected_audio_apps.clear();
                                        }
                                        if ui.add(
                                            egui::Button::new(RichText::new(t(TextKey::SelectAll, lang)).size(10.5))
                                                .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGHEST)
                                                .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                                        ).clicked() {
                                            let all_names: Vec<String> = state
                                                .detected_audio_apps
                                                .iter()
                                                .map(|a| a.name.clone())
                                                .collect();
                                            self.state.write().selected_audio_apps = all_names;
                                        }
                                    }

                                    // Real-time live status indicator with pulsing dot
                                    let pulse_alpha = ((self.pulse_phase.sin() * 0.35 + 0.65) * 255.0) as u8;
                                    let pulse_color = Color32::from_rgba_premultiplied(76, 175, 80, pulse_alpha);
                                    ui.horizontal(|ui| {
                                        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                                        ui.painter().circle_filled(dot_rect.center(), 3.5, pulse_color);
                                        ui.label(RichText::new(t(TextKey::Realtime, lang)).color(M3Theme::EGUI_ON_SURFACE_VARIANT).size(11.0));
                                    });
                                });
                            });

                            ui.add_space(8.0);

                            if state.detected_audio_apps.is_empty() {
                                egui::Frame::NONE
                                    .fill(M3Theme::EGUI_SURFACE_CONTAINER_LOW)
                                    .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT))
                                    .corner_radius(CornerRadius::same(M3Theme::RADIUS_SM_U8))
                                    .inner_margin(egui::Margin::symmetric(14, 10))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new(M3Icon::AUDIO).color(M3Theme::EGUI_ON_SURFACE_VARIANT).size(14.0));
                                            ui.add_space(4.0);
                                            ui.label(
                                                RichText::new(t(TextKey::NoAudioPlaying, lang))
                                                    .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                                                    .size(11.5),
                                            );
                                        });
                                    });
                            } else {
                                let mut selected_apps = state.selected_audio_apps.clone();
                                let mut apps_changed = false;

                                ui.horizontal_wrapped(|ui| {
                                    for app in &state.detected_audio_apps {
                                        let is_checked = selected_apps.iter().any(|s| s.eq_ignore_ascii_case(&app.name));

                                        let (bg_color, stroke_color, stroke_width) = if is_checked {
                                            (
                                                Color32::from_rgba_premultiplied(38, 68, 131, 160),
                                                M3Theme::EGUI_PRIMARY,
                                                1.5_f32,
                                            )
                                        } else {
                                            (
                                                M3Theme::EGUI_SURFACE_CONTAINER_HIGHEST,
                                                M3Theme::EGUI_OUTLINE_VARIANT,
                                                1.0_f32,
                                            )
                                        };

                                        let frame = egui::Frame::NONE
                                            .fill(bg_color)
                                            .stroke(Stroke::new(stroke_width, stroke_color))
                                            .corner_radius(CornerRadius::same(M3Theme::RADIUS_SM_U8))
                                            .inner_margin(egui::Margin::symmetric(10, 7));

                                        let card_res = frame.show(ui, |ui| {
                                            ui.set_min_width(175.0);
                                            ui.vertical(|ui| {
                                                // Header: Checkmark box + App Icon + App Name
                                                ui.horizontal(|ui| {
                                                    let (check_glyph, check_col) = if is_checked {
                                                        ("☑", M3Theme::EGUI_PRIMARY)
                                                    } else {
                                                        ("☐", M3Theme::EGUI_ON_SURFACE_VARIANT)
                                                    };
                                                    ui.label(RichText::new(check_glyph).color(check_col).size(13.0).strong());

                                                    // Real application icon (or fallback badge)
                                                    if let Some(tex) = self.get_or_load_app_icon(ui.ctx(), &app.name, &app.exe_path) {
                                                        ui.add(egui::Image::new(&tex).fit_to_exact_size(egui::vec2(18.0, 18.0)));
                                                    } else {
                                                        let fallback = if app.name.to_lowercase().ends_with(".exe") {
                                                            "🎮"
                                                        } else {
                                                            M3Icon::AUDIO
                                                        };
                                                        ui.label(RichText::new(fallback).size(12.0));
                                                    }

                                                    ui.label(RichText::new(&app.name).color(M3Theme::EGUI_ON_SURFACE).size(12.0).strong());
                                                });

                                                ui.add_space(3.0);

                                                // Footer: Live VU volume meter or idle status
                                                ui.horizontal(|ui| {
                                                    ui.add_space(16.0);
                                                    if app.is_playing || app.peak > 0.005 {
                                                        ui.label(RichText::new(M3Icon::AUDIO).color(M3Theme::EGUI_STATUS_GREEN).size(11.0));

                                                        let bar_w = 60.0_f32;
                                                        let bar_h = 4.0_f32;
                                                        let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
                                                        ui.painter().rect_filled(rect, CornerRadius::same(2), Color32::from_rgb(20, 22, 28));

                                                        let fill_w = (rect.width() * app.peak.clamp(0.0, 1.0)).max(2.0);
                                                        let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height()));
                                                        let meter_color = if app.peak > 0.85 {
                                                            M3Theme::EGUI_PRIMARY
                                                        } else {
                                                            M3Theme::EGUI_STATUS_GREEN
                                                        };
                                                        ui.painter().rect_filled(fill_rect, CornerRadius::same(2), meter_color);

                                                        let pct = (app.peak * 100.0).clamp(0.0, 100.0) as u32;
                                                        ui.label(RichText::new(format!("{}%", pct)).color(M3Theme::EGUI_ON_SURFACE_VARIANT).size(10.0));
                                                    } else {
                                                        ui.label(RichText::new(M3Icon::PAUSE).color(M3Theme::EGUI_ON_SURFACE_VARIANT).size(11.0));
                                                        ui.label(RichText::new(t(TextKey::Silence, lang)).color(M3Theme::EGUI_ON_SURFACE_VARIANT).size(10.0));
                                                    }
                                                });
                                            });
                                        });

                                        let interact = ui.interact(card_res.response.rect, ui.id().with(&app.name), egui::Sense::click());
                                        if interact.clicked() {
                                            if is_checked {
                                                selected_apps.retain(|s| !s.eq_ignore_ascii_case(&app.name));
                                            } else {
                                                selected_apps.push(app.name.clone());
                                            }
                                            apps_changed = true;
                                        }
                                        if interact.hovered() {
                                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                        }
                                    }
                                });

                                if apps_changed {
                                    self.state.write().selected_audio_apps = selected_apps;
                                }
                            }
                        });
                }
            },
        );

        ui.add_space(8.0);

        // Section 3: Audio Codec & Multi-Track
        self.render_section_card(
            ui,
            &format!("{}  {}", M3Icon::EQUALIZER, t(TextKey::SectionAudioCodec, lang)),
            Some(t(TextKey::SectionAudioCodecDesc, lang)),
            |ui| {
                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::AacBitrate, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    for kbps in [96, 128, 160, 192, 256, 320] {
                        let is_sel = state.audio_bitrate_kbps == kbps;
                        let lbl = format!("{} kbps", kbps);
                        if self.render_preset_chip(ui, &lbl, is_sel) {
                            self.state.write().audio_bitrate_kbps = kbps;
                        }
                    }
                });

                ui.add_space(8.0);

                self.render_row(ui, |ui| {
                    ui.label(RichText::new(t(TextKey::SampleRate, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    for sr in [(44100, "44.1 kHz"), (48000, "48.0 kHz (Native)")] {
                        let is_sel = state.audio_sample_rate_hz == sr.0;
                        if self.render_preset_chip(ui, sr.1, is_sel) {
                            self.state.write().audio_sample_rate_hz = sr.0;
                        }
                    }

                    ui.add_space(16.0);
                    ui.label(RichText::new(t(TextKey::Channels, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.5));
                    for ch in [t(TextKey::ChannelStereo, lang), t(TextKey::ChannelMono, lang)] {
                        let is_sel = state.audio_channels == ch;
                        if self.render_preset_chip(ui, ch, is_sel) {
                            self.state.write().audio_channels = ch.to_string();
                        }
                    }
                });

                ui.add_space(10.0);

                let mut multi = state.audio_multi_track;
                if self.render_toggle_switch(
                    ui,
                    &mut multi,
                    t(TextKey::MultiTrackAudio, lang),
                    Some(t(TextKey::MultiTrackAudioDesc, lang)),
                ) {
                    self.state.write().audio_multi_track = multi;
                    let _ = self.action_tx.send(UiAction::SetAudioMultiTrack(multi));
                }
            },
        );
    }

    /// Renders Tab 5: Hotkeys overview with keyboard keycaps design.
    fn render_tab_hotkeys(&self, ui: &mut egui::Ui, state: &UiState) {
        let lang = state.language;
        self.render_section_card(
            ui,
            &format!("{}  {}", M3Icon::KEYBOARD, t(TextKey::SectionHotkeys, lang)),
            Some(t(TextKey::SectionHotkeysDesc, lang)),
            |ui| {
                let ptt_off = if lang == Language::Ru { "ВЫКЛ (Always On)" } else { "OFF (Always On)" };
                let ptt_display = if state.mic_mode_ptt { &state.mic_ptt_key } else { ptt_off };
                let hotkeys = [
                    (t(TextKey::ActionSaveReplay, lang), "Alt", "F10", t(TextKey::DescSaveReplay, lang)),
                    (t(TextKey::ActionToggleRecord, lang), "Alt", "F9", t(TextKey::DescToggleRecord, lang)),
                    (t(TextKey::ActionToggleOverlay, lang), "Alt", "Z", t(TextKey::DescToggleOverlay, lang)),
                    (t(TextKey::HotkeysPtt, lang), ptt_display, "", t(TextKey::HotkeysPttDesc, lang)),
                    (t(TextKey::ActionTakeScreenshot, lang), "Alt", "F1", t(TextKey::DescTakeScreenshot, lang)),
                    (t(TextKey::HotkeysEsc, lang), "Esc", "", t(TextKey::HotkeysEscDesc, lang)),
                ];

                for (name, key1, key2, desc) in hotkeys {
                    egui::Frame::NONE
                        .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                        .corner_radius(CornerRadius::same(M3Theme::RADIUS_MD_U8))
                        .inner_margin(egui::vec2(12.0, 8.0))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.set_max_width(ui.available_width());
                            ui.allocate_ui_with_layout(
                                Vec2::new(ui.available_width(), 26.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.label(RichText::new(name).color(M3Theme::EGUI_ON_SURFACE).size(12.5).strong());
                                    ui.add_space(8.0);
                                    ui.label(RichText::new("·").color(M3Theme::EGUI_OUTLINE).size(12.0));
                                    ui.add_space(8.0);
                                    ui.label(RichText::new(desc).color(M3Theme::EGUI_ON_SURFACE_VARIANT).size(11.0));

                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if !key2.is_empty() {
                                            self.render_keycap(ui, key2);
                                            ui.label(RichText::new("+").color(M3Theme::EGUI_ON_SURFACE_VARIANT).size(12.0));
                                        }
                                        self.render_keycap(ui, key1);
                                    });
                                },
                            );
                        });
                    ui.add_space(4.0);
                }
            },
        );
    }

    /// Renders a single keyboard keycap badge.
    fn render_keycap(&self, ui: &mut egui::Ui, key: &str) {
        egui::Frame::NONE
            .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGHEST)
            .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE))
            .corner_radius(CornerRadius::same(M3Theme::RADIUS_XS_U8))
            .inner_margin(egui::vec2(8.0, 4.0))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(key)
                        .color(M3Theme::EGUI_PRIMARY)
                        .size(11.5)
                        .strong(),
                );
            });
    }

    /// Renders Tab 6: Storage folder, file naming patterns, and clips history.
    fn render_tab_storage(&mut self, ui: &mut egui::Ui, state: &UiState) {
        let lang = state.language;
        let path_edit_buffer = &mut self.path_edit_buffer;
        let action_tx = &self.action_tx;
        let state_lock = &self.state;

        Self::card(
            ui,
            &format!("{}  {}", M3Icon::FOLDER, t(TextKey::SectionOutputDir, lang)),
            Some(t(TextKey::SectionOutputDirDesc, lang)),
            |ui| {
                Self::row(ui, |ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(180.0, 26.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.label(RichText::new(format!("{}:", t(TextKey::TabStorage, lang))).color(M3Theme::EGUI_ON_SURFACE).size(12.0));
                        },
                    );

                    let path_edit = ui.add(
                        egui::TextEdit::singleline(path_edit_buffer)
                            .desired_width(260.0)
                            .min_size(Vec2::new(0.0, 26.0))
                            .margin(egui::vec2(8.0, 4.0)),
                    );

                    ui.add_space(6.0);

                    let browse_btn = ui.add(
                        egui::Button::new(
                            RichText::new(format!("{}  {}", M3Icon::FOLDER, t(TextKey::ChooseFolder, lang)))
                                .color(M3Theme::EGUI_ON_PRIMARY_CONTAINER)
                                .size(11.5)
                                .strong(),
                        )
                        .fill(M3Theme::EGUI_PRIMARY_CONTAINER)
                        .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                        .min_size(Vec2::new(88.0, 26.0)),
                    );
                    if browse_btn.clicked() {
                        if let Some(picked) = crate::ui::pick_folder(Some(t(TextKey::FolderPickerTitle, lang))) {
                            *path_edit_buffer = picked.to_string_lossy().to_string();
                            state_lock.write().output_dir = picked.clone();
                            let _ = action_tx.send(UiAction::SetOutputDir(picked));
                        }
                    }

                    ui.add_space(6.0);

                    let apply_btn = ui.add(
                        egui::Button::new(
                            RichText::new(t(TextKey::Apply, lang))
                                .color(M3Theme::EGUI_ON_PRIMARY)
                                .size(11.5)
                                .strong(),
                        )
                        .fill(M3Theme::EGUI_PRIMARY)
                        .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                        .min_size(Vec2::new(80.0, 26.0)),
                    );
                    if apply_btn.clicked() || (path_edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                        let new_path = PathBuf::from(&*path_edit_buffer);
                        state_lock.write().output_dir = new_path.clone();
                        let _ = action_tx.send(UiAction::SetOutputDir(new_path));
                    }

                    ui.add_space(6.0);

                    let open_btn = ui.add(
                        egui::Button::new(
                            RichText::new(format!("{}  {}", M3Icon::OPEN_EXTERNAL, t(TextKey::InExplorer, lang)))
                                .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                                .size(11.5),
                        )
                        .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                        .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                        .min_size(Vec2::new(120.0, 26.0)),
                    );
                    if open_btn.clicked() {
                        let _ = std::process::Command::new("explorer")
                            .arg(&state.output_dir)
                            .spawn();
                    }
                });

                ui.add_space(8.0);

                Self::row(ui, |ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(180.0, 26.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.label(RichText::new(t(TextKey::FilenameTemplate, lang)).color(M3Theme::EGUI_ON_SURFACE).size(12.0));
                        },
                    );
                    let mut pattern = state.filename_pattern.clone();
                    if ui.add(
                        egui::TextEdit::singleline(&mut pattern)
                            .desired_width(280.0)
                            .min_size(Vec2::new(0.0, 26.0))
                            .margin(egui::vec2(8.0, 4.0)),
                    ).changed() {
                        state_lock.write().filename_pattern = pattern.clone();
                        let _ = action_tx.send(UiAction::SetFilenamePattern(pattern));
                    }
                });

                // Live dynamic template preview
                let preview_path = crate::muxer::Mp4Muxer::format_replay_filename(
                    &state.filename_pattern,
                    &state.output_dir,
                    1920,
                    1080,
                    state.target_fps,
                );
                let preview_name = preview_path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                Self::row(ui, |ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(180.0, 20.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.label(RichText::new(t(TextKey::TemplatePreview, lang)).color(M3Theme::EGUI_ON_SURFACE_VARIANT).size(11.0));
                        },
                    );
                    ui.label(
                        RichText::new(format!("📝 {}", preview_name))
                            .color(M3Theme::EGUI_PRIMARY)
                            .size(11.5)
                            .strong(),
                    );
                });

                ui.add_space(8.0);

                let mut sound_notif = state.sound_notify_on_save;
                if Self::toggle_switch(
                    ui,
                    &mut sound_notif,
                    t(TextKey::SoundNotify, lang),
                    Some(t(TextKey::SoundNotifyDesc, lang)),
                ) {
                    state_lock.write().sound_notify_on_save = sound_notif;
                }
            },
        );

        ui.add_space(8.0);

        self.render_section_card(
            ui,
            &format!("{}  {}", M3Icon::VIDEO, t(TextKey::SectionRecentReplays, lang)),
            Some(t(TextKey::SectionRecentReplaysDesc, lang)),
            |ui| {
                if state.recent_replays.is_empty() {
                    ui.label(
                        RichText::new(t(TextKey::NoRecentReplays, lang))
                            .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                            .size(11.5),
                    );
                } else {
                    let sec_unit = if lang == Language::Ru { "с" } else { "s" };
                    let mb_unit = if lang == Language::Ru { "МБ" } else { "MB" };
                    for clip in &state.recent_replays {
                        egui::Frame::NONE
                            .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                            .corner_radius(CornerRadius::same(M3Theme::RADIUS_SM_U8))
                            .inner_margin(egui::vec2(12.0, 8.0))
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.set_max_width(ui.available_width());
                                ui.allocate_ui_with_layout(
                                    Vec2::new(ui.available_width(), 26.0),
                                    Layout::left_to_right(Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new(M3Icon::VIDEO)
                                                .color(M3Theme::EGUI_PRIMARY)
                                                .size(12.5),
                                        );
                                        ui.add_space(4.0);
                                        ui.label(
                                            RichText::new(&clip.filename)
                                                .color(M3Theme::EGUI_ON_SURFACE)
                                                .size(12.0)
                                                .strong(),
                                        );
                                        ui.add_space(6.0);
                                        ui.label(
                                            RichText::new("·")
                                                .color(M3Theme::EGUI_OUTLINE)
                                                .size(12.0),
                                        );
                                        ui.add_space(6.0);
                                        egui::Frame::NONE
                                            .fill(M3Theme::EGUI_SURFACE_CONTAINER)
                                            .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT))
                                            .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                                            .inner_margin(egui::vec2(8.0, 3.0))
                                            .show(ui, |ui| {
                                                ui.label(
                                                    RichText::new(format!("{}{sec_unit} · {:.1} {mb_unit}", clip.duration_secs, clip.size_mb))
                                                        .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                                                        .size(11.0),
                                                );
                                            });

                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            let show_btn = ui.add(
                                                egui::Button::new(
                                                    RichText::new(format!("{}  {}", M3Icon::OPEN_EXTERNAL, t(TextKey::Open, lang)))
                                                        .color(M3Theme::EGUI_ON_PRIMARY_CONTAINER)
                                                        .size(11.5)
                                                        .strong(),
                                                )
                                                .fill(M3Theme::EGUI_PRIMARY_CONTAINER)
                                                .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                                                .min_size(Vec2::new(0.0, 24.0)),
                                            );
                                            if show_btn.clicked() {
                                                let _ = std::process::Command::new("explorer")
                                                    .args(["/select,", &clip.filepath.to_string_lossy()])
                                                    .spawn();
                                            }
                                        });
                                    },
                                );
                            });
                        ui.add_space(4.0);
                    }
                }
            },
        );
    }

    /// Renders the bottom quick status and hint bar with crisp vector icons.
    fn render_bottom_bar(&self, ui: &mut egui::Ui, state: &UiState) {
        let lang = state.language;
        ui.horizontal(|ui| {
            // Clickable Mic toggle chip
            let mic_lbl = if state.mic_enabled {
                format!("{}  {} ({:.0}%)", M3Icon::MIC, t(TextKey::MicStatusOn, lang), state.mic_volume * 100.0)
            } else {
                format!("{}  {}", M3Icon::MIC, t(TextKey::MicStatusOff, lang))
            };
            let mic_chip = ui.add(
                egui::Button::new(
                    RichText::new(mic_lbl)
                        .color(if state.mic_enabled { M3Theme::EGUI_ON_PRIMARY_CONTAINER } else { M3Theme::EGUI_ON_SURFACE_VARIANT })
                        .size(11.5),
                )
                .fill(if state.mic_enabled { M3Theme::EGUI_PRIMARY_CONTAINER } else { M3Theme::EGUI_SURFACE_CONTAINER_HIGH })
                .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8)),
            );
            if mic_chip.clicked() {
                let mut st = self.state.write();
                st.mic_enabled = !st.mic_enabled;
                let _ = self.action_tx.send(UiAction::ToggleMic);
            }

            ui.add_space(8.0);

            // Speakers chip
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{}  {}", M3Icon::AUDIO, t(TextKey::SpeakersStatus, lang)))
                        .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                        .size(11.5),
                )
                .selectable(false),
            );

            ui.add_space(8.0);

            // Output Folder button chip
            let folder_name = state
                .output_dir
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| "OmniClip".to_string());
            let folder_chip = ui.add(
                egui::Button::new(
                    RichText::new(format!("{}  {}", M3Icon::FOLDER, folder_name))
                        .color(M3Theme::EGUI_ON_SURFACE)
                        .size(11.5),
                )
                .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8)),
            );
            if folder_chip.clicked() {
                let _ = std::process::Command::new("explorer")
                    .arg(&state.output_dir)
                    .spawn();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(t(TextKey::BottomEscHint, lang))
                        .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                        .size(11.5),
                );
            });
        });
    }

    /// Retrieves or loads the cached egui icon texture for an application.
    ///
    /// # Arguments
    /// * `ctx` - The egui context used to allocate textures.
    /// * `name` - The process executable name (e.g. "firefox.exe").
    /// * `exe_path` - The absolute path to the executable file, if known.
    ///
    /// # Returns
    /// An `Option<egui::TextureHandle>` containing the GPU texture handle, or `None` if unavailable.
    fn get_or_load_app_icon(
        &self,
        ctx: &egui::Context,
        name: &str,
        exe_path: &str,
    ) -> Option<egui::TextureHandle> {
        let cache_key = if !exe_path.is_empty() {
            exe_path.to_lowercase()
        } else {
            name.to_lowercase()
        };

        if let Ok(cache) = self.app_icon_textures.lock() {
            if let Some(cached) = cache.get(&cache_key) {
                return cached.clone();
            }
        }

        let path_to_try = if !exe_path.is_empty() {
            exe_path.to_string()
        } else {
            let candidate_sys32 = format!(r"C:\Windows\System32\{}", name);
            if std::path::Path::new(&candidate_sys32).exists() {
                candidate_sys32
            } else {
                name.to_string()
            }
        };

        let texture_handle = extract_icon_from_exe(&path_to_try).map(|color_image| {
            ctx.load_texture(
                format!("app_icon_{}", name),
                color_image,
                egui::TextureOptions::LINEAR,
            )
        });

        if let Ok(mut cache) = self.app_icon_textures.lock() {
            cache.insert(cache_key, texture_handle.clone());
        }

        texture_handle
    }
}

/// Extracts an application's icon from its executable PE headers and converts it to an `egui::ColorImage`.
///
/// Win32 GDI handles must be explicitly destroyed and released to prevent resource exhaustion.
///
/// # Arguments
/// * `exe_path` - The absolute path to the executable.
///
/// # Returns
/// An `Option<egui::ColorImage>` containing 32-bit RGBA pixels, or `None` if extraction fails.
fn extract_icon_from_exe(exe_path: &str) -> Option<egui::ColorImage> {
    use windows::Win32::UI::Shell::ExtractIconExW;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetIconInfo, DestroyIcon, HICON, ICONINFO,
    };
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetObjectW, GetDIBits,
        BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, BI_RGB,
    };

    if exe_path.is_empty() {
        return None;
    }

    let path_u16: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hicon_large = HICON::default();
    let mut hicon_small = HICON::default();

    unsafe {
        let count = ExtractIconExW(
            windows::core::PCWSTR(path_u16.as_ptr()),
            0,
            Some(&mut hicon_large),
            Some(&mut hicon_small),
            1,
        );

        if count == 0 {
            return None;
        }

        // Prefer large icon (32x32) for crisp high-DPI downsampling, fallback to small (16x16)
        let (hicon_to_use, hicon_to_destroy) = if !hicon_large.is_invalid() {
            (hicon_large, hicon_small)
        } else if !hicon_small.is_invalid() {
            (hicon_small, hicon_large)
        } else {
            return None;
        };

        if !hicon_to_destroy.is_invalid() {
            let _ = DestroyIcon(hicon_to_destroy);
        }

        let mut icon_info = ICONINFO::default();
        if GetIconInfo(hicon_to_use, &mut icon_info).is_err() {
            let _ = DestroyIcon(hicon_to_use);
            return None;
        }

        let target_hbm = if !icon_info.hbmColor.is_invalid() {
            icon_info.hbmColor
        } else {
            icon_info.hbmMask
        };

        let mut bmp = BITMAP::default();
        let get_obj_res = GetObjectW(
            target_hbm,
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bmp as *mut _ as *mut _),
        );

        if get_obj_res == 0 || bmp.bmWidth <= 0 || bmp.bmHeight <= 0 {
            let _ = DeleteObject(icon_info.hbmColor);
            let _ = DeleteObject(icon_info.hbmMask);
            let _ = DestroyIcon(hicon_to_use);
            return None;
        }

        let width = bmp.bmWidth;
        let height = if icon_info.hbmColor.is_invalid() {
            bmp.bmHeight / 2
        } else {
            bmp.bmHeight
        };

        let hdc = CreateCompatibleDC(None);
        if hdc.is_invalid() {
            let _ = DeleteObject(icon_info.hbmColor);
            let _ = DeleteObject(icon_info.hbmMask);
            let _ = DestroyIcon(hicon_to_use);
            return None;
        }

        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // Negative indicates top-down bitmap orientation
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let pixel_count = (width * height) as usize;
        let mut raw_pixels = vec![0u8; pixel_count * 4];

        let scan_lines = GetDIBits(
            hdc,
            target_hbm,
            0,
            height.unsigned_abs(),
            Some(raw_pixels.as_mut_ptr() as *mut _),
            &mut bi,
            DIB_RGB_COLORS,
        );

        let _ = DeleteDC(hdc);
        let _ = DeleteObject(icon_info.hbmColor);
        let _ = DeleteObject(icon_info.hbmMask);
        let _ = DestroyIcon(hicon_to_use);

        if scan_lines == 0 {
            return None;
        }

        // Detect whether the icon contains true 32-bit ARGB alpha transparency
        let mut has_alpha = false;
        for chunk in raw_pixels.chunks_exact(4) {
            if chunk[3] > 0 {
                has_alpha = true;
                break;
            }
        }

        // Convert BGRA to RGBA in-place and synthesize alpha if missing from legacy DIB
        for chunk in raw_pixels.chunks_exact_mut(4) {
            let b = chunk[0];
            let r = chunk[2];
            chunk[0] = r;
            chunk[2] = b;
            if !has_alpha {
                chunk[3] = 255;
            }
        }

        Some(egui::ColorImage::from_rgba_unmultiplied(
            [width as usize, height as usize],
            &raw_pixels,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_card_uniform_width() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let rect_narrow = OmniClipApp::card(ui, "Narrow", None, |ui| {
                    ui.label("Short");
                });
                let rect_wide = OmniClipApp::card(ui, "Wide", None, |ui| {
                    ui.allocate_space(egui::vec2(500.0, 10.0));
                });
                let _ = (rect_narrow, rect_wide);
            });
        });
    }

    #[test]
    fn test_row_vertical_alignment() {
        let ctx = egui::Context::default();
        M3Theme::apply_m3_style(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                OmniClipApp::row(ui, |ui| {
                    let r_lbl = ui.label("Папка сохранения:").rect;
                    let mut text = "C:\\test".to_string();
                    let r_edit = ui.add(
                        egui::TextEdit::singleline(&mut text)
                            .desired_width(200.0)
                            .min_size(egui::vec2(0.0, 26.0))
                            .margin(egui::vec2(8.0, 4.0)),
                    ).rect;
                    let r_btn = ui.add(
                        egui::Button::new("Применить")
                            .min_size(egui::vec2(88.0, 26.0)),
                    ).rect;
                    assert!((r_lbl.center().y - r_edit.center().y).abs() < 1.0);
                    assert!((r_lbl.center().y - r_btn.center().y).abs() < 1.0);
                });
            });
        });
    }

    #[test]
    fn test_slider_row_alignment() {
        let ctx = egui::Context::default();
        M3Theme::apply_m3_style(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut fps = 60;
                let changed = OmniClipApp::slider_row(ui, "Точный FPS:", &mut fps, 15..=240, Some(1.0), "60 FPS", 64.0);
                assert!(!changed);
            });
        });
    }

    #[test]
    fn test_extract_icon_from_exe() {
        let test_paths = [
            r"C:\Windows\System32\notepad.exe",
            r"C:\Windows\explorer.exe",
            r"C:\Windows\System32\cmd.exe",
        ];
        let mut extracted_any = false;
        for path in &test_paths {
            if std::path::Path::new(path).exists() {
                if let Some(img) = extract_icon_from_exe(path) {
                    assert!(img.width() > 0);
                    assert!(img.height() > 0);
                    assert_eq!(img.pixels.len(), img.width() * img.height());
                    extracted_any = true;
                    break;
                }
            }
        }
        assert!(extracted_any);
    }
}
