//! On-demand direct-contact and group information. Direct conversations never
//! render a participants section; group metadata remains honest until the
//! backend projection has populated real participants.

use gpui::prelude::*;
use gpui::{Context, px};
use gpui_component::{Icon, IconName};
use wasabi_domain::{ConversationDetails, Participant, ParticipantRole};

use crate::state::chats;
use crate::theme;
use crate::views::root::MainWindow;

pub fn info_panel(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> impl IntoElement {
    let selected = this
        .chats
        .selected
        .as_ref()
        .and_then(|id| this.chats.chats.iter().find(|chat| chat.id.as_str() == id));

    let (fallback_name, is_group) = selected
        .map(|chat| {
            (
                chats::fallback_name(chat),
                chats::is_group(chat.id.as_str()),
            )
        })
        .unwrap_or_else(|| ("Conversation".to_string(), false));
    let (name, subtitle, about) = match this.conversation_details.as_ref() {
        Some(ConversationDetails::Direct(details)) => (
            details.display_name.clone(),
            details
                .phone_number
                .clone()
                .unwrap_or_else(|| "Contact".to_string()),
            details.about.clone(),
        ),
        Some(ConversationDetails::Group(details)) => (
            details.subject.clone(),
            format!("{} participants", details.participant_count),
            details.description.clone(),
        ),
        None => (
            fallback_name,
            if is_group { "Group" } else { "Contact" }.to_string(),
            None,
        ),
    };
    let initial = name.chars().next().unwrap_or('#').to_string();

    let mut panel = gpui::div()
        .id("conversation-info")
        .w(px(theme::RIGHT_PANEL_W))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .bg(theme::surface())
        .border_l_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .h(px(58.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(12.0))
                .px(px(14.0))
                .border_b_1()
                .border_color(theme::border())
                .child(
                    gpui::div()
                        .id("close-info")
                        .size(px(34.0))
                        .rounded_full()
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|style| style.bg(theme::row_hover()))
                        .text_color(theme::text_secondary())
                        .on_click(cx.listener(|this, _, _, cx| this.close_right_panel(cx)))
                        .child(Icon::new(IconName::ChevronLeft).size(px(18.0))),
                )
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_NAME))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(if is_group { "Group info" } else { "Contact info" }),
                ),
        )
        .child(
            gpui::div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(7.0))
                .px(px(20.0))
                .py(px(22.0))
                .child(
                    gpui::div()
                        .size(px(82.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme::row_selected())
                        .text_color(theme::accent_text())
                        .text_size(px(30.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(initial),
                )
                .child(
                    gpui::div()
                        .text_size(px(18.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_primary())
                        .child(name),
                )
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .child(subtitle),
                ),
        )
        .child(section(
            "ABOUT",
            about.unwrap_or_else(|| {
                if this.details_loading {
                    "Loading conversation information…".to_string()
                } else if is_group {
                    "No group description".to_string()
                } else {
                    "About unavailable".to_string()
                }
            }),
        ))
        .child(chat_sync_action(this, cx, ChatSyncAction::Pin))
        .child(chat_sync_action(this, cx, ChatSyncAction::Mute))
        .child(chat_sync_action(this, cx, ChatSyncAction::Archive))
        .child(chat_sync_action(this, cx, ChatSyncAction::MarkRead))
        .child(favorite_action(this, cx))
        .child(action_row("Encryption", "End-to-end encrypted"));

    if let Some(error) = this.details_error.clone() {
        panel = panel.child(section("INFORMATION UNAVAILABLE", error));
    }

    if is_group {
        panel = panel.child(participants_section(this));
    }

    panel = panel
        .child(destructive_chat_action(this, cx, DestructiveChatAction::Clear))
        .child(destructive_chat_action(this, cx, DestructiveChatAction::Delete));

    panel
}

fn section(label: &'static str, body: impl Into<String>) -> gpui::Div {
    gpui::div()
        .mx(px(16.0))
        .py(px(14.0))
        .border_t_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .mb(px(6.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent_text())
                .child(label),
        )
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_secondary())
                .child(body.into()),
        )
}

fn action_row(label: &'static str, detail: impl Into<String>) -> gpui::Div {
    gpui::div()
        .mx(px(16.0))
        .min_h(px(52.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .border_t_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_primary())
                .child(label),
        )
        .child(
            gpui::div()
                .max_w(px(170.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::text_secondary())
                .child(detail.into()),
        )
}

fn favorite_action(
    this: &mut MainWindow,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    let favorite = this
        .chats
        .selected
        .as_ref()
        .and_then(|selected| {
            this.chats
                .chats
                .iter()
                .find(|chat| chat.id.as_str() == selected)
        })
        .is_some_and(|chat| chat.favorite);
    gpui::div()
        .id("toggle-favorite")
        .mx(px(16.0))
        .min_h(px(52.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .cursor_pointer()
        .border_t_1()
        .border_color(theme::border())
        .hover(|style| style.bg(theme::row_hover()))
        .on_click(cx.listener(|this, _, _, cx| this.toggle_favorite(cx)))
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_primary())
                .child("Favorite"),
        )
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(if favorite {
                    theme::accent_text()
                } else {
                    theme::text_secondary()
                })
                .child(if favorite { "On this device" } else { "Off" }),
        )
}

#[derive(Clone, Copy)]
enum ChatSyncAction {
    Pin,
    Mute,
    Archive,
    MarkRead,
}

fn chat_sync_action(
    this: &mut MainWindow,
    cx: &mut Context<MainWindow>,
    kind: ChatSyncAction,
) -> gpui::Stateful<gpui::Div> {
    let selected = this.chats.selected.clone().unwrap_or_default();
    let summary = this
        .chats
        .chats
        .iter()
        .find(|chat| chat.id.as_str() == selected);
    let (label, enabled, detail, action) = match kind {
        ChatSyncAction::Pin => {
            let enabled = summary.is_some_and(|chat| chat.pinned_at_ms.is_some());
            (
                "Pin chat",
                enabled,
                if enabled { "On" } else { "Off" },
                wasabi_domain::ChatAction::Pin {
                    chat: wasabi_domain::ChatId::new(selected),
                    pinned: !enabled,
                },
            )
        }
        ChatSyncAction::Mute => {
            let now = chrono::Utc::now().timestamp_millis();
            let enabled = summary.is_some_and(|chat| {
                chat.muted_until_ms.is_some_and(|until| until == 0 || until > now)
            });
            (
                "Mute notifications",
                enabled,
                if enabled { "On" } else { "Off" },
                wasabi_domain::ChatAction::Mute {
                    chat: wasabi_domain::ChatId::new(selected),
                    muted: !enabled,
                },
            )
        }
        ChatSyncAction::Archive => {
            let enabled = summary.is_some_and(|chat| chat.archived);
            (
                if enabled { "Unarchive chat" } else { "Archive chat" },
                enabled,
                if enabled { "Archived" } else { "Active" },
                wasabi_domain::ChatAction::Archive {
                    chat: wasabi_domain::ChatId::new(selected),
                    archived: !enabled,
                },
            )
        }
        ChatSyncAction::MarkRead => {
            let enabled = summary.is_some_and(|chat| chat.unread_count != 0);
            (
                if enabled { "Mark as read" } else { "Mark as unread" },
                enabled,
                if enabled { "Unread" } else { "Read" },
                wasabi_domain::ChatAction::MarkRead {
                    chat: wasabi_domain::ChatId::new(selected),
                    read: enabled,
                },
            )
        }
    };
    gpui::div()
        .id(match kind {
            ChatSyncAction::Pin => "toggle-pin",
            ChatSyncAction::Mute => "toggle-mute",
            ChatSyncAction::Archive => "toggle-archive",
            ChatSyncAction::MarkRead => "toggle-read",
        })
        .mx(px(16.0))
        .min_h(px(52.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .cursor_pointer()
        .border_t_1()
        .border_color(theme::border())
        .hover(|style| style.bg(theme::row_hover()))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.perform_chat_action(action.clone(), cx)
        }))
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_primary())
                .child(label),
        )
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(if enabled {
                    theme::accent_text()
                } else {
                    theme::text_secondary()
                })
                .child(detail),
        )
}

#[derive(Clone, Copy)]
enum DestructiveChatAction {
    Clear,
    Delete,
}

fn destructive_chat_action(
    this: &mut MainWindow,
    cx: &mut Context<MainWindow>,
    kind: DestructiveChatAction,
) -> gpui::Stateful<gpui::Div> {
    let selected = this.chats.selected.clone().unwrap_or_default();
    let pending = this.destructive_chats.contains(&selected);
    let (id, label, action) = match kind {
        DestructiveChatAction::Clear => (
            "clear-chat",
            "Clear messages…",
            wasabi_domain::ChatAction::Clear {
                chat: wasabi_domain::ChatId::new(selected),
                delete_starred: false,
                delete_media: false,
            },
        ),
        DestructiveChatAction::Delete => (
            "delete-chat",
            "Delete chat…",
            wasabi_domain::ChatAction::Delete {
                chat: wasabi_domain::ChatId::new(selected),
                delete_media: false,
            },
        ),
    };
    gpui::div()
        .id(id)
        .mx(px(16.0))
        .min_h(px(52.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .border_t_1()
        .border_color(theme::border())
        .when(!pending, |row| {
            row.cursor_pointer()
                .hover(|style| style.bg(theme::row_hover()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.confirm_chat_action(action.clone(), cx)
                }))
        })
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(if pending {
                    theme::text_secondary()
                } else {
                    theme::danger()
                })
                .child(if pending { "Working…" } else { label }),
        )
}

fn participants_section(this: &MainWindow) -> gpui::Div {
    let mut body = gpui::div()
        .mx(px(16.0))
        .py(px(14.0))
        .border_t_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .mb(px(8.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent_text())
                .child("PARTICIPANTS"),
        );

    match this.conversation_details.as_ref() {
        Some(ConversationDetails::Group(details)) if details.participants.is_empty() => {
            body = body.child(
                gpui::div()
                    .py(px(8.0))
                    .text_size(px(theme::TEXT_SIZE))
                    .text_color(theme::text_secondary())
                    .child("No participant data was returned"),
            );
        }
        Some(ConversationDetails::Group(details)) => {
            for participant in &details.participants {
                body = body.child(participant_row(participant));
            }
        }
        _ if this.details_loading => {
            // Neutral skeletons communicate pending data without invented
            // names, roles, or participant counts.
            for width in [180.0, 220.0, 160.0] {
                body = body.child(
                    gpui::div()
                        .h(px(52.0))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            gpui::div()
                                .size(px(34.0))
                                .rounded_full()
                                .bg(theme::row_hover()),
                        )
                        .child(
                            gpui::div()
                                .w(px(width))
                                .h(px(10.0))
                                .rounded(px(5.0))
                                .bg(theme::row_hover()),
                        ),
                );
            }
        }
        _ => {
            body = body.child(
                gpui::div()
                    .py(px(8.0))
                    .text_size(px(theme::TEXT_SIZE))
                    .text_color(theme::text_secondary())
                    .child("Participant information is unavailable offline"),
            );
        }
    }
    body
}

fn participant_row(participant: &Participant) -> gpui::Div {
    let initial = participant
        .display_name
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    let role = match participant.role {
        ParticipantRole::Member => None,
        ParticipantRole::Admin => Some("admin"),
        ParticipantRole::SuperAdmin => Some("creator"),
    };
    gpui::div()
        .min_h(px(52.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            gpui::div()
                .size(px(34.0))
                .rounded_full()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme::sender_color(&participant.display_name))
                .text_color(theme::text_on_accent())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(initial),
        )
        .child(
            gpui::div()
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_primary())
                .child(participant.display_name.clone()),
        )
        .children(role.map(|role| {
            gpui::div()
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .bg(theme::row_selected())
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::accent_text())
                .child(role)
        }))
}
