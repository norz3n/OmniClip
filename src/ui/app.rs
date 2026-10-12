use std::path::PathBuf;
use std::time::Duration;
use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Stroke, Vec2};

use crate::platform::hotkey::HotkeyEvent;
use crate::ui::state::SharedUiState;
use crate::ui::theme::M3Theme;

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
}

impl OmniClipApp {
    /// Creates a new OmniClipApp instance with communication channels.
    pub fn new(
        state: SharedUiState,
        action_tx: Sender<UiAction>,
        hotkey_rx: Receiver<HotkeyEvent>,
        save_notify_rx: Receiver<(PathBuf, u32, f64)>,
        running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            state,
            action_tx,
            _hotkey_rx: hotkey_rx,
            save_notify_rx,
            pulse_phase: 0.0,
            running,
        }
    }
}

impl eframe::App for OmniClipApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Deep M3 Surface (#111318)
        [0.067, 0.075, 0.094, 1.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 0. Check global stop signal (Ctrl+C / console close) — close egui window cleanly
        if !self.running.load(std::sync::atomic::Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Animation phase for pulsing status indicators
        self.pulse_phase = (self.pulse_phase + 0.06) % (std::f32::consts::PI * 2.0);

        // 1. Drain replay save notifications from disk writer
        while let Ok((saved_path, duration_secs, size_mb)) = self.save_notify_rx.try_recv() {
            let mut st = self.state.write();
            st.push_toast("Повтор сохранен", saved_path, duration_secs, size_mb);
            st.is_saving_disk = false;
        }

        // 2. Check overlay visibility: if hidden, suppress rendering and enforce viewport invisibility
        if !crate::ui::is_overlay_visible() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            return;
        }

        // 3. Handle Escape key to dismiss Hub
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            crate::ui::set_overlay_visible(false);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            return;
        }

        // 4. Check and clear expired toast notifications
        self.state.write().dismiss_expired_toast();

        let state_snapshot = self.state.read().clone();

        // Material 3 Control Hub Interface
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(M3Theme::EGUI_SURFACE)
                    .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT))
                    .corner_radius(CornerRadius::same(M3Theme::RADIUS_LG_U8))
                    .inner_margin(24.0),
            )
            .show(ctx, |ui| {
                // --- Header Bar ---
                ui.horizontal(|ui| {
                    ui.heading(
                        RichText::new("OmniClip")
                            .color(M3Theme::EGUI_PRIMARY)
                            .size(26.0)
                            .strong(),
                    );
                    ui.add_space(16.0);
                    let ram_mb = state_snapshot.current_ram_bytes / (1024 * 1024);
                    ui.label(
                        RichText::new(format!(
                            "● Буфер активен  •  ОЗУ: {} MB  •  FPS: {}",
                            ram_mb, state_snapshot.target_fps
                        ))
                        .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                        .size(14.0),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let close_btn = ui.add(
                            egui::Button::new(
                                RichText::new("✕")
                                    .color(M3Theme::EGUI_ON_SURFACE)
                                    .size(16.0),
                            )
                            .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                            .corner_radius(CornerRadius::same(M3Theme::RADIUS_SM_U8)),
                        );
                        if close_btn.clicked() {
                            crate::ui::set_overlay_visible(false);
                            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                        }
                    });
                });

                // Toast notification banner if a replay was just saved
                if let Some(toast) = &state_snapshot.active_toast {
                    ui.add_space(8.0);
                    egui::Frame::NONE
                        .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                        .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_PRIMARY))
                        .corner_radius(CornerRadius::same(M3Theme::RADIUS_MD_U8))
                        .inner_margin(egui::vec2(16.0, 8.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(&toast.title)
                                        .color(M3Theme::EGUI_PRIMARY)
                                        .size(14.0)
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(format!(
                                        "{} • {}с • {:.1} МБ",
                                        toast.filename, toast.duration_secs, toast.size_mb
                                    ))
                                    .color(M3Theme::EGUI_ON_SURFACE)
                                    .size(13.0),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let reveal_btn = ui.add(
                                        egui::Button::new(
                                            RichText::new("Показать в папке")
                                                .color(M3Theme::EGUI_ON_PRIMARY_CONTAINER)
                                                .size(12.0)
                                                .strong(),
                                        )
                                        .fill(M3Theme::EGUI_PRIMARY_CONTAINER)
                                        .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8)),
                                    );
                                    if reveal_btn.clicked() {
                                        let _ = std::process::Command::new("explorer")
                                            .args(["/select,", &toast.filepath.to_string_lossy()])
                                            .spawn();
                                    }
                                });
                            });
                        });
                }

                ui.add_space(20.0);

                // --- 3 Central Cards ---
                ui.columns(3, |columns| {
                    // Column 1: Instant Replay
                    self.render_card_replay(&mut columns[0], &state_snapshot);

                    // Column 2: Manual Record
                    self.render_card_record(&mut columns[1], &state_snapshot);

                    // Column 3: Quick Settings
                    self.render_card_settings(&mut columns[2], &state_snapshot);
                });

                ui.add_space(24.0);

                // --- Bottom Status Bar ---
                ui.horizontal(|ui| {
                    // Mic chip
                    let mic_lbl = if state_snapshot.mic_enabled {
                        "🎙 Микрофон: ВКЛ (100%)"
                    } else {
                        "🎙 Микрофон: ВЫКЛ"
                    };
                    self.render_chip(ui, mic_lbl, M3Theme::EGUI_SURFACE_CONTAINER_HIGH);

                    ui.add_space(12.0);
                    // Speakers chip
                    self.render_chip(
                        ui,
                        "🔊 Динамики: 48kHz Stereo",
                        M3Theme::EGUI_SURFACE_CONTAINER_HIGH,
                    );

                    ui.add_space(12.0);
                    // Folder button chip
                    let folder_name = state_snapshot
                        .output_dir
                        .file_name()
                        .map(|f| f.to_string_lossy().to_string())
                        .unwrap_or_else(|| "OmniClip".to_string());
                    let folder_chip = ui.add(
                        egui::Button::new(
                            RichText::new(format!("📁 Папка: {}", folder_name))
                                .color(M3Theme::EGUI_ON_SURFACE)
                                .size(13.0),
                        )
                        .fill(M3Theme::EGUI_SURFACE_CONTAINER_HIGH)
                        .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8)),
                    );
                    if folder_chip.clicked() {
                        let _ = std::process::Command::new("explorer")
                            .arg(&state_snapshot.output_dir)
                            .spawn();
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new("Нажмите Esc или Alt+Z для возврата в игру")
                                .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                                .size(12.0),
                        );
                    });
                });
            });

        // Repaint at 30 Hz for animations while window is shown
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

impl OmniClipApp {
    /// Renders Card 1: Instant Replay controls.
    fn render_card_replay(&self, ui: &mut egui::Ui, state: &crate::ui::state::UiState) {
        egui::Frame::NONE
            .fill(M3Theme::EGUI_SURFACE_CONTAINER)
            .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT))
            .corner_radius(CornerRadius::same(M3Theme::RADIUS_LG_U8))
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.set_height(340.0);
                ui.label(
                    RichText::new("МГНОВЕННЫЙ ПОВТОР")
                        .color(M3Theme::EGUI_ON_SURFACE)
                        .size(16.0)
                        .strong(),
                );
                ui.label(
                    RichText::new("Фоновый буфер в оперативной памяти")
                        .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                        .size(12.0),
                );

                ui.add_space(20.0);

                // Big Save Pill Button (Alt+F10)
                let save_btn = ui.add(
                    egui::Button::new(
                        RichText::new("Сохранить (Alt+F10)")
                            .color(M3Theme::EGUI_ON_PRIMARY)
                            .size(15.0)
                            .strong(),
                    )
                    .fill(M3Theme::EGUI_PRIMARY)
                    .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                    .min_size(Vec2::new(ui.available_width(), 46.0)),
                );
                if save_btn.clicked() {
                    let _ = self.action_tx.send(UiAction::SaveReplay);
                    self.state.write().is_saving_disk = true;
                }

                ui.add_space(24.0);

                ui.label(
                    RichText::new("Длительность буфера:")
                        .color(M3Theme::EGUI_ON_SURFACE)
                        .size(13.0),
                );
                ui.add_space(6.0);

                // Duration Selector Chips
                let durations = [15, 30, 60, 120, 300];
                ui.horizontal_wrapped(|ui| {
                    for secs in durations {
                        let is_sel = state.buffer_duration_secs == secs;
                        let text = match secs {
                            15 => "15с",
                            30 => "30с",
                            60 => "60с",
                            120 => "2м",
                            300 => "5м",
                            _ => "??",
                        };

                        let bg = if is_sel {
                            M3Theme::EGUI_PRIMARY_CONTAINER
                        } else {
                            M3Theme::EGUI_SURFACE_CONTAINER_HIGH
                        };
                        let fg = if is_sel {
                            M3Theme::EGUI_ON_PRIMARY_CONTAINER
                        } else {
                            M3Theme::EGUI_ON_SURFACE
                        };

                        let btn = ui.add(
                            egui::Button::new(RichText::new(text).color(fg).size(12.0))
                                .fill(bg)
                                .corner_radius(CornerRadius::same(M3Theme::RADIUS_SM_U8))
                                .min_size(Vec2::new(42.0, 30.0)),
                        );
                        if btn.clicked() {
                            self.state.write().set_buffer_duration(secs);
                            let _ = self.action_tx.send(UiAction::SetBufferDuration(secs));
                        }
                    }
                });

                ui.add_space(16.0);
                ui.label(
                    RichText::new(format!(
                        "Оценка ОЗУ: ~{} МБ ({} сек @ {} Mbps)",
                        state.estimated_ram_mb, state.buffer_duration_secs, state.video_bitrate_mbps
                    ))
                    .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                    .size(12.0),
                );
            });
    }

    /// Renders Card 2: Manual Recording controls.
    fn render_card_record(&self, ui: &mut egui::Ui, state: &crate::ui::state::UiState) {
        egui::Frame::NONE
            .fill(M3Theme::EGUI_SURFACE_CONTAINER)
            .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT))
            .corner_radius(CornerRadius::same(M3Theme::RADIUS_LG_U8))
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.set_height(340.0);
                ui.label(
                    RichText::new("РУЧНАЯ ЗАПИСЬ")
                        .color(M3Theme::EGUI_ON_SURFACE)
                        .size(16.0)
                        .strong(),
                );
                ui.label(
                    RichText::new("Прямая запись на диск без лимита")
                        .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                        .size(12.0),
                );

                ui.add_space(20.0);

                let (btn_bg, btn_fg, btn_text) = if state.manual_record_active {
                    (
                        M3Theme::EGUI_STATUS_RED,
                        M3Theme::EGUI_SURFACE,
                        "Остановить запись",
                    )
                } else {
                    (
                        M3Theme::EGUI_SURFACE_CONTAINER_HIGH,
                        M3Theme::EGUI_PRIMARY,
                        "Начать запись (Alt+F9)",
                    )
                };

                let rec_btn = ui.add(
                    egui::Button::new(
                        RichText::new(btn_text).color(btn_fg).size(15.0).strong(),
                    )
                    .fill(btn_bg)
                    .corner_radius(CornerRadius::same(M3Theme::RADIUS_PILL_U8))
                    .min_size(Vec2::new(ui.available_width(), 46.0)),
                );
                if rec_btn.clicked() {
                    let mut st = self.state.write();
                    st.manual_record_active = !st.manual_record_active;
                    let _ = self.action_tx.send(UiAction::ToggleManualRecord);
                }

                ui.add_space(24.0);
                let status_lbl = if state.manual_record_active {
                    "● Идет непрерывная запись в MP4"
                } else {
                    "Статус: В ожидании вызова Alt+F9"
                };
                ui.label(
                    RichText::new(status_lbl)
                        .color(M3Theme::EGUI_ON_SURFACE_VARIANT)
                        .size(13.0),
                );
            });
    }

    /// Renders Card 3: Quick Settings.
    fn render_card_settings(&self, ui: &mut egui::Ui, state: &crate::ui::state::UiState) {
        egui::Frame::NONE
            .fill(M3Theme::EGUI_SURFACE_CONTAINER)
            .stroke(Stroke::new(1.0_f32, M3Theme::EGUI_OUTLINE_VARIANT))
            .corner_radius(CornerRadius::same(M3Theme::RADIUS_LG_U8))
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.set_height(340.0);
                ui.label(
                    RichText::new("НАСТРОЙКИ")
                        .color(M3Theme::EGUI_ON_SURFACE)
                        .size(16.0)
                        .strong(),
                );
                ui.add_space(10.0);

                // FPS
                ui.label(
                    RichText::new("Частота кадров (FPS):")
                        .color(M3Theme::EGUI_ON_SURFACE)
                        .size(13.0),
                );
                ui.horizontal(|ui| {
                    for fps in [60, 120] {
                        let is_sel = state.target_fps == fps;
                        let bg = if is_sel {
                            M3Theme::EGUI_PRIMARY_CONTAINER
                        } else {
                            M3Theme::EGUI_SURFACE_CONTAINER_HIGH
                        };
                        let fg = if is_sel {
                            M3Theme::EGUI_ON_PRIMARY_CONTAINER
                        } else {
                            M3Theme::EGUI_ON_SURFACE
                        };

                        let btn = ui.add(
                            egui::Button::new(
                                RichText::new(format!("{} FPS", fps))
                                    .color(fg)
                                    .size(12.0),
                            )
                            .fill(bg)
                            .corner_radius(CornerRadius::same(M3Theme::RADIUS_SM_U8))
                            .min_size(Vec2::new(70.0, 30.0)),
                        );
                        if btn.clicked() {
                            self.state.write().target_fps = fps;
                            let _ = self.action_tx.send(UiAction::SetFps(fps));
                        }
                    }
                });

                ui.add_space(14.0);

                // Bitrate
                ui.label(
                    RichText::new("Битрейт видео:")
                        .color(M3Theme::EGUI_ON_SURFACE)
                        .size(13.0),
                );
                ui.horizontal(|ui| {
                    for mbps in [15, 35, 50] {
                        let is_sel = state.video_bitrate_mbps == mbps;
                        let bg = if is_sel {
                            M3Theme::EGUI_PRIMARY_CONTAINER
                        } else {
                            M3Theme::EGUI_SURFACE_CONTAINER_HIGH
                        };
                        let fg = if is_sel {
                            M3Theme::EGUI_ON_PRIMARY_CONTAINER
                        } else {
                            M3Theme::EGUI_ON_SURFACE
                        };

                        let btn = ui.add(
                            egui::Button::new(
                                RichText::new(format!("{} Mbps", mbps))
                                    .color(fg)
                                    .size(12.0),
                            )
                            .fill(bg)
                            .corner_radius(CornerRadius::same(M3Theme::RADIUS_SM_U8))
                            .min_size(Vec2::new(64.0, 30.0)),
                        );
                        if btn.clicked() {
                            self.state.write().video_bitrate_mbps = mbps;
                            let _ = self.action_tx.send(UiAction::SetBitrate(mbps));
                        }
                    }
                });

                ui.add_space(16.0);

                // Mic toggle
                let mic_text = if state.mic_enabled {
                    "Микрофон: ВКЛЮЧЕН"
                } else {
                    "Микрофон: ОТКЛЮЧЕН"
                };
                let mic_bg = if state.mic_enabled {
                    M3Theme::EGUI_PRIMARY_CONTAINER
                } else {
                    M3Theme::EGUI_SURFACE_CONTAINER_HIGH
                };
                let mic_fg = if state.mic_enabled {
                    M3Theme::EGUI_ON_PRIMARY_CONTAINER
                } else {
                    M3Theme::EGUI_ON_SURFACE_VARIANT
                };

                let mic_btn = ui.add(
                    egui::Button::new(RichText::new(mic_text).color(mic_fg).size(13.0))
                        .fill(mic_bg)
                        .corner_radius(CornerRadius::same(M3Theme::RADIUS_SM_U8))
                        .min_size(Vec2::new(ui.available_width(), 36.0)),
                );
                if mic_btn.clicked() {
                    let mut st = self.state.write();
                    st.mic_enabled = !st.mic_enabled;
                    let _ = self.action_tx.send(UiAction::ToggleMic);
                }
            });
    }

    /// Renders a Material 3 Status Chip.
    fn render_chip(&self, ui: &mut egui::Ui, text: &str, bg: Color32) {
        ui.add(
            egui::Label::new(
                RichText::new(text)
                    .color(M3Theme::EGUI_ON_SURFACE)
                    .size(13.0),
            )
            .selectable(false),
        );
        let _ = bg; // kept for consistent style signature
    }
}

