use std::ops::Range;

use gpui::{
    actions, div, fill, hsla, prelude::*, px, rgb, white, App, ClipboardItem, Context, CursorStyle,
    Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable,
    GlobalElementId, IntoElement, LayoutId, MouseButton, Pixels, Point, ShapedLine, Style, TextRun,
    UTF16Selection, Window,
};
use unicode_segmentation::UnicodeSegmentation;

actions!(
    input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Paste,
        Cut,
        Copy,
        Home,
        End
    ]
);

pub struct ChatInput {
    focus_handle: FocusHandle,
    content: String,
    placeholder: &'static str,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<gpui::Bounds<Pixels>>,
}

impl ChatInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: String::new(),
            placeholder: "Message DeepSeek Harness",
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
        }
    }

    pub fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn text(&self) -> String {
        self.content.clone()
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.content.clear();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    fn replace_range(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        self.content.replace_range(range.clone(), text);
        let cursor = range.start + text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let start = self.previous_boundary(self.cursor_offset());
            self.replace_range(start..self.cursor_offset(), "", cx);
        } else {
            self.replace_range(self.selected_range.clone(), "", cx);
        }
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let end = self.next_boundary(self.cursor_offset());
            self.replace_range(self.cursor_offset()..end, "", cx);
        } else {
            self.replace_range(self.selected_range.clone(), "", cx);
        }
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        let target = if self.selected_range.is_empty() {
            self.previous_boundary(self.cursor_offset())
        } else {
            self.selected_range.start
        };
        self.move_to(target, cx);
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        let target = if self.selected_range.is_empty() {
            self.next_boundary(self.cursor_offset())
        } else {
            self.selected_range.end
        };
        self.move_to(target, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_range(self.selected_range.clone(), &text, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_range(self.selected_range.clone(), "", cx);
        }
    }

    fn home(&mut self, _: &crate::input::Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &crate::input::End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.content.chars().take_while(|_| false).count()
            + self
                .content
                .char_indices()
                .take_while(|(idx, _)| *idx < offset)
                .map(|(_, ch)| ch.len_utf16())
                .sum::<usize>()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }
}

impl Focusable for ChatInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for ChatInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let start = self.offset_from_utf16(range_utf16.start);
        let end = self.offset_from_utf16(range_utf16.end);
        actual_range.replace(start..end);
        Some(self.content[start..end].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.offset_to_utf16(self.selected_range.start)
                ..self.offset_to_utf16(self.selected_range.end),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .map(|range| self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end))
            .unwrap_or_else(|| self.selected_range.clone());
        self.replace_range(range, new_text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .map(|range| self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end))
            .unwrap_or_else(|| self.selected_range.clone());
        self.replace_range(range.clone(), new_text, cx);
        self.marked_range = Some(range.start..range.start + new_text.len());
        if let Some(selected) = new_selected_range_utf16 {
            let start = self.offset_from_utf16(selected.start);
            let end = self.offset_from_utf16(selected.end);
            self.selected_range = start..end;
            cx.notify();
        }
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: gpui::Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<gpui::Bounds<Pixels>> {
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        if point.y < bounds.top() || point.y > bounds.bottom() {
            return None;
        }
        let index = line.index_for_x(point.x - bounds.left())?;
        Some(self.offset_to_utf16(index))
    }
}

struct ChatTextElement {
    input: Entity<ChatInput>,
}

impl IntoElement for ChatTextElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for ChatTextElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<(ShapedLine, Option<gpui::PaintQuad>, Option<gpui::PaintQuad>)>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let display = if input.content.is_empty() {
            input.placeholder.to_string()
        } else {
            input.content.clone()
        };
        let style = window.text_style();
        let color = if input.content.is_empty() {
            hsla(0., 0., 1., 0.35)
        } else {
            style.color
        };
        let run = TextRun {
            len: display.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display.into(), font_size, &[run], None);
        let cursor_x = line.x_for_index(input.cursor_offset());
        let cursor = Some(fill(
            gpui::Bounds::new(
                Point::new(bounds.left() + cursor_x, bounds.top()),
                gpui::size(px(2.), bounds.bottom() - bounds.top()),
            ),
            rgb(0x8ee7ff),
        ));
        Some((line, cursor, None))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some((line, cursor, _)) = prepaint.take() else {
            return;
        };
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        line.paint(bounds.origin, window.line_height(), window, cx)
            .ok();
        if focus_handle.is_focused(window) {
            if let Some(cursor) = cursor {
                window.paint_quad(cursor);
            }
        }
        self.input.update(cx, |input, _| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for ChatInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("ChatInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_mouse_down(MouseButton::Left, |_, _, _| {})
            .h(px(42.))
            .w_full()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(0x191d25))
            .border_1()
            .border_color(rgb(0x303743))
            .text_size(px(14.))
            .text_color(white())
            .child(ChatTextElement { input: cx.entity() })
    }
}
