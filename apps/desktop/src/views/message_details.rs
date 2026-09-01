//! Reaction and receipt detail overlays. Names come from the store projection;
//! empty lists stay empty instead of inventing people.

use gpui::prelude::*;
use gpui::{Context, px};

use crate::state::messages;
use crate::theme;
use crate::views::root::{MainWindow, MessageDetailsLoad};

pub(crate) const REACTION_EMPTY: &str = "No reactions to show";
pub(crate) const RECEIPT_EMPTY: &str = "Receipt details are not available yet";
pub(crate) const DETAILS_LOADING: &str = "Loading…";
pub(crate) const DETAILS_ERROR: &str = "Couldn’t load details";

pub(crate) fn reaction_overlay(
    actors: MessageDetailsLoad<Vec<wasabi_domain::ReactionActor>>,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    details_card(
        "Reactions",
        "close-reaction-details",
        match &actors {
            MessageDetailsLoad::Loading => vec![status_line(DETAILS_LOADING)],
            MessageDetailsLoad::Failed(_) => vec![status_line(DETAILS_ERROR)],
            MessageDetailsLoad::Ready(actors) if actors.is_empty() => {
                vec![status_line(REACTION_EMPTY)]
            }
            MessageDetailsLoad::Ready(actors) => reaction_groups(actors),
        },
        cx,
    )
}

pub(crate) fn receipt_overlay(
    actors: MessageDetailsLoad<Vec<wasabi_domain::ReceiptActor>>,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    details_card(
        "Receipt details",
        "close-receipt-details",
        match &actors {
            MessageDetailsLoad::Loading => vec![status_line(DETAILS_LOADING)],
            MessageDetailsLoad::Failed(_) => vec![status_line(DETAILS_ERROR)],
            MessageDetailsLoad::Ready(actors) if actors.is_empty() => {
                vec![status_line(RECEIPT_EMPTY)]
            }
            MessageDetailsLoad::Ready(actors) => receipt_groups(actors),
        },
        cx,
    )
}

pub(crate) fn group_reaction_actors(
    actors: &[wasabi_domain::ReactionActor],
) -> Vec<(String, Vec<&wasabi_domain::ReactionActor>)> {
    let mut groups = Vec::<(String, Vec<&wasabi_domain::ReactionActor>)>::new();
    for actor in actors {
        if let Some((_, members)) = groups.iter_mut().find(|(emoji, _)| emoji == &actor.emoji) {
            members.push(actor);
        } else {
            groups.push((actor.emoji.clone(), vec![actor]));
        }
    }
    groups
}

pub(crate) fn group_receipt_actors(
    actors: &[wasabi_domain::ReceiptActor],
) -> Vec<(&'static str, Vec<&wasabi_domain::ReceiptActor>)> {
    let mut read = Vec::new();
    let mut delivered = Vec::new();
    for actor in actors {
        match actor.status {
            wasabi_domain::MessageStatus::Read => read.push(actor),
            wasabi_domain::MessageStatus::Delivered => delivered.push(actor),
            _ => {}
        }
    }
    let mut groups = Vec::new();
    if !read.is_empty() {
        groups.push(("Read", read));
    }
    if !delivered.is_empty() {
        groups.push(("Delivered", delivered));
    }
    groups
}

fn details_card(
    title: &'static str,
    close_id: &'static str,
    body: Vec<gpui::AnyElement>,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    gpui::div()
        .w(px(360.0))
        .max_w_full()
        .max_h(px(420.0))
        .rounded(px(theme::RADIUS_MODAL))
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_elevated())
        .shadow(theme::modal_shadow())
        .px(px(20.0))
        .pt(px(18.0))
        .pb(px(16.0))
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(
            gpui::div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    gpui::div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_primary())
                        .child(title),
                )
                .child(
                    gpui::div()
                        .id(close_id)
                        .cursor_pointer()
                        .rounded(px(theme::RADIUS_MD))
                        .border_1()
                        .border_color(theme::border())
                        .px(px(10.0))
                        .py(px(5.0))
                        .hover(|style| style.bg(theme::row_hover()))
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_primary())
                        .on_click(cx.listener(|this, _, _, cx| this.close_message_overlay(cx)))
                        .child("Close"),
                ),
        )
        .child(
            gpui::div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .overflow_hidden()
                .children(body),
        )
}

fn status_line(copy: &'static str) -> gpui::AnyElement {
    gpui::div()
        .text_size(px(theme::TEXT_SIZE_SM))
        .text_color(theme::text_secondary())
        .child(copy)
        .into_any_element()
}

fn reaction_groups(actors: &[wasabi_domain::ReactionActor]) -> Vec<gpui::AnyElement> {
    group_reaction_actors(actors)
        .into_iter()
        .map(|(emoji, members)| {
            gpui::div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    gpui::div()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::text_primary())
                        .child(format!("{emoji} · {}", members.len())),
                )
                .children(members.into_iter().map(|actor| {
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .child(actor.display_name.clone())
                }))
                .into_any_element()
        })
        .collect()
}

fn receipt_groups(actors: &[wasabi_domain::ReceiptActor]) -> Vec<gpui::AnyElement> {
    group_receipt_actors(actors)
        .into_iter()
        .map(|(label, members)| {
            gpui::div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    gpui::div()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::text_primary())
                        .child(label),
                )
                .children(members.into_iter().map(|actor| {
                    gpui::div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.0))
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .child(actor.display_name.clone())
                        .child(messages::relative_time(actor.timestamp_ms))
                }))
                .into_any_element()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{REACTION_EMPTY, RECEIPT_EMPTY, group_reaction_actors, group_receipt_actors};

    fn reaction(name: &str, emoji: &str, is_self: bool) -> wasabi_domain::ReactionActor {
        wasabi_domain::ReactionActor {
            display_name: name.to_string(),
            emoji: emoji.to_string(),
            is_self,
        }
    }

    fn receipt(
        name: &str,
        status: wasabi_domain::MessageStatus,
        timestamp_ms: i64,
    ) -> wasabi_domain::ReceiptActor {
        wasabi_domain::ReceiptActor {
            display_name: name.to_string(),
            status,
            timestamp_ms,
        }
    }

    #[test]
    fn two_senders_with_the_same_emoji_stay_two_people() {
        let actors = [
            reaction("Alice", "👍", false),
            reaction("You", "👍", true),
            reaction("Cara", "❤️", false),
        ];
        let groups = group_reaction_actors(&actors);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "👍");
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[0].1[0].display_name, "Alice");
        assert_eq!(groups[0].1[1].display_name, "You");
        assert_eq!(groups[1].1.len(), 1);
        assert_eq!(REACTION_EMPTY, "No reactions to show");
    }

    #[test]
    fn receipt_groups_split_read_and_delivered_without_inventing_rows() {
        let actors = [
            receipt("Alice", wasabi_domain::MessageStatus::Read, 2),
            receipt("Bob", wasabi_domain::MessageStatus::Delivered, 1),
        ];
        let groups = group_receipt_actors(&actors);
        assert_eq!(groups[0].0, "Read");
        assert_eq!(groups[0].1.len(), 1);
        assert_eq!(groups[1].0, "Delivered");
        assert_eq!(groups[1].1.len(), 1);
        assert!(group_receipt_actors(&[]).is_empty());
        assert_eq!(RECEIPT_EMPTY, "Receipt details are not available yet");
    }
}
