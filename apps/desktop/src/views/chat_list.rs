//! Chat list pane: filter chips plus a fixed-row-height virtual list.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{Context, Window, px, size};
use gpui_component::v_virtual_list;
use gpui_component::{Icon, IconName};

use crate::state::chats::{self, ChatFilter};
use crate::state::messages;
use crate::theme;
use crate::views::root::MainWindow;

pub fn pane_header(_this: &mut MainWindow, _cx: &mut Context<MainWindow>) -> gpui::Div {
    gpui::div()
        .h(px(58.0))
        .flex_shrink_0()
        .flex()
        .items_center()
        .px(px(16.0))
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_TITLE))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::text_primary())
                .child("Wasabi"),
        )
}

pub fn search_bar(this: &mut MainWindow) -> gpui::Div {
    gpui::div().px(px(12.0)).pb(px(8.0)).child(
        gpui_component::input::Input::new(&this.search_input)
            .prefix(Icon::new(IconName::Search).size(px(16.0))),
    )
}

pub fn filter_bar(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let chips = ChatFilter::ALL.map(|filter| {
        let active = this.chats.filter == filter;
        let mut chip = gpui::div()
            .id(("filter-chip", filter as usize))
            .cursor_pointer()
            .rounded_full()
            .px(px(12.0))
            .py(px(5.0))
            .text_size(px(theme::TEXT_SIZE_SM));
        if active {
            chip = chip
                .bg(theme::accent())
                .text_color(theme::text_on_accent())
                .font_weight(gpui::FontWeight::MEDIUM);
        } else {
            chip = chip
                .bg(theme::chip_idle())
                .text_color(theme::text_secondary())
                .hover(|s| s.bg(theme::row_hover()));
        }
        chip = chip.on_click(cx.listener(move |this, _, _, cx| {
            this.set_chat_filter(filter, cx);
        }));
        chip.child(filter.label())
    });

    gpui::div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .px(px(10.0))
        .py(px(8.0))
        .bg(theme::surface())
        .border_b_1()
        .border_color(theme::border())
        .children(chips)
}

pub fn chat_list(
    this: &mut MainWindow,
    _window: &mut Window,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let rows = this.chats.visible_cache.len();
    let item_sizes: Rc<Vec<gpui::Size<gpui::Pixels>>> = Rc::new(vec![
        size(px(theme::CHAT_LIST_W), px(theme::CHAT_ROW_H));
        rows
    ]);

    let view = cx.entity().clone();
    let mut pane = gpui::div().flex_1().min_h(px(0.0)).relative();
    if this.chats.loading && rows == 0 {
        pane = pane.child(centered_label("Loading chats…"));
    } else if let Some(err) = &this.chats.error {
        pane = pane.child(centered_label(err));
    } else if rows == 0 {
        pane = pane.child(centered_label("No conversations yet"));
    } else {
        pane = pane.child(
            v_virtual_list(view, "chat-list", item_sizes, |this, range, _, cx| {
                range.map(|ix| chat_row(this, cx, ix)).collect::<Vec<_>>()
            })
            .track_scroll(&this.chat_scroll),
        );
    }
    pane
}

fn chat_row(
    this: &mut MainWindow,
    cx: &mut Context<MainWindow>,
    ix: usize,
) -> gpui::Stateful<gpui::Div> {
    let Some(&row_index) = this.chats.visible_cache.get(ix) else {
        return gpui::div()
            .id(("chat-row-empty", ix))
            .h(px(theme::CHAT_ROW_H));
    };
    let Some(chat) = this.chats.chats.get(row_index) else {
        return gpui::div()
            .id(("chat-row-empty", ix))
            .h(px(theme::CHAT_ROW_H));
    };

    let id = chat.id.as_str().to_string();
    let selected = this.chats.selected.as_deref() == Some(id.as_str());
    let name = chats::fallback_name(chat);
    let initials = messages::avatar_initials(chat);
    let avatar_bg = theme::sender_color(&name);
    let time = messages::relative_time(chat.last_activity_ms);

    let typing_here = this.typing.contains_key(&id);
    let preview = if typing_here {
        "typing…".to_string()
    } else {
        chat.last_message_preview.clone().unwrap_or_default()
    };

    let unread = chat.unread_count;
    let text_scale = this.settings.text_scale;
    let unread_pill = (unread != 0).then(|| {
        let label = match unread {
            n if n < 0 => "•".to_string(),
            n if n > 99 => "99+".to_string(),
            n => n.to_string(),
        };
        gpui::div()
            .min_w(px(20.0))
            .px(px(6.0))
            .py(px(1.0))
            .rounded_full()
            .bg(theme::accent())
            .text_color(theme::text_on_accent())
            .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
            .flex()
            .justify_center()
            .child(label)
    });

    let pinned = chat.pinned_at_ms.is_some();

    let row = gpui::div()
        .id(("chat-row", ix))
        .cursor_pointer()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(10.0))
        .w(px(theme::CHAT_LIST_W))
        .h(px(theme::CHAT_ROW_H))
        .overflow_hidden()
        .when(selected, |el| el.bg(theme::row_selected()))
        .hover(|s| s.bg(theme::row_hover()))
        .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
            this.select_chat(id.clone(), cx);
        }))
        .child(
            gpui::div()
                .size(px(46.0))
                .rounded_full()
                .flex_shrink(0.0)
                .flex()
                .items_center()
                .justify_center()
                .bg(avatar_bg)
                .text_color(theme::text_on_accent())
                .text_size(px(theme::scaled_text(theme::TEXT_NAME, text_scale)))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(initials),
        )
        .child(
            gpui::div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    gpui::div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .child(
                            gpui::div()
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .text_size(px(theme::scaled_text(theme::TEXT_NAME, text_scale)))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::text_primary())
                                .child(name),
                        )
                        .when(pinned, |el| {
                            el.child(
                                gpui::div()
                                    .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                                    .text_color(theme::accent_text())
                                    .child(Icon::new(IconName::Star).size(px(13.0))),
                            )
                        })
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
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            gpui::div()
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .text_size(px(theme::scaled_text(theme::TEXT_SIZE, text_scale)))
                                .when(typing_here, |el| el.text_color(theme::accent_text()))
                                .when(!typing_here, |el| el.text_color(theme::text_secondary()))
                                .child(preview),
                        )
                        .children(unread_pill),
                ),
        );

    row
}

fn centered_label(text: impl Into<String>) -> gpui::Div {
    gpui::div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::TEXT_SIZE))
        .text_color(theme::text_secondary())
        .child(text.into())
}
