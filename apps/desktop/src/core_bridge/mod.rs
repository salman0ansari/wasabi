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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use tokio_util::sync::CancellationToken;
use wasabi_core::events::{Invalidation, InvalidationPublisher};
use wasabi_core::state::SessionState;
use wasabi_domain::{
    ChatId, ChatPage, ChatScope, DirectContactDetails, ErrorKind, GroupDetails, GroupPermissions,
    MessageId, MessagePage, PageCursor, Participant, ParticipantRole, SearchPage, SendContent,
    SendReceipt, SendRequest, ServiceError,
};
use wasabi_repository::AccountStore;
use wasabi_whatsapp::lifecycle::QrState;
use wasabi_whatsapp::outbox::Outbox;
use wasabi_whatsapp::session::{AccountSession, SessionConfig};

/// Shared handle bundle handed to the UI.
pub struct CoreBridge {
    runtime: tokio::runtime::Handle,
    invalidations: InvalidationPublisher,
    command_gate: Arc<AtomicBool>,
    root_token: OnceLock<CancellationToken>,
    store: Arc<RwLock<Option<Arc<AccountStore>>>>,
    session: Arc<RwLock<Option<Arc<AccountSession>>>>,
    outbox: Arc<RwLock<Option<Outbox>>>,
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

    /// Forward materialization-store changes at their real scope. Lag is the
    /// only case that deliberately falls back to a coarse chat refresh.
    fn forward_store_changes(&self) {
        let Ok(store) = self.store_snapshot() else {
            return;
        };
        let invalidations = self.invalidations.clone();
        self.runtime.spawn(async move {
            let mut changes = store.subscribe_changes();
            loop {
                match changes.recv().await {
                    Ok(wasabi_repository::StoreChange::Chats) => {
                        invalidations.publish(Invalidation::Chats)
                    }
                    Ok(wasabi_repository::StoreChange::Contacts) => {
                        invalidations.publish(Invalidation::Contacts)
                    }
                    Ok(wasabi_repository::StoreChange::Messages { chat }) => {
                        invalidations.publish(Invalidation::Messages { chat })
                    }
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
        scope: ChatScope,
        after: Option<wasabi_domain::page::ChatPageCursor>,
        limit: usize,
    ) -> Result<ChatPage, String> {
        let store = self.store_snapshot()?;
        self.run_on_core(async move {
            store
                .chat_page(scope, after, limit)
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

    pub async fn search_messages(
        &self,
        query: String,
        chat_scope: Option<String>,
        page: usize,
    ) -> Result<SearchPage, String> {
        let store = self.store_snapshot()?;
        self.run_on_core(async move {
            wasabi_repository::search::SearchService::new(Arc::clone(store.chats()))
                .search(&query, chat_scope, page)
                .await
                .map_err(service_message)
        })
        .await
    }

    pub async fn direct_contact_details(
        &self,
        jid: String,
    ) -> Result<DirectContactDetails, String> {
        let store = self.store_snapshot()?;
        self.run_on_core(async move {
            store
                .direct_contact_details(&jid)
                .await
                .map_err(service_message)
        })
        .await
    }

    /// Fetch complete group metadata from the live client and immediately
    /// project it into product types. No protocol value crosses this method.
    pub async fn group_details(&self, chat: String) -> Result<GroupDetails, String> {
        let session = self.session_snapshot()?;
        self.run_on_core(async move {
            let jid: whatsapp_rust::Jid = chat
                .parse()
                .map_err(|error| format!("Invalid group identity: {error}"))?;
            let client = session
                .client()
                .await
                .ok_or_else(|| "Connect to refresh group information".to_string())?;
            let metadata = client
                .groups()
                .get_metadata(&jid)
                .await
                .map_err(|error| error.to_string())?;

            let mut participants = Vec::with_capacity(metadata.participants.len());
            for participant in metadata.participants {
                let identity = participant
                    .phone_number
                    .as_ref()
                    .unwrap_or(&participant.jid)
                    .clone();
                let contact = session
                    .chats
                    .contact(&identity)
                    .await
                    .map_err(|error| error.to_string())?;
                let display_name = contact
                    .as_ref()
                    .and_then(|contact| contact.display_name())
                    .map(str::to_string)
                    .or_else(|| participant.username.as_ref().map(ToString::to_string))
                    .unwrap_or_else(|| identity.user.to_string());
                let role = match participant.participant_type {
                    whatsapp_rust::ParticipantType::Member => ParticipantRole::Member,
                    whatsapp_rust::ParticipantType::Admin => ParticipantRole::Admin,
                    whatsapp_rust::ParticipantType::SuperAdmin => ParticipantRole::SuperAdmin,
                };
                participants.push(Participant {
                    jid: identity.to_string(),
                    display_name,
                    avatar: None,
                    role,
                    // The dependency intentionally keeps own-JID matching
                    // internal. Until the profile projection exposes it, the
                    // UI does not guess which participant is the local user.
                    is_self: false,
                });
            }

            participants.sort_by(|left, right| {
                role_rank(right.role)
                    .cmp(&role_rank(left.role))
                    .then_with(|| {
                        left.display_name
                            .to_lowercase()
                            .cmp(&right.display_name.to_lowercase())
                    })
            });
            let participant_count = metadata.size.map_or(participants.len(), |size| size as usize);
            Ok(GroupDetails {
                chat: ChatId::new(metadata.id.to_string()),
                subject: metadata.subject,
                description: metadata.description,
                avatar: None,
                participant_count,
                participants,
                permissions: GroupPermissions {
                    only_admins_edit: metadata.is_locked,
                    only_admins_send: metadata.is_announcement,
                    membership_approval: metadata.membership_approval,
                    current_user_role: None,
                },
            })
        })
        .await
    }

    // ---- Sending ------------------------------------------------------------

    /// Submit an immutable product request through the durable account
    /// outbox. The live client is resolved inside the core runtime at the
    /// moment of submission; no protocol/client type crosses this boundary.
    pub async fn send(&self, request: SendRequest) -> Result<SendReceipt, ServiceError> {
        if !self.commands_accepted() {
            return Err(ServiceError::new(ErrorKind::Cancelled, "shutting down"));
        }
        let session = self
            .session_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::NotPaired, detail))?;
        let outbox = self
            .outbox_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        self.run_on_core_service(async move {
            let to = request
                .chat
                .as_str()
                .parse::<whatsapp_rust::Jid>()
                .map_err(|e| {
                    ServiceError::new(ErrorKind::InvalidRequest, format!("bad chat id: {e}"))
                })?;
            let client = session.client().await.ok_or_else(|| {
                ServiceError::new(ErrorKind::NotConnected, "no live protocol client")
            })?;
            let receipt = match request.content {
                SendContent::Text { body } => outbox.send_text(&client, to, body).await,
                _ => {
                    return Err(ServiceError::new(
                        ErrorKind::Unsupported,
                        "send content is not implemented by this backend",
                    ));
                }
            }
            .map_err(map_outbox_error)?;
            Ok(SendReceipt {
                message: MessageId::new(receipt.message_id),
            })
        })
        .await
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

    fn outbox_snapshot(&self) -> Result<Outbox, String> {
        self.outbox
            .read()
            .expect("outbox lock")
            .clone()
            .ok_or_else(|| "outbox not ready".to_string())
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

    async fn run_on_core_service<T, F>(&self, fut: F) -> Result<T, ServiceError>
    where
        F: std::future::Future<Output = Result<T, ServiceError>> + Send + 'static,
        T: Send + 'static,
    {
        self.runtime
            .spawn(fut)
            .await
            .map_err(|e| ServiceError::new(ErrorKind::Internal, format!("core task: {e}")))?
    }
}

fn map_outbox_error(error: wasabi_whatsapp::outbox::OutboxError) -> ServiceError {
    use wasabi_whatsapp::outbox::OutboxError;

    let kind = match &error {
        OutboxError::NotConnected => ErrorKind::NotConnected,
        OutboxError::Store(_) => ErrorKind::Database,
        OutboxError::Send { .. } => ErrorKind::Protocol,
        OutboxError::InvalidRequest(_) => ErrorKind::InvalidRequest,
        _ => ErrorKind::Internal,
    };
    ServiceError::new(kind, error.to_string())
}

fn role_rank(role: ParticipantRole) -> u8 {
    match role {
        ParticipantRole::Member => 0,
        ParticipantRole::Admin => 1,
        ParticipantRole::SuperAdmin => 2,
    }
}

/// Coarse, user-renderable message; diagnostics stay in logs.
fn service_message(e: ServiceError) -> String {
    tracing::warn!(kind = %e.kind, detail = %e.detail, "core query failed");
    e.ui_message().to_string()
}
