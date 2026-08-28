//! Two-step, cache-first creation flow for real protocol groups.

use gpui::prelude::*;
use gpui::{Context, px};
use gpui_component::input::Input;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{Icon, IconName};

use crate::theme;
use crate::views::root::{MainWindow, NewChatMode};

pub fn overlay(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let content = match this.new_chat_mode {
        NewChatMode::GroupParticipants | NewChatMode::AddGroupMembers => {
            participant_step(this, cx)
        }
        NewChatMode::GroupSubject => subject_step(this, cx),
        NewChatMode::Direct => gpui::div(),
    };
    let creating = this.group_creating;

    gpui::div()
        .absolute()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui::div()
                .id("new-group-scrim")
                .absolute()
                .size_full()
                .occlude()
                .bg(theme::scrim())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if !creating {
                        this.close_new_chat(cx);
                    }
                })),
        )
        .child(
            gpui::div()
                .relative()
                .occlude()
                .w(px(460.0))
                .h(px(650.0))
                .max_w_full()
                .max_h_full()
                .rounded(px(theme::RADIUS_MD))
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .flex()
                .flex_col()
                .child(group_header(this, cx))
                .child(content),
        )
}

fn group_header(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let title = match this.new_chat_mode {
        NewChatMode::GroupParticipants => "Add group members",
        NewChatMode::AddGroupMembers => "Add members",
        NewChatMode::GroupSubject => "New group",
        NewChatMode::Direct => "New chat",
    };
    let creating = this.group_creating;
    gpui::div()
        .h(px(58.0))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(8.0))
        .px(px(12.0))
        .child(
            gpui::div()
                .id("new-group-back")
                .size(px(34.0))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .when(!creating, |button| {
                    button
                        .cursor_pointer()
                        .aria_label("Back")
                        .hover(|button| button.bg(theme::row_hover()))
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.back_new_group(window, cx)
                        }))
                })
                .child(Icon::new(IconName::ArrowLeft).size(px(17.0))),
        )
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_TITLE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
        .when(!creating, |header| {
            header.child(
                gpui::div()
                    .id("close-new-group")
                    .size(px(34.0))
                    .rounded_full()
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .aria_label("Close New group")
                    .hover(|button| button.bg(theme::row_hover()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.close_new_chat(cx)
                    }))
                    .child(Icon::new(IconName::Close).size(px(16.0))),
            )
        })
}

fn participant_step(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let adding_to_existing_group = this.new_chat_mode == NewChatMode::AddGroupMembers;
    let existing_members = this
        .conversation_details
        .as_ref()
        .and_then(|details| match details {
            wasabi_domain::ConversationDetails::Group(details) => Some(
                details
                    .participants
                    .iter()
                    .map(|participant| participant.jid.as_str())
                    .collect::<std::collections::HashSet<_>>(),
            ),
            wasabi_domain::ConversationDetails::Direct(_) => None,
        })
        .unwrap_or_default();
    let available_contacts = available_group_contacts(&this.contacts, &existing_members);
    let selected_count = this.group_participants.len();
    let selected_names = this
        .group_participants
        .iter()
        .take(3)
        .map(|contact| contact.display_name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let selected_summary = if selected_count == 0 {
        "Select at least one contact".to_string()
    } else if selected_count > 3 {
        format!("{selected_names} +{} more", selected_count - 3)
    } else {
        selected_names
    };

    let mut list = gpui::div()
        .flex_1()
        .min_h(px(0.0))
        .overflow_y_scrollbar()
        .border_t_1()
        .border_color(theme::border());
    if let Some(error) = this.contacts_error.clone() {
        list = list.child(state_message("Contacts unavailable", error));
    } else if this.contacts.is_empty() && this.contacts_loading {
        list = list.child(state_message(
            "Loading contacts…",
            "Reading the encrypted local account cache",
        ));
    } else if available_contacts.is_empty() {
        list = list.child(state_message(
            if adding_to_existing_group {
                "No new contacts found"
            } else {
                "No contacts found"
            },
            if adding_to_existing_group {
                "Everyone in these results is already in the group. Try another name."
            } else {
                "Try another name or wait for contact synchronization."
            },
        ));
    } else {
        for (index, contact) in available_contacts.into_iter().enumerate() {
            let selected = this
                .group_participants
                .iter()
                .any(|candidate| candidate.jid == contact.jid);
            let initial = contact
                .display_name
                .chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string();
            let label = contact.display_name.clone();
            let phone = contact.phone_number.clone();
            list = list.child(
                gpui::div()
                    .id(("new-group-contact", index))
                    .min_h(px(62.0))
                    .px(px(14.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .cursor_pointer()
                    .aria_label(format!(
                        "{} {label}",
                        if selected { "Remove" } else { "Select" }
                    ))
                    .when(selected, |row| row.bg(theme::row_selected()))
                    .hover(|row| row.bg(theme::row_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_group_participant(contact.clone(), cx)
                    }))
                    .child(
                        gpui::div()
                            .size(px(40.0))
                            .flex_shrink_0()
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme::sender_color(&label))
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
                            .child(
                                gpui::div()
                                    .truncate()
                                    .text_size(px(theme::TEXT_NAME))
                                    .text_color(theme::text_primary())
                                    .child(label),
                            )
                            .children(phone.map(|phone| {
                                gpui::div()
                                    .text_size(px(theme::TEXT_SIZE_SM))
                                    .text_color(theme::text_secondary())
                                    .child(format!("+{phone}"))
                            })),
                    )
                    .child(selection_mark(selected)),
            );
        }
        if this.contacts_loading {
            list = list.child(state_message("Loading more…", ""));
        } else if this.contacts_next.is_some() {
            list = list.child(
                gpui::div()
                    .id("load-more-group-contacts")
                    .h(px(44.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_size(px(theme::TEXT_SIZE_SM))
                    .text_color(theme::accent_text())
                    .hover(|row| row.bg(theme::row_hover()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.load_more_contacts(cx)
                    }))
                    .child("Load more contacts"),
            );
        }
    }

    let footer = if adding_to_existing_group {
        group_footer(
            this.group_creation_error.clone(),
            if this.group_creating { "Adding…" } else { "Add" },
            selected_count > 0 && !this.group_creating && !this.group_creation_uncertain,
            cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.submit_add_group_members(cx)
            }),
        )
    } else {
        group_footer(
            this.group_creation_error.clone(),
            "Next",
            selected_count > 0,
            cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.continue_new_group(window, cx)
            }),
        )
    };

    gpui::div()
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .child(
            gpui::div()
                .px(px(14.0))
                .pb(px(10.0))
                .child(
                    Input::new(&this.contact_search_input)
                        .prefix(Icon::new(IconName::Search).size(px(16.0)))
                        .cleanable(true),
                ),
        )
        .child(
            gpui::div()
                .min_h(px(48.0))
                .px(px(16.0))
                .py(px(8.0))
                .border_t_1()
                .border_color(theme::border())
                .flex()
                .flex_col()
                .justify_center()
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(if selected_count == 0 {
                            theme::text_secondary()
                        } else {
                            theme::accent_text()
                        })
                        .child(format!("{selected_count} selected")),
                )
                .child(
                    gpui::div()
                        .truncate()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .child(selected_summary),
                ),
        )
        .child(list)
        .child(footer)
}

fn subject_step(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let subject = this.group_subject_input.read(cx).value().trim().to_string();
    let valid_subject = !subject.is_empty()
        && subject.chars().count() <= wasabi_domain::GROUP_SUBJECT_MAX_CHARS;
    let creating = this.group_creating;
    let count = this.group_participants.len();
    let mut members = gpui::div()
        .flex_1()
        .min_h(px(0.0))
        .overflow_y_scrollbar()
        .border_t_1()
        .border_color(theme::border());
    for (index, contact) in this.group_participants.clone().into_iter().enumerate() {
        let initial = contact
            .display_name
            .chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .to_string();
        let label = contact.display_name.clone();
        members = members.child(
            gpui::div()
                .id(("new-group-member-summary", index))
                .h(px(54.0))
                .px(px(16.0))
                .flex()
                .items_center()
                .gap(px(11.0))
                .child(
                    gpui::div()
                        .size(px(34.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme::sender_color(&label))
                        .text_color(theme::text_on_accent())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(initial),
                )
                .child(
                    gpui::div()
                        .truncate()
                        .text_color(theme::text_primary())
                        .child(label),
                ),
        );
    }

    gpui::div()
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .child(
            gpui::div()
                .px(px(18.0))
                .pt(px(14.0))
                .pb(px(18.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_secondary())
                        .child("GROUP NAME"),
                )
                .child(
                    Input::new(&this.group_subject_input)
                        .cleanable(!creating)
                        .disabled(creating),
                )
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .child(format!(
                            "{} / {} characters",
                            subject.chars().count(),
                            wasabi_domain::GROUP_SUBJECT_MAX_CHARS
                        )),
                ),
        )
        .child(
            gpui::div()
                .h(px(42.0))
                .px(px(16.0))
                .flex()
                .items_center()
                .justify_between()
                .bg(theme::chip_idle())
                .child(
                    gpui::div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Participants"),
                )
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .child(format!("{count} selected")),
                ),
        )
        .child(members)
        .child(group_footer(
            this.group_creation_error.clone(),
            if creating { "Creating…" } else { "Create group" },
            valid_subject && !creating && !this.group_creation_uncertain,
            cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.submit_new_group(window, cx)
            }),
        ))
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

fn group_footer(
    error: Option<String>,
    label: &'static str,
    enabled: bool,
    listener: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Div {
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
                .text_color(if error.is_some() {
                    theme::danger()
                } else {
                    theme::text_secondary()
                })
                .child(error.unwrap_or_else(|| {
                    "Group membership is sent only after you confirm.".to_string()
                })),
        )
        .child(
            gpui::div()
                .id(if label == "Next" {
                    "new-group-next"
                } else if matches!(label, "Add" | "Adding…") {
                    "add-group-members"
                } else {
                    "new-group-create"
                })
                .min_w(px(96.0))
                .h(px(38.0))
                .px(px(14.0))
                .rounded(px(theme::RADIUS_SM))
                .flex()
                .items_center()
                .justify_center()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .bg(if enabled {
                    theme::accent()
                } else {
                    theme::chip_idle()
                })
                .text_color(if enabled {
                    theme::text_on_accent()
                } else {
                    theme::text_secondary()
                })
                .when(enabled, |button| {
                    button
                        .cursor_pointer()
                        .aria_label(label)
                        .hover(|button| button.opacity(0.9))
                        .on_click(listener)
                })
                .child(label),
        )
}

fn state_message(title: impl Into<String>, detail: impl Into<String>) -> gpui::Div {
    gpui::div()
        .min_h(px(110.0))
        .p(px(20.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(5.0))
        .text_align(gpui::TextAlign::Center)
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

fn available_group_contacts(
    contacts: &[wasabi_domain::ContactSummary],
    existing_members: &std::collections::HashSet<&str>,
) -> Vec<wasabi_domain::ContactSummary> {
    contacts
        .iter()
        .filter(|contact| !existing_members.contains(contact.jid.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_group_members_are_not_offered_again() {
        let contacts = [
            ("Already here", "existing@s.whatsapp.net"),
            ("Available", "available@s.whatsapp.net"),
        ]
        .into_iter()
        .map(|(display_name, jid)| wasabi_domain::ContactSummary {
            jid: wasabi_domain::ChatId::new(jid),
            display_name: display_name.to_string(),
            phone_number: None,
            avatar: None,
        })
        .collect::<Vec<_>>();
        let members = std::collections::HashSet::from(["existing@s.whatsapp.net"]);

        let available = available_group_contacts(&contacts, &members);

        assert_eq!(available.len(), 1);
        assert_eq!(available[0].jid.as_str(), "available@s.whatsapp.net");
    }
}
