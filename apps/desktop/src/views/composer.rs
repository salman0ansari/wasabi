//! Message composer: text input plus send affordance, gated on connection.

use gpui::prelude::*;
use gpui::{ClickEvent, Context, Window, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Position;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Icon, IconName};

use crate::state::messages;
use crate::theme;
use crate::views::root::MainWindow;

pub const COMPOSER_H: f32 = 64.0;
const COMPOSER_PILL_H: f32 = 52.0;
const COMPOSER_PILL_INSET: f32 = 12.0;
const COMPOSER_PILL_PADDING: f32 = 5.0;
const COMPOSER_INPUT_ROW_H: f32 = COMPOSER_PILL_H - COMPOSER_PILL_PADDING * 2.0;
const COMPOSER_MAX_ROWS: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnterBehavior {
    Submit,
    InsertNewline,
    AlreadyHandled,
}

fn enter_behavior(secondary: bool, shift: bool, enter_to_send: bool) -> EnterBehavior {
    if secondary || shift {
        EnterBehavior::AlreadyHandled
    } else if enter_to_send {
        EnterBehavior::Submit
    } else {
        EnterBehavior::InsertNewline
    }
}

pub fn set_text_at_end(
    input: &mut InputState,
    value: impl Into<String>,
    window: &mut Window,
    cx: &mut Context<InputState>,
) {
    let value = value.into();
    let position = end_position(&value);
    input.set_value(value, window, cx);
    if position != Position::new(0, 0) {
        input.set_cursor_position(position, window, cx);
    }
}

fn end_position(value: &str) -> Position {
    let line = value.bytes().filter(|byte| *byte == b'\n').count();
    let character = value
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .chars()
        .count();
    Position::new(line as u32, character as u32)
}

pub fn build_input(window: &mut Window, cx: &mut Context<MainWindow>) -> gpui::Entity<InputState> {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .auto_grow(1, COMPOSER_MAX_ROWS)
            // The event subscriber inserts a newline for plain Enter when the
            // user's setting disables submit-on-enter. Keeping this enabled
            // guarantees Shift+Enter is always handled natively as newline.
            .submit_on_enter(true)
            .placeholder("Type a message")
    });
    cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
        let InputEvent::PressEnter { secondary, shift } = event else {
            return;
        };
        match enter_behavior(*secondary, *shift, this.settings.enter_to_send) {
            EnterBehavior::Submit => this.send_current(window, cx),
            EnterBehavior::InsertNewline => {
                this.composer_input
                    .update(cx, |input, cx| input.insert("\n", window, cx));
            }
            EnterBehavior::AlreadyHandled => {}
        }
    })
    .detach();
    input
}

pub fn composer_bar(
    this: &mut MainWindow,
    window: &mut Window,
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
    let reply = this.active_draft.reply_to.as_ref().map(|message| {
        this.messages
            .rows
            .iter()
            .find(|row| &row.id == message)
            .map(|row| {
                let sender = if row.direction == wasabi_domain::MessageDirection::Outgoing {
                    "You".to_string()
                } else {
                    messages::sender_display(row)
                };
                (sender, compact_preview(&messages::body_text(row)))
            })
            .unwrap_or_else(|| {
                (
                    "Original message".to_string(),
                    "Preview unavailable in this history window".to_string(),
                )
            })
    });
    let editing = this.active_draft.edit_target.as_ref().map(|message| {
        this.messages
            .rows
            .iter()
            .find(|row| &row.id == message)
            .map(|row| compact_preview(&messages::body_text(row)))
            .unwrap_or_else(|| "Original text is outside this history window".to_string())
    });
    let editing_in_flight = selected.as_ref().is_some_and(|chat| {
        this.active_draft
            .edit_target
            .as_ref()
            .is_some_and(|message| {
                this.editing_messages
                    .contains(&(chat.clone(), message.as_str().to_string()))
            })
    });
    let can_compose =
        session_can_send && selected.is_some() && !staging && !sending && !editing_in_flight;
    let has_payload =
        !this.composer_input.read(cx).value().trim().is_empty() || attachment.is_some();
    let can_submit = can_compose && has_payload;
    let send_label = if sending {
        "Sending…"
    } else if editing_in_flight {
        "Saving…"
    } else if editing.is_some() && can_compose {
        "Save edit"
    } else if can_compose {
        "Send message"
    } else if !session_can_send {
        "Connect to send"
    } else {
        "Select a chat"
    };

    let attach = {
        let enabled = can_compose && attachment.is_none() && editing.is_none();
        let mut button = Button::new("attach-button")
            .icon(IconName::File)
            .ghost()
            .tooltip("Attach a file")
            .disabled(!enabled)
            .size(px(theme::ACTION_SIZE))
            .rounded_full()
            .text_color(theme::text_secondary())
            .when(enabled, |button| {
                button.hover(|style| style.bg(theme::row_hover()))
            })
            .when(!enabled, |button| button.opacity(0.4));
        if enabled {
            button = button.on_click(
                cx.listener(|this, _: &ClickEvent, _window, cx| this.choose_attachment(cx)),
            );
        }
        button
    };

    let send = {
        let busy = sending || editing_in_flight;
        let show_action = has_payload || busy;
        let actionable = can_submit && !busy;
        let (bg, fg) = if actionable || busy {
            (theme::action_surface(), theme::action_content())
        } else {
            (theme::chip_idle(), theme::text_secondary())
        };
        if show_action {
            let mut button = Button::new("send-button")
                .icon(IconName::ArrowUp)
                .ghost()
                .tooltip(send_label)
                .disabled(!actionable)
                .size(px(theme::ACTION_SIZE))
                .rounded_full()
                .bg(bg)
                .text_color(fg)
                .when(actionable, |button| {
                    button.hover(|style| style.opacity(0.88))
                })
                .when(busy, |button| button.opacity(0.68))
                .when(!actionable && !busy, |button| button.opacity(0.45));
            if actionable {
                button = button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.send_current(window, cx);
                }));
            }
            button.into_any_element()
        } else {
            // Voice notes are not implemented yet. Reserve the measured trailing
            // action slot without advertising a control that cannot work.
            gpui::div()
                .size(px(theme::ACTION_SIZE))
                .flex_shrink_0()
                .into_any_element()
        }
    };

    let bar = gpui::div()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .min_h(px(COMPOSER_H))
        .px(px(COMPOSER_PILL_INSET))
        .pb(px(COMPOSER_PILL_INSET))
        .bg(theme::surface_elevated());

    let mut pill = gpui::div()
        .flex()
        .flex_col()
        .gap(px(COMPOSER_PILL_PADDING))
        .min_h(px(COMPOSER_PILL_H))
        .p(px(COMPOSER_PILL_PADDING))
        .rounded(px(theme::RADIUS_COMPOSER))
        .bg(theme::composer_surface())
        .shadow(theme::composer_shadow());

    if let Some((sender, preview)) = reply {
        pill = pill.child(reply_preview(sender, preview, cx));
    }
    if let Some(preview) = editing {
        pill = pill.child(edit_preview(preview, cx));
    }

    if staging {
        pill = pill.child(attachment_preview(
            "Preparing attachment…".to_string(),
            "Validating and copying into wasabi".to_string(),
            None,
            cx,
        ));
    } else if let (Some(chat), Some(attachment)) = (selected.clone(), attachment.clone()) {
        let detail = format!(
            "{} · {}",
            attachment_kind_label(attachment.kind),
            format_bytes(attachment.bytes_total)
        );
        pill = pill.child(attachment_preview(
            attachment.display_name,
            detail,
            (!sending).then_some(chat),
            cx,
        ));
    }

    pill = pill.child(
        gpui::div()
            .flex()
            .items_end()
            .gap(px(4.0))
            .min_h(px(COMPOSER_INPUT_ROW_H))
            .child(attach)
            .child(
                gpui::div()
                    .size(px(theme::ACTION_SIZE))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(super::emoji::picker_button(this, can_compose, window, cx)),
            )
            .child(
                Input::new(&this.composer_input)
                    .cleanable(false)
                    .disabled(!can_compose)
                    .appearance(false)
                    .bordered(false)
                    .focus_bordered(false)
                    .flex_1()
                    .min_w(px(0.0))
                    .min_h(px(COMPOSER_INPUT_ROW_H))
                    .px(px(6.0))
                    .py(px(10.0))
                    .text_size(px(theme::scaled_text(
                        theme::TEXT_COMPOSER,
                        this.settings.text_scale,
                    ))),
            )
            .child(send),
    );

    if let Some(err) = this.send_error.clone() {
        pill = pill.child(
            gpui::div()
                .px(px(6.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::danger())
                .whitespace_nowrap()
                .child(err),
        );
    }
    bar.child(pill)
}

fn edit_preview(preview: String, cx: &mut Context<MainWindow>) -> gpui::Div {
    gpui::div()
        .min_h(px(48.0))
        .rounded(px(theme::RADIUS_SM))
        .border_l_2()
        .border_color(theme::accent())
        .bg(theme::bubble_overlay())
        .px(px(10.0))
        .py(px(6.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            gpui::div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::accent_text())
                        .child("Edit message"),
                )
                .child(
                    gpui::div()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .child(preview),
                ),
        )
        .child(
            gpui::div()
                .id("cancel-edit")
                .cursor_pointer()
                .rounded_full()
                .px(px(7.0))
                .py(px(3.0))
                .text_color(theme::text_secondary())
                .hover(|style| style.bg(theme::surface()).text_color(theme::text_primary()))
                .on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.cancel_edit(window, cx)),
                )
                .child("×"),
        )
}

fn reply_preview(sender: String, preview: String, cx: &mut Context<MainWindow>) -> gpui::Div {
    gpui::div()
        .min_h(px(48.0))
        .rounded(px(theme::RADIUS_SM))
        .border_l_2()
        .border_color(theme::accent())
        .bg(theme::bubble_overlay())
        .px(px(10.0))
        .py(px(6.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            gpui::div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::accent_text())
                        .child(sender),
                )
                .child(
                    gpui::div()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .child(preview),
                ),
        )
        .child(
            gpui::div()
                .id("cancel-reply")
                .cursor_pointer()
                .rounded_full()
                .px(px(7.0))
                .py(px(3.0))
                .text_color(theme::text_secondary())
                .hover(|style| style.bg(theme::surface()).text_color(theme::text_primary()))
                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| this.cancel_reply(cx)))
                .child("×"),
        )
}

fn compact_preview(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let preview = chars.by_ref().take(120).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn attachment_preview(
    name: String,
    detail: String,
    removable_chat: Option<String>,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let mut row = gpui::div()
        .h(px(48.0))
        .rounded(px(theme::RADIUS_SM))
        .bg(theme::bubble_overlay())
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

    #[test]
    fn reply_preview_normalizes_lines_and_respects_unicode_boundaries() {
        let source = format!("first\n{} tail", "界".repeat(130));
        let preview = compact_preview(&source);
        assert!(!preview.contains('\n'));
        assert!(preview.ends_with('…'));
        assert!(preview.is_char_boundary(preview.len()));
    }

    #[test]
    fn enter_policy_matches_desktop_messaging_conventions() {
        assert_eq!(enter_behavior(false, false, true), EnterBehavior::Submit);
        assert_eq!(
            enter_behavior(false, false, false),
            EnterBehavior::InsertNewline
        );
        assert_eq!(
            enter_behavior(false, true, true),
            EnterBehavior::AlreadyHandled
        );
        assert_eq!(
            enter_behavior(false, true, false),
            EnterBehavior::AlreadyHandled
        );
    }

    #[test]
    fn restored_multilingual_drafts_place_the_cursor_at_the_true_end() {
        assert_eq!(end_position("hello"), Position::new(0, 5));
        assert_eq!(end_position("hello\n界🎉"), Position::new(1, 2));
        assert_eq!(end_position("trailing\n"), Position::new(1, 0));
    }
}
