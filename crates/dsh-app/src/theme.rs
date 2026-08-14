// Comet's complete token set lands incrementally; unused members are contract
// constants for the remaining exact-shell slices, not accidental dead code.
#![allow(dead_code)]

use gpui::{hsla, Hsla, SharedString};

pub const SIDEBAR_WIDTH: f32 = 256.0;
pub const CONTEXT_PANE_WIDTH: f32 = 520.0;
pub const TITLEBAR_HEIGHT: f32 = 38.0;
pub const TITLEBAR_TOP_PAD: f32 = 2.0;
pub const HEADER_HEIGHT: f32 = 44.0;
pub const STATUS_HEIGHT: f32 = 24.0;
pub const TERMINAL_DOCK_HEIGHT: f32 = 220.0;
pub const TRANSCRIPT_FADE_BAND: f32 = 24.0;
pub const BUBBLE_RADIUS: f32 = 16.0;
pub const PANEL_RADIUS: f32 = 10.0;
pub const CONTROL_RADIUS: f32 = 6.0;
pub const SPACE_XS: f32 = 4.0;
pub const SPACE_SM: f32 = 8.0;
pub const SPACE_MD: f32 = 12.0;
pub const SPACE_LG: f32 = 16.0;
pub const TITLEBAR_CLUSTER_BUTTONS_WIDTH: f32 = 24.0 * 3.0 + 2.0 * 2.0;

pub fn titlebar_cluster_start(fullscreen: bool) -> f32 {
    if fullscreen {
        12.0
    } else {
        88.0
    }
}

pub fn titlebar_spacer_width(is_macos: bool, fullscreen: bool, container_pad: f32) -> f32 {
    if !is_macos {
        return 0.0;
    }
    (titlebar_cluster_start(fullscreen) - container_pad).max(0.0)
}

pub fn cluster_buttons_start(is_macos: bool, fullscreen: bool) -> f32 {
    if is_macos {
        titlebar_cluster_start(fullscreen)
    } else {
        10.0
    }
}

pub fn cluster_clearance(is_macos: bool, fullscreen: bool, container_pad: f32) -> f32 {
    (cluster_buttons_start(is_macos, fullscreen) + TITLEBAR_CLUSTER_BUTTONS_WIDTH + SPACE_SM
        - container_pad)
        .max(0.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub background: Hsla,
    pub surface: Hsla,
    pub raised: Hsla,
    pub surface_card: Hsla,
    pub surface_dialog: Hsla,
    pub surface_overlay: Hsla,
    pub element_hover: Hsla,
    pub element_active: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub dim: Hsla,
    pub solid: Hsla,
    pub on_solid: Hsla,
    pub accent: Hsla,
    pub accent_strong: Hsla,
    pub on_accent: Hsla,
    pub success: Hsla,
    pub success_muted: Hsla,
    pub warning: Hsla,
    pub warning_muted: Hsla,
    pub danger: Hsla,
    pub danger_muted: Hsla,
    pub danger_strong: Hsla,
    pub busy: Hsla,
    pub surface_raised_hover: Hsla,
    pub band: Hsla,
    pub input_bg: Hsla,
    pub selection: Hsla,
    pub cursor: Hsla,
    pub caret: Hsla,
    pub code_text: Hsla,
    pub code_wash: Hsla,
    pub syntax_keyword: Hsla,
    pub syntax_string: Hsla,
    pub syntax_number: Hsla,
    pub diff_add: Hsla,
    pub diff_del: Hsla,
    pub diff_hunk_bg: Hsla,
    pub font_sans: SharedString,
    pub font_mono: SharedString,
}

impl Theme {
    pub fn dark() -> Self {
        let code_wash_base = oklch(0.702, 0.183, 293.541);
        Self {
            background: grey(6),
            surface: grey(13),
            raised: neutral(0.235),
            surface_card: grey(0x0e),
            surface_dialog: grey(0x10),
            surface_overlay: grey(0x16),
            element_hover: hsla(0.0, 0.0, 0.92, 0.11),
            element_active: hsla(0.0, 0.0, 0.92, 0.16),
            border: hsla(0.0, 0.0, 1.0, 0.08),
            border_strong: hsla(0.0, 0.0, 1.0, 0.14),
            text: neutral(0.922),
            muted: neutral(0.708),
            faint: neutral(0.556),
            dim: grey(0x98),
            solid: neutral(0.922),
            on_solid: grey(0x0e),
            accent: oklch(0.673, 0.182, 276.935),
            accent_strong: oklch(0.585, 0.233, 277.117),
            on_accent: neutral(0.985),
            success: oklch(0.765, 0.177, 163.223),
            success_muted: oklch(0.845, 0.143, 164.978),
            warning: oklch(0.828, 0.189, 84.429),
            warning_muted: oklch(0.924, 0.12, 95.746),
            danger: oklch(0.704, 0.191, 22.216),
            danger_muted: oklch(0.808, 0.114, 19.571),
            danger_strong: oklch(0.58, 0.16, 25.0),
            busy: oklch(0.718, 0.202, 349.761),
            surface_raised_hover: neutral(0.29),
            band: hsla(0.0, 0.0, 0.0, 0.16),
            input_bg: hsla(0.0, 0.0, 1.0, 0.03),
            selection: hsla(0.66, 0.6, 0.55, 0.35),
            cursor: hsla(0.0, 0.0, 1.0, 0.35),
            caret: hsla(0.66, 0.7, 0.7, 1.0),
            code_text: oklch(0.811, 0.111, 293.571),
            code_wash: hsla(code_wash_base.h, code_wash_base.s, code_wash_base.l, 0.12),
            syntax_keyword: oklch(0.709, 0.129, 20.0),
            syntax_string: oklch(0.77, 0.11, 168.0),
            syntax_number: oklch(0.78, 0.12, 80.0),
            diff_add: oklch(0.765, 0.177, 163.223),
            diff_del: oklch(0.704, 0.191, 22.216),
            diff_hunk_bg: hsla(0.6, 0.35, 0.6, 0.05),
            font_sans: SharedString::from("Geist"),
            font_mono: SharedString::from("Geist Mono"),
        }
    }
}

fn grey(value: u8) -> Hsla {
    hsla(0.0, 0.0, value as f32 / 255.0, 1.0)
}

fn neutral(lightness: f32) -> Hsla {
    let [r, g, b] = oklch_to_srgb(lightness, 0.0, 0.0);
    let (h, s, l) = rgb_to_hsl(r, g, b);
    hsla(h, s, l, 1.0)
}

fn oklch(l: f32, c: f32, h_deg: f32) -> Hsla {
    let [r, g, b] = oklch_to_srgb(l, c, h_deg);
    let (h, s, l) = rgb_to_hsl(r, g, b);
    hsla(h, s, l, 1.0)
}

fn oklch_to_srgb(l: f32, c: f32, h_deg: f32) -> [f32; 3] {
    let h = h_deg.to_radians();
    let a = c * h.cos();
    let b = c * h.sin();
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        gamma_encode(4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_93 * s3),
        gamma_encode(-1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_4 * s3),
        gamma_encode(-0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3),
    ]
}

fn gamma_encode(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let delta = max - min;
    if delta < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 {
        delta / (2.0 - max - min)
    } else {
        delta / (max + min)
    };
    let h = if (max - r).abs() < f32::EPSILON {
        ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() < f32::EPSILON {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    } / 6.0;
    (h, s, l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comet_layout_constants_match_the_source_shell() {
        assert_eq!(SIDEBAR_WIDTH, 256.0);
        assert_eq!(CONTEXT_PANE_WIDTH, 520.0);
        assert_eq!(TITLEBAR_HEIGHT, 38.0);
        assert_eq!(HEADER_HEIGHT, 44.0);
        assert_eq!(STATUS_HEIGHT, 24.0);
        assert_eq!(TRANSCRIPT_FADE_BAND, 24.0);
        assert_eq!(BUBBLE_RADIUS, 16.0);
        assert_eq!(PANEL_RADIUS, 10.0);
        assert_eq!(CONTROL_RADIUS, 6.0);
        assert_eq!(SPACE_XS, 4.0);
        assert_eq!(SPACE_SM, 8.0);
        assert_eq!(SPACE_MD, 12.0);
        assert_eq!(SPACE_LG, 16.0);
    }

    #[test]
    fn dark_theme_matches_comet_paint_tokens() {
        let theme = Theme::dark();
        assert_eq!(theme.background, grey(6));
        assert_eq!(theme.surface, grey(13));
        assert_eq!(theme.surface_card, grey(0x0e));
        assert_eq!(theme.surface_dialog, grey(0x10));
        assert_eq!(theme.surface_overlay, grey(0x16));
        assert_eq!(theme.element_hover, hsla(0.0, 0.0, 0.92, 0.11));
        assert_eq!(theme.element_active, hsla(0.0, 0.0, 0.92, 0.16));
        assert_eq!(theme.border, hsla(0.0, 0.0, 1.0, 0.08));
        assert_eq!(theme.border_strong, hsla(0.0, 0.0, 1.0, 0.14));
        assert_eq!(theme.accent, oklch(0.673, 0.182, 276.935));
        assert_eq!(theme.warning, oklch(0.828, 0.189, 84.429));
        assert_eq!(theme.danger, oklch(0.704, 0.191, 22.216));
        assert_eq!(theme.success, oklch(0.765, 0.177, 163.223));
        assert_eq!(theme.font_sans, "Geist");
        assert_eq!(theme.font_mono, "Geist Mono");
    }

    #[test]
    fn titlebar_clearance_matches_comet_window_controls() {
        assert_eq!(titlebar_cluster_start(false), 88.0);
        assert_eq!(titlebar_cluster_start(true), 12.0);
        assert_eq!(titlebar_spacer_width(true, false, 12.0), 76.0);
        assert_eq!(titlebar_spacer_width(true, true, 12.0), 0.0);
        assert_eq!(titlebar_spacer_width(false, false, 12.0), 0.0);
        assert_eq!(cluster_buttons_start(true, false), 88.0);
        assert_eq!(cluster_buttons_start(false, false), 10.0);
        assert_eq!(cluster_clearance(true, false, 16.0), 156.0);
    }
}
