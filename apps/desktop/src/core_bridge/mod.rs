//! The single boundary between the GPUI process and the core domain.
//!
//! Only this module holds core handles (the Tokio runtime handle, session,
//! store, outbox, cancellation root). Every method exposed to GPUI tasks
//! either returns plain data or a future that is safe to await without a
//! Tokio runtime context: long-running core work is driven on the core
//! runtime through [`tokio::runtime::Handle::spawn`], and the GPUI side
//! awaits the returned join handle, which is context-free.
//!
//! Durable-change signals from the materialization store are forwarded into
//! the core [`InvalidationPublisher`] here as well, so the UI listens to one
//! bounded channel regardless of how many producers exist underneath.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use tokio_util::sync::CancellationToken;
use wasabi_core::events::{Invalidation, InvalidationPublisher};
use wasabi_core::state::SessionState;
use wasabi_domain::ServiceError;
use wasabi_domain::{ChatSummary, MessagePage, PageCursor};
use wasabi_repository::AccountStore;
use wasabi_whatsapp::lifecycle::QrState;
use wasabi_whatsapp::outbox::Outbox;
use wasabi_whatsapp::session::{AccountSession, SessionConfig};

/// Maximum queued outgoing texts while no transport seam is attached.
const PENDING_SEND_CAPACITY: usize = 32;

/// Transport seam for outgoing text.
///
/// The live vendored client handle is not reachable through the session
/// facade yet, so the final wiring happens at startup: a `TextSender` takes
/// `(chat_jid, text)` and returns a one-shot receiver for the pipeline
/// result. It spawns onto the core runtime and drives
/// `Outbox::send_text(&client, jid, text)`:
///
/// ```ignore
/// let sender: TextSender = Arc::new(move |to, text| {
///     let (tx, rx) = tokio::sync::oneshot::channel();
///     let client = Arc::clone(&client);
///     let outbox = outbox.clone();
///     core_handle.spawn(async move {
///         let res = match to.parse::<whatsapp_rust::Jid>() {
///             Ok(jid) => outbox.send_text(&client, jid, text).await
///                 .map(|r| r.message_id)
///                 .map_err(|e| e.to_string()),
///             Err(e) => Err(format!("bad jid: {e}")),
///         };
///         let _ = tx.send(res);
///     });
///     rx
/// });
/// bridge.set_text_sender(sender);
/// ```
pub type TextSender = Arc<
    dyn Fn(String, String) -> tokio::sync::oneshot::Receiver<Result<String, String>> + Send + Sync,
>;

struct PendingSend {
    chat: String,
    text: String,
}

/// Shared handle bundle handed to the UI.
pub struct CoreBridge {
    runtime: tokio::runtime::Handle,
    invalidations: InvalidationPublisher,
    command_gate: Arc<AtomicBool>,
    root_token: OnceLock<CancellationToken>,
    store: Arc<RwLock<Option<Arc<AccountStore>>>>,
    session: Arc<RwLock<Option<Arc<AccountSession>>>>,
    outbox: Arc<RwLock<Option<Outbox>>>,
    sender: Arc<RwLock<Option<TextSender>>>,
    pending: Arc<Mutex<VecDeque<PendingSend>>>,
}

impl CoreBridge {
    pub fn new(
        runtime: tokio::runtime::Handle,
        invalidations: InvalidationPublisher,
        command_gate: Arc<AtomicBool>,
    ) -> Self {
        Self {
            runtime,
            invalidations,
            command_gate,
            root_token: OnceLock::new(),
            store: Arc::new(RwLock::new(None)),
            session: Arc::new(RwLock::new(None)),
            outbox: Arc::new(RwLock::new(None)),
            sender: Arc::new(RwLock::new(None)),
            pending: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Install the cancellation-tree root; required before connecting.
    pub fn set_root_token(&self, token: CancellationToken) {
        let _ = self.root_token.set(token);
    }

    /// Install the account session opened during startup hydration. Derives
    /// the store and outbox handles from it and starts the durable-change
    /// forwarder.
    pub fn install_session(&self, session: Arc<AccountSession>) {
        *self.store.write().expect("store lock") = Some(Arc::clone(&session.store));
        *self.outbox.write().expect("outbox lock") = Some(Outbox::new(Arc::clone(&session.chats)));
        *self.session.write().expect("session lock") = Some(session);
        self.forward_store_changes();
    }

    /// Attach the outgoing-text transport seam. See [`TextSender`].
    // Called by integration once a live client handle exists.
    #[allow(dead_code)]
    pub fn set_text_sender(&self, sender: TextSender) {
        *self.sender.write().expect("sender lock") = Some(sender);
    }

    /// Snapshot of the account outbox for the transport-seam wiring.
    #[allow(dead_code)]
    pub fn outbox_snapshot(&self) -> Option<Outbox> {
        self.outbox.read().expect("outbox lock").clone()
    }

    pub fn store_ready(&self) -> bool {
        self.store.read().expect("store lock").is_some()
    }

    pub fn commands_accepted(&self) -> bool {
        self.command_gate.load(Ordering::Acquire)
    }

    pub fn invalidations(&self) -> &InvalidationPublisher {
        &self.invalidations
    }

    // ---- Feeds --------------------------------------------------------------

    pub fn subscribe_state(&self) -> Option<tokio::sync::watch::Receiver<SessionState>> {
        self.session
            .read()
            .expect("session lock")
            .as_ref()
            .map(|s| s.subscribe_state())
    }

    pub fn subscribe_qr(&self) -> Option<tokio::sync::watch::Receiver<Option<QrState>>> {
        self.session
            .read()
            .expect("session lock")
            .as_ref()
            .map(|s| s.subscribe_qr())
    }

    /// Forward materialization-store change signals into the invalidation
    /// publisher as a coarse `Chats` signal. Payloads are deliberately not
    /// inspected: projections recover by re-querying durable state, so the
    /// coarsest correct signal is enough.
    fn forward_store_changes(&self) {
        let Ok(store) = self.store_snapshot() else {
            return;
        };
        let invalidations = self.invalidations.clone();
        self.runtime.spawn(async move {
            let mut changes = store.subscribe_changes();
            loop {
                match changes.recv().await {
                    Ok(_) => invalidations.publish(Invalidation::Chats),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        invalidations.publish(Invalidation::Chats)
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // ---- Session lifecycle --------------------------------------------------

    pub async fn connect_session(&self) -> Result<(), String> {
        let session = self.session_snapshot()?;
        let root = self.root_token.get().cloned().ok_or("no cancel root")?;
        self.run_on_core(async move {
            let config: SessionConfig = session.config().clone();
            session
                .connect(&config, root.child_token())
                .await
                .map_err(|e| e.to_string())
        })
        .await
    }

    pub async fn start_pairing(&self) -> Result<(), String> {
        let session = self.session_snapshot()?;
        let root = self.root_token.get().cloned().ok_or("no cancel root")?;
        self.run_on_core(async move {
            session
                .start_pairing(root)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .await
    }

    pub async fn stop_session(&self) -> Result<(), String> {
        let session = self.session_snapshot()?;
        self.run_on_core(async move {
            session.stop().await;
            Ok(())
        })
        .await
    }

    /// Barrier over all writes enqueued before this call.
    pub async fn flush_storage(&self) -> Result<(), String> {
        let session = self.session_snapshot()?;
        self.run_on_core(async move { session.store.flush().await.map_err(|e| e.to_string()) })
            .await
    }

    // ---- Queries ------------------------------------------------------------

    pub async fn load_chat_page(
        &self,
        include_archived: bool,
        after: Option<wasabi_domain::page::ChatPageCursor>,
        limit: usize,
    ) -> Result<Vec<ChatSummary>, String> {
        let store = self.store_snapshot()?;
        self.run_on_core(async move {
            store
                .chat_page(include_archived, after, limit)
                .await
                .map_err(service_message)
        })
        .await
    }

    pub async fn load_message_page(
        &self,
        chat: &str,
        before: Option<PageCursor>,
        limit: usize,
    ) -> Result<MessagePage, String> {
        let store = self.store_snapshot()?;
        let chat = chat.to_string();
        self.run_on_core(async move {
            store
                .message_page(&chat, before, limit)
                .await
                .map_err(service_message)
        })
        .await
    }

    // ---- Sending ------------------------------------------------------------

    /// Send one text through the transport seam, parking it while no seam is
    /// attached yet. Returns the pipeline message id, or `"queued"` when the
    /// message waits for the transport to appear.
    pub async fn send_text(&self, chat: String, text: String) -> Result<String, String> {
        if !self.commands_accepted() {
            return Err("shutting down".to_string());
        }
        if let Some(rx) = self.dispatch(chat.clone(), text.clone()) {
            match rx.await {
                Ok(Ok(message_id)) => return Ok(message_id),
                Ok(Err(err)) if err != "not connected" => return Err(err),
                Ok(Err(_)) | Err(_) => {
                    // The seam exists for the lifetime of the app, but the
                    // live client does not. Preserve the user's message in
                    // the bounded in-memory handoff until the next
                    // Connected transition instead of losing it at the
                    // disconnect boundary.
                }
            }
        }
        self.enqueue_pending(chat, text)?;
        Ok("queued".to_string())
    }

    /// Drain messages parked while the transport was absent. Called when the
    /// session reaches Connected; each entry goes through the same seam as a
    /// fresh send.
    pub async fn flush_pending(&self) {
        loop {
            let next = self.pending.lock().expect("pending lock").pop_front();
            let Some(PendingSend { chat, text }) = next else {
                break;
            };
            if let Some(rx) = self.dispatch(chat.clone(), text.clone()) {
                match rx.await {
                    Ok(Ok(_)) => {}
                    Ok(Err(_)) | Err(_) => {
                        // Put the current item back at the front. The
                        // transport may have disappeared between items; a
                        // later Connected transition should retry it with
                        // its original text and destination.
                        self.pending
                            .lock()
                            .expect("pending lock")
                            .push_front(PendingSend { chat, text });
                        break;
                    }
                }
            } else {
                self.pending
                    .lock()
                    .expect("pending lock")
                    .push_front(PendingSend { chat, text });
                break;
            }
        }
    }

    pub fn has_pending_sends(&self) -> bool {
        !self.pending.lock().expect("pending lock").is_empty()
    }

    fn dispatch(
        &self,
        chat: String,
        text: String,
    ) -> Option<tokio::sync::oneshot::Receiver<Result<String, String>>> {
        let guard = self.sender.read().expect("sender lock");
        guard.as_ref().map(|send| send(chat, text))
    }

    fn enqueue_pending(&self, chat: String, text: String) -> Result<(), String> {
        let mut queue = self.pending.lock().expect("pending lock");
        if queue.len() >= PENDING_SEND_CAPACITY {
            return Err("outbox full, try again shortly".to_string());
        }
        queue.push_back(PendingSend { chat, text });
        Ok(())
    }

    // ---- Plumbing -----------------------------------------------------------

    fn store_snapshot(&self) -> Result<Arc<AccountStore>, String> {
        self.store
            .read()
            .expect("store lock")
            .clone()
            .ok_or_else(|| "storage not ready".to_string())
    }

    fn session_snapshot(&self) -> Result<Arc<AccountSession>, String> {
        self.session
            .read()
            .expect("session lock")
            .clone()
            .ok_or_else(|| "no active session".to_string())
    }

    /// Drive a future on the core runtime and hand back its result. Awaiting
    /// the join handle needs no runtime context, keeping GPUI tasks clean.
    async fn run_on_core<T, F>(&self, fut: F) -> Result<T, String>
    where
        F: std::future::Future<Output = Result<T, String>> + Send + 'static,
        T: Send + 'static,
    {
        self.runtime
            .spawn(fut)
            .await
            .map_err(|e| format!("core task: {e}"))?
    }
}

/// Coarse, user-renderable message; diagnostics stay in logs.
fn service_message(e: ServiceError) -> String {
    tracing::warn!(kind = %e.kind, detail = %e.detail, "core query failed");
    e.ui_message().to_string()
}
