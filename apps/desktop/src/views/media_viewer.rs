//! Lightbox overlay for downloaded photos, stickers, and video stills.
//!
//! Navigation is limited to locally cached visual messages in the current
//! timeline window. Videos are not decoded here: a still is shown when one
//! exists, and playback is handed to the system handler.

use std::collections::HashMap;
use std::path::Path;

use gpui::prelude::*;
use gpui::{Context, ObjectFit, StyledImage, px};
use gpui_component::{Icon, IconName};

use crate::theme;
use crate::views::conversation::{self, CachedMediaAccess};
use crate::views::root::{
    MainWindow, MediaDownloadUi, MediaThumbUi, MediaViewerNext, MediaViewerPrev,
};

pub(crate) const KEY_CONTEXT: &str = "MediaViewer";

const VIEWER_ORIGINAL_MAX_BYTES: u64 = 8 * 1024 * 1024;
const VIEWER_ORIGINAL_MAX_EDGE: u32 = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LightboxKind {
    Image,
    Sticker,
    Video,
}

pub(crate) fn lightbox_kind(kind: &wasabi_domain::MessageKind) -> Option<LightboxKind> {
    match kind {
        wasabi_domain::MessageKind::Image { .. } => Some(LightboxKind::Image),
        wasabi_domain::MessageKind::Sticker { .. } => Some(LightboxKind::Sticker),
        wasabi_domain::MessageKind::Video { .. } => Some(LightboxKind::Video),
        _ => None,
    }
}

pub(crate) fn is_lightbox_kind(kind: &wasabi_domain::MessageKind) -> bool {
    lightbox_kind(kind).is_some()
}

pub(crate) fn viewer_uses_original(media: &wasabi_domain::MediaDescriptor, path: &Path) -> bool {
    if let (Some(width), Some(height)) = (media.width, media.height)
        && width.max(height) > VIEWER_ORIGINAL_MAX_EDGE
    {
        return false;
    }
    let size = media
        .file_size
        .or_else(|| std::fs::metadata(path).ok().map(|meta| meta.len()));
    if let Some(size) = size
        && size > VIEWER_ORIGINAL_MAX_BYTES
    {
        return false;
    }
    true
}

pub(crate) fn media_viewer_targets(
    rows: &[wasabi_domain::MessageRow],
    downloads: &HashMap<(wasabi_domain::ChatId, wasabi_domain::MediaId), MediaDownloadUi>,
) -> Vec<wasabi_domain::MessageId> {
    rows.iter()
        .filter(|row| is_lightbox_kind(&row.kind))
        .filter(|row| {
            conversation::media_descriptor(&row.kind).is_some_and(|media| {
                matches!(
                    downloads.get(&(row.chat.clone(), media.id.clone())),
                    Some(MediaDownloadUi::Ready(path))
                        if conversation::classify_cached_media(path) == CachedMediaAccess::Available
                )
            })
        })
        .map(|row| row.id.clone())
        .collect()
}

pub(crate) fn media_viewer_neighbor(
    targets: &[wasabi_domain::MessageId],
    current: &wasabi_domain::MessageId,
    delta: isize,
) -> Option<wasabi_domain::MessageId> {
    let index = targets.iter().position(|id| id == current)?;
    let next = index as isize + delta;
    if next < 0 || next >= targets.len() as isize {
        None
    } else {
        Some(targets[next as usize].clone())
    }
}

pub fn overlay(
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
        return gpui::div()
            .absolute()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::scrim())
            .child("This message is no longer available");
    };
    let Some(kind) = lightbox_kind(&row.kind) else {
        return gpui::div();
    };
    let Some(media) = conversation::media_descriptor(&row.kind).cloned() else {
        return gpui::div();
    };
    let cached = this
        .media_downloads
        .get(&(row.chat.clone(), media.id.clone()))
        .and_then(|state| match state {
            MediaDownloadUi::Ready(path)
                if conversation::classify_cached_media(path) == CachedMediaAccess::Available =>
            {
                Some(path.clone())
            }
            _ => None,
        });
    let thumb = this
        .media_thumbs
        .get(&(row.chat.clone(), media.id.clone()))
        .and_then(|state| match state {
            MediaThumbUi::Ready(path) => Some(path.clone()),
            _ => None,
        });
    let targets = media_viewer_targets(&this.messages.rows, &this.media_downloads);
    let has_prev = media_viewer_neighbor(&targets, &message, -1).is_some();
    let has_next = media_viewer_neighbor(&targets, &message, 1).is_some();
    let caption = match &row.kind {
        wasabi_domain::MessageKind::Image { caption, .. }
        | wasabi_domain::MessageKind::Video { caption, .. } => caption.clone(),
        _ => None,
    };
    let suggested = conversation::suggested_save_name(&row.kind, cached.as_deref());
    let title = match kind {
        LightboxKind::Image => "Photo",
        LightboxKind::Sticker => "Sticker",
        LightboxKind::Video => "Video",
    };

    let mut stage = gpui::div()
        .flex_1()
        .min_h(px(0.0))
        .min_w(px(0.0))
        .flex()
        .items_center()
        .justify_center()
        .px(px(24.0))
        .py(px(12.0));
    match (kind, cached.as_ref()) {
        (LightboxKind::Video, Some(path)) => {
            let still = thumb.clone().filter(|path| {
                conversation::classify_cached_media(path) == CachedMediaAccess::Available
            });
            let open_chat = row.chat.clone();
            let open_media = media.id.clone();
            stage =
                stage.child(
                    gpui::div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(12.0))
                        .child(match still {
                            Some(still) => gpui::img(still)
                                .max_h(px(520.0))
                                .max_w(px(720.0))
                                .object_fit(ObjectFit::Contain)
                                .into_any_element(),
                            None => gpui::div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(px(8.0))
                                .text_color(theme::text_on_accent())
                                .child(Icon::new(IconName::GalleryVerticalEnd).size(px(36.0)))
                                .child("Video")
                                .into_any_element(),
                        })
                        .child(
                            gpui::div()
                                .text_size(px(theme::TEXT_SIZE_SM))
                                .text_color(theme::text_on_accent())
                                .child("Open in the system player — Wasabi does not decode video."),
                        )
                        .child(viewer_chip("open-in-player", "Open in player").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.open_cached_media_in_player(
                                    open_chat.clone(),
                                    open_media.clone(),
                                    cx,
                                )
                            }),
                        )),
                );
            let _ = path;
        }
        (_, Some(path)) => {
            let paint = if viewer_uses_original(&media, path) {
                path.clone()
            } else {
                thumb.clone().unwrap_or_else(|| path.clone())
            };
            stage = stage.child(
                gpui::img(paint)
                    .id("media-viewer-image")
                    .max_h(px(640.0))
                    .max_w(px(860.0))
                    .object_fit(ObjectFit::Contain)
                    .with_fallback(|| {
                        gpui::div()
                            .text_color(theme::text_on_accent())
                            .child("Could not display this file")
                            .into_any_element()
                    }),
            );
        }
        _ => {
            stage = stage.child(
                gpui::div()
                    .text_color(theme::text_on_accent())
                    .child("This file is no longer in the cache"),
            );
        }
    }

    let save_chat = row.chat.clone();
    let save_media = media.id.clone();
    let reveal_chat = row.chat.clone();
    let reveal_media = media.id.clone();
    let cached_ok = cached.is_some();

    gpui::div()
        .id("media-viewer")
        .key_context(KEY_CONTEXT)
        .track_focus(&this.media_viewer_focus)
        .focusable()
        .absolute()
        .size_full()
        .flex()
        .flex_col()
        .bg(theme::scrim())
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| this.close_message_overlay(cx)),
        )
        .on_action(cx.listener(|this, _: &MediaViewerPrev, _, cx| {
            this.media_viewer_step(-1, cx);
        }))
        .on_action(cx.listener(|this, _: &MediaViewerNext, _, cx| {
            this.media_viewer_step(1, cx);
        }))
        .child(
            gpui::div()
                .flex_shrink_0()
                .h(px(theme::HEADER_H))
                .px(px(16.0))
                .flex()
                .items_center()
                .justify_between()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    gpui::div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_on_accent())
                        .child(title),
                )
                .child(
                    gpui::div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .children(cached_ok.then(|| {
                            viewer_chip("viewer-save-as", "Save as…").on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.save_downloaded_media(
                                        save_chat.clone(),
                                        save_media.clone(),
                                        suggested.clone(),
                                        cx,
                                    )
                                },
                            ))
                        }))
                        .children(cached_ok.then(|| {
                            viewer_chip("viewer-reveal", "Reveal in Files").on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.reveal_downloaded_media(
                                        reveal_chat.clone(),
                                        reveal_media.clone(),
                                        cx,
                                    )
                                },
                            ))
                        }))
                        .child(viewer_chip("viewer-close", "Close").on_click(
                            cx.listener(|this, _, _, cx| this.close_message_overlay(cx)),
                        )),
                ),
        )
        .child(
            gpui::div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .items_center()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    nav_button("viewer-prev", IconName::ArrowLeft, has_prev).when(has_prev, |el| {
                        el.on_click(cx.listener(|this, _, _, cx| this.media_viewer_step(-1, cx)))
                    }),
                )
                .child(stage)
                .child(
                    nav_button("viewer-next", IconName::ChevronRight, has_next).when(
                        has_next,
                        |el| {
                            el.on_click(cx.listener(|this, _, _, cx| this.media_viewer_step(1, cx)))
                        },
                    ),
                ),
        )
        .when_some(caption, |el, caption| {
            el.child(
                gpui::div()
                    .flex_shrink_0()
                    .px(px(24.0))
                    .pb(px(16.0))
                    .text_color(theme::text_on_accent())
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(caption),
            )
        })
}

fn viewer_chip(id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    gpui::div()
        .id(id)
        .cursor_pointer()
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(theme::border())
        .px(px(8.0))
        .py(px(4.0))
        .text_size(px(theme::TEXT_SIZE_SM))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::text_on_accent())
        .hover(|style| style.bg(theme::row_hover()))
        .child(label)
}

fn nav_button(id: &'static str, icon: IconName, enabled: bool) -> gpui::Stateful<gpui::Div> {
    gpui::div()
        .id(id)
        .w(px(48.0))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .when(enabled, |el| {
            el.cursor_pointer()
                .text_color(theme::text_on_accent())
                .hover(|style| style.bg(theme::row_hover()))
        })
        .when(!enabled, |el| el.text_color(theme::text_secondary()))
        .child(Icon::new(icon).size(px(22.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::root::MediaDownloadUi;

    fn media(id: &str) -> wasabi_domain::MediaDescriptor {
        wasabi_domain::MediaDescriptor {
            id: wasabi_domain::MediaId::new(id),
            mime_type: None,
            file_name: None,
            file_size: None,
            duration_seconds: None,
            width: None,
            height: None,
            availability: wasabi_domain::MediaAvailability::Local,
        }
    }

    fn row(id: &str, kind: wasabi_domain::MessageKind) -> wasabi_domain::MessageRow {
        wasabi_domain::MessageRow {
            id: wasabi_domain::MessageId::new(id),
            chat: wasabi_domain::ChatId::new("chat-a@s.whatsapp.net"),
            direction: wasabi_domain::MessageDirection::Incoming,
            sender: wasabi_domain::SenderJid {
                bare: "a@s.whatsapp.net".to_string(),
                push_name: None,
            },
            timestamp_ms: 1,
            seq: wasabi_domain::LocalCursor(1),
            kind,
            quoted: None,
            reactions: Vec::new(),
            status: wasabi_domain::MessageStatus::Delivered,
            edited_at_ms: None,
            revoked: false,
            starred: false,
        }
    }

    #[test]
    fn navigation_skips_non_cached_and_non_visual_kinds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let present = dir.path().join("payload");
        std::fs::write(&present, b"ok").expect("write cache file");
        let missing = dir.path().join("missing");

        let chat = wasabi_domain::ChatId::new("chat-a@s.whatsapp.net");
        let photo = media("photo");
        let video = media("video");
        let audio = media("audio");
        let sticker = media("sticker");
        let uncached = media("uncached");
        let evicted = media("evicted");

        let rows = vec![
            row(
                "text",
                wasabi_domain::MessageKind::Text {
                    body: "hi".to_string(),
                },
            ),
            row(
                "photo",
                wasabi_domain::MessageKind::Image {
                    caption: None,
                    media: photo.clone(),
                },
            ),
            row(
                "audio",
                wasabi_domain::MessageKind::Audio {
                    voice_note: false,
                    media: audio.clone(),
                },
            ),
            row(
                "uncached",
                wasabi_domain::MessageKind::Image {
                    caption: None,
                    media: uncached.clone(),
                },
            ),
            row(
                "evicted",
                wasabi_domain::MessageKind::Image {
                    caption: None,
                    media: evicted.clone(),
                },
            ),
            row(
                "video",
                wasabi_domain::MessageKind::Video {
                    caption: None,
                    video_note: false,
                    media: video.clone(),
                },
            ),
            row(
                "sticker",
                wasabi_domain::MessageKind::Sticker {
                    animated: false,
                    media: sticker.clone(),
                },
            ),
        ];
        let mut downloads = HashMap::new();
        downloads.insert(
            (chat.clone(), photo.id.clone()),
            MediaDownloadUi::Ready(present.clone()),
        );
        downloads.insert(
            (chat.clone(), audio.id.clone()),
            MediaDownloadUi::Ready(present.clone()),
        );
        downloads.insert(
            (chat.clone(), evicted.id.clone()),
            MediaDownloadUi::Ready(missing),
        );
        downloads.insert(
            (chat.clone(), video.id.clone()),
            MediaDownloadUi::Ready(present.clone()),
        );
        downloads.insert((chat, sticker.id.clone()), MediaDownloadUi::Ready(present));

        let targets = media_viewer_targets(&rows, &downloads);
        let ids: Vec<_> = targets.iter().map(|id| id.as_str().to_string()).collect();
        assert_eq!(ids, vec!["photo", "video", "sticker"]);
        assert_eq!(
            media_viewer_neighbor(&targets, &wasabi_domain::MessageId::new("photo"), 1)
                .as_ref()
                .map(|id| id.as_str()),
            Some("video")
        );
        assert_eq!(
            media_viewer_neighbor(&targets, &wasabi_domain::MessageId::new("video"), -1)
                .as_ref()
                .map(|id| id.as_str()),
            Some("photo")
        );
        assert_eq!(
            media_viewer_neighbor(&targets, &wasabi_domain::MessageId::new("sticker"), 1),
            None
        );
        assert_eq!(
            media_viewer_neighbor(&targets, &wasabi_domain::MessageId::new("photo"), -1),
            None
        );
    }

    #[test]
    fn huge_originals_use_a_bounded_preview() {
        let mut media = media("huge");
        media.width = Some(8000);
        media.height = Some(4000);
        assert!(!viewer_uses_original(
            &media,
            Path::new("/cache/does-not-matter")
        ));
        media.width = Some(800);
        media.height = Some(600);
        media.file_size = Some(200_000);
        assert!(viewer_uses_original(
            &media,
            Path::new("/cache/does-not-matter")
        ));
        media.file_size = Some(VIEWER_ORIGINAL_MAX_BYTES + 1);
        assert!(!viewer_uses_original(
            &media,
            Path::new("/cache/does-not-matter")
        ));
    }
}
