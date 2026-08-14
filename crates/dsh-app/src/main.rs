mod changes;
mod icons;
mod input;
mod settings;
mod terminal;
mod theme;
mod workspace;

use gpui::{
    actions, point, px, size, App, AppContext, Application, Bounds, KeyBinding, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowOptions,
};
use input::{
    Backspace, Copy, Cut, Delete, End, Home, Left, Paste, Right, SelectAll, SelectLeft, SelectRight,
};
use std::borrow::Cow;
use workspace::{
    CancelTurn, FocusTerminal, NewSession, NextHarness, NextSpace, PreviousHarness, PreviousSpace,
    RenameSession, RunTerminalCommand, SaveCredential, SearchSessions, Submit, ToggleContext,
    ToggleSidebar, ToggleTerminal, Workspace,
};

const WINDOW_WIDTH: f32 = 1320.0;
const WINDOW_HEIGHT: f32 = 880.0;
const WINDOW_MIN_WIDTH: f32 = 900.0;
const WINDOW_MIN_HEIGHT: f32 = 600.0;
actions!(dsh_app, [Quit]);

static FONT_GEIST: &[u8] = include_bytes!("../assets/fonts/Geist.ttf");
static FONT_GEIST_MEDIUM: &[u8] = include_bytes!("../assets/fonts/Geist-Medium.ttf");
static FONT_GEIST_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Geist-SemiBold.ttf");
static FONT_GEIST_BOLD: &[u8] = include_bytes!("../assets/fonts/Geist-Bold.ttf");
static FONT_GEIST_MONO: &[u8] = include_bytes!("../assets/fonts/GeistMono.ttf");

fn register_fonts(cx: &mut App) {
    let fonts = vec![
        Cow::Borrowed(FONT_GEIST),
        Cow::Borrowed(FONT_GEIST_MEDIUM),
        Cow::Borrowed(FONT_GEIST_SEMIBOLD),
        Cow::Borrowed(FONT_GEIST_BOLD),
        Cow::Borrowed(FONT_GEIST_MONO),
    ];
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        eprintln!("failed to register Geist fonts: {error}");
    }
}

fn main() {
    Application::new()
        .with_assets(icons::Assets)
        .run(|cx: &mut App| {
            register_fonts(cx);
            cx.bind_keys([
                KeyBinding::new("enter", Submit, Some("ChatInput")),
                KeyBinding::new("enter", SearchSessions, Some("SearchInput")),
                KeyBinding::new("enter", RenameSession, Some("RenameInput")),
                KeyBinding::new("enter", SaveCredential, Some("ApiKeyInput")),
                KeyBinding::new("enter", RunTerminalCommand, Some("TerminalInput")),
                KeyBinding::new("cmd-n", NewSession, Some("Workspace")),
                KeyBinding::new("cmd-.", CancelTurn, Some("Workspace")),
                KeyBinding::new("cmd-s", ToggleSidebar, Some("Workspace")),
                KeyBinding::new("cmd-b", ToggleContext, Some("Workspace")),
                KeyBinding::new("cmd-t", ToggleTerminal, Some("Workspace")),
                KeyBinding::new("cmd-shift-t", FocusTerminal, Some("Workspace")),
                KeyBinding::new("cmd-shift-right", NextSpace, Some("Workspace")),
                KeyBinding::new("cmd-shift-left", PreviousSpace, Some("Workspace")),
                KeyBinding::new("cmd-shift-down", NextHarness, Some("Workspace")),
                KeyBinding::new("cmd-shift-up", PreviousHarness, Some("Workspace")),
                KeyBinding::new("backspace", Backspace, None),
                KeyBinding::new("delete", Delete, None),
                KeyBinding::new("left", Left, None),
                KeyBinding::new("right", Right, None),
                KeyBinding::new("shift-left", SelectLeft, None),
                KeyBinding::new("shift-right", SelectRight, None),
                KeyBinding::new("cmd-a", SelectAll, None),
                KeyBinding::new("cmd-v", Paste, None),
                KeyBinding::new("cmd-c", Copy, None),
                KeyBinding::new("cmd-x", Cut, None),
                KeyBinding::new("home", Home, None),
                KeyBinding::new("end", End, None),
                KeyBinding::new("cmd-q", Quit, None),
            ]);

            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.on_action(|_: &Quit, cx| cx.quit());

            let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        window_min_size: Some(size(px(WINDOW_MIN_WIDTH), px(WINDOW_MIN_HEIGHT))),
                        titlebar: Some(TitlebarOptions {
                            title: None,
                            appears_transparent: true,
                            traffic_light_position: Some(point(px(14.0), px(14.0))),
                        }),
                        window_background: WindowBackgroundAppearance::Blurred,
                        app_id: Some(String::from("dev.arnavdas.dsh-rs")),
                        ..Default::default()
                    },
                    |window, cx| {
                        let workspace = cx.new(Workspace::new);
                        let input = workspace.read(cx).input.clone();
                        window.focus(&input.read(cx).focus_handle(cx));
                        workspace
                    },
                )
                .unwrap();
            window
                .update(cx, |_, _, cx| {
                    cx.activate(true);
                })
                .unwrap();
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_comet_fonts_are_present() {
        assert!(FONT_GEIST.len() > 1_000);
        assert!(FONT_GEIST_MEDIUM.len() > 1_000);
        assert!(FONT_GEIST_SEMIBOLD.len() > 1_000);
        assert!(FONT_GEIST_BOLD.len() > 1_000);
        assert!(FONT_GEIST_MONO.len() > 1_000);
    }

    #[test]
    fn comet_window_geometry_matches_the_source_app() {
        assert_eq!(WINDOW_WIDTH, 1320.0);
        assert_eq!(WINDOW_HEIGHT, 880.0);
        assert_eq!(WINDOW_MIN_WIDTH, 900.0);
        assert_eq!(WINDOW_MIN_HEIGHT, 600.0);
    }
}
