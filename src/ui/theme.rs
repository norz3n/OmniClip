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

    // --- egui Color32 Tokens ---

    /// Primary accent color (#ADC6FF) for buttons, active toggles, and highlights.
    pub const EGUI_PRIMARY: Color32 = Color32::from_rgb(173, 198, 255);

    /// Text/icon color placed on top of Primary (#002E69).
    pub const EGUI_ON_PRIMARY: Color32 = Color32::from_rgb(0, 46, 105);

    /// Container background for active recording states (#264483).
    pub const EGUI_PRIMARY_CONTAINER: Color32 = Color32::from_rgb(38, 68, 131);

    /// Text color on top of Primary Container (#D8E2FF).
    pub const EGUI_ON_PRIMARY_CONTAINER: Color32 = Color32::from_rgb(216, 226, 255);

    /// Deep background surface (#111318).
    pub const EGUI_SURFACE: Color32 = Color32::from_rgb(17, 19, 24);

    /// Semi-transparent scrim backdrop (alpha ~85%).
    pub const EGUI_SCRIM: Color32 = Color32::from_rgba_premultiplied(10, 11, 15, 215);

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

    pub const RADIUS_SM_U8: u8 = 8;
    pub const RADIUS_MD_U8: u8 = 12;
    pub const RADIUS_LG_U8: u8 = 16;
    pub const RADIUS_PILL_U8: u8 = 255;
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
    }
}
