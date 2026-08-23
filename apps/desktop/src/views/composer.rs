//! Message composer: text input plus send affordance, gated on connection.

use gpui::prelude::*;
use gpui::{ClickEvent, Context, Window, px};
use gpui_component::input::{Input, InputState};

use crate::theme;
use crate::views::root::MainWindow;

pub const COMPOSER_H: f32 = 64.0;

pub fn build_input(window: &mut Window, cx: &mut Context<MainWindow>) -> gpui::Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder("Type a message"))
}

pub fn composer_bar(
    this: &mut MainWindow,
    _window: &mut Window,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let can_send = this.session.can_send() && this.chats.selected.is_some();

    let send = {
        let (bg, fg) = if can_send {
            (theme::ACCENT, theme::TEXT_ON_ACCENT)
        } else {
            (theme::CHIP_IDLE, theme::TEXT_SECONDARY)
        };
        let mut button = gpui::div()
            .id("send-button")
            .cursor_pointer()
            .rounded(px(theme::RADIUS_MD))
            .px(px(16.0))
            .py(px(9.0))
            .bg(bg)
            .text_color(fg)
            .text_size(px(theme::TEXT_SIZE))
            .child("Send");
        if can_send {
            button = button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.send_current(window, cx);
            }));
        }
        button
    };

    let mut bar = gpui::div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .px(px(12.0))
        .h(px(COMPOSER_H))
        .bg(theme::SURFACE)
        .border_t_1()
        .border_color(theme::BORDER)
        .child(
            Input::new(&this.composer_input)
                .cleanable(false)
                .disabled(!can_send),
        )
        .child(send);

    if let Some(err) = this.send_error.clone() {
        bar = bar.child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::DANGER)
                .whitespace_nowrap()
                .child(err),
        );
    }
    bar
}
