//! QR pairing panel: placeholder for the rendered code plus a validity
//! countdown driven by the session's QR watch.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{Context, px};

use crate::state::SessionMirror;
use crate::theme;
use crate::views::root::MainWindow;

pub fn pairing_panel(session: &SessionMirror, cx: &mut Context<MainWindow>) -> gpui::Div {
    let countdown = session
        .qr_deadline
        .map(|deadline| deadline.saturating_duration_since(Instant::now()).as_secs() + 1);

    let status_line = match countdown {
        Some(secs) => format!("Code refreshes in {secs}s"),
        None => "Waiting for QR…".to_string(),
    };

    let start_button = {
        let mut button = gpui::div()
            .id("start-pairing")
            .cursor_pointer()
            .rounded(px(theme::RADIUS_MD))
            .px(px(20.0))
            .py(px(10.0))
            .bg(theme::ACCENT)
            .text_color(theme::TEXT_ON_ACCENT)
            .child("Link this device");
        button = button.on_click(cx.listener(move |this, _, _, cx| {
            this.request_pairing(cx);
        }));
        button
    };

    // QR rendering arrives with the media pipeline; the dashed frame reserves
    // the exact footprint so layout stays stable once codes are drawn.
    let qr_placeholder = gpui::div()
        .size(px(220.0), px(220.0))
        .rounded(px(theme::RADIUS_MD))
        .border_1()
        .border_dashed()
        .border_color(theme::BORDER)
        .bg(theme::SURFACE)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(28.0))
        .text_color(theme::BORDER)
        .child("▦");

    gpui::div()
        .flex_1()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(16.0))
        .bg(theme::CANVAS)
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_NAME))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::TEXT_PRIMARY)
                .child("Link with WhatsApp"),
        )
        .child(qr_placeholder)
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::TEXT_SECONDARY)
                .child(status_line),
        )
        .when(countdown.is_none(), |el| el.child(start_button))
        .child(
            gpui::div()
                .max_w(px(360.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::TEXT_SECONDARY)
                .flex()
                .flex_col()
                .items_center()
                .child("Open WhatsApp on your phone")
                .child("Settings → Linked devices → Link a device"),
        )
}
