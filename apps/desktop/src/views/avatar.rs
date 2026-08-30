use std::path::Path;

use gpui::prelude::*;
use gpui::{ObjectFit, StyledImage, px};

pub(crate) fn avatar_face(
    size: f32,
    photo: Option<&Path>,
    initials: impl Into<String>,
    background: gpui::Rgba,
    text_color: gpui::Rgba,
    text_size: Option<f32>,
) -> gpui::Div {
    let initials = initials.into();
    let Some(path) = photo else {
        return initials_face(size, initials, background, text_color, text_size);
    };
    let path = path.to_path_buf();
    gpui::div()
        .size(px(size))
        .rounded_full()
        .overflow_hidden()
        .flex_shrink_0()
        .child(
            gpui::img(path)
                .size(px(size))
                .object_fit(ObjectFit::Cover)
                .with_fallback(move || {
                    initials_face(size, initials.clone(), background, text_color, text_size)
                        .into_any_element()
                }),
        )
}

fn initials_face(
    size: f32,
    initials: String,
    background: gpui::Rgba,
    text_color: gpui::Rgba,
    text_size: Option<f32>,
) -> gpui::Div {
    let mut face = gpui::div()
        .size(px(size))
        .rounded_full()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(background)
        .text_color(text_color)
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(initials);
    if let Some(text_size) = text_size {
        face = face.text_size(px(text_size));
    }
    face
}

pub(crate) fn first_initial(name: &str) -> String {
    name.chars().next().unwrap_or('#').to_string()
}
