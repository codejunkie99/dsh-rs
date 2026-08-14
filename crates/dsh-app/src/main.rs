mod input;
mod workspace;

use gpui::{
    actions, px, size, App, AppContext, Application, Bounds, KeyBinding, SharedString,
    TitlebarOptions, WindowBounds, WindowOptions,
};
use input::{
    Backspace, Copy, Cut, Delete, End, Home, Left, Paste, Right, SelectAll, SelectLeft, SelectRight,
};
use workspace::{CancelTurn, NewSession, RenameSession, SearchSessions, Submit, Workspace};

const APP_TITLE: &str = "DeepSeek Harness RS";
actions!(dsh_app, [Quit]);

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("enter", Submit, Some("ChatInput")),
            KeyBinding::new("enter", SearchSessions, Some("SearchInput")),
            KeyBinding::new("enter", RenameSession, Some("RenameInput")),
            KeyBinding::new("cmd-n", NewSession, Some("Workspace")),
            KeyBinding::new("cmd-.", CancelTurn, Some("Workspace")),
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

        let bounds = Bounds::centered(None, size(px(1200.0), px(760.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some(SharedString::from(APP_TITLE)),
                        ..Default::default()
                    }),
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
