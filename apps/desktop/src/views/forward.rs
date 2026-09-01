//! Cache-first picker for forwarding a message into existing conversations.

use std::collections::HashSet;

use gpui::prelude::*;
use gpui::{Context, px};
use gpui_component::input::Input;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{Icon, IconName};

use crate::state::chats;
use crate::theme;
use crate::views::avatar;
use crate::views::root::MainWindow;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ForwardCandidate {
    pub id: wasabi_domain::ChatId,
    pub display_name: String,
    pub is_group: bool,
}

pub fn overlay(
    this: &mut MainWindow,
    target: &wasabi_domain::MessageActionTarget,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let query = this
        .forward_search_input
        .read(cx)
        .value()
        .trim()
        .to_string();
    let connected = this.session.state.is_connected();
    let forwarding = this.forwarding;
    let selected = this.forward_selected.clone();
    let candidates = forward_candidates(&this.chats.chats, &this.contacts, &target.chat, &query);
    let loading = this.contacts_loading && !query.is_empty() && candidates.is_empty();
    let empty_unfiltered = query.is_empty()
        && !this.contacts_loading
        && forward_candidates(&this.chats.chats, &[], &target.chat, "").is_empty();

    let mut list = gpui::div()
        .flex_1()
        .min_h(px(0.0))
        .overflow_y_scrollbar()
        .border_t_1()
        .border_color(theme::border());

    if loading {
        list = list.child(state_message(
            "Loading conversations…",
            "Searching the local address book",
        ));
    } else if empty_unfiltered {
        list = list.child(state_message(
            "No conversations to forward to",
            "Open or search another chat first.",
        ));
    } else if candidates.is_empty() {
        let (title, detail) = if let Some(error) = this.contacts_error.clone() {
            ("Contacts unavailable", error)
        } else {
            (
                "No matching conversations",
                "Try another name from your chats or address book.".to_string(),
            )
        };
        list = list.child(state_message(title, detail));
    } else {
        for (index, candidate) in candidates.into_iter().enumerate() {
            let chosen = selected.iter().any(|id| *id == candidate.id);
            let name = candidate.display_name.clone();
            let id = candidate.id.clone();
            let photo = this
                .chats
                .chats
                .iter()
                .find(|chat| chat.id == candidate.id)
                .and_then(|chat| this.list_avatar_path(chat))
                .or_else(|| {
                    this.contacts
                        .iter()
                        .find(|contact| contact.jid == candidate.id)
                        .and_then(|contact| {
                            this.bridge
                                .cached_avatar_path(contact.jid.as_str(), contact.avatar.as_ref()?)
                        })
                });
            let initial = name
                .chars()
                .find(|character| character.is_alphanumeric())
                .map(|character| character.to_uppercase().to_string())
                .unwrap_or_else(|| "#".to_string());
            list = list.child(
                gpui::div()
                    .id(("forward-chat", index))
                    .min_h(px(62.0))
                    .px(px(14.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .cursor_pointer()
                    .aria_label(format!(
                        "{} {name}",
                        if chosen { "Remove" } else { "Select" }
                    ))
                    .when(chosen, |row| row.bg(theme::row_selected()))
                    .hover(|row| row.bg(theme::row_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_forward_destination(id.clone(), cx)
                    }))
                    .child(avatar::avatar_face(
                        40.0,
                        photo.as_deref(),
                        initial,
                        theme::sender_color(&name),
                        theme::text_on_accent(),
                        None,
                    ))
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
                                    .text_color(theme::text_primary())
                                    .child(name),
                            )
                            .when(candidate.is_group, |column| {
                                column.child(
                                    gpui::div()
                                        .text_size(px(theme::TEXT_SIZE_SM))
                                        .text_color(theme::text_secondary())
                                        .child("Group"),
                                )
                            }),
                    )
                    .child(selection_mark(chosen)),
            );
        }
        if this.contacts_loading && !query.is_empty() {
            list = list.child(state_message("Loading more…", ""));
        }
    }

    let selected_count = selected.len();
    let can_send = selected_count > 0 && connected && !forwarding;
    let status = if let Some(error) = this.forward_error.clone() {
        error
    } else if forwarding {
        "Forwarding…".to_string()
    } else if !connected {
        "Connect to forward this message".to_string()
    } else if selected_count == 0 {
        "Choose conversations, then Forward. Closing sends nothing.".to_string()
    } else if selected_count == 1 {
        "Forward to 1 conversation".to_string()
    } else {
        format!("Forward to {selected_count} conversations")
    };

    gpui::div()
        .absolute()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui::div()
                .id("forward-scrim")
                .absolute()
                .size_full()
                .occlude()
                .bg(theme::scrim())
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.close_message_overlay(cx)
                })),
        )
        .child(
            gpui::div()
                .relative()
                .occlude()
                .w(px(440.0))
                .h(px(620.0))
                .max_w_full()
                .max_h_full()
                .rounded(px(theme::RADIUS_MD))
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .flex()
                .flex_col()
                .child(
                    gpui::div()
                        .h(px(58.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px(px(16.0))
                        .child(
                            gpui::div()
                                .text_size(px(theme::TEXT_TITLE))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Forward"),
                        )
                        .child(
                            gpui::div()
                                .id("close-forward")
                                .size(px(34.0))
                                .rounded_full()
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .justify_center()
                                .aria_label("Close Forward")
                                .hover(|button| button.bg(theme::row_hover()))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.close_message_overlay(cx)
                                }))
                                .child(Icon::new(IconName::Close).size(px(16.0))),
                        ),
                )
                .child(
                    gpui::div().px(px(14.0)).pb(px(12.0)).child(
                        Input::new(&this.forward_search_input)
                            .prefix(Icon::new(IconName::Search).size(px(16.0)))
                            .cleanable(true),
                    ),
                )
                .child(list)
                .child(
                    gpui::div()
                        .min_h(px(66.0))
                        .flex_shrink_0()
                        .px(px(14.0))
                        .py(px(10.0))
                        .border_t_1()
                        .border_color(theme::border())
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .child(
                            gpui::div()
                                .flex_1()
                                .min_w(px(0.0))
                                .text_size(px(theme::TEXT_SIZE_SM))
                                .text_color(if this.forward_error.is_some() {
                                    theme::danger()
                                } else {
                                    theme::text_secondary()
                                })
                                .child(status),
                        )
                        .child(
                            gpui::div()
                                .id("confirm-forward")
                                .min_w(px(96.0))
                                .h(px(38.0))
                                .px(px(14.0))
                                .rounded(px(theme::RADIUS_SM))
                                .flex()
                                .items_center()
                                .justify_center()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .bg(if can_send {
                                    theme::accent()
                                } else {
                                    theme::chip_idle()
                                })
                                .text_color(if can_send {
                                    theme::text_on_accent()
                                } else {
                                    theme::text_secondary()
                                })
                                .when(can_send, |button| {
                                    button.cursor_pointer().on_click(cx.listener(
                                        |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.submit_forward(cx)
                                        },
                                    ))
                                })
                                .child(if forwarding {
                                    "Forwarding…"
                                } else {
                                    "Forward"
                                }),
                        ),
                ),
        )
}

fn selection_mark(selected: bool) -> gpui::Div {
    gpui::div()
        .size(px(24.0))
        .rounded_full()
        .border_1()
        .border_color(if selected {
            theme::accent()
        } else {
            theme::border()
        })
        .bg(if selected {
            theme::accent()
        } else {
            theme::surface()
        })
        .text_color(theme::text_on_accent())
        .flex()
        .items_center()
        .justify_center()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .when(selected, |mark| {
            mark.child(Icon::new(IconName::Check).size(px(15.0)))
        })
}

fn state_message(title: &'static str, detail: impl Into<String>) -> gpui::Div {
    let detail = detail.into();
    gpui::div()
        .px(px(18.0))
        .py(px(24.0))
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            gpui::div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_primary())
                .child(title),
        )
        .when(!detail.is_empty(), |column| {
            column.child(
                gpui::div()
                    .text_size(px(theme::TEXT_SIZE_SM))
                    .text_color(theme::text_secondary())
                    .child(detail),
            )
        })
}

pub(crate) fn forward_candidates(
    chats: &[wasabi_domain::ChatSummary],
    contacts: &[wasabi_domain::ContactSummary],
    source: &wasabi_domain::ChatId,
    query: &str,
) -> Vec<ForwardCandidate> {
    let query = query.trim().to_lowercase();
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    let mut chat_rows = chats
        .iter()
        .filter(|chat| {
            chat.id != *source
                && !chat.archived
                && matches!(
                    chat.kind,
                    wasabi_domain::ChatKind::Direct | wasabi_domain::ChatKind::Group
                )
        })
        .collect::<Vec<_>>();
    chat_rows.sort_by(|left, right| {
        right
            .pinned_at_ms
            .cmp(&left.pinned_at_ms)
            .then_with(|| right.last_activity_ms.cmp(&left.last_activity_ms))
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    for chat in chat_rows {
        if !seen.insert(chat.id.clone()) {
            continue;
        }
        let name = chats::fallback_name(chat);
        if !query.is_empty()
            && !name.to_lowercase().contains(&query)
            && !chat
                .display_name
                .as_ref()
                .is_some_and(|display| display.to_lowercase().contains(&query))
        {
            continue;
        }
        candidates.push(ForwardCandidate {
            id: chat.id.clone(),
            display_name: name,
            is_group: matches!(chat.kind, wasabi_domain::ChatKind::Group),
        });
    }

    if !query.is_empty() {
        for contact in contacts {
            if contact.jid == *source || !seen.insert(contact.jid.clone()) {
                continue;
            }
            if !contact.display_name.to_lowercase().contains(&query) {
                continue;
            }
            candidates.push(ForwardCandidate {
                id: contact.jid.clone(),
                display_name: contact.display_name.clone(),
                is_group: false,
            });
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasabi_domain::{ChatId, ChatKind, ChatSummary, ContactSummary};

    fn chat(id: &str, name: &str, archived: bool, kind: ChatKind) -> ChatSummary {
        ChatSummary {
            id: ChatId::new(id),
            kind,
            display_name: Some(name.to_string()),
            last_activity_ms: 1,
            last_message_preview: None,
            unread_count: 0,
            pinned_at_ms: None,
            muted_until_ms: None,
            archived,
            favorite: false,
            draft_preview: None,
            draft: None,
            avatar: None,
        }
    }

    fn contact(id: &str, name: &str) -> ContactSummary {
        ContactSummary {
            jid: ChatId::new(id),
            display_name: name.to_string(),
            phone_number: None,
            avatar: None,
        }
    }

    #[test]
    fn candidates_exclude_source_archived_and_empty_search_contacts() {
        let source = ChatId::new("me@s.whatsapp.net");
        let chats = vec![
            chat("me@s.whatsapp.net", "Current", false, ChatKind::Direct),
            chat("alice@s.whatsapp.net", "Alice", false, ChatKind::Direct),
            chat("old@s.whatsapp.net", "Old", true, ChatKind::Direct),
            chat("crew@g.us", "Crew", false, ChatKind::Group),
        ];
        let contacts = vec![
            contact("alice@s.whatsapp.net", "Alice"),
            contact("bob@s.whatsapp.net", "Bob"),
        ];

        let listed = forward_candidates(&chats, &contacts, &source, "");
        assert_eq!(
            listed.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec!["alice@s.whatsapp.net", "crew@g.us"]
        );

        let searched = forward_candidates(&chats, &contacts, &source, "bo");
        assert_eq!(
            searched
                .iter()
                .map(|row| row.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Bob"]
        );
        assert!(
            forward_candidates(&chats, &contacts, &source, "")
                .iter()
                .all(|row| row.id != source)
        );
    }
}
