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
    if this.chats.scope == wasabi_domain::ChatScope::Archived {
        return gpui::div().child(
            gpui::div()
                .id("archived-back")
                .h(px(48.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(12.0))
                .border_b_1()
                .border_color(theme::border())
                .cursor_pointer()
                .hover(|style| style.bg(theme::row_hover()))
                .on_click(cx.listener(|this, _, _, cx| this.show_active_chats(cx)))
                .child(Icon::new(IconName::ArrowLeft).size(px(17.0)))
                .child(
                    gpui::div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_primary())
                        .child("Archived"),
                ),
        );
    }

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
        .flex_shrink_0()
        .flex()
        .flex_col()
        .child(
            gpui::div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .bg(theme::surface())
                .children(chips),
        )
        .child(
            gpui::div()
                .id("archived-chats")
                .h(px(42.0))
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(14.0))
                .border_t_1()
                .border_b_1()
                .border_color(theme::border())
                .cursor_pointer()
                .text_color(theme::text_secondary())
                .hover(|style| style.bg(theme::row_hover()))
                .on_click(cx.listener(|this, _, _, cx| this.show_archived(cx)))
                .child(Icon::new(IconName::Inbox).size(px(17.0)))
                .child("Archived"),
        )
}

pub fn chat_list(
    this: &mut MainWindow,
    _window: &mut Window,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    if !this.chats.query.trim().is_empty() {
        return search_results_list(this, cx);
    }

    let rows = this.chats.visible_cache.len();
    let item_count = rows + usize::from(this.chats.next_after.is_some());
    let item_sizes: Rc<Vec<gpui::Size<gpui::Pixels>>> =
        Rc::new(vec![
            size(px(theme::CHAT_LIST_W), px(theme::CHAT_ROW_H));
            item_count
        ]);

    let view = cx.entity().clone();
    let mut pane = gpui::div().flex_1().min_h(px(0.0)).relative();
    if this.chats.loading && rows == 0 {
        pane = pane.child(centered_label("Loading chats…"));
    } else if let Some(err) = &this.chats.error {
        pane = pane.child(centered_label(err));
    } else if rows == 0 {
        let empty = if this.chats.scope == wasabi_domain::ChatScope::Archived {
            "No archived conversations"
        } else {
            "No conversations yet"
        };
        pane = pane.child(centered_label(empty));
    } else {
        pane = pane.child(
            v_virtual_list(view, "chat-list", item_sizes, |this, range, _, cx| {
                range
                    .map(|ix| {
                        if ix == this.chats.visible_cache.len() {
                            load_more_row(this, cx).into_any_element()
                        } else {
                            chat_row(this, cx, ix).into_any_element()
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .track_scroll(&this.chat_scroll),
        );
    }
    pane
}

fn search_results_list(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    const HEADER_H: f32 = 34.0;

    let local_count = this.chats.visible_cache.len();
    let message_count = this.chats.search_messages.len();
    let chat_header_count = usize::from(local_count > 0);
    let message_header = chat_header_count + local_count;
    let footer_count = usize::from(message_count == 0 || this.chats.search_has_more);
    let item_count = message_header + 1 + message_count + footer_count;
    let mut sizes = Vec::with_capacity(item_count);
    if local_count > 0 {
        sizes.push(size(px(theme::CHAT_LIST_W), px(HEADER_H)));
        sizes.extend(std::iter::repeat_n(
            size(px(theme::CHAT_LIST_W), px(theme::CHAT_ROW_H)),
            local_count,
        ));
    }
    sizes.push(size(px(theme::CHAT_LIST_W), px(HEADER_H)));
    sizes.extend(std::iter::repeat_n(
        size(px(theme::CHAT_LIST_W), px(theme::CHAT_ROW_H)),
        message_count + footer_count,
    ));

    let view = cx.entity().clone();
    gpui::div().flex_1().min_h(px(0.0)).child(
        v_virtual_list(
            view,
            "search-results",
            Rc::new(sizes),
            move |this, range, _, cx| {
                range
                    .map(|ix| {
                        if local_count > 0 && ix == 0 {
                            return section_header("Chats", None).into_any_element();
                        }
                        if local_count > 0 && ix <= local_count {
                            return chat_row(this, cx, ix - 1).into_any_element();
                        }
                        if ix == message_header {
                            let status = this.chats.search_loading.then_some("Searching…");
                            return section_header("Messages", status).into_any_element();
                        }
                        let message_index = ix.saturating_sub(message_header + 1);
                        if let Some(hit) = this.chats.search_messages.get(message_index).cloned() {
                            return message_search_row(this, cx, message_index, hit)
                                .into_any_element();
                        }
                        if this.chats.search_has_more {
                            return load_more_search_row(this, cx).into_any_element();
                        }
                        if this.chats.search_loading {
                            return search_status_row("Searching messages…").into_any_element();
                        }
                        let status = this
                            .chats
                            .search_error
                            .clone()
                            .unwrap_or_else(|| "No matching messages".to_string());
                        search_status_row(status).into_any_element()
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&this.chat_scroll),
    )
}

fn load_more_search_row(
    this: &mut MainWindow,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    let loading = this.chats.search_loading;
    gpui::div()
        .id("load-more-search")
        .h(px(theme::CHAT_ROW_H))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::TEXT_SIZE))
        .text_color(theme::accent_text())
        .when(!loading, |row| {
            row.cursor_pointer()
                .hover(|style| style.bg(theme::row_hover()))
                .on_click(cx.listener(|this, _, _, cx| this.load_more_search(cx)))
        })
        .child(if loading {
            "Loading more results…"
        } else {
            "Load more messages"
        })
}

fn section_header(label: &'static str, status: Option<&'static str>) -> gpui::Div {
    gpui::div()
        .h(px(34.0))
        .flex()
        .items_center()
        .justify_between()
        .px(px(12.0))
        .bg(theme::canvas())
        .text_size(px(theme::TEXT_SIZE_SM))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::text_secondary())
        .child(label)
        .children(status.map(|status| {
            gpui::div()
                .font_weight(gpui::FontWeight::NORMAL)
                .child(status)
        }))
}

fn message_search_row(
    this: &mut MainWindow,
    cx: &mut Context<MainWindow>,
    index: usize,
    hit: wasabi_domain::MessageSearchHit,
) -> gpui::Stateful<gpui::Div> {
    let chat_id = hit.row.chat.as_str().to_string();
    let name = this
        .chats
        .chats
        .iter()
        .find(|chat| chat.id == hit.row.chat)
        .map(chats::fallback_name)
        .unwrap_or_else(|| {
            chat_id
                .split('@')
                .next()
                .unwrap_or(chat_id.as_str())
                .to_string()
        });
    let initial = name
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    let time = messages::relative_time(hit.row.timestamp_ms);
    let text_scale = this.settings.text_scale;
    gpui::div()
        .id(("message-search-hit", index))
        .h(px(theme::CHAT_ROW_H))
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(10.0))
        .cursor_pointer()
        .hover(|style| style.bg(theme::row_hover()))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.select_chat(chat_id.clone(), window, cx)
        }))
        .child(
            gpui::div()
                .size(px(42.0))
                .rounded_full()
                .flex_shrink(0.0)
                .flex()
                .items_center()
                .justify_center()
                .bg(theme::sender_color(&name))
                .text_color(theme::text_on_accent())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(initial),
        )
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
                                .text_color(theme::text_primary())
                                .child(name),
                        )
                        .child(
                            gpui::div()
                                .text_size(px(theme::TEXT_SIZE_SM))
                                .text_color(theme::text_secondary())
                                .child(time),
                        ),
                )
                .child(
                    gpui::div()
                        .truncate()
                        .text_size(px(theme::scaled_text(theme::TEXT_SIZE, text_scale)))
                        .text_color(theme::text_secondary())
                        .child(hit.snippet),
                ),
        )
}

fn search_status_row(text: impl Into<String>) -> gpui::Div {
    gpui::div()
        .h(px(theme::CHAT_ROW_H))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::TEXT_SIZE))
        .text_color(theme::text_secondary())
        .child(text.into())
}

fn load_more_row(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Stateful<gpui::Div> {
    let loading = this.chats.loading_more;
    gpui::div()
        .id("load-more-chats")
        .h(px(theme::CHAT_ROW_H))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::TEXT_SIZE))
        .text_color(theme::accent_text())
        .when(!loading, |row| {
            row.cursor_pointer()
                .hover(|style| style.bg(theme::row_hover()))
                .on_click(cx.listener(|this, _, _, cx| this.load_more_chats(cx)))
        })
        .child(if loading {
            "Loading…"
        } else {
            "Load more chats"
        })
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
    let has_draft = chat.draft_preview.is_some();
    let preview = if typing_here {
        "typing…".to_string()
    } else if let Some(draft) = chat.draft_preview.as_ref() {
        format!("Draft: {draft}")
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
    let favorite = chat.favorite;

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
        .on_click(cx.listener(move |this, _: &gpui::ClickEvent, window, cx| {
            this.select_chat(id.clone(), window, cx);
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
                                    .text_size(px(theme::scaled_text(
                                        theme::TEXT_SIZE_SM,
                                        text_scale,
                                    )))
                                    .text_color(theme::accent_text())
                                    .child(Icon::new(IconName::ArrowUp).size(px(13.0))),
                            )
                        })
                        .when(favorite, |el| {
                            el.child(
                                gpui::div()
                                    .text_size(px(theme::scaled_text(
                                        theme::TEXT_SIZE_SM,
                                        text_scale,
                                    )))
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
                                .when(typing_here || has_draft, |el| {
                                    el.text_color(theme::accent_text())
                                })
                                .when(!typing_here && !has_draft, |el| {
                                    el.text_color(theme::text_secondary())
                                })
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
