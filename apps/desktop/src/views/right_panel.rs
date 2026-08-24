//! On-demand direct-contact and group information. Direct conversations never
//! render a participants section; group metadata remains honest until the
//! backend projection has populated real participants.

use gpui::prelude::*;
use gpui::{Context, px};
use gpui_component::{Icon, IconName};

use crate::state::chats;
use crate::theme;
use crate::views::root::MainWindow;

pub fn info_panel(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> impl IntoElement {
    let selected = this
        .chats
        .selected
        .as_ref()
        .and_then(|id| this.chats.chats.iter().find(|chat| chat.id.as_str() == id));

    let (name, is_group) = selected
        .map(|chat| {
            (
                chats::fallback_name(chat),
                chats::is_group(chat.id.as_str()),
            )
        })
        .unwrap_or_else(|| ("Conversation".to_string(), false));
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
                        .child(if is_group { "Group" } else { "Contact" }),
                ),
        )
        .child(section(
            "ABOUT",
            if is_group {
                "Group description is unavailable until metadata sync completes."
            } else {
                "About is unavailable for this contact."
            },
        ))
        .child(action_row("Media, links and documents", "No cached media"))
        .child(action_row("Starred messages", "None cached"))
        .child(action_row("Notifications", "Default"))
        .child(action_row("Disappearing messages", "Off"))
        .child(action_row("Encryption", "End-to-end encrypted"));

    if is_group {
        panel = panel.child(section(
            "PARTICIPANTS",
            "Participant data will appear after real group metadata is available.",
        ));
    } else {
        panel = panel.child(action_row("Groups in common", "None cached"));
    }

    panel
}

fn section(label: &'static str, body: &'static str) -> gpui::Div {
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
                .child(body),
        )
}

fn action_row(label: &'static str, detail: &'static str) -> gpui::Div {
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
                .child(detail),
        )
}
