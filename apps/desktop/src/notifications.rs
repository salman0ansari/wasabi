//! Linux desktop notification delivery and click routing.

use notify_rust::{Notification, Timeout};
use tokio::sync::mpsc::UnboundedSender;

use crate::state::DeviceSettings;

#[derive(Clone)]
pub struct NotificationDispatcher {
    clicked: UnboundedSender<wasabi_domain::NotificationCandidate>,
}

impl NotificationDispatcher {
    pub fn new(clicked: UnboundedSender<wasabi_domain::NotificationCandidate>) -> Self {
        Self { clicked }
    }

    /// Show one already-policy-checked candidate off the UI thread. The
    /// worker logs only delivery failures, never title/body/chat identity.
    pub fn show(&self, candidate: wasabi_domain::NotificationCandidate, settings: &DeviceSettings) {
        let clicked = self.clicked.clone();
        let clicked_candidate = candidate.clone();
        let body = notification_body(&candidate, settings);
        let sound = settings.notification_sound;
        let title = candidate.title;
        let _ = std::thread::Builder::new()
            .name("wasabi-notification".to_string())
            .spawn(move || {
                let mut notification = Notification::new();
                notification
                    .appname("wasabi")
                    .summary(&title)
                    .body(&body)
                    .icon("wasabi")
                    .action("default", "Open")
                    .timeout(Timeout::Milliseconds(8_000));
                if sound {
                    notification.sound_name("message-new-instant");
                }
                match notification.show() {
                    Ok(handle) => handle.wait_for_action(|action| {
                        if action == "default" {
                            let _ = clicked.send(clicked_candidate);
                        }
                    }),
                    Err(error) => tracing::warn!(error = %error, "desktop notification failed"),
                }
            });
    }
}

pub fn should_deliver(
    candidate: &wasabi_domain::NotificationCandidate,
    settings: &DeviceSettings,
    window_active: bool,
    started_at_ms: i64,
) -> bool {
    settings.desktop_notifications
        && !candidate.outgoing
        && !candidate.muted
        && candidate.eligible
        && candidate.timestamp_ms >= started_at_ms - 60_000
        && !(settings.suppress_when_focused && window_active)
}

fn notification_body(
    candidate: &wasabi_domain::NotificationCandidate,
    settings: &DeviceSettings,
) -> String {
    if settings.notification_previews {
        candidate.preview.clone()
    } else {
        "New message".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasabi_domain::{ChatId, MessageId, NotificationCandidate};

    fn candidate() -> NotificationCandidate {
        NotificationCandidate {
            chat: ChatId::new("chat@s.whatsapp.net"),
            message: MessageId::new("message"),
            title: "Contact".to_string(),
            preview: "private body".to_string(),
            timestamp_ms: 100_000,
            outgoing: false,
            muted: false,
            eligible: true,
        }
    }

    #[test]
    fn delivery_policy_suppresses_focus_mute_outgoing_and_stale_history() {
        let settings = DeviceSettings::default();
        assert!(should_deliver(&candidate(), &settings, false, 100_000));
        assert!(!should_deliver(&candidate(), &settings, true, 100_000));

        let mut muted = candidate();
        muted.muted = true;
        assert!(!should_deliver(&muted, &settings, false, 100_000));
        let mut outgoing = candidate();
        outgoing.outgoing = true;
        assert!(!should_deliver(&outgoing, &settings, false, 100_000));
        assert!(!should_deliver(&candidate(), &settings, false, 200_001));
    }

    #[test]
    fn hidden_previews_never_return_message_text() {
        let mut settings = DeviceSettings::default();
        settings.notification_previews = false;
        let body = notification_body(&candidate(), &settings);
        assert_eq!(body, "New message");
        assert!(!body.contains("private body"));
    }
}
