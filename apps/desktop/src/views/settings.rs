//! Complete, device-local settings surface. Every interactive control here
//! writes the versioned settings file immediately.

use gpui::prelude::*;
use gpui::{Context, px};

use crate::state::{SettingsSection, ThemePreference};
use crate::theme;
use crate::views::root::MainWindow;

pub fn settings_page(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> impl IntoElement {
    gpui::div()
        .id("settings-page")
        .flex_1()
        .min_w(px(0.0))
        .h_full()
        .flex()
        .bg(theme::surface())
        .child(settings_navigation(this, cx))
        .child(settings_content(this, cx))
}

fn settings_navigation(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let mut navigation = gpui::div()
        .w(px(282.0))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .child(
            gpui::div()
                .h(px(72.0))
                .flex()
                .items_center()
                .px(px(20.0))
                .text_size(px(theme::TEXT_TITLE))
                .font_weight(gpui::FontWeight::BOLD)
                .child("Settings"),
        )
        .child(
            gpui::div()
                .mx(px(12.0))
                .mb(px(10.0))
                .p(px(12.0))
                .rounded(px(theme::RADIUS_MD))
                .bg(theme::canvas())
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_NAME))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Wasabi account"),
                )
                .child(
                    gpui::div()
                        .mt(px(3.0))
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .child(this.session.status_label()),
                ),
        );

    for (index, section) in SettingsSection::ALL.into_iter().enumerate() {
        let selected = this.settings_section == section;
        navigation = navigation.child(
            gpui::div()
                .id(("settings-section", index))
                .mx(px(8.0))
                .px(px(12.0))
                .h(px(42.0))
                .rounded(px(theme::RADIUS_MD))
                .cursor_pointer()
                .flex()
                .items_center()
                .text_size(px(theme::TEXT_SIZE))
                .font_weight(if selected {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .when(selected, |el| {
                    el.bg(theme::row_selected()).text_color(theme::text_primary())
                })
                .when(!selected, |el| {
                    el.text_color(theme::text_secondary())
                        .hover(|style| style.bg(theme::row_hover()))
                })
                .child(section.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_settings_section(section, cx);
                })),
        );
    }
    navigation
}

fn settings_content(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> impl IntoElement {
    let section = this.settings_section;
    gpui::div()
        .id("settings-content-scroll")
        .flex_1()
        .min_w(px(0.0))
        .h_full()
        .overflow_y_scroll()
        .bg(theme::canvas())
        .child(
            gpui::div()
                .max_w(px(760.0))
                .mx_auto()
                .px(px(36.0))
                .py(px(32.0))
                .child(
                    gpui::div()
                        .mb(px(22.0))
                        .text_size(px(theme::TEXT_TITLE))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::text_primary())
                        .child(section.label()),
                )
                .child(match section {
                    SettingsSection::General => general(this, cx),
                    SettingsSection::Account => account(this, cx),
                    SettingsSection::Privacy => privacy(this, cx),
                    SettingsSection::Chats => chats(this, cx),
                    SettingsSection::Notifications => notifications(this, cx),
                    SettingsSection::Storage => storage(this),
                    SettingsSection::Shortcuts => shortcuts(),
                    SettingsSection::Help => help(),
                }),
        )
}

fn card(title: &'static str) -> gpui::Div {
    gpui::div()
        .mb(px(18.0))
        .rounded(px(theme::RADIUS_MD))
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .overflow_hidden()
        .child(
            gpui::div()
                .px(px(18.0))
                .pt(px(15.0))
                .pb(px(8.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent_text())
                .child(title),
        )
}

fn value_row(label: impl Into<String>, detail: impl Into<String>) -> gpui::Div {
    gpui::div()
        .min_h(px(52.0))
        .px(px(18.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .gap(px(20.0))
        .border_t_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_primary())
                .child(label.into()),
        )
        .child(
            gpui::div()
                .max_w(px(360.0))
                .truncate()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_secondary())
                .child(detail.into()),
        )
}

fn toggle_visual(checked: bool) -> gpui::Div {
    gpui::div()
        .w(px(36.0))
        .h(px(20.0))
        .rounded_full()
        .p(px(2.0))
        .flex()
        .items_center()
        .when(checked, |el| el.justify_end().bg(theme::accent()))
        .when(!checked, |el| el.justify_start().bg(theme::skeleton()))
        .child(gpui::div().size(px(16.0)).rounded_full().bg(theme::surface()))
}

fn toggle_row(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    checked: bool,
    cx: &mut Context<MainWindow>,
    mutate: impl Fn(&mut MainWindow) + 'static,
) -> gpui::Stateful<gpui::Div> {
    gpui::div()
        .id(id)
        .min_h(px(62.0))
        .px(px(18.0))
        .py(px(9.0))
        .cursor_pointer()
        .flex()
        .items_center()
        .gap(px(16.0))
        .border_t_1()
        .border_color(theme::border())
        .hover(|style| style.bg(theme::row_hover()))
        .child(
            gpui::div()
                .flex_1()
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE))
                        .text_color(theme::text_primary())
                        .child(label),
                )
                .child(
                    gpui::div()
                        .mt(px(2.0))
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .child(description),
                ),
        )
        .child(toggle_visual(checked))
        .on_click(cx.listener(move |this, _, _, cx| {
            mutate(this);
            this.save_settings(cx);
        }))
}

fn general(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::AnyElement {
    card("APPLICATION")
        .child(value_row("Language", this.settings.language.clone()))
        .child(toggle_row(
            "setting-startup",
            "Launch Wasabi at startup",
            "Start after you sign in to the Linux desktop.",
            this.settings.launch_at_startup,
            cx,
            |this| this.settings.launch_at_startup = !this.settings.launch_at_startup,
        ))
        .child(value_row("Close behavior", "Close the window"))
        .child(toggle_row(
            "setting-diagnostics",
            "Share diagnostic data",
            "Content-free technical diagnostics only.",
            this.settings.diagnostics,
            cx,
            |this| this.settings.diagnostics = !this.settings.diagnostics,
        ))
        .into_any_element()
}

fn account(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::AnyElement {
    card("LINKED ACCOUNT")
        .child(value_row("Connection", this.session.status_label()))
        .child(value_row("Profile", "Local account profile"))
        .child(
            gpui::div()
                .id("settings-link-device")
                .min_h(px(54.0))
                .px(px(18.0))
                .cursor_pointer()
                .flex()
                .items_center()
                .border_t_1()
                .border_color(theme::border())
                .text_size(px(theme::TEXT_SIZE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent_text())
                .hover(|style| style.bg(theme::row_hover()))
                .child("Link another device with QR")
                .on_click(cx.listener(|this, _, _, cx| this.request_pairing(cx))),
        )
        .into_any_element()
}

fn privacy(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::AnyElement {
    card("MESSAGE PRIVACY")
        .child(toggle_row(
            "setting-preview-privacy",
            "Show message previews",
            "Hide message text in desktop notifications when disabled.",
            this.settings.notification_previews,
            cx,
            |this| this.settings.notification_previews = !this.settings.notification_previews,
        ))
        .child(value_row("Default disappearing messages", "Off"))
        .child(value_row("Blocked contacts", "Managed by the linked account"))
        .into_any_element()
}

fn chats(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::AnyElement {
    let mut theme_picker = gpui::div()
        .px(px(18.0))
        .py(px(12.0))
        .border_t_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .mb(px(8.0))
                .text_size(px(theme::TEXT_SIZE))
                .child("Theme"),
        )
        .child(gpui::div().flex().gap(px(8.0)));
    let mut choices = gpui::div().flex().gap(px(8.0));
    for (index, preference) in ThemePreference::ALL.into_iter().enumerate() {
        let selected = this.settings.theme == preference;
        choices = choices.child(
            gpui::div()
                .id(("theme-choice", index))
                .px(px(14.0))
                .py(px(7.0))
                .rounded(px(theme::RADIUS_MD))
                .cursor_pointer()
                .border_1()
                .when(selected, |el| {
                    el.border_color(theme::accent()).text_color(theme::accent_text())
                })
                .when(!selected, |el| {
                    el.border_color(theme::border())
                        .text_color(theme::text_secondary())
                        .hover(|style| style.bg(theme::row_hover()))
                })
                .child(preference.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.settings.theme = preference;
                    let mode = match preference {
                        ThemePreference::Light => gpui_component::theme::ThemeMode::Light,
                        ThemePreference::Dark => gpui_component::theme::ThemeMode::Dark,
                        ThemePreference::System => cx.window_appearance().into(),
                    };
                    theme::set_dark_mode(mode.is_dark());
                    gpui_component::Theme::change(mode, None, cx);
                    this.save_settings(cx);
                })),
        );
    }
    theme_picker = theme_picker.child(choices);

    let mut text_size_picker = gpui::div()
        .px(px(18.0))
        .py(px(12.0))
        .border_t_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .mb(px(8.0))
                .text_size(px(theme::TEXT_SIZE))
                .child("Text size"),
        );
    let mut size_choices = gpui::div().flex().gap(px(8.0));
    for (index, scale) in [100_u16, 125, 150].into_iter().enumerate() {
        let selected = this.settings.text_scale == scale;
        size_choices = size_choices.child(
            gpui::div()
                .id(("text-scale", index))
                .px(px(14.0))
                .py(px(7.0))
                .rounded(px(theme::RADIUS_MD))
                .cursor_pointer()
                .border_1()
                .when(selected, |el| {
                    el.border_color(theme::accent()).text_color(theme::accent_text())
                })
                .when(!selected, |el| {
                    el.border_color(theme::border())
                        .text_color(theme::text_secondary())
                        .hover(|style| style.bg(theme::row_hover()))
                })
                .child(format!("{scale}%"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.settings.text_scale = scale;
                    this.messages.clear_layout_estimates();
                    this.save_settings(cx);
                })),
        );
    }
    text_size_picker = text_size_picker.child(size_choices);

    card("APPEARANCE AND COMPOSING")
        .child(theme_picker)
        .child(value_row("Wallpaper", "Wasabi line pattern"))
        .child(text_size_picker)
        .child(toggle_row(
            "setting-enter-send",
            "Enter sends a message",
            "Use Shift+Enter for a new line.",
            this.settings.enter_to_send,
            cx,
            |this| this.settings.enter_to_send = !this.settings.enter_to_send,
        ))
        .child(toggle_row(
            "setting-spellcheck",
            "Spellcheck",
            "Use the desktop spellchecker while composing.",
            this.settings.spellcheck,
            cx,
            |this| this.settings.spellcheck = !this.settings.spellcheck,
        ))
        .child(toggle_row(
            "setting-link-preview",
            "Link previews",
            "Create a preview when a supported link is pasted.",
            this.settings.link_previews,
            cx,
            |this| this.settings.link_previews = !this.settings.link_previews,
        ))
        .into_any_element()
}

fn notifications(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::AnyElement {
    card("DESKTOP NOTIFICATIONS")
        .child(toggle_row(
            "setting-notifications",
            "Desktop notifications",
            "Allow Wasabi to notify you about new messages.",
            this.settings.desktop_notifications,
            cx,
            |this| this.settings.desktop_notifications = !this.settings.desktop_notifications,
        ))
        .child(toggle_row(
            "setting-sound",
            "Notification sound",
            "Play a sound for eligible messages.",
            this.settings.notification_sound,
            cx,
            |this| this.settings.notification_sound = !this.settings.notification_sound,
        ))
        .child(toggle_row(
            "setting-focused",
            "Suppress while focused",
            "Do not show notifications while this window is active.",
            this.settings.suppress_when_focused,
            cx,
            |this| this.settings.suppress_when_focused = !this.settings.suppress_when_focused,
        ))
        .into_any_element()
}

fn storage(this: &MainWindow) -> gpui::AnyElement {
    card("DOWNLOADS AND CACHE")
        .child(value_row(
            "Download location",
            this.settings.download_path.clone(),
        ))
        .child(value_row(
            "Cache quota",
            format!("{} MB", this.settings.cache_quota_mb),
        ))
        .child(value_row("Automatic downloads", "Photos on any network"))
        .into_any_element()
}

fn shortcuts() -> gpui::AnyElement {
    card("ACTIVE BINDINGS")
        .child(value_row("Focus chat search", "Ctrl+K"))
        .child(value_row("Open Settings", "Ctrl+,"))
        .child(value_row("Close info panel", "Escape"))
        .child(value_row("Send message", "Enter"))
        .child(value_row("New line", "Shift+Enter"))
        .into_any_element()
}

fn help() -> gpui::AnyElement {
    card("ABOUT WASABI")
        .child(value_row("Version", env!("CARGO_PKG_VERSION")))
        .child(value_row("Build", "Linux / GPUI"))
        .child(value_row(
            "Settings file",
            crate::state::DeviceSettings::path().to_string_lossy(),
        ))
        .child(value_row("Logs", "Content-free and identity-redacted"))
        .into_any_element()
}
