//! Settings -> Providers: logo-backed model access.
//!
//! The layout follows Comet's Accounts page: a 24px provider mark, provider
//! section header, frosted section card, quiet status badge, and inline
//! authentication controls. DSH starts with the one provider its runtime can
//! actually use; the descriptor table keeps additional providers structural
//! rather than turning this page into a static logo gallery.

use gpui::{div, prelude::*, px, Context, Entity, IntoElement, Render, SharedString, Window};

use crate::icons::{self, icon};
use crate::input::ChatInput;
use crate::settings::widgets;
use crate::theme::Theme;

pub struct ProviderDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub mark: &'static str,
    pub endpoint: &'static str,
    pub auth_kind: &'static str,
}

pub const PROVIDERS: [ProviderDefinition; 1] = [ProviderDefinition {
    id: "deepseek",
    name: "DeepSeek",
    mark: icons::DEEPSEEK_MARK,
    endpoint: "api.deepseek.com",
    auth_kind: "API key",
}];

pub enum ProvidersEvent {
    SaveRequested(String),
    RemoveRequested,
}

pub struct ProvidersPage {
    api_key_input: Entity<ChatInput>,
    environment_override: bool,
    file_configured: bool,
    model: SharedString,
    status: Option<SharedString>,
}

impl gpui::EventEmitter<ProvidersEvent> for ProvidersPage {}

impl ProvidersPage {
    pub fn new(
        api_key_input: Entity<ChatInput>,
        environment_override: bool,
        file_configured: bool,
        model: SharedString,
    ) -> Self {
        Self {
            api_key_input,
            environment_override,
            file_configured,
            model,
            status: None,
        }
    }

    pub fn set_state(
        &mut self,
        environment_override: bool,
        file_configured: bool,
        model: SharedString,
        status: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.environment_override = environment_override;
        self.file_configured = file_configured;
        self.model = model;
        self.status = status;
        cx.notify();
    }

    fn request_save(&mut self, cx: &mut Context<Self>) {
        let key = self.api_key_input.read(cx).text();
        self.api_key_input.update(cx, |input, cx| input.clear(cx));
        cx.emit(ProvidersEvent::SaveRequested(key));
    }

    fn request_remove(&mut self, cx: &mut Context<Self>) {
        cx.emit(ProvidersEvent::RemoveRequested);
    }

    fn state_label(&self) -> &'static str {
        if self.environment_override {
            "Environment active"
        } else if self.file_configured {
            "Stored key active"
        } else {
            "Not connected"
        }
    }
}

impl Render for ProvidersPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let provider = &PROVIDERS[0];
        let state_color = if self.environment_override {
            theme.warning
        } else if self.file_configured {
            theme.success
        } else {
            theme.text_muted
        };

        let identity = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(14.0))
            .child(
                div()
                    .flex_none()
                    .size(px(36.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.wash(0.03))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(provider.mark).size(px(18.0)).text_color(theme.text)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(provider.name),
                    )
                    .child(
                        div()
                            .mt(px(3.0))
                            .text_size(px(11.5))
                            .text_color(theme.text_muted.opacity(0.65))
                            .child(SharedString::from(format!(
                                "{} · {} · {}",
                                provider.auth_kind, provider.endpoint, self.model
                            ))),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .px(px(8.0))
                    .py(px(2.0))
                    .rounded_full()
                    .border_1()
                    .border_color(state_color.opacity(0.28))
                    .bg(state_color.opacity(0.08))
                    .text_size(px(10.5))
                    .text_color(state_color.opacity(0.9))
                    .child(self.state_label()),
            );

        let mut controls = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .px(px(20.0))
            .py(px(14.0))
            .border_t_1()
            .border_color(theme.border);

        if self.environment_override {
            controls = controls
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(12.5))
                        .text_color(theme.text_muted)
                        .child(
                            "DEEPSEEK_API_KEY is provided by the launching environment. Stored \
                             credentials stay read-only while it is active.",
                        ),
                )
                .child(
                    widgets::ghost_action(&theme)
                        .id("provider-remove-environment")
                        .hover(|style| widgets::ghost_hover(&theme, style))
                        .on_click(cx.listener(|this, _, _, cx| this.request_remove(cx)))
                        .child("Remove stored key"),
                );
        } else {
            controls = controls
                .child(div().flex_1().min_w_0().child(self.api_key_input.clone()))
                .child(
                    div()
                        .id("provider-save")
                        .flex_none()
                        .px(px(16.0))
                        .py(px(7.0))
                        .rounded(px(8.0))
                        .bg(theme.accent)
                        .text_size(px(12.5))
                        .text_color(theme.on_accent)
                        .cursor_pointer()
                        .hover(|style| style.opacity(0.9))
                        .on_click(cx.listener(|this, _, _, cx| this.request_save(cx)))
                        .child("Connect"),
                )
                .child(
                    widgets::ghost_action(&theme)
                        .id("provider-remove")
                        .hover(|style| widgets::ghost_hover(&theme, style))
                        .on_click(cx.listener(|this, _, _, cx| this.request_remove(cx)))
                        .child("Remove"),
                );
        }

        div()
            .id("providers-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(widgets::page_header(
                        &theme,
                        "Providers",
                        Some(PROVIDERS.len()),
                    ))
                    .child(widgets::page_subtitle(
                        &theme,
                        "Connect model providers with credentials stored locally for this device.",
                    ))
                    .child(
                        div()
                            .mt(px(24.0))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .flex_none()
                                            .size(px(24.0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                icon(provider.mark)
                                                    .size(px(16.0))
                                                    .text_color(theme.text_muted),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(14.0))
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(theme.text)
                                            .child(provider.name),
                                    ),
                            )
                            .child(
                                widgets::section_card(&theme)
                                    .mt(px(8.0))
                                    .child(div().px(px(20.0)).py(px(14.0)).child(identity))
                                    .child(controls),
                            ),
                    )
                    .when_some(self.status.clone(), |element, status| {
                        element.child(
                            div()
                                .mt(px(16.0))
                                .text_size(px(12.0))
                                .line_height(px(18.0))
                                .text_color(theme.text_muted)
                                .child(status),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_has_complete_branding() {
        for provider in PROVIDERS {
            assert!(!provider.id.is_empty());
            assert!(!provider.name.is_empty());
            assert!(provider.mark.starts_with("icons/"));
            assert!(provider.mark.ends_with(".svg"));
            assert!(provider.endpoint.contains('.'));
            assert!(!provider.auth_kind.is_empty());
        }
    }
}
