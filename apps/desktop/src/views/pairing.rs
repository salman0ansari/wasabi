//! QR pairing panel with request feedback and a validity countdown driven by
//! the session's QR watch.

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

    let status_line = if session.pairing_requesting {
        "Starting secure pairing…".to_string()
    } else {
        match countdown {
            Some(secs) => format!("Code refreshes in {secs}s"),
            None => "Waiting for QR…".to_string(),
        }
    };

    let start_button = if session.pairing_requesting {
        gpui::div()
            .id("pairing-requesting")
            .rounded(px(theme::RADIUS_MD))
            .px(px(20.0))
            .py(px(10.0))
            .bg(theme::ROW_SELECTED)
            .text_color(theme::TEXT_SECONDARY)
            .child("Starting…")
    } else {
        let mut button = gpui::div()
            .id("start-pairing")
            .cursor_pointer()
            .rounded(px(theme::RADIUS_MD))
            .px(px(20.0))
            .py(px(10.0))
            .bg(theme::ACCENT)
            .text_color(theme::TEXT_ON_ACCENT)
            .child(if session.pairing_error.is_some() {
                "Try again"
            } else {
                "Link this device"
            });
        button = button.on_click(cx.listener(move |this, _, _, cx| {
            this.request_pairing(cx);
        }));
        button
    };

    let error_view = session.pairing_error.as_deref().map(|error| {
        gpui::div()
            .max_w(px(360.0))
            .rounded(px(theme::RADIUS_MD))
            .border_1()
            .border_color(theme::DANGER)
            .bg(theme::SURFACE)
            .px(px(12.0))
            .py(px(10.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .text_size(px(theme::TEXT_SIZE_SM))
            .text_color(theme::DANGER)
            .child("Couldn’t start pairing")
            .child(error.to_string())
    });

    let qr_view = match session.qr_code.as_deref() {
        Some(payload) => match qrcode::QrCode::new(payload.as_bytes()) {
            Ok(code) => {
                // Dense1x2 keeps the modules square on a desktop text grid
                // while avoiding a large raster allocation on the GPUI side.
                let rendered = code
                    .render::<qrcode::render::unicode::Dense1x2>()
                    .quiet_zone(true)
                    .build()
                    .to_string();
                gpui::div()
                    .size(px(220.0))
                    .rounded(px(theme::RADIUS_MD))
                    .border_1()
                    .border_color(theme::BORDER)
                    .bg(theme::SURFACE)
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .font_family("monospace")
                    .text_size(px(8.0))
                    .text_color(theme::TEXT_PRIMARY)
                    .child(rendered)
            }
            Err(_) => qr_status("QR unavailable — waiting for a fresh code"),
        },
        None => qr_status("Waiting for QR…"),
    };

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
        .child(qr_view)
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::TEXT_SECONDARY)
                .child(status_line),
        )
        .when(countdown.is_none(), |el| el.child(start_button))
        .when_some(error_view, |el, error| el.child(error))
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

fn qr_status(text: &'static str) -> gpui::Div {
    gpui::div()
        .size(px(220.0))
        .rounded(px(theme::RADIUS_MD))
        .border_1()
        .border_dashed()
        .border_color(theme::BORDER)
        .bg(theme::SURFACE)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::TEXT_SIZE_SM))
        .text_color(theme::TEXT_SECONDARY)
        .child(text)
}
