//! Wasabi desktop entry point.
//!
//! Startup order is deliberate (hydrate-first): the supervisor runtime is
//! built, the account session (which owns storage) is opened synchronously
//! before the GPUI event loop starts, so the window never renders against a
//! half-initialized core. Only then does the UI come up and drive everything
//! through the bridge.

mod core_bridge;
mod state;
mod theme;
mod views;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Context as _;
use gpui::prelude::*;
use gpui::{Bounds, TitlebarOptions, WindowBounds, WindowOptions, px};
use tracing_subscriber::EnvFilter;

use wasabi_core::supervisor::SupervisorConfig;
use wasabi_repository::{StorageLayout, StoreTuning};
use wasabi_whatsapp::session::{AccountSession, SessionConfig};

use crate::core_bridge::CoreBridge;
use crate::views::{BridgeGlobal, MainWindow, key_bindings};

const WINDOW_SIZE: f32 = 1280.0;
const WINDOW_MIN_SIZE: f32 = 980.0;

fn main() -> anyhow::Result<()> {
    install_tracing();

    let data_dir = dirs::data_dir()
        .context("no system data directory")?
        .join("wasabi");
    let supervisor =
        wasabi_core::supervisor::CoreSupervisor::start(SupervisorConfig::new(data_dir.clone()))?;
    let command_gate = Arc::new(AtomicBool::new(true));

    // Open storage + session before any UI exists. Failures surface
    // in-window instead of aborting the process.
    let layout = StorageLayout::new(data_dir);
    let opened = supervisor.handle().block_on(open_session(&layout));
    let open_error = opened.as_ref().err().map(|e| e.to_string());

    let bridge = CoreBridge::new(
        supervisor.handle().clone(),
        supervisor.invalidations().clone(),
        Arc::clone(&command_gate),
    );
    bridge.set_root_token(supervisor.root_cancellation());
    match opened {
        Ok(session) => {
            attach_text_sender(&bridge, &supervisor, &session);
            bridge.install_session(session);
        }
        Err(err) => tracing::error!(error = %err, "account session failed to open"),
    }
    let bridge = Arc::new(bridge);

    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx| {
            gpui_component::init(cx);
            // The shell is designed around the light reference palette.
            gpui_component::Theme::change(gpui_component::theme::ThemeMode::Light, None, cx);

            cx.bind_keys(key_bindings());
            cx.set_global(BridgeGlobal(Arc::clone(&bridge)));

            if let Some(err) = &open_error {
                tracing::error!(error = %err, "storage unavailable at startup");
            }

            let bounds = Bounds::centered(
                None,
                gpui::size(px(WINDOW_SIZE), px(WINDOW_SIZE * 0.72)),
                cx,
            );
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Wasabi".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                window_min_size: Some(gpui::size(px(WINDOW_MIN_SIZE), px(WINDOW_MIN_SIZE * 0.7))),
                ..Default::default()
            };

            cx.open_window(options, |window, cx| {
                // Root is the required top-level view for gpui-component
                // overlays (input context menus, tooltips).
                let main = cx.new(|cx| MainWindow::new(window, cx));
                cx.new(|cx| gpui_component::Root::new(main, window, cx))
            })
            .expect("open main window");
        });

    // The UI loop ended; run the deterministic shutdown sequence (command
    // gate closes first, then cancellation + drain inside the supervisor).
    command_gate.store(false, std::sync::atomic::Ordering::Release);
    supervisor.shutdown();
    Ok(())
}

/// Open (and migrate) the account database plus its session wrapper.
async fn open_session(layout: &StorageLayout) -> Result<Arc<AccountSession>, anyhow::Error> {
    let account = wasabi_domain::AccountId::FIRST;
    layout
        .ensure_dirs(Some(account.get()))
        .context("create data directories")?;

    let tuning = StoreTuning::default();
    let config = SessionConfig::default();
    AccountSession::open(layout.account_db(account.get()), &tuning, &config)
        .await
        .context("open account store")
}

/// Bind the composer's outgoing path to the durable outbox pipeline. The
/// client handle is resolved per send (it exists only while a Bot loop is
/// running); the bridge queues sends attempted before that.
fn attach_text_sender(
    bridge: &CoreBridge,
    supervisor: &wasabi_core::supervisor::CoreSupervisor,
    session: &Arc<AccountSession>,
) {
    let outbox = wasabi_whatsapp::outbox::Outbox::new(Arc::clone(session.chats()));
    let session = Arc::clone(session);
    let handle = supervisor.handle();
    let sender: core_bridge::TextSender = Arc::new(move |to: String, text: String| {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let session = Arc::clone(&session);
        let outbox = outbox.clone();
        handle.spawn(async move {
            let result = match to.parse::<whatsapp_rust::Jid>() {
                Ok(jid) => match session.client().await {
                    Some(client) => outbox
                        .send_text(&client, jid, text)
                        .await
                        .map(|r| r.message_id)
                        .map_err(|e| e.to_string()),
                    None => Err("not connected".to_string()),
                },
                Err(e) => Err(format!("bad jid: {e}")),
            };
            let _ = tx.send(result);
        });
        rx
    });
    bridge.set_text_sender(sender);
}

fn install_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
