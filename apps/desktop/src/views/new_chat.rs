//! Searchable, cache-first contact picker for starting a direct conversation.

use gpui::prelude::*;
use gpui::{Context, px};
use gpui_component::input::Input;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{Icon, IconName};

use crate::theme;
use crate::views::root::{MainWindow, PhoneLookupUi};

pub fn overlay(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let query = this.contact_search_input.read(cx).value().trim().to_string();
    let connected = this.session.state.is_connected();
    let phone = wasabi_domain::ContactPhoneNumber::parse(&query).ok();
    let has_cached_number = phone.as_ref().is_some_and(|phone| {
        this.contacts
            .iter()
            .any(|contact| contact.phone_number.as_deref() == Some(phone.as_str()))
    });
    let phone_panel = phone
        .filter(|_| !has_cached_number && !this.contacts_loading)
        .map(|_| phone_lookup_panel(this, connected, cx));
    let mut list = gpui::div()
        .flex_1()
        .min_h(px(0.0))
        .overflow_y_scrollbar()
        .border_t_1()
        .border_b_1()
        .border_color(theme::border());

    if let Some(error) = this.contacts_error.clone() {
        list = list.child(state_message("Contacts unavailable", error));
    } else if this.contacts.is_empty() && this.contacts_loading {
        list = list.child(state_message(
            "Loading contacts…",
            "Reading the encrypted local account cache",
        ));
    } else if this.contacts.is_empty() {
        let (title, detail) = if matches!(this.phone_lookup, PhoneLookupUi::Registered(_)) {
            (
                "Not in saved contacts",
                "Use the verified account above to start this conversation.",
            )
        } else if query.is_empty() {
            if connected {
                (
                    "No contacts found",
                    "Contact names will appear as account synchronization completes.",
                )
            } else {
                (
                    "No cached contacts",
                    "Reconnect to refresh the address book for this linked account.",
                )
            }
        } else {
            (
                "No matching contacts",
                "Try a saved name or complete phone number.",
            )
        };
        list = list.child(state_message(title, detail));
    } else {
        for (index, contact) in this.contacts.clone().into_iter().enumerate() {
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
                    .id(("new-chat-contact", index))
                    .min_h(px(62.0))
                    .px(px(14.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .cursor_pointer()
                    .aria_label(format!("Start chat with {label}"))
                    .hover(|row| row.bg(theme::row_hover()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.start_contact_chat(contact.clone(), window, cx)
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
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .size(px(16.0))
                            .text_color(theme::text_secondary()),
                    ),
            );
        }
        if this.contacts_loading {
            list = list.child(state_message("Loading more…", ""));
        } else if this.contacts_next.is_some() {
            list = list.child(
                gpui::div()
                    .id("load-more-contacts")
                    .h(px(44.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(theme::TEXT_SIZE_SM))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
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

    gpui::div()
        .absolute()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui::div()
                .id("new-chat-scrim")
                .absolute()
                .size_full()
                .occlude()
                .bg(theme::scrim())
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.close_new_chat(cx)
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
                                .child("New chat"),
                        )
                        .child(
                            gpui::div()
                                .id("close-new-chat")
                                .size(px(34.0))
                                .rounded_full()
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .justify_center()
                                .aria_label("Close New chat")
                                .hover(|button| button.bg(theme::row_hover()))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.close_new_chat(cx)
                                }))
                                .child("×"),
                        ),
                )
                .child(
                    gpui::div()
                        .px(px(14.0))
                        .pb(px(12.0))
                        .child(
                            Input::new(&this.contact_search_input)
                                .prefix(Icon::new(IconName::Search).size(px(16.0)))
                                .cleanable(true),
                        ),
                )
                .children(phone_panel)
                .child(list),
        )
}

fn phone_lookup_panel(
    this: &mut MainWindow,
    connected: bool,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let base = gpui::div()
        .flex_shrink_0()
        .border_t_1()
        .border_color(theme::border())
        .px(px(14.0))
        .py(px(10.0));

    match this.phone_lookup.clone() {
        PhoneLookupUi::Idle if connected => base.child(
            gpui::div()
                .id("check-phone-number")
                .min_h(px(48.0))
                .rounded(px(theme::RADIUS_SM))
                .px(px(10.0))
                .flex()
                .items_center()
                .gap(px(11.0))
                .cursor_pointer()
                .aria_label("Check this phone number")
                .hover(|row| row.bg(theme::row_hover()))
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.lookup_phone_contact(cx)
                }))
                .child(
                    gpui::div()
                        .size(px(34.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme::chip_idle())
                        .text_color(theme::accent_text())
                        .child(Icon::new(IconName::Search).size(px(16.0))),
                )
                .child(
                    gpui::div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            gpui::div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::text_primary())
                                .child("Check this phone number"),
                        )
                        .child(
                            gpui::div()
                                .text_size(px(theme::TEXT_SIZE_SM))
                                .text_color(theme::text_secondary())
                                .child("We’ll verify it before starting a chat."),
                        ),
                )
                .child(
                    Icon::new(IconName::ChevronRight)
                        .size(px(16.0))
                        .text_color(theme::text_secondary()),
                ),
        ),
        PhoneLookupUi::Idle => base.child(compact_status(
            "Connect to check this number",
            "Saved contacts remain available offline.",
        )),
        PhoneLookupUi::Checking => base.child(compact_status(
            "Checking this number…",
            "Waiting for a secure response",
        )),
        PhoneLookupUi::Registered(contact) => {
            let initial = contact
                .display_name
                .chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string();
            let label = contact.display_name.clone();
            base.child(
                gpui::div()
                    .id("registered-phone-result")
                    .min_h(px(54.0))
                    .rounded(px(theme::RADIUS_SM))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(11.0))
                    .cursor_pointer()
                    .aria_label(format!("Start chat with {label}"))
                    .hover(|row| row.bg(theme::row_hover()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.start_contact_chat(contact.clone(), window, cx)
                    }))
                    .child(
                        gpui::div()
                            .size(px(38.0))
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
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child(label),
                            )
                            .child(
                                gpui::div()
                                    .text_size(px(theme::TEXT_SIZE_SM))
                                    .text_color(theme::accent_text())
                                    .child("Account found · Start chat"),
                            ),
                    )
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .size(px(16.0))
                            .text_color(theme::accent_text()),
                    ),
            )
        }
        PhoneLookupUi::NotRegistered => base.child(compact_status(
            "No account found",
            "Check the country code and number, then try again.",
        )),
        PhoneLookupUi::Failed(error) => base.child(
            gpui::div()
                .min_h(px(54.0))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .child(
                    gpui::div()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .child(
                            gpui::div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::text_primary())
                                .child("Number check unavailable"),
                        )
                        .child(
                            gpui::div()
                                .text_size(px(theme::TEXT_SIZE_SM))
                                .text_color(theme::text_secondary())
                                .child(error),
                        ),
                )
                .when(connected, |row| {
                    row.child(
                        gpui::div()
                            .id("retry-phone-lookup")
                            .px(px(10.0))
                            .py(px(7.0))
                            .rounded(px(theme::RADIUS_SM))
                            .cursor_pointer()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_size(px(theme::TEXT_SIZE_SM))
                            .text_color(theme::accent_text())
                            .hover(|button| button.bg(theme::row_hover()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.lookup_phone_contact(cx)
                            }))
                            .child("Try again"),
                    )
                }),
        ),
    }
}

fn compact_status(title: impl Into<String>, detail: impl Into<String>) -> gpui::Div {
    gpui::div()
        .min_h(px(52.0))
        .flex()
        .flex_col()
        .justify_center()
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

fn state_message(title: impl Into<String>, detail: impl Into<String>) -> gpui::Div {
    let detail = detail.into();
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
        .when(!detail.is_empty(), |state| {
            state.child(
                gpui::div()
                    .text_size(px(theme::TEXT_SIZE_SM))
                    .text_color(theme::text_secondary())
                    .child(detail),
            )
        })
}
