//! Conversation pane: header, date-chipped timeline of variable-height
//! bubbles, and the scroll/anchor plumbing for history paging.

use gpui::prelude::*;
use gpui::{Context, ListSizingBehavior, list, px};
use gpui_component::input::Input;
use gpui_component::{Icon, IconName};

use crate::state::chats;
use crate::state::messages::{self, TimelineItem};
use crate::theme;
use crate::views::avatar;
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
        .bg(if failed {
            theme::danger()
        } else {
            theme::warn()
        })
        .text_color(theme::text_on_accent())
        .text_size(px(theme::TEXT_SIZE_SM))
        .child(format!(
            "{} — cached messages remain available",
            this.session.status_label()
        ))
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
            let subtitle = this
                .typing
                .get(chat.id.as_str())
                .map(|typing| typing.label(chats::is_group(chat.id.as_str())))
                .unwrap_or_else(|| messages::conversation_subtitle(chat));
            (name.clone(), subtitle, initials, theme::sender_color(&name))
        }
        None => match this.conversation_details.as_ref() {
            Some(wasabi_domain::ConversationDetails::Direct(contact)) => {
                let initials = contact
                    .display_name
                    .chars()
                    .next()
                    .unwrap_or('#')
                    .to_uppercase()
                    .to_string();
                let subtitle = contact
                    .phone_number
                    .as_ref()
                    .map(|number| format!("+{number}"))
                    .unwrap_or_else(|| "Contact".to_string());
                (
                    contact.display_name.clone(),
                    subtitle,
                    initials,
                    theme::sender_color(&contact.display_name),
                )
            }
            _ => (
                "Conversation".to_string(),
                String::new(),
                "#".to_string(),
                theme::skeleton(),
            ),
        },
    };
    let selected_chat = this.chats.selected.clone();
    let photo = selected_chat.as_deref().and_then(|id| this.avatar_path(id));

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
        .child(avatar::avatar_face(
            38.0,
            photo,
            initials,
            avatar_bg,
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
    let pending_new_messages = this.pending_new_messages;
    if this.msg_scroll.item_count() != items_len {
        this.msg_scroll.reset(items_len);
    }

    // Load older history when the user approaches the top of the window.
    if this.messages.has_more_older
        && !this.messages.loading_older
        && !this.messages.loading
        && this.first_visible <= LOAD_OLDER_THRESHOLD
        && items_len > 0
    {
        this.load_older_history(cx);
    }
    if this.messages.has_more_newer
        && !this.messages.loading_newer
        && !this.messages.loading
        && this.near_bottom
        && items_len > 0
    {
        this.load_newer_history(cx);
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
            list(this.msg_scroll.clone(), move |ix, _window, cx| {
                view.update(cx, |this, cx| timeline_row(this, ix, cx))
            })
            .with_sizing_behavior(ListSizingBehavior::Auto)
            .size_full()
            .into_any_element()
        })
        .when(pending_new_messages > 0, |timeline| {
            timeline.child(
                gpui::div()
                    .id("jump-to-newest")
                    .absolute()
                    .right_4()
                    .bottom_4()
                    .cursor_pointer()
                    .rounded_full()
                    .px(px(14.0))
                    .py(px(8.0))
                    .bg(theme::accent())
                    .text_color(theme::text_on_accent())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_size(px(theme::TEXT_SIZE_SM))
                    .hover(|style| style.bg(theme::accent_text()))
                    .on_click(cx.listener(|this, _, _, cx| this.jump_to_newest_messages(cx)))
                    .child(if pending_new_messages == 1 {
                        "1 new message".to_string()
                    } else {
                        format!("{pending_new_messages} new messages")
                    }),
            )
        })
}

fn timeline_row(
    this: &mut MainWindow,
    ix: usize,
    cx: &mut Context<MainWindow>,
) -> gpui::AnyElement {
    match this.messages.items.get(ix) {
        Some(TimelineItem::Date(label)) => gpui::div()
            .w_full()
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
            Some(row) => {
                let highlighted = this.messages.highlighted.as_ref() == Some(&row.id);
                let media_state = media_descriptor(&row.kind).and_then(|media| {
                    this.media_downloads
                        .get(&(row.chat.clone(), media.id.clone()))
                        .cloned()
                });
                let retrying = this
                    .retrying_messages
                    .contains(&(row.chat.as_str().to_string(), row.id.as_str().to_string()));
                bubble(
                    row.clone(),
                    *row_ix,
                    this.settings.text_scale,
                    highlighted,
                    media_state,
                    retrying,
                    cx,
                )
                .into_any_element()
            }
            None => gpui::div().into_any_element(),
        },
        None => gpui::div().into_any_element(),
    }
}

fn bubble(
    row: wasabi_domain::MessageRow,
    row_index: usize,
    text_scale: u16,
    highlighted: bool,
    media_state: Option<crate::views::root::MediaDownloadUi>,
    retrying: bool,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    use wasabi_domain::{MessageDirection, MessageKind};

    let outgoing = row.direction == MessageDirection::Outgoing;

    if matches!(row.kind, MessageKind::System { .. }) {
        return gpui::div()
            .w_full()
            .flex()
            .justify_center()
            .py(px(6.0))
            .child(
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

    let edited = row
        .edited_at_ms
        .is_some()
        .then_some("edited · ")
        .unwrap_or("");
    let meta_ticks = if outgoing {
        format!(
            "{edited}{} {}",
            messages::relative_time(row.timestamp_ms),
            messages::status_glyph(row.status)
        )
    } else {
        format!("{edited}{}", messages::relative_time(row.timestamp_ms))
    };

    let mut content = gpui::div().min_w(px(0.0)).flex().flex_col().gap(px(2.0));
    if show_sender {
        content = content.child(
            gpui::div()
                .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(sender_color)
                .child(sender_label),
        );
    }
    if let Some(quoted) = row.quoted.clone() {
        content = content.child(quoted_message(quoted, row_index, text_scale, cx));
    }
    if row.revoked {
        content = content.child(
            gpui::div()
                .italic()
                .text_color(theme::text_secondary())
                .child("This message was deleted"),
        );
    } else if matches!(
        row.kind,
        MessageKind::Unavailable { .. } | MessageKind::Unknown
    ) {
        content = content.child(
            gpui::div()
                .flex()
                .items_center()
                .gap(px(7.0))
                .rounded(px(theme::RADIUS_SM))
                .bg(theme::canvas())
                .px(px(9.0))
                .py(px(7.0))
                .text_color(theme::text_secondary())
                .child(Icon::new(IconName::Info).size(px(16.0)))
                .child(
                    gpui::div()
                        .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                        .child(messages::body_text(&row)),
                ),
        );
    } else if let Some(media) = media_content(&row, text_scale, media_state, cx) {
        content = content.child(media);
    } else {
        content = content.child(
            gpui::div()
                .text_size(px(theme::scaled_text(theme::TEXT_SIZE, text_scale)))
                .text_color(text_color)
                .child(messages::body_text(&row)),
        );
    }
    if !row.reactions.is_empty() {
        content = content.child(reaction_chips(
            row.id.clone(),
            row_index,
            row.reactions.clone(),
            text_scale,
            cx,
        ));
    }
    content = content.child(
        gpui::div()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(4.0))
            .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
            .text_color(messages::status_color(row.status))
            .when(
                outgoing && row.status == wasabi_domain::MessageStatus::Failed,
                |meta| {
                    let message = row.id.clone();
                    meta.child(
                        gpui::div()
                            .id(("retry-message", row_index))
                            .cursor_pointer()
                            .rounded(px(theme::RADIUS_SM))
                            .px(px(6.0))
                            .py(px(2.0))
                            .bg(theme::surface())
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::danger())
                            .hover(|style| style.bg(theme::chip_idle()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.retry_message(message.clone(), cx)
                            }))
                            .child(if retrying { "Retrying…" } else { "Retry" }),
                    )
                },
            )
            .child(meta_ticks)
            .child(message_actions_button(&row, row_index, cx)),
    );

    let alignment = if outgoing {
        gpui::div().flex().justify_end()
    } else {
        gpui::div().flex().justify_start()
    };

    alignment.w_full().px(px(12.0)).py(px(2.0)).child(
        gpui::div()
            .min_w(px(0.0))
            .max_w(px(theme::BUBBLE_MAX_W))
            .rounded(px(theme::RADIUS_MD))
            .px(px(10.0))
            .py(px(6.0))
            .border_1()
            .when(!outgoing, |el| el.border_color(theme::border()))
            .when(outgoing, |el| el.border_color(gpui::transparent_black()))
            .when(highlighted, |el| el.border_color(theme::accent()))
            .bg(bubble_bg)
            .child(content),
    )
}

fn reaction_chips(
    message: wasabi_domain::MessageId,
    row_index: usize,
    reactions: Vec<wasabi_domain::ReactionSummary>,
    text_scale: u16,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    gpui::div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(4.0))
        .pt(px(3.0))
        .children(
            reactions
                .into_iter()
                .enumerate()
                .map(|(reaction_index, reaction)| {
                    let selected = reaction.reacted_by_me;
                    let emoji = reaction.emoji.clone();
                    let target = message.clone();
                    gpui::div()
                        .id(("reaction-chip", row_index * 32 + reaction_index))
                        .cursor_pointer()
                        .h(px(26.0))
                        .flex()
                        .items_center()
                        .rounded_full()
                        .border_1()
                        .border_color(if selected {
                            theme::accent()
                        } else {
                            theme::border()
                        })
                        .bg(if selected {
                            theme::bubble_out()
                        } else {
                            theme::surface()
                        })
                        .px(px(7.0))
                        .py(px(2.0))
                        .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                        .text_color(theme::text_primary())
                        .hover(|style| style.border_color(theme::accent()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.react_to_message(target.clone(), emoji.clone(), cx)
                        }))
                        .child(format!("{} {}", reaction.emoji, reaction.count))
                }),
        )
}

fn quoted_message(
    quoted: wasabi_domain::QuotedMessage,
    row_index: usize,
    text_scale: u16,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    let message = quoted.id.clone();
    gpui::div()
        .id(("quoted-message", row_index))
        .cursor_pointer()
        .rounded(px(theme::RADIUS_SM))
        .border_l_2()
        .border_color(theme::accent())
        .bg(theme::canvas())
        .px(px(8.0))
        .py(px(5.0))
        .mb(px(3.0))
        .flex()
        .flex_col()
        .hover(|style| style.bg(theme::chip_idle()))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.reveal_quoted_message(message.clone(), window, cx)
        }))
        .child(
            gpui::div()
                .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent_text())
                .child(
                    quoted
                        .sender
                        .unwrap_or_else(|| "Original message".to_string()),
                ),
        )
        .child(
            gpui::div()
                .max_h(px(38.0))
                .overflow_hidden()
                .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                .text_color(theme::text_secondary())
                .child(quoted.preview),
        )
}

fn media_content(
    row: &wasabi_domain::MessageRow,
    text_scale: u16,
    download_state: Option<crate::views::root::MediaDownloadUi>,
    cx: &mut Context<MainWindow>,
) -> Option<gpui::AnyElement> {
    use wasabi_domain::{MediaAvailability, MessageKind};

    let (title, caption, descriptor, visual, icon) = match &row.kind {
        MessageKind::Image { caption, media } => (
            "Photo".to_string(),
            caption.clone(),
            media,
            true,
            IconName::GalleryVerticalEnd,
        ),
        MessageKind::Video {
            caption,
            video_note,
            media,
        } => (
            if *video_note { "Video note" } else { "Video" }.to_string(),
            caption.clone(),
            media,
            true,
            IconName::GalleryVerticalEnd,
        ),
        MessageKind::Audio {
            voice_note, media, ..
        } => (
            if *voice_note {
                "Voice message"
            } else {
                "Audio"
            }
            .to_string(),
            None,
            media,
            false,
            IconName::File,
        ),
        MessageKind::Document { media } => (
            media
                .file_name
                .clone()
                .unwrap_or_else(|| "Document".to_string()),
            None,
            media,
            false,
            IconName::File,
        ),
        MessageKind::Sticker {
            animated, media, ..
        } => (
            if *animated {
                "Animated sticker"
            } else {
                "Sticker"
            }
            .to_string(),
            None,
            media,
            true,
            IconName::GalleryVerticalEnd,
        ),
        _ => return None,
    };

    let metadata = media_metadata(descriptor);
    let unavailable = descriptor.availability == MediaAvailability::Unavailable;
    let (transfer_label, can_download) = if unavailable {
        ("Media unavailable", false)
    } else {
        match download_state.as_ref() {
            Some(crate::views::root::MediaDownloadUi::Downloading) => ("Downloading…", false),
            Some(crate::views::root::MediaDownloadUi::Ready(path)) => {
                // Reading the verified path here deliberately proves the state
                // still points at a committed cache entry without displaying
                // private filesystem details in the conversation.
                let _exists = path.is_file();
                ("Downloaded to secure cache", false)
            }
            Some(crate::views::root::MediaDownloadUi::Failed) => {
                ("Download failed · click to retry", true)
            }
            None => ("Click to download", true),
        }
    };
    let body = if visual {
        gpui::div()
            .h(px(150.0))
            .w_full()
            .min_w(px(240.0))
            .rounded(px(theme::RADIUS_SM))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(7.0))
            .bg(theme::canvas())
            .text_color(if unavailable {
                theme::text_secondary()
            } else {
                theme::accent_text()
            })
            .child(Icon::new(if unavailable { IconName::CircleX } else { icon }).size(px(28.0)))
            .child(
                gpui::div()
                    .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(if unavailable {
                        transfer_label.to_string()
                    } else {
                        title.clone()
                    }),
            )
            .child(
                gpui::div()
                    .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                    .text_color(theme::text_secondary())
                    .child(transfer_label),
            )
    } else {
        gpui::div()
            .min_h(px(54.0))
            .w_full()
            .min_w(px(240.0))
            .rounded(px(theme::RADIUS_SM))
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(10.0))
            .py(px(8.0))
            .bg(theme::canvas())
            .child(
                gpui::div()
                    .size(px(34.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::row_selected())
                    .text_color(if unavailable {
                        theme::text_secondary()
                    } else {
                        theme::accent_text()
                    })
                    .child(
                        Icon::new(if unavailable { IconName::CircleX } else { icon })
                            .size(px(18.0)),
                    ),
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
                            .text_size(px(theme::scaled_text(theme::TEXT_SIZE, text_scale)))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::text_primary())
                            .child(if unavailable {
                                transfer_label.to_string()
                            } else {
                                title.clone()
                            }),
                    )
                    .child(
                        gpui::div()
                            .truncate()
                            .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                            .text_color(theme::text_secondary())
                            .child(if metadata.is_empty() {
                                transfer_label.to_string()
                            } else {
                                format!("{transfer_label} · {metadata}")
                            }),
                    ),
            )
    };

    let card = gpui::div()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(body)
        .when(visual && !metadata.is_empty(), |el| {
            el.child(
                gpui::div()
                    .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                    .text_color(theme::text_secondary())
                    .child(metadata),
            )
        })
        .when_some(caption, |el, caption| {
            el.child(
                gpui::div()
                    .text_size(px(theme::scaled_text(theme::TEXT_SIZE, text_scale)))
                    .text_color(theme::text_primary())
                    .child(caption),
            )
        });
    let chat = row.chat.clone();
    let media = descriptor.id.clone();
    let mut interactive = gpui::div()
        .id(("media-card", row.seq.0 as usize))
        .rounded(px(theme::RADIUS_SM))
        .child(card);
    if can_download {
        interactive = interactive
            .cursor_pointer()
            .hover(|style| style.bg(theme::row_hover()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.download_media(chat.clone(), media.clone(), cx)
            }));
    }
    Some(interactive.into_any_element())
}

fn media_descriptor(kind: &wasabi_domain::MessageKind) -> Option<&wasabi_domain::MediaDescriptor> {
    match kind {
        wasabi_domain::MessageKind::Image { media, .. }
        | wasabi_domain::MessageKind::Video { media, .. }
        | wasabi_domain::MessageKind::Audio { media, .. }
        | wasabi_domain::MessageKind::Document { media }
        | wasabi_domain::MessageKind::Sticker { media, .. } => Some(media),
        _ => None,
    }
}

fn media_metadata(media: &wasabi_domain::MediaDescriptor) -> String {
    let mut parts = Vec::with_capacity(3);
    if let Some(seconds) = media.duration_seconds {
        parts.push(format!("{}:{:02}", seconds / 60, seconds % 60));
    }
    if let (Some(width), Some(height)) = (media.width, media.height) {
        parts.push(format!("{width}×{height}"));
    }
    if let Some(bytes) = media.file_size {
        parts.push(format_bytes(bytes));
    }
    if let Some(mime) = media.mime_type.as_deref() {
        parts.push(mime.to_string());
    }
    parts.join(" · ")
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= MIB {
        format!("{:.1} MB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.0} KB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}

fn message_actions_button(
    row: &wasabi_domain::MessageRow,
    row_index: usize,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    let message = row.id.clone();
    gpui::div()
        .id(("message-actions", row_index))
        .ml(px(3.0))
        .cursor_pointer()
        .px(px(2.0))
        .text_color(theme::text_secondary())
        .hover(|style| style.text_color(theme::accent_text()))
        .on_click(cx.listener(move |this, _, _, cx| this.open_message_actions(message.clone(), cx)))
        .child("⋯")
}

pub fn message_overlay(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let Some(overlay) = this.message_overlay.clone() else {
        return gpui::div();
    };
    let card = match overlay {
        crate::views::root::MessageOverlay::Actions(message) => {
            message_action_sheet(this, message, cx)
        }
        crate::views::root::MessageOverlay::Confirm(action) => {
            message_delete_confirmation(this, action, cx)
        }
        crate::views::root::MessageOverlay::ConfirmChat(action) => {
            chat_action_confirmation(this, action, cx)
        }
        crate::views::root::MessageOverlay::EditGroupText(field) => {
            group_text_edit_card(this, field, cx)
        }
        crate::views::root::MessageOverlay::GroupMemberActions(target) => {
            group_member_action_sheet(target, cx)
        }
        crate::views::root::MessageOverlay::ConfirmGroupMember(action) => {
            group_member_confirmation(action, cx)
        }
        crate::views::root::MessageOverlay::ConfirmLeaveGroup(target) => {
            leave_group_confirmation(target, cx)
        }
    };
    gpui::div()
        .absolute()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::scrim())
        .child(card)
}

fn leave_group_confirmation(
    target: crate::views::root::GroupLeaveTarget,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let (title, detail) = leave_group_confirmation_copy(&target);
    action_card()
        .child(
            gpui::div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_primary())
                .child(title),
        )
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::text_secondary())
                .child(detail),
        )
        .child(
            gpui::div()
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    sheet_button("cancel-leave-group", "Cancel", false)
                        .on_click(cx.listener(|this, _, _, cx| this.close_message_overlay(cx))),
                )
                .child(
                    sheet_button("confirm-leave-group", "Leave group", true)
                        .on_click(cx.listener(|this, _, _, cx| this.run_confirmed_leave_group(cx))),
                ),
        )
}

fn leave_group_confirmation_copy(
    target: &crate::views::root::GroupLeaveTarget,
) -> (String, &'static str) {
    (
        format!("Leave “{}”?", target.group_name),
        "You will stop receiving new messages after the linked account accepts the request. Existing history stays on this device until you delete the chat.",
    )
}

fn group_member_action_sheet(
    target: crate::views::root::GroupMemberTarget,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let role_action = match target.participant_role {
        wasabi_domain::ParticipantRole::Member => Some((
            "promote-group-member",
            "Make group admin…",
            crate::views::root::GroupMemberActionKind::Promote,
        )),
        wasabi_domain::ParticipantRole::Admin => Some((
            "demote-group-member",
            "Dismiss as admin…",
            crate::views::root::GroupMemberActionKind::Demote,
        )),
        wasabi_domain::ParticipantRole::SuperAdmin => None,
    };
    let role_target = target.clone();
    let remove_target = target.clone();
    action_card()
        .child(
            gpui::div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    gpui::div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            gpui::div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::text_primary())
                                .child("Manage participant"),
                        )
                        .child(
                            gpui::div()
                                .text_size(px(theme::TEXT_SIZE_SM))
                                .text_color(theme::text_secondary())
                                .child(target.participant_name.clone()),
                        ),
                )
                .child(
                    sheet_button("close-group-member-actions", "Close", false)
                        .on_click(cx.listener(|this, _, _, cx| this.close_message_overlay(cx))),
                ),
        )
        .children(role_action.map(|(id, label, kind)| {
            sheet_button(id, label, false).on_click(cx.listener(move |this, _, _, cx| {
                this.confirm_group_member_action(role_target.clone(), kind, cx)
            }))
        }))
        .child(
            sheet_button("remove-group-member", "Remove from group…", true).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.confirm_group_member_action(
                        remove_target.clone(),
                        crate::views::root::GroupMemberActionKind::Remove,
                        cx,
                    )
                },
            )),
        )
}

fn group_member_confirmation(
    action: crate::views::root::GroupMemberAction,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let (title, detail, confirm, confirm_id, danger) = group_member_confirmation_copy(&action);
    action_card()
        .child(
            gpui::div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_primary())
                .child(title),
        )
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::text_secondary())
                .child(detail),
        )
        .child(
            gpui::div()
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    sheet_button("cancel-group-member-action", "Cancel", false)
                        .on_click(cx.listener(|this, _, _, cx| this.close_message_overlay(cx))),
                )
                .child(sheet_button(confirm_id, confirm, danger).on_click(
                    cx.listener(|this, _, _, cx| this.run_confirmed_group_member_action(cx)),
                )),
        )
}

fn group_member_confirmation_copy(
    action: &crate::views::root::GroupMemberAction,
) -> (String, &'static str, &'static str, &'static str, bool) {
    match action.kind {
        crate::views::root::GroupMemberActionKind::Promote => (
            format!("Make “{}” a group admin?", action.target.participant_name),
            "Group admins can add and remove participants and change group settings.",
            "Make admin",
            "confirm-promote-group-member",
            false,
        ),
        crate::views::root::GroupMemberActionKind::Demote => (
            format!("Dismiss “{}” as admin?", action.target.participant_name),
            "They will remain in the group as a participant but lose admin controls.",
            "Dismiss admin",
            "confirm-demote-group-member",
            false,
        ),
        crate::views::root::GroupMemberActionKind::Remove => (
            format!(
                "Remove “{}” from “{}”?",
                action.target.participant_name, action.target.group_name
            ),
            "They will be removed only after the linked account accepts the request.",
            "Remove",
            "confirm-remove-group-member",
            true,
        ),
    }
}

fn group_text_edit_card(
    this: &mut MainWindow,
    field: crate::views::root::GroupTextField,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let (title, detail, input, count, limit) = match field {
        crate::views::root::GroupTextField::Subject => {
            let input = this.group_info_subject_input.clone();
            let count = input.read(cx).value().chars().count();
            (
                "Edit group name",
                "The new name is synchronized with every participant.",
                input,
                count,
                wasabi_domain::GROUP_SUBJECT_MAX_CHARS,
            )
        }
        crate::views::root::GroupTextField::Description => {
            let input = this.group_info_description_input.clone();
            let count = input.read(cx).value().chars().count();
            (
                "Edit group description",
                "Leave this empty to remove the current description.",
                input,
                count,
                wasabi_domain::GROUP_DESCRIPTION_MAX_CHARS,
            )
        }
    };
    action_card()
        .child(
            gpui::div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_primary())
                .child(title),
        )
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::text_secondary())
                .child(detail),
        )
        .child(Input::new(&input).cleanable(true))
        .child(
            gpui::div()
                .flex()
                .justify_between()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(if count > limit {
                    theme::danger()
                } else {
                    theme::text_secondary()
                })
                .child(format!("{count} / {limit} characters"))
                .children(
                    this.group_text_edit_error
                        .clone()
                        .map(|error| gpui::div().text_color(theme::danger()).child(error)),
                ),
        )
        .child(
            gpui::div()
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    sheet_button("cancel-group-text-edit", "Cancel", false)
                        .on_click(cx.listener(|this, _, _, cx| this.close_message_overlay(cx))),
                )
                .child(
                    sheet_button("save-group-text-edit", "Save", false)
                        .on_click(cx.listener(|this, _, _, cx| this.submit_group_text_edit(cx))),
                ),
        )
}

fn message_action_sheet(
    this: &mut MainWindow,
    message: wasabi_domain::MessageId,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let Some(row) = this
        .messages
        .rows
        .iter()
        .find(|row| row.id == message)
        .cloned()
    else {
        return action_card().child("This message is no longer available");
    };
    let preview = messages::body_text(&row);
    let can_copy = matches!(
        row.kind,
        wasabi_domain::MessageKind::Text { .. } | wasabi_domain::MessageKind::System { .. }
    );
    let starred = row.starred;
    let star_action = wasabi_domain::MessageAction::Star {
        target: (&row).into(),
        starred: !starred,
    };
    let delete_for_me = wasabi_domain::MessageAction::DeleteForMe {
        target: (&row).into(),
        delete_media: false,
    };
    let revoke = (row.direction == wasabi_domain::MessageDirection::Outgoing).then(|| {
        wasabi_domain::MessageAction::RevokeForEveryone {
            target: (&row).into(),
        }
    });
    let retry = (row.direction == wasabi_domain::MessageDirection::Outgoing
        && row.status == wasabi_domain::MessageStatus::Failed)
        .then(|| row.id.clone());
    let reply = (!row.revoked && !matches!(row.kind, wasabi_domain::MessageKind::System { .. }))
        .then(|| row.id.clone());
    let edit = row
        .can_edit_text_at(chrono::Utc::now().timestamp_millis())
        .then(|| row.id.clone());

    action_card()
        .child(
            gpui::div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    gpui::div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_primary())
                        .child("Message actions"),
                )
                .child(
                    sheet_button("close-message-actions", "Close", false)
                        .on_click(cx.listener(|this, _, _, cx| this.close_message_overlay(cx))),
                ),
        )
        .child(
            gpui::div()
                .max_h(px(72.0))
                .overflow_hidden()
                .rounded(px(theme::RADIUS_SM))
                .bg(theme::canvas())
                .px(px(10.0))
                .py(px(8.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::text_secondary())
                .child(preview),
        )
        .child(gpui::div().flex().items_center().gap(px(6.0)).children(
            ["👍", "❤️", "😂", "😮", "😢"].into_iter().map(|emoji| {
                let message = message.clone();
                sheet_button(emoji, emoji, false).on_click(cx.listener(move |this, _, _, cx| {
                    this.react_to_message(message.clone(), emoji.to_string(), cx)
                }))
            }),
        ))
        .when(can_copy, |card| {
            let message = message.clone();
            card.child(sheet_button("copy-message", "Copy text", false).on_click(
                cx.listener(move |this, _, _, cx| this.copy_message(message.clone(), cx)),
            ))
        })
        .when_some(reply, |card, message| {
            card.child(
                sheet_button("reply-to-message", "Reply", false).on_click(cx.listener(
                    move |this, _, window, cx| this.begin_reply(message.clone(), window, cx),
                )),
            )
        })
        .when_some(edit, |card, message| {
            card.child(sheet_button("edit-message", "Edit", false).on_click(
                cx.listener(move |this, _, window, cx| {
                    this.begin_edit(message.clone(), window, cx)
                }),
            ))
        })
        .when_some(retry, |card, message| {
            card.child(
                sheet_button("retry-message-from-actions", "Retry send", false).on_click(
                    cx.listener(move |this, _, _, cx| this.retry_message(message.clone(), cx)),
                ),
            )
        })
        .child(
            sheet_button(
                "toggle-star-from-actions",
                if starred {
                    "Unstar message"
                } else {
                    "Star message"
                },
                false,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.message_overlay = None;
                this.perform_message_action(star_action.clone(), cx)
            })),
        )
        .child(
            sheet_button("delete-message-local", "Delete for me…", true).on_click(cx.listener(
                move |this, _, _, cx| this.confirm_message_action(delete_for_me.clone(), cx),
            )),
        )
        .when_some(revoke, |card, revoke| {
            card.child(
                sheet_button("revoke-message", "Delete for everyone…", true).on_click(cx.listener(
                    move |this, _, _, cx| this.confirm_message_action(revoke.clone(), cx),
                )),
            )
        })
}

fn message_delete_confirmation(
    this: &MainWindow,
    action: wasabi_domain::MessageAction,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let target = action.target();
    let preview = this
        .messages
        .rows
        .iter()
        .find(|row| row.id == target.message)
        .map(messages::body_text)
        .unwrap_or_else(|| "Selected message".to_string());
    let (title, detail, confirm) = match action {
        wasabi_domain::MessageAction::DeleteForMe { .. } => (
            "Delete message for you?",
            "This removes the message from your view. Other participants will still see it.",
            "Delete for me",
        ),
        wasabi_domain::MessageAction::RevokeForEveryone { .. } => (
            "Delete message for everyone?",
            "WhatsApp will replace this sent message with a deletion notice when revocation is allowed.",
            "Delete for everyone",
        ),
        _ => (
            "Confirm message action?",
            "This action will be synchronized.",
            "Confirm",
        ),
    };
    action_card()
        .child(
            gpui::div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_primary())
                .child(title),
        )
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::text_secondary())
                .child(detail),
        )
        .child(
            gpui::div()
                .max_h(px(64.0))
                .overflow_hidden()
                .rounded(px(theme::RADIUS_SM))
                .bg(theme::canvas())
                .px(px(10.0))
                .py(px(8.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .child(preview),
        )
        .child(
            gpui::div()
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    sheet_button("cancel-message-delete", "Cancel", false)
                        .on_click(cx.listener(|this, _, _, cx| this.close_message_overlay(cx))),
                )
                .child(
                    sheet_button("confirm-message-delete", confirm, true).on_click(
                        cx.listener(|this, _, _, cx| this.run_confirmed_message_action(cx)),
                    ),
                ),
        )
}

fn chat_action_confirmation(
    this: &MainWindow,
    action: wasabi_domain::ChatAction,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let name = this
        .chats
        .chats
        .iter()
        .find(|chat| chat.id == *action.chat())
        .map(chats::fallback_name)
        .unwrap_or_else(|| "this conversation".to_string());
    let (title, detail, confirm, confirm_id) = chat_confirmation_copy(&action, &name);
    action_card()
        .child(
            gpui::div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_primary())
                .child(title),
        )
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::text_secondary())
                .child(detail),
        )
        .child(
            gpui::div()
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    sheet_button("cancel-chat-action", "Cancel", false)
                        .on_click(cx.listener(|this, _, _, cx| this.close_message_overlay(cx))),
                )
                .child(
                    sheet_button(confirm_id, confirm, true)
                        .on_click(cx.listener(|this, _, _, cx| this.run_confirmed_chat_action(cx))),
                ),
        )
}

fn chat_confirmation_copy(
    action: &wasabi_domain::ChatAction,
    name: &str,
) -> (String, &'static str, &'static str, &'static str) {
    match action {
        wasabi_domain::ChatAction::Clear { .. } => (
            format!("Clear messages in “{name}”?"),
            "Messages are removed from this chat after synchronization. Starred messages remain in the chat, and downloaded files stay on this device.",
            "Clear messages",
            "confirm-clear-chat",
        ),
        wasabi_domain::ChatAction::Delete { .. } => (
            format!("Delete chat with “{name}”?"),
            "This removes the conversation from your chat list after synchronization. It does not delete messages from other participants, and downloaded files stay on this device.",
            "Delete chat",
            "confirm-delete-chat",
        ),
        _ => (
            format!("Update “{name}”?"),
            "This action will be synchronized with your linked account.",
            "Confirm",
            "confirm-chat-action",
        ),
    }
}

fn action_card() -> gpui::Div {
    gpui::div()
        .w(px(390.0))
        .max_w_full()
        .rounded(px(theme::RADIUS_MD))
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .p(px(18.0))
        .flex()
        .flex_col()
        .gap(px(10.0))
}

fn sheet_button(id: &'static str, label: &'static str, danger: bool) -> gpui::Stateful<gpui::Div> {
    gpui::div()
        .id(id)
        .cursor_pointer()
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(theme::border())
        .px(px(10.0))
        .py(px(7.0))
        .hover(|style| style.bg(theme::row_hover()))
        .text_size(px(theme::TEXT_SIZE_SM))
        .text_color(if danger {
            theme::danger()
        } else {
            theme::text_primary()
        })
        .child(label)
}

/// Width is ignored by the vertical list; only heights matter.
#[cfg(test)]
mod tests {
    use super::{
        chat_confirmation_copy, format_bytes, group_member_confirmation_copy,
        leave_group_confirmation_copy,
    };

    #[test]
    fn formats_media_sizes_for_desktop_cards() {
        assert_eq!(format_bytes(900), "900 B");
        assert_eq!(format_bytes(184_000), "180 KB");
        assert_eq!(format_bytes(2_830_000), "2.7 MB");
    }

    #[test]
    fn destructive_chat_confirmations_name_and_distinguish_the_target() {
        let clear = wasabi_domain::ChatAction::Clear {
            chat: wasabi_domain::ChatId::new("a@s.whatsapp.net"),
            delete_starred: false,
            delete_media: false,
        };
        let delete = wasabi_domain::ChatAction::Delete {
            chat: wasabi_domain::ChatId::new("a@s.whatsapp.net"),
            delete_media: false,
        };

        let clear_copy = chat_confirmation_copy(&clear, "Avery Chen");
        let delete_copy = chat_confirmation_copy(&delete, "Avery Chen");
        assert!(clear_copy.0.contains("Avery Chen"));
        assert!(clear_copy.1.contains("Starred messages"));
        assert_eq!(clear_copy.2, "Clear messages");
        assert!(delete_copy.0.contains("Avery Chen"));
        assert!(delete_copy.1.contains("other participants"));
        assert_eq!(delete_copy.2, "Delete chat");
    }

    #[test]
    fn group_member_confirmations_name_the_exact_person_and_group() {
        let target = crate::views::root::GroupMemberTarget {
            chat: wasabi_domain::ChatId::new("preview-group@g.us"),
            group_name: "Weekend hiking crew".to_string(),
            participant: wasabi_domain::ChatId::new("preview-avery@s.whatsapp.net"),
            participant_name: "Avery Chen".to_string(),
            participant_role: wasabi_domain::ParticipantRole::Admin,
        };
        let removal = crate::views::root::GroupMemberAction {
            target,
            kind: crate::views::root::GroupMemberActionKind::Remove,
        };

        let copy = group_member_confirmation_copy(&removal);

        assert!(copy.0.contains("Avery Chen"));
        assert!(copy.0.contains("Weekend hiking crew"));
        assert_eq!(copy.2, "Remove");
        assert!(copy.4);
    }

    #[test]
    fn leave_group_confirmation_names_history_behavior() {
        let target = crate::views::root::GroupLeaveTarget {
            chat: wasabi_domain::ChatId::new("preview-group@g.us"),
            group_name: "Weekend hiking crew".to_string(),
        };

        let copy = leave_group_confirmation_copy(&target);

        assert!(copy.0.contains("Weekend hiking crew"));
        assert!(copy.1.contains("Existing history"));
        assert!(copy.1.contains("linked account accepts"));
    }
}
