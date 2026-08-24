//! Conversation pane: header, date-chipped timeline of variable-height
//! bubbles, and the scroll/anchor plumbing for history paging.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{Context, px};
use gpui_component::v_virtual_list;
use gpui_component::{Icon, IconName};

use crate::state::chats;
use crate::state::messages::{self, TimelineItem};
use crate::theme;
use crate::views::root::MainWindow;

const LOAD_OLDER_THRESHOLD: usize = 8;

pub fn conversation(
    this: &mut MainWindow,
    window: &mut gpui::Window,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let mut pane = gpui::div()
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .bg(theme::canvas());

    let Some(_) = this.chats.selected.clone() else {
        pane = pane.child(empty_conversation());
        return pane;
    };

    pane = pane.child(header(this, cx));
    if !this.session.state.is_connected() {
        pane = pane.child(connection_banner(this));
    }
    pane = pane.child(timeline(this, window, cx));
    pane = pane.child(crate::views::composer::composer_bar(this, window, cx));
    pane
}

fn connection_banner(this: &MainWindow) -> gpui::Div {
    let failed = matches!(
        this.session.state,
        wasabi_core::state::SessionState::Disconnected { .. }
            | wasabi_core::state::SessionState::Failed { .. }
    );
    gpui::div()
        .min_h(px(34.0))
        .flex_shrink_0()
        .px(px(14.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(if failed { theme::danger() } else { theme::warn() })
        .text_color(theme::text_on_accent())
        .text_size(px(theme::TEXT_SIZE_SM))
        .child(format!("{} — cached messages remain available", this.session.status_label()))
}

fn empty_conversation() -> gpui::Div {
    gpui::div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::TEXT_SIZE))
        .text_color(theme::text_secondary())
        .child("Select a conversation to start messaging")
}

fn header(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let selected = this
        .chats
        .selected
        .as_ref()
        .and_then(|id| this.chats.chats.iter().find(|c| c.id.as_str() == id));

    let (name, subtitle, initials, avatar_bg) = match selected {
        Some(chat) => {
            let name = chats::fallback_name(chat);
            let initials = messages::avatar_initials(chat);
            (
                name.clone(),
                messages::conversation_subtitle(chat),
                initials,
                theme::sender_color(&name),
            )
        }
        None => (
            "Conversation".to_string(),
            String::new(),
            "#".to_string(),
            theme::skeleton(),
        ),
    };

    let panel_open = this.show_right_panel;
    gpui::div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .h(px(crate::views::composer::COMPOSER_H))
        .bg(theme::surface())
        .border_b_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .size(px(38.0))
                .rounded_full()
                .flex_shrink(0.0)
                .flex()
                .items_center()
                .justify_center()
                .bg(avatar_bg)
                .text_color(theme::text_on_accent())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(initials),
        )
        .child(
            gpui::div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .child(
                    gpui::div()
                        .truncate()
                        .text_size(px(theme::TEXT_NAME))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_primary())
                        .child(name),
                )
                .child(
                    gpui::div()
                        .truncate()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .child(subtitle),
                ),
        )
        .child(
            gpui::div()
                .id("toggle-info")
                .cursor_pointer()
                .size(px(34.0))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .when(panel_open, |el| el.bg(theme::row_selected()))
                .hover(|s| s.bg(theme::row_hover()))
                .text_color(theme::text_secondary())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_right_panel(cx);
                }))
                .child(Icon::new(IconName::Info).size(px(18.0))),
        )
}

fn timeline(
    this: &mut MainWindow,
    _window: &mut gpui::Window,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    let items_len = this.messages.items.len();
    let item_sizes: Rc<Vec<gpui::Size<gpui::Pixels>>> =
        Rc::new(this.messages.sizes.iter().map(|h| size_bare(*h)).collect());

    // Load older history when the user approaches the top of the window.
    if this.messages.has_more_older
        && !this.messages.loading_older
        && !this.messages.loading
        && this.first_visible <= LOAD_OLDER_THRESHOLD
        && items_len > 0
    {
        this.load_older_history(cx);
    }

    let view = cx.entity().clone();
    gpui::div()
        .id("timeline")
        .flex_1()
        .min_h(px(0.0))
        .relative()
        .child(if this.messages.loading {
            gpui::div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme::text_secondary())
                .child("Loading messages…")
                .into_any_element()
        } else if let Some(err) = &this.messages.error {
            gpui::div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme::danger())
                .child(err.clone())
                .into_any_element()
        } else {
            v_virtual_list(view, "timeline-list", item_sizes, |this, range, _, cx| {
                this.first_visible = range.start;
                this.near_bottom = range.end >= this.messages.items.len().saturating_sub(2);
                range
                    .map(|ix| timeline_row(this, ix, cx))
                    .collect::<Vec<_>>()
            })
            .track_scroll(&this.msg_scroll)
            .into_any_element()
        })
}

fn timeline_row(
    this: &mut MainWindow,
    ix: usize,
    cx: &mut Context<MainWindow>,
) -> gpui::AnyElement {
    match this.messages.items.get(ix) {
        Some(TimelineItem::Date(label)) => gpui::div()
            .flex()
            .justify_center()
            .py(px(8.0))
            .child(
                gpui::div()
                    .rounded(px(theme::RADIUS_SM))
                    .px(px(10.0))
                    .py(px(3.0))
                    .bg(theme::surface())
                    .text_size(px(theme::TEXT_SIZE_SM))
                    .text_color(theme::text_secondary())
                    .child(label.clone()),
            )
            .into_any_element(),
        Some(TimelineItem::Message(row_ix)) => match this.messages.rows.get(*row_ix) {
            Some(row) => bubble(row.clone(), *row_ix, this.settings.text_scale, cx).into_any_element(),
            None => gpui::div().into_any_element(),
        },
        None => gpui::div().into_any_element(),
    }
}

fn bubble(
    row: wasabi_domain::MessageRow,
    row_index: usize,
    text_scale: u16,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    use wasabi_domain::{MessageDirection, MessageKind};

    let outgoing = row.direction == MessageDirection::Outgoing;

    if matches!(row.kind, MessageKind::System { .. }) {
        return gpui::div().flex().justify_center().py(px(6.0)).child(
            gpui::div()
                .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                .text_color(theme::text_secondary())
                .child(messages::body_text(&row)),
        );
    }

    let (bubble_bg, text_color) = if outgoing {
        (theme::bubble_out(), theme::text_primary())
    } else {
        (theme::bubble_in(), theme::text_primary())
    };

    let show_sender = messages::sender_is_group_member(&row);
    let sender_label = messages::sender_display(&row);
    let sender_color = theme::sender_color(&sender_label);

    let meta_ticks = if outgoing {
        format!(
            "{} {}",
            messages::relative_time(row.timestamp_ms),
            messages::status_glyph(row.status)
        )
    } else {
        messages::relative_time(row.timestamp_ms)
    };

    let mut content = gpui::div().flex().flex_col().gap(px(2.0));
    if show_sender {
        content = content.child(
            gpui::div()
                .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(sender_color)
                .child(sender_label),
        );
    }
    if row.revoked {
        content = content.child(
            gpui::div()
                .italic()
                .text_color(theme::text_secondary())
                .child("This message was deleted"),
        );
    } else {
        content = content.child(
            gpui::div()
                .text_size(px(theme::scaled_text(theme::TEXT_SIZE, text_scale)))
                .text_color(text_color)
                .child(messages::body_text(&row)),
        );
    }
    content = content.child(
        gpui::div()
            .flex()
            .justify_end()
            .gap(px(4.0))
            .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
            .text_color(messages::status_color(row.status))
            .child(meta_ticks)
            .child(message_star_action(&row, row_index, cx)),
    );

    let alignment = if outgoing {
        gpui::div().flex().justify_end()
    } else {
        gpui::div().flex().justify_start()
    };

    alignment.px(px(12.0)).py(px(2.0)).child(
        gpui::div()
            .max_w(px(theme::BUBBLE_MAX_W))
            .rounded(px(theme::RADIUS_MD))
            .px(px(10.0))
            .py(px(6.0))
            .border_1()
            .when(!outgoing, |el| el.border_color(theme::border()))
            .when(outgoing, |el| el.border_color(gpui::transparent_black()))
            .bg(bubble_bg)
            .child(content),
    )
}

fn message_star_action(
    row: &wasabi_domain::MessageRow,
    row_index: usize,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    let starred = row.starred;
    let action = wasabi_domain::MessageAction::Star {
        target: row.into(),
        starred: !starred,
    };
    gpui::div()
        .id(("star-message", row_index))
        .ml(px(2.0))
        .cursor_pointer()
        .text_color(if starred {
            theme::accent_text()
        } else {
            theme::text_secondary()
        })
        .hover(|style| style.text_color(theme::accent_text()))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.perform_message_action(action.clone(), cx)
        }))
        .child(if starred { "★" } else { "☆" })
}

/// Width is ignored by the vertical list; only heights matter.
fn size_bare(h: f32) -> gpui::Size<gpui::Pixels> {
    gpui::size(px(1.0), px(h))
}
