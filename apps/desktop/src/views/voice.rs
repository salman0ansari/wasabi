//! Timeline player for cached audio and voice notes.

use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{ClickEvent, Context, px};
use gpui_component::{Icon, IconName};

use crate::audio;
use crate::theme;
use crate::views::root::MainWindow;

#[derive(Clone, Copy, Debug)]
pub(crate) struct PlaybackView {
    pub playing: bool,
    pub progress: f32,
    pub position_seconds: u32,
    pub duration_seconds: u32,
}

pub(crate) fn player(
    row: &wasabi_domain::MessageRow,
    path: PathBuf,
    playback: PlaybackView,
    text_scale: u16,
    cx: &mut Context<MainWindow>,
) -> gpui::AnyElement {
    let seq = row.seq.0 as usize;
    let chat = row.chat.clone();
    let media = match &row.kind {
        wasabi_domain::MessageKind::Audio { media, .. } => media.id.clone(),
        _ => {
            return gpui::div().into_any_element();
        }
    };
    let title = match &row.kind {
        wasabi_domain::MessageKind::Audio {
            voice_note: true, ..
        } => "Voice message",
        _ => "Audio",
    };
    let play_chat = chat.clone();
    let play_media = media.clone();
    let play_path = path.clone();
    let icon = if playback.playing {
        IconName::Pause
    } else {
        IconName::Play
    };
    let tooltip = if playback.playing { "Pause" } else { "Play" };
    let progress = playback.progress.clamp(0.0, 1.0);
    let bar_w = 168.0;
    let filled = bar_w * progress;
    let clock = format!(
        "{} / {}",
        audio::format_clock(playback.position_seconds),
        audio::format_clock(playback.duration_seconds)
    );

    gpui::div()
        .id(("voice-player", seq))
        .min_h(px(54.0))
        .w_full()
        .min_w(px(240.0))
        .rounded(px(theme::RADIUS_SM))
        .flex()
        .flex_col()
        .gap(px(6.0))
        .px(px(10.0))
        .py(px(8.0))
        .bg(theme::canvas())
        .child(
            gpui::div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(
                    gpui::div()
                        .id(("voice-play", seq))
                        .size(px(34.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .bg(theme::action_surface())
                        .text_color(theme::action_content())
                        .hover(|style| style.opacity(0.88))
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(tooltip).build(window, cx)
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            this.toggle_voice_playback(
                                play_chat.clone(),
                                play_media.clone(),
                                play_path.clone(),
                                cx,
                            );
                        }))
                        .child(Icon::new(icon).size(px(16.0))),
                )
                .child(
                    gpui::div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            gpui::div()
                                .truncate()
                                .text_size(px(theme::scaled_text(theme::TEXT_SIZE, text_scale)))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::text_primary())
                                .child(title),
                        )
                        .child(
                            gpui::div()
                                .h(px(4.0))
                                .w(px(bar_w))
                                .rounded_full()
                                .bg(theme::chip_idle())
                                .child(
                                    gpui::div()
                                        .h(px(4.0))
                                        .w(px(filled.max(if playback.playing { 4.0 } else { 0.0 })))
                                        .rounded_full()
                                        .bg(theme::accent()),
                                ),
                        )
                        .child(
                            gpui::div()
                                .text_size(px(theme::scaled_text(theme::TEXT_SIZE_SM, text_scale)))
                                .text_color(theme::text_secondary())
                                .child(clock),
                        ),
                ),
        )
        .into_any_element()
}
