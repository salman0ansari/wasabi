//! Right-hand chat info panel: identity summary, media grid and
//! participants skeletons ahead of richer store projections.

use gpui::prelude::*;
use gpui::{Context, px};
use gpui_component::{Icon, IconName};

use crate::state::chats;
use crate::theme;
use crate::views::root::MainWindow;

pub fn info_panel(this: &mut MainWindow, _cx: &mut Context<MainWindow>) -> impl IntoElement {
    let selected = this
        .chats
        .selected
        .as_ref()
        .and_then(|id| this.chats.chats.iter().find(|c| c.id.as_str() == id));

    let (name, subtitle) = match selected {
        Some(chat) => (
            chats::fallback_name(chat),
            if chats::is_group(chat.id.as_str()) {
                "group".to_string()
            } else {
                "contact".to_string()
            },
        ),
        None => ("No conversation".to_string(), String::new()),
    };

    let initial = name.chars().next().unwrap_or('#').to_string();

    let avatar = gpui::div()
        .size(px(72.0))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::ROW_SELECTED)
        .text_color(theme::ACCENT_TEXT)
        .text_size(px(28.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(initial);

    let section_title = |label: &'static str| {
        gpui::div()
            .px(px(16.0))
            .pt(px(14.0))
            .pb(px(6.0))
            .text_size(px(theme::TEXT_SIZE_SM))
            .text_color(theme::TEXT_SECONDARY)
            .child(label)
    };

    // Media handles are not part of the current chat projection yet. Keep
    // the section honest and useful rather than showing fake thumbnails.
    let media_grid = gpui::div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .px(px(16.0))
        .py(px(8.0))
        .text_color(theme::TEXT_SECONDARY)
        .child(Icon::new(IconName::GalleryVerticalEnd).size(px(18.0)))
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE_SM))
                .child("No media shared yet"),
        );

    let divider = || {
        gpui::div()
            .mx(px(16.0))
            .mt(px(12.0))
            .h(px(1.0))
            .bg(theme::BORDER)
    };

    let participant_row = |label: &str, detail: &str, color: gpui::Rgba| {
        gpui::div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(16.0))
            .py(px(8.0))
            .child(
                gpui::div()
                    .size(px(36.0))
                    .rounded_full()
                    .bg(theme::SKELETON),
            )
            .child(
                gpui::div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(
                        gpui::div()
                            .text_size(px(theme::TEXT_SIZE))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::TEXT_PRIMARY)
                            .child(label.to_string()),
                    )
                    .child(
                        gpui::div()
                            .text_size(px(theme::TEXT_SIZE_SM))
                            .text_color(color)
                            .child(detail.to_string()),
                    ),
            )
    };

    gpui::div()
        .id("right-panel-scroll")
        .w(px(theme::RIGHT_PANEL_W))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .bg(theme::SURFACE)
        .border_l_1()
        .border_color(theme::BORDER)
        .child(
            gpui::div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.0))
                .pt(px(20.0))
                .pb(px(4.0))
                .px(px(16.0))
                .child(avatar)
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_NAME))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::TEXT_PRIMARY)
                        .child(name),
                )
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::TEXT_SECONDARY)
                        .child(subtitle),
                ),
        )
        .child(divider())
        .child(section_title("Media"))
        .child(media_grid)
        .child(divider())
        .child(section_title("Starred"))
        .child(
            gpui::div()
                .px(px(16.0))
                .py(px(6.0))
                .flex()
                .items_center()
                .gap(px(4.0))
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::ACCENT_TEXT)
                .child("Show all")
                .child(Icon::new(IconName::ChevronRight).size(px(15.0))),
        )
        .child(divider())
        .child(section_title("Participants"))
        .child(participant_row("You", "admin", theme::ACCENT_TEXT))
        .child(participant_row(
            "Member",
            "last seen recently",
            theme::TEXT_SECONDARY,
        ))
        .child(participant_row("Member", "typing…", theme::ACCENT_TEXT))
}
