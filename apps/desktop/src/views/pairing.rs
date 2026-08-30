//! QR pairing panel with request feedback and a validity countdown driven by
//! the session's QR watch.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{Context, px};
use gpui_component::input::Input;

use crate::theme;
use crate::views::root::MainWindow;
use wasabi_core::state::SessionState;

const QR_FRAME_SIZE: f32 = 240.0;
const QR_QUIET_ZONE: usize = 4;

pub fn pairing_panel(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let session = this.session.clone();
    if session.use_phone_pairing {
        return phone_pairing_panel(this, &session, cx);
    }

    let countdown = session
        .qr_deadline
        .map(|deadline| deadline.saturating_duration_since(Instant::now()).as_secs() + 1);

    let status_line = if session.pairing_requesting {
        "Starting secure pairing…".to_string()
    } else {
        match countdown {
            Some(secs) => format!("Code refreshes in {secs}s"),
            None => match session.state {
                SessionState::Connecting => "Connecting to WhatsApp…".to_string(),
                SessionState::Failed { .. } | SessionState::Disconnected { .. } => {
                    "Connection needs attention. Try again.".to_string()
                }
                _ => "Waiting for QR…".to_string(),
            },
        }
    };

    let start_button = if session.pairing_requesting {
        gpui::div()
            .id("pairing-requesting")
            .rounded(px(theme::RADIUS_MD))
            .px(px(20.0))
            .py(px(10.0))
            .bg(theme::row_selected())
            .text_color(theme::text_secondary())
            .child("Starting…")
    } else if matches!(session.state, SessionState::Connecting) {
        gpui::div()
            .id("pairing-connecting")
            .rounded(px(theme::RADIUS_MD))
            .px(px(20.0))
            .py(px(10.0))
            .bg(theme::row_selected())
            .text_color(theme::text_secondary())
            .child("Connecting…")
    } else {
        let mut button = gpui::div()
            .id("start-pairing")
            .cursor_pointer()
            .rounded(px(theme::RADIUS_MD))
            .px(px(20.0))
            .py(px(10.0))
            .bg(theme::accent())
            .text_color(theme::text_on_accent())
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
            .border_color(theme::danger())
            .bg(theme::surface())
            .px(px(12.0))
            .py(px(10.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .text_size(px(theme::TEXT_SIZE_SM))
            .text_color(theme::danger())
            .child("Couldn’t start pairing")
            .child(error.to_string())
    });

    let qr_view = match session.qr_code.as_deref() {
        Some(payload) => match qrcode::QrCode::new(payload.as_bytes()) {
            Ok(code) => qr_code_view(code),
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
        .bg(theme::canvas())
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_NAME))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_primary())
                .child("Link with WhatsApp"),
        )
        .child(qr_view)
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_secondary())
                .child(status_line),
        )
        .when(
            countdown.is_none() && !matches!(session.state, SessionState::Connecting),
            |el| el.child(start_button),
        )
        .when_some(error_view, |el, error| el.child(error))
        .child(
            gpui::div()
                .max_w(px(360.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::text_secondary())
                .flex()
                .flex_col()
                .items_center()
                .child("Open WhatsApp on your phone")
                .child("Settings → Linked devices → Link a device"),
        )
        .child(
            link_button("Link with phone number instead")
                .on_click(cx.listener(|this, _, _, cx| this.show_phone_pairing(cx))),
        )
}

fn phone_pairing_panel(
    this: &mut MainWindow,
    session: &crate::state::SessionMirror,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let countdown = session
        .phone_pair_deadline
        .map(|deadline| deadline.saturating_duration_since(Instant::now()).as_secs() + 1);
    let code = session.phone_pair_code.as_deref().map(display_pair_code);
    let can_request = !session.phone_pair_requesting
        && !matches!(session.state, SessionState::Connecting)
        && code.is_none();

    let action = if session.phone_pair_requesting {
        action_status("Requesting code…").into_any_element()
    } else if matches!(session.state, SessionState::Connecting) {
        action_status("Connecting…").into_any_element()
    } else if can_request {
        gpui::div()
            .id("request-phone-pair-code")
            .cursor_pointer()
            .rounded(px(theme::RADIUS_MD))
            .px(px(20.0))
            .py(px(10.0))
            .bg(theme::accent())
            .text_color(theme::text_on_accent())
            .child(if session.phone_pair_error.is_some() {
                "Try again"
            } else {
                "Next"
            })
            .on_click(cx.listener(|this, _, _, cx| this.request_phone_pairing(cx)))
            .into_any_element()
    } else {
        action_status("Code ready").into_any_element()
    };

    let error = session.phone_pair_error.as_deref().map(|message| {
        gpui::div()
            .w_full()
            .rounded(px(theme::RADIUS_MD))
            .border_1()
            .border_color(theme::danger())
            .bg(theme::surface())
            .px(px(12.0))
            .py(px(10.0))
            .text_size(px(theme::TEXT_SIZE_SM))
            .text_color(theme::danger())
            .child(message.to_string())
    });

    gpui::div()
        .flex_1()
        .min_w(px(0.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::canvas())
        .child(
            gpui::div()
                .w(px(420.0))
                .max_w_full()
                .rounded(px(theme::RADIUS_MD))
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .p(px(28.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(16.0))
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_NAME))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_primary())
                        .child("Link with phone number"),
                )
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .text_center()
                        .child("Enter the number for your WhatsApp account, including its country code."),
                )
                .when(code.is_none(), |el| {
                    el.child(
                        gpui::div()
                            .w_full()
                            .child(Input::new(&this.phone_pair_input).cleanable(true)),
                    )
                    .child(action)
                })
                .when_some(code, |el, code| {
                    el.child(
                        gpui::div()
                            .w_full()
                            .rounded(px(theme::RADIUS_MD))
                            .bg(theme::row_selected())
                            .px(px(20.0))
                            .py(px(18.0))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                gpui::div()
                                    .text_size(px(26.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child(code),
                            )
                            .when_some(countdown, |el, seconds| {
                                el.child(
                                    gpui::div()
                                        .text_size(px(theme::TEXT_SIZE_SM))
                                        .text_color(theme::text_secondary())
                                        .child(format!("Expires in {seconds}s")),
                                )
                            }),
                    )
                    .child(
                        gpui::div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(px(5.0))
                            .text_size(px(theme::TEXT_SIZE_SM))
                            .text_color(theme::text_secondary())
                            .child("1. Open WhatsApp on your phone")
                            .child("2. Open Linked devices → Link a device")
                            .child("3. Choose Link with phone number and enter this code"),
                    )
                })
                .when_some(error, |el, error| el.child(error))
                .child(link_button("Use a QR code instead").on_click(cx.listener(
                    |this, _, _, cx| this.show_qr_pairing(cx),
                ))),
        )
}

fn display_pair_code(code: &str) -> String {
    let compact = code
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    match compact.get(0..4).zip(compact.get(4..8)) {
        Some((first, second)) => format!("{first}  {second}"),
        None => compact,
    }
}

fn action_status(text: &'static str) -> gpui::Div {
    gpui::div()
        .rounded(px(theme::RADIUS_MD))
        .px(px(20.0))
        .py(px(10.0))
        .bg(theme::row_selected())
        .text_color(theme::text_secondary())
        .child(text)
}

fn link_button(text: &'static str) -> gpui::Stateful<gpui::Div> {
    gpui::div()
        .id(text)
        .cursor_pointer()
        .px(px(8.0))
        .py(px(6.0))
        .text_size(px(theme::TEXT_SIZE_SM))
        .text_color(theme::accent())
        .child(text)
}

#[cfg(test)]
mod tests {
    use super::display_pair_code;

    #[test]
    fn companion_code_is_grouped_for_readability() {
        assert_eq!(display_pair_code("ABCD1234"), "ABCD  1234");
        assert_eq!(display_pair_code("ABCD 1234"), "ABCD  1234");
    }
}

fn qr_status(text: &'static str) -> gpui::Div {
    gpui::div()
        .size(px(QR_FRAME_SIZE))
        .rounded(px(theme::RADIUS_MD))
        .border_1()
        .border_dashed()
        .border_color(theme::border())
        .bg(theme::surface())
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::TEXT_SIZE_SM))
        .text_color(theme::text_secondary())
        .child(text)
}

fn qr_code_view(code: qrcode::QrCode) -> gpui::Div {
    let module_count = code.width();
    let matrix = code
        .to_colors()
        .into_iter()
        .map(|color| color == qrcode::Color::Dark)
        .collect::<Vec<_>>();
    let total_modules = module_count + QR_QUIET_ZONE * 2;

    gpui::div()
        .size(px(QR_FRAME_SIZE))
        .rounded(px(theme::RADIUS_MD))
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .overflow_hidden()
        .child(
            gpui::canvas(
                |_bounds, _window, _cx| (),
                move |bounds, (), window, _cx| {
                    let frame_size = bounds.size.width.min(bounds.size.height);
                    let module_size = px((frame_size / px(total_modules as f32)).floor().max(1.0));
                    let code_size = module_size * total_modules;
                    let origin = bounds.origin
                        + gpui::point(
                            (bounds.size.width - code_size) * 0.5,
                            (bounds.size.height - code_size) * 0.5,
                        );

                    window.paint_quad(gpui::fill(bounds, theme::surface()));

                    for y in 0..module_count {
                        for x in 0..module_count {
                            if !matrix[y * module_count + x] {
                                continue;
                            }

                            let module_origin = origin
                                + gpui::point(
                                    module_size * (QR_QUIET_ZONE + x),
                                    module_size * (QR_QUIET_ZONE + y),
                                );
                            let module_bounds =
                                gpui::bounds(module_origin, gpui::size(module_size, module_size));
                            window.paint_quad(gpui::fill(module_bounds, theme::text_primary()));
                        }
                    }
                },
            )
            .size_full(),
        )
}
