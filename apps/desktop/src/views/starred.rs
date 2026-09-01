//! Starred messages takeover: durable starred rows, not a rail destination.

use gpui::prelude::*;
use gpui::{Context, px};
use gpui_component::input::Input;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName};

use crate::state::messages;
use crate::theme;
use crate::views::root::MainWindow;

pub fn page(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    gpui::div()
        .id("starred-page")
        .flex_1()
        .min_w(px(0.0))
        .h_full()
        .flex()
        .flex_col()
        .bg(theme::surface())
        .child(header(cx))
        .child(search_bar(this))
        .child(list(this, cx))
}

fn header(cx: &mut Context<MainWindow>) -> gpui::Div {
    gpui::div()
        .h(px(theme::HEADER_H))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(16.0))
        .border_b_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .id("close-starred-messages")
                .size(px(theme::ACTION_SIZE))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .aria_label("Back")
                .tooltip(|window, cx| Tooltip::new("Back").build(window, cx))
                .hover(|button| button.bg(theme::row_hover()))
                .on_click(cx.listener(|this, _, _, cx| this.close_starred_messages(cx)))
                .child(Icon::new(IconName::ArrowLeft).size(px(18.0))),
        )
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_TITLE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_primary())
                .child("Starred messages"),
        )
}

fn search_bar(this: &mut MainWindow) -> gpui::Div {
    gpui::div().px(px(12.0)).py(px(8.0)).child(
        gpui::div()
            .h(px(40.0))
            .w_full()
            .rounded(px(theme::RADIUS_MD))
            .bg(theme::surface_elevated())
            .px(px(4.0))
            .flex()
            .items_center()
            .child(
                Input::new(&this.starred_search_input)
                    .appearance(false)
                    .bordered(false)
                    .focus_bordered(false)
                    .prefix(Icon::new(IconName::Search).size(px(16.0)))
                    .text_size(px(theme::TEXT_PREVIEW)),
            ),
    )
}

fn list(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let query = this
        .starred_search_input
        .read(cx)
        .value()
        .trim()
        .to_lowercase();
    let hits: Vec<(usize, wasabi_domain::StarredMessageHit)> = this
        .starred_hits
        .iter()
        .enumerate()
        .filter(|(_, hit)| {
            query.is_empty()
                || hit.chat_name.to_lowercase().contains(&query)
                || messages::body_text(&hit.row)
                    .to_lowercase()
                    .contains(&query)
        })
        .map(|(index, hit)| (index, hit.clone()))
        .collect();

    let mut list = gpui::div()
        .flex_1()
        .min_h(px(0.0))
        .overflow_y_scrollbar()
        .border_t_1()
        .border_color(theme::border());

    if this.starred_hits.is_empty() && this.starred_loading {
        list = list.child(state_message(
            "Loading starred messages",
            "Reading starred messages from this account.",
        ));
    } else if this.starred_hits.is_empty() {
        if let Some(error) = this.starred_error.clone() {
            list = list.child(state_message("Couldn't load starred messages", error));
        } else {
            list = list.child(state_message(
                "No starred messages",
                "Star a message in a chat to find it here.",
            ));
        }
    } else if hits.is_empty() {
        list = list.child(state_message(
            "No matching starred messages",
            "Try a different name or preview.",
        ));
        if this.starred_has_more {
            list = list.child(load_more_row(this, cx));
        }
    } else {
        let text_scale = this.settings.text_scale;
        for (index, hit) in hits {
            list = list.child(starred_row(cx, index, hit, text_scale));
        }
        if this.starred_has_more {
            list = list.child(load_more_row(this, cx));
        }
        if let Some(error) = this.starred_error.clone() {
            list = list.child(state_message("Couldn't load starred messages", error));
        }
    }
    list
}

fn starred_row(
    cx: &mut Context<MainWindow>,
    index: usize,
    hit: wasabi_domain::StarredMessageHit,
    text_scale: u16,
) -> gpui::Stateful<gpui::Div> {
    let chat_id = hit.row.chat.as_str().to_string();
    let message_id = hit.row.id.clone();
    let unstar_chat = hit.row.chat.clone();
    let unstar_message = hit.row.id.clone();
    let time = messages::relative_time(hit.row.timestamp_ms);
    let preview = messages::body_text(&hit.row);
    let name = hit.chat_name.clone();
    gpui::div()
        .id(("starred-hit", index))
        .min_h(px(theme::CHAT_ROW_H))
        .px(px(16.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .cursor_pointer()
        .aria_label(format!("Open starred message in {name}"))
        .hover(|row| row.bg(theme::row_hover()))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.open_starred_hit(chat_id.clone(), message_id.clone(), window, cx)
        }))
        .child(
            gpui::div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(3.0))
                .child(
                    gpui::div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            gpui::div()
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_size(px(theme::scaled_text(theme::TEXT_NAME, text_scale)))
                                .text_color(theme::text_primary())
                                .child(name),
                        )
                        .child(
                            gpui::div()
                                .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                                .text_color(theme::text_secondary())
                                .whitespace_nowrap()
                                .child(time),
                        ),
                )
                .child(
                    gpui::div()
                        .w_full()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .truncate()
                        .text_size(px(theme::scaled_text(theme::TEXT_SIZE, text_scale)))
                        .text_color(theme::text_secondary())
                        .child(preview),
                ),
        )
        .child(
            gpui::div()
                .id(("unstar-starred", index))
                .size(px(theme::ACTION_SIZE))
                .rounded_full()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .aria_label("Unstar message")
                .tooltip(|window, cx| Tooltip::new("Unstar").build(window, cx))
                .hover(|button| button.bg(theme::row_hover()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.unstar_starred_hit(unstar_chat.clone(), unstar_message.clone(), cx)
                }))
                .child(
                    Icon::new(IconName::Star)
                        .size(px(16.0))
                        .text_color(theme::accent_text()),
                ),
        )
}

fn load_more_row(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Stateful<gpui::Div> {
    let loading = this.starred_loading_more;
    gpui::div()
        .id("load-more-starred")
        .h(px(theme::CHAT_ROW_H))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::TEXT_SIZE))
        .text_color(theme::accent_text())
        .when(!loading, |row| {
            row.cursor_pointer()
                .hover(|style| style.bg(theme::row_hover()))
                .on_click(cx.listener(|this, _, _, cx| this.load_more_starred(cx)))
        })
        .child(if loading {
            "Loading more…"
        } else {
            "Load more starred messages"
        })
}

fn state_message(title: impl Into<String>, detail: impl Into<String>) -> gpui::Div {
    gpui::div()
        .flex_1()
        .min_h(px(160.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .px(px(24.0))
        .gap(px(6.0))
        .child(
            gpui::div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_primary())
                .child(title.into()),
        )
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::text_secondary())
                .child(detail.into()),
        )
}
