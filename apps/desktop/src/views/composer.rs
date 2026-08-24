//! Message composer: text input plus send affordance, gated on connection.

use gpui::prelude::*;
use gpui::{ClickEvent, Context, Window, px};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Icon, IconName};
use gpui_component::button::{Button, ButtonVariants as _};

use crate::theme;
use crate::views::root::MainWindow;

pub const COMPOSER_H: f32 = 64.0;

pub fn build_input(window: &mut Window, cx: &mut Context<MainWindow>) -> gpui::Entity<InputState> {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder("Type a message"));
    cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
        if matches!(
            event,
            InputEvent::PressEnter {
                secondary: false,
                shift: false,
            }
        ) && this.settings.enter_to_send
        {
            this.send_current(window, cx);
        }
    })
    .detach();
    input
}

pub fn composer_bar(
    this: &mut MainWindow,
    _window: &mut Window,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let session_can_send = this.session.can_send();
    let selected = this.chats.selected.clone();
    let staging = selected
        .as_ref()
        .is_some_and(|chat| this.attachment_staging.contains(chat));
    let sending = selected
        .as_ref()
        .is_some_and(|chat| this.attachment_sending.contains(chat));
    let attachment = selected
        .as_ref()
        .and_then(|chat| this.staged_attachments.get(chat))
        .cloned();
    let can_send = session_can_send && selected.is_some() && !staging && !sending;
    let send_label = if sending {
        "Sending…"
    } else if can_send {
        "Send"
    } else if !session_can_send {
        "Connect to send"
    } else {
        "Select a chat"
    };

    let attach = {
        let enabled = can_send && attachment.is_none();
        let mut button = Button::new("attach-button")
            .icon(IconName::File)
            .ghost()
            .tooltip("Attach a file")
            .disabled(!enabled);
        if enabled {
            button = button.on_click(cx.listener(
                |this, _: &ClickEvent, _window, cx| this.choose_attachment(cx),
            ));
        }
        button
    };

    let send = {
        let (bg, fg) = if can_send {
            (theme::accent(), theme::text_on_accent())
        } else {
            (theme::chip_idle(), theme::text_secondary())
        };
        let mut button = gpui::div()
            .id("send-button")
            .rounded(px(theme::RADIUS_MD))
            .px(px(16.0))
            .py(px(9.0))
            .bg(bg)
            .text_color(fg)
            .text_size(px(theme::scaled_text(theme::TEXT_SIZE, this.settings.text_scale)))
            .child(send_label);
        if can_send {
            button = button.cursor_pointer().on_click(cx.listener(
                |this, _: &ClickEvent, window, cx| {
                    this.send_current(window, cx);
                },
            ));
        }
        button
    };

    let mut bar = gpui::div()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .px(px(12.0))
        .py(px(8.0))
        .min_h(px(COMPOSER_H))
        .bg(theme::surface())
        .border_t_1()
        .border_color(theme::border());

    if staging {
        bar = bar.child(attachment_preview(
            "Preparing attachment…".to_string(),
            "Validating and copying into Wasabi".to_string(),
            None,
            cx,
        ));
    } else if let (Some(chat), Some(attachment)) = (selected.clone(), attachment.clone()) {
        let detail = format!(
            "{} · {}",
            attachment_kind_label(attachment.kind),
            format_bytes(attachment.bytes_total)
        );
        bar = bar.child(attachment_preview(
            attachment.display_name,
            detail,
            (!sending).then_some(chat),
            cx,
        ));
    }

    bar = bar.child(
        gpui::div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(attach)
            .child(
                Input::new(&this.composer_input)
                    .cleanable(false)
                    .disabled(!can_send),
            )
            .child(send),
    );

    if let Some(err) = this.send_error.clone() {
        bar = bar.child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::danger())
                .whitespace_nowrap()
                .child(err),
        );
    }
    bar
}

fn attachment_preview(
    name: String,
    detail: String,
    removable_chat: Option<String>,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let mut row = gpui::div()
        .h(px(48.0))
        .rounded(px(theme::RADIUS_MD))
        .border_1()
        .border_color(theme::border())
        .bg(theme::chip_idle())
        .px(px(10.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(Icon::new(IconName::File).size(px(18.0)))
        .child(
            gpui::div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE))
                        .text_color(theme::text_primary())
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(name),
                )
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .child(detail),
                ),
        );
    if let Some(chat) = removable_chat {
        row = row.child(
            gpui::div()
                .id("remove-attachment")
                .cursor_pointer()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::danger())
                .child("Remove")
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.remove_attachment(chat.clone(), cx);
                })),
        );
    }
    row
}

fn attachment_kind_label(kind: wasabi_domain::AttachmentKind) -> &'static str {
    match kind {
        wasabi_domain::AttachmentKind::Image => "Image",
        wasabi_domain::AttachmentKind::Video => "Video",
        wasabi_domain::AttachmentKind::Audio => "Audio",
        wasabi_domain::AttachmentKind::Document => "Document",
    }
}

fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_metadata_labels_are_stable_and_human_readable() {
        assert_eq!(format_bytes(42), "42 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.0 MiB");
        assert_eq!(
            attachment_kind_label(wasabi_domain::AttachmentKind::Document),
            "Document"
        );
    }
}
