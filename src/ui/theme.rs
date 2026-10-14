use windows::Win32::Foundation::COLORREF;
use egui::Color32;

/// Material Design 3 (M3) color palette tokens for Dark Theme.
/// Specified in specs/08_UI_UX_OVERLAY.md.
pub struct M3Theme;

impl M3Theme {
    /// Creates a Win32 COLORREF value (0x00BBGGRR) from RGB components.
    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        // GDI encodes colors as 0x00BBGGRR in little-endian COLORREF structure
        COLORREF(r as u32 | ((g as u32) << 8) | ((b as u32) << 16))
    }

    // --- Core M3 Dark Theme Color Tokens ---

    /// Primary accent color (#ADC6FF) for buttons, active toggles, and highlights.
    pub const PRIMARY: COLORREF = Self::rgb(173, 198, 255);

    /// Text/icon color placed on top of Primary (#002E69).
    pub const ON_PRIMARY: COLORREF = Self::rgb(0, 46, 105);

    /// Container background for active recording states (#264483).
    pub const PRIMARY_CONTAINER: COLORREF = Self::rgb(38, 68, 131);

    /// Text color on top of Primary Container (#D8E2FF).
    pub const ON_PRIMARY_CONTAINER: COLORREF = Self::rgb(216, 226, 255);

    /// Deep background surface (#111318).
    pub const SURFACE: COLORREF = Self::rgb(17, 19, 24);

    /// Primary surface card container (#1E2025).
    pub const SURFACE_CONTAINER: COLORREF = Self::rgb(30, 32, 37);

    /// Elevated surface container for active cards and popups (#282A2F).
    pub const SURFACE_CONTAINER_HIGH: COLORREF = Self::rgb(40, 42, 47);

    /// Highest elevated container for hovers and dividers (#33353A).
    pub const SURFACE_CONTAINER_HIGHEST: COLORREF = Self::rgb(51, 53, 58);

    /// Outline color for card borders and inputs (#8E9099).
    pub const OUTLINE: COLORREF = Self::rgb(142, 144, 153);

    /// Subtle outline variant for dividers (#44474E).
    pub const OUTLINE_VARIANT: COLORREF = Self::rgb(68, 71, 78);

    /// Text primary color (#E2E2E9).
    pub const ON_SURFACE: COLORREF = Self::rgb(226, 226, 233);

    /// Text secondary/variant color (#C4C6D0).
    pub const ON_SURFACE_VARIANT: COLORREF = Self::rgb(196, 198, 208);

    /// Error accent color for warnings and errors (#FFB4AB).
    pub const ERROR: COLORREF = Self::rgb(255, 180, 171);

    /// Active pulsing green indicator for recording status (#4CAF50).
    pub const STATUS_ACTIVE_GREEN: COLORREF = Self::rgb(76, 175, 80);

    /// Active red indicator for manual recording (#FF5252).
    pub const STATUS_ACTIVE_RED: COLORREF = Self::rgb(255, 82, 82);

    /// Secondary accent color (#BCC7E0).
    pub const SECONDARY: COLORREF = Self::rgb(188, 199, 224);

    /// Secondary container background (#3E4759).
    pub const SECONDARY_CONTAINER: COLORREF = Self::rgb(62, 71, 89);

    /// Deepest background level (#0C0E13).
    pub const SURFACE_CONTAINER_LOW: COLORREF = Self::rgb(23, 25, 30);

    // --- egui Color32 Tokens ---

    /// Primary accent color (#ADC6FF) for buttons, active toggles, and highlights.
    pub const EGUI_PRIMARY: Color32 = Color32::from_rgb(173, 198, 255);

    /// Text/icon color placed on top of Primary (#002E69).
    pub const EGUI_ON_PRIMARY: Color32 = Color32::from_rgb(0, 46, 105);

    /// Container background for active recording states (#264483).
    pub const EGUI_PRIMARY_CONTAINER: Color32 = Color32::from_rgb(38, 68, 131);

    /// Text color on top of Primary Container (#D8E2FF).
    pub const EGUI_ON_PRIMARY_CONTAINER: Color32 = Color32::from_rgb(216, 226, 255);

    /// Secondary accent color (#BCC7E0).
    pub const EGUI_SECONDARY: Color32 = Color32::from_rgb(188, 199, 224);

    /// Secondary container background (#3E4759).
    pub const EGUI_SECONDARY_CONTAINER: Color32 = Color32::from_rgb(62, 71, 89);

    /// Text color on top of Secondary Container (#DDE3F8).
    pub const EGUI_ON_SECONDARY_CONTAINER: Color32 = Color32::from_rgb(221, 227, 248);

    /// Deep background surface (#111318).
    pub const EGUI_SURFACE: Color32 = Color32::from_rgb(17, 19, 24);

    /// Semi-transparent scrim backdrop (alpha ~85%).
    pub const EGUI_SCRIM: Color32 = Color32::from_rgba_premultiplied(10, 11, 15, 215);

    /// Lowest container level (#17191E).
    pub const EGUI_SURFACE_CONTAINER_LOW: Color32 = Color32::from_rgb(23, 25, 30);

    /// Primary surface card container (#1E2025).
    pub const EGUI_SURFACE_CONTAINER: Color32 = Color32::from_rgb(30, 32, 37);

    /// Elevated surface container for active cards and popups (#282A2F).
    pub const EGUI_SURFACE_CONTAINER_HIGH: Color32 = Color32::from_rgb(40, 42, 47);

    /// Highest elevated container for hovers and dividers (#33353A).
    pub const EGUI_SURFACE_CONTAINER_HIGHEST: Color32 = Color32::from_rgb(51, 53, 58);

    /// Outline color for card borders and inputs (#8E9099).
    pub const EGUI_OUTLINE: Color32 = Color32::from_rgb(142, 144, 153);

    /// Subtle outline variant for dividers (#44474E).
    pub const EGUI_OUTLINE_VARIANT: Color32 = Color32::from_rgb(68, 71, 78);

    /// Text primary color (#E2E2E9).
    pub const EGUI_ON_SURFACE: Color32 = Color32::from_rgb(226, 226, 233);

    /// Text secondary/variant color (#C4C6D0).
    pub const EGUI_ON_SURFACE_VARIANT: Color32 = Color32::from_rgb(196, 198, 208);

    /// Active green indicator (#4CAF50).
    pub const EGUI_STATUS_GREEN: Color32 = Color32::from_rgb(76, 175, 80);

    /// Active red indicator (#FF5252).
    pub const EGUI_STATUS_RED: Color32 = Color32::from_rgb(255, 82, 82);

    // --- Shape Corner Radii (pixels) ---

    /// Extra small shape radius (e.g. tooltips, badges).
    pub const RADIUS_XS: i32 = 4;
    /// Small shape radius (e.g. chips, filter buttons, sliders).
    pub const RADIUS_SM: i32 = 8;
    /// Medium shape radius (e.g. dropdowns, context menus).
    pub const RADIUS_MD: i32 = 12;
    /// Large shape radius for main Hub cards.
    pub const RADIUS_LG: i32 = 16;
    /// Extra large shape radius for snackbar toast notifications.
    pub const RADIUS_XL: i32 = 24;
    /// Full pill shape radius for action buttons.
    pub const RADIUS_PILL: i32 = 999;

    pub const RADIUS_XS_U8: u8 = 4;
    pub const RADIUS_SM_U8: u8 = 8;
    pub const RADIUS_MD_U8: u8 = 12;
    pub const RADIUS_LG_U8: u8 = 16;
    pub const RADIUS_XL_U8: u8 = 24;
    pub const RADIUS_PILL_U8: u8 = 255;

    /// Configures the egui Context with bundled Montserrat and Segoe MDL2 icon typography.
    /// Both font files are embedded directly into the binary at compile time via include_bytes!
    pub fn setup_fonts(ctx: &egui::Context) {
        static FONT_MONTSERRAT: &[u8] = include_bytes!("../../assets/fonts/Montserrat-Medium.ttf");
        static FONT_SEGOE_MDL2: &[u8] = include_bytes!("../../assets/fonts/segmdl2.ttf");

        let mut fonts = egui::FontDefinitions::default();

        // 1. Primary UI Typography: Montserrat Medium
        fonts.font_data.insert(
            "Montserrat".to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(FONT_MONTSERRAT)),
        );
        if let Some(prop) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            prop.insert(0, "Montserrat".to_owned());
        }

        // 2. Vector Icon Typography: Segoe MDL2 Assets with optical alignment tweak
        let icon_tweak = egui::FontTweak {
            scale: 0.88,
            y_offset_factor: 0.14,
            ..Default::default()
        };
        fonts.font_data.insert(
            "SegoeMDL2".to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(FONT_SEGOE_MDL2).tweak(icon_tweak)),
        );
        if let Some(prop) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            prop.push("SegoeMDL2".to_owned());
        }

        ctx.set_fonts(fonts);
    }

    /// Applies Material Design 3 style guidelines to the egui Context.
    pub fn apply_m3_style(ctx: &egui::Context) {
        let mut style = (*ctx.style()).clone();

        // Material 3 Typography Scale mapped to egui TextStyles
        style.text_styles = [
            (
                egui::TextStyle::Heading,
                egui::FontId::new(20.0, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Body,
                egui::FontId::new(13.5, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Button,
                egui::FontId::new(13.0, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Small,
                egui::FontId::new(11.0, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Monospace,
                egui::FontId::new(12.0, egui::FontFamily::Monospace),
            ),
        ]
        .into();

        // M3 Dark Palette Visuals
        let mut visuals = egui::Visuals::dark();
        visuals.dark_mode = true;
        visuals.window_fill = Self::EGUI_SURFACE;
        visuals.panel_fill = Self::EGUI_SURFACE;
        visuals.faint_bg_color = Self::EGUI_SURFACE_CONTAINER_LOW;
        visuals.extreme_bg_color = Self::EGUI_SURFACE_CONTAINER_LOW;
        visuals.override_text_color = Some(Self::EGUI_ON_SURFACE);

        // Widget styling: non-interactive
        visuals.widgets.noninteractive.bg_fill = Self::EGUI_SURFACE_CONTAINER;
        visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, Self::EGUI_ON_SURFACE);
        visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(Self::RADIUS_SM_U8);

        // Inactive widgets (buttons, inputs)
        visuals.widgets.inactive.bg_fill = Self::EGUI_SURFACE_CONTAINER_HIGH;
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, Self::EGUI_ON_SURFACE);
        visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(Self::RADIUS_SM_U8);

        // Hovered widgets
        visuals.widgets.hovered.bg_fill = Self::EGUI_SURFACE_CONTAINER_HIGHEST;
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, Self::EGUI_PRIMARY);
        visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(Self::RADIUS_SM_U8);

        // Active / Pressed widgets
        visuals.widgets.active.bg_fill = Self::EGUI_PRIMARY_CONTAINER;
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, Self::EGUI_ON_PRIMARY_CONTAINER);
        visuals.widgets.active.corner_radius = egui::CornerRadius::same(Self::RADIUS_SM_U8);

        // Selection
        visuals.selection.bg_fill = Self::EGUI_PRIMARY_CONTAINER;
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, Self::EGUI_PRIMARY);

        style.visuals = visuals;
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 5.0);

        ctx.set_style(style);
    }
}

/// Material Design & Windows Fluent MDL2 icon glyph constants.
pub struct M3Icon;

impl M3Icon {
    /// ChromeClose window close icon (U+E8BB).
    pub const CLOSE: &'static str = "\u{E8BB}";
    /// Lightning bolt / Hub dashboard icon (U+E7E6).
    pub const LIGHTNING: &'static str = "\u{E7E6}";
    /// Video camera capture icon (U+E714).
    pub const VIDEO: &'static str = "\u{E714}";
    /// Stopwatch / Buffer duration icon (U+E916).
    pub const STOPWATCH: &'static str = "\u{E916}";
    /// Audio speaker / volume icon (U+E767).
    pub const AUDIO: &'static str = "\u{E767}";
    /// Microphone icon (U+E720).
    pub const MIC: &'static str = "\u{E720}";
    /// Keyboard shortcut icon (U+E765).
    pub const KEYBOARD: &'static str = "\u{E765}";
    /// File folder icon (U+E838).
    pub const FOLDER: &'static str = "\u{E838}";
    /// Disk save icon (U+E74E).
    pub const SAVE: &'static str = "\u{E74E}";
    /// Play recording icon (U+E768).
    pub const PLAY: &'static str = "\u{E768}";
    /// Stop recording icon (U+E71A).
    pub const STOP: &'static str = "\u{E71A}";
    /// Checkmark success icon (U+E73E).
    pub const CHECK: &'static str = "\u{E73E}";
    /// Open in Explorer icon (U+E8DA).
    pub const OPEN_EXTERNAL: &'static str = "\u{E8DA}";
    /// Refresh icon (U+E72C).
    pub const REFRESH: &'static str = "\u{E72C}";
    /// Equalizer / Audio format & tracks tuning icon (U+E9E9).
    pub const EQUALIZER: &'static str = "\u{E9E9}";
    /// Sliders / Mixer tuning icon (U+E71D).
    pub const SLIDERS: &'static str = "\u{E71D}";
    /// Pause icon (U+E769).
    pub const PAUSE: &'static str = "\u{E769}";
    /// Mute audio icon (U+E74F).
    pub const MUTE: &'static str = "\u{E74F}";
    /// Settings gear icon (U+E713).
    pub const SETTINGS: &'static str = "\u{E713}";
    /// Globe / Language & Locale icon (U+E774).
    pub const GLOBE: &'static str = "\u{E774}";
    /// Photo camera / Screenshot icon (U+E722).
    pub const CAMERA: &'static str = "\u{E722}";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_m3_rgb_encoding() {
        // Red: R=255, G=0, B=0 -> 0x000000FF
        assert_eq!(M3Theme::rgb(255, 0, 0).0, 0x000000FF);
        // Green: R=0, G=255, B=0 -> 0x0000FF00
        assert_eq!(M3Theme::rgb(0, 255, 0).0, 0x0000FF00);
        // Blue: R=0, G=0, B=255 -> 0x00FF0000
        assert_eq!(M3Theme::rgb(0, 0, 255).0, 0x00FF0000);
        // White: 0x00FFFFFF
        assert_eq!(M3Theme::rgb(255, 255, 255).0, 0x00FFFFFF);

        let ctx = egui::Context::default();
        M3Theme::setup_fonts(&ctx);
        M3Theme::apply_m3_style(&ctx);
        let _ = ctx.run(Default::default(), |_| {});

        // Test glyph constants
        assert_eq!(M3Icon::CLOSE, "\u{E8BB}");
        assert_eq!(M3Icon::LIGHTNING, "\u{E7E6}");
        assert_eq!(M3Icon::VIDEO, "\u{E714}");
        assert_eq!(M3Icon::STOPWATCH, "\u{E916}");
        assert_eq!(M3Icon::AUDIO, "\u{E767}");
        assert_eq!(M3Icon::MIC, "\u{E720}");
        assert_eq!(M3Icon::KEYBOARD, "\u{E765}");
        assert_eq!(M3Icon::FOLDER, "\u{E838}");
        assert_eq!(M3Icon::EQUALIZER, "\u{E9E9}");
        assert_eq!(M3Icon::SLIDERS, "\u{E71D}");
        assert_eq!(M3Icon::PAUSE, "\u{E769}");
        assert_eq!(M3Icon::MUTE, "\u{E74F}");
        assert_eq!(M3Icon::SETTINGS, "\u{E713}");
    }
}
