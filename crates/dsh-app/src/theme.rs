use gpui::{rgb, Rgba};

pub const SIDEBAR_WIDTH: f32 = 256.0;
pub const CONTEXT_PANE_WIDTH: f32 = 420.0;
pub const HEADER_HEIGHT: f32 = 44.0;
pub const STATUS_HEIGHT: f32 = 24.0;
pub const TERMINAL_DOCK_HEIGHT: f32 = 220.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub background: Rgba,
    pub surface: Rgba,
    pub raised: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub faint: Rgba,
    pub accent: Rgba,
    pub success: Rgba,
    pub warning: Rgba,
    pub danger: Rgba,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            background: rgb(0x060606),
            surface: rgb(0x0d0d0d),
            raised: rgb(0x101014),
            border: rgb(0x232326),
            text: rgb(0xebebeb),
            muted: rgb(0xa1a1a5),
            faint: rgb(0x75757a),
            accent: rgb(0xff8a3d),
            success: rgb(0x35d59b),
            warning: rgb(0xffcc66),
            danger: rgb(0xff6b6b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comet_layout_constants_use_the_adapted_workbench_grid() {
        assert_eq!(SIDEBAR_WIDTH, 256.0);
        assert_eq!(CONTEXT_PANE_WIDTH, 420.0);
        assert_eq!(HEADER_HEIGHT, 44.0);
        assert_eq!(STATUS_HEIGHT, 24.0);
    }

    #[test]
    fn dark_theme_uses_neutral_comet_surfaces_and_one_restrained_accent() {
        let theme = Theme::dark();
        assert_eq!(theme.background, rgb(0x060606));
        assert_eq!(theme.surface, rgb(0x0d0d0d));
        assert_eq!(theme.raised, rgb(0x101014));
        assert_eq!(theme.accent, rgb(0xff8a3d));
        assert_eq!(theme.success, rgb(0x35d59b));
        assert_eq!(theme.danger, rgb(0xff6b6b));
    }
}
