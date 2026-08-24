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

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use tokio_util::sync::CancellationToken;
use wasabi_core::events::{Invalidation, InvalidationPublisher};
use wasabi_core::state::SessionState;
use wasabi_domain::{
    CachedMedia, ChatAction, ChatId, ChatPage, ChatScope, DirectContactDetails, ErrorKind,
    GroupDetails, GroupPermissions, MediaDownloadRequest, MessageAction, MessageContext, MessageId,
    MessagePage, NotificationCandidate, PageCursor, PairingPhoneNumber, Participant,
    ParticipantRole, PhonePairCode, SearchPage, SendContent, SendReceipt, SendRequest, ServiceError,
    StagedAttachment, TransferId, TransferJob,
};
use wasabi_repository::AccountStore;
use wasabi_whatsapp::lifecycle::QrState;
use wasabi_whatsapp::outbox::Outbox;
use wasabi_whatsapp::session::{AccountSession, SessionConfig};

/// Mockable product boundary consumed by GPUI entities. Implementations may
/// use protocol, SQLite, or network types internally, but every value crossing
/// this trait is a Wasabi domain projection or a bounded feed.
#[async_trait::async_trait]
pub trait DesktopBackend: Send + Sync {
    fn store_ready(&self) -> bool;
    fn commands_accepted(&self) -> bool;
    fn invalidations(&self) -> &InvalidationPublisher;
    fn subscribe_state(&self) -> Option<tokio::sync::watch::Receiver<SessionState>>;
    fn subscribe_qr(&self) -> Option<tokio::sync::watch::Receiver<Option<QrState>>>;
    fn subscribe_typing(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<wasabi_domain::TypingUpdate>>;

    async fn connect_session(&self) -> Result<(), String>;
    async fn start_pairing(&self) -> Result<(), String>;
    async fn start_phone_pairing(
        &self,
        phone: PairingPhoneNumber,
    ) -> Result<PhonePairCode, String>;
    async fn cancel_phone_pairing(&self) -> Result<(), String>;
    async fn stop_session(&self) -> Result<(), String>;
    async fn logout(&self) -> Result<(), ServiceError>;
    async fn flush_storage(&self) -> Result<(), String>;
    async fn media_cache_usage(&self) -> Result<u64, ServiceError>;
    async fn set_media_cache_quota(&self, bytes: u64) -> Result<u64, ServiceError>;
    async fn clear_media_cache(&self) -> Result<(), ServiceError>;
    async fn load_chat_page(
        &self,
        scope: ChatScope,
        after: Option<wasabi_domain::ChatPageCursor>,
        limit: usize,
    ) -> Result<ChatPage, String>;
    async fn load_message_page(
        &self,
        chat: &str,
        before: Option<PageCursor>,
        limit: usize,
    ) -> Result<MessagePage, String>;
    async fn load_message_context(
        &self,
        chat: String,
        anchor: MessageId,
        before: usize,
        after: usize,
    ) -> Result<MessageContext, String>;
    async fn notification_candidate(
        &self,
        chat: String,
    ) -> Result<Option<NotificationCandidate>, String>;
    async fn search_messages(
        &self,
        query: String,
        chat_scope: Option<String>,
        page: usize,
    ) -> Result<SearchPage, String>;
    async fn direct_contact_details(&self, jid: String) -> Result<DirectContactDetails, String>;
    async fn group_details(&self, chat: String) -> Result<GroupDetails, String>;
    async fn set_favorite(&self, chat: ChatId, favorite: bool) -> Result<(), String>;
    async fn save_draft(
        &self,
        chat: ChatId,
        draft: Option<wasabi_domain::Draft>,
    ) -> Result<(), String>;
    async fn download_media(
        &self,
        request: MediaDownloadRequest,
    ) -> Result<CachedMedia, ServiceError>;
    // Kept hidden from navigation/composer until media sending is wired; the
    // service itself is complete and testable without exposing an inert icon.
    #[allow(dead_code)]
    async fn stage_attachment(
        &self,
        chat: ChatId,
        source: PathBuf,
    ) -> Result<StagedAttachment, ServiceError>;
    async fn cancel_transfer(&self, transfer: TransferId) -> Result<(), ServiceError>;
    async fn recover_staged_attachments(
        &self,
    ) -> Result<Vec<(ChatId, StagedAttachment)>, ServiceError>;
    async fn set_typing(&self, chat: ChatId, composing: bool) -> Result<(), ServiceError>;
    async fn send(&self, request: SendRequest) -> Result<SendReceipt, ServiceError>;
    async fn perform_message_action(&self, action: MessageAction) -> Result<(), ServiceError>;
    async fn perform_chat_action(&self, action: ChatAction) -> Result<(), ServiceError>;
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
    media_cache: wasabi_media::DiskCache,
    media: Arc<RwLock<Option<wasabi_media::MediaManager>>>,
}

struct InstalledSessionClientProvider {
    session: Arc<RwLock<Option<Arc<AccountSession>>>>,
}

#[async_trait::async_trait]
impl wasabi_media::ClientProvider for InstalledSessionClientProvider {
    async fn client(&self) -> Option<Arc<whatsapp_rust::client::Client>> {
        let session = self.session.read().expect("session lock").clone()?;
        session.client().await
    }
}

impl CoreBridge {
    pub fn new(
        runtime: tokio::runtime::Handle,
        invalidations: InvalidationPublisher,
        command_gate: Arc<AtomicBool>,
        media_cache: wasabi_media::DiskCache,
    ) -> Self {
        Self {
            runtime,
            invalidations,
            command_gate,
            root_token: OnceLock::new(),
            store: Arc::new(RwLock::new(None)),
            session: Arc::new(RwLock::new(None)),
            outbox: Arc::new(RwLock::new(None)),
            media_cache,
            media: Arc::new(RwLock::new(None)),
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
        *self.session.write().expect("session lock") = Some(Arc::clone(&session));
        let provider = Arc::new(InstalledSessionClientProvider {
            session: Arc::clone(&self.session),
        });
        *self.media.write().expect("media lock") = Some(wasabi_media::MediaManager::with_provider(
            self.media_cache.clone(),
            Arc::clone(&session.chats),
            provider,
        ));
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

    pub fn subscribe_typing(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<wasabi_domain::TypingUpdate>> {
        self.session
            .read()
            .expect("session lock")
            .as_ref()
            .map(|session| session.subscribe_typing())
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

    pub async fn start_phone_pairing(
        &self,
        phone: PairingPhoneNumber,
    ) -> Result<PhonePairCode, String> {
        let session = self.session_snapshot()?;
        self.run_on_core(async move { session.pair_with_phone(phone).await })
            .await
    }

    pub async fn cancel_phone_pairing(&self) -> Result<(), String> {
        let session = self.session_snapshot()?;
        self.run_on_core(async move {
            session.cancel_phone_pairing().await;
            Ok(())
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

    pub async fn logout(&self) -> Result<(), ServiceError> {
        if !self.commands_accepted() {
            return Err(ServiceError::new(ErrorKind::Cancelled, "shutting down"));
        }
        let session = self
            .session_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::NotPaired, detail))?;
        self.run_on_core_service(async move {
            session.logout().await;
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

    pub async fn media_cache_usage(&self) -> Result<u64, ServiceError> {
        let cache = self.media_cache.clone();
        self.run_on_core_service(async move {
            cache
                .total_bytes()
                .await
                .map_err(|error| ServiceError::new(ErrorKind::Internal, error.to_string()))
        })
        .await
    }

    pub async fn set_media_cache_quota(&self, bytes: u64) -> Result<u64, ServiceError> {
        let cache = self.media_cache.clone();
        self.run_on_core_service(async move {
            cache.set_quota(bytes);
            cache
                .evict_to(bytes)
                .await
                .map_err(|error| ServiceError::new(ErrorKind::Internal, error.to_string()))
        })
        .await
    }

    pub async fn clear_media_cache(&self) -> Result<(), ServiceError> {
        let cache = self.media_cache.clone();
        self.run_on_core_service(async move {
            cache
                .evict_to(0)
                .await
                .map(|_| ())
                .map_err(|error| ServiceError::new(ErrorKind::Internal, error.to_string()))
        })
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

    pub async fn load_message_context(
        &self,
        chat: String,
        anchor: MessageId,
        before: usize,
        after: usize,
    ) -> Result<MessageContext, String> {
        let store = self.store_snapshot()?;
        self.run_on_core(async move {
            store
                .message_context(&chat, anchor, before, after)
                .await
                .map_err(service_message)
        })
        .await
    }

    pub async fn notification_candidate(
        &self,
        chat: String,
    ) -> Result<Option<NotificationCandidate>, String> {
        let store = self.store_snapshot()?;
        self.run_on_core(async move {
            store
                .notification_candidate(&chat)
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

    pub async fn set_favorite(&self, chat: ChatId, favorite: bool) -> Result<(), String> {
        let store = self.store_snapshot()?;
        self.run_on_core(async move {
            store
                .set_favorite(chat, favorite)
                .await
                .map_err(service_message)
        })
        .await
    }

    pub async fn save_draft(
        &self,
        chat: ChatId,
        draft: Option<wasabi_domain::Draft>,
    ) -> Result<(), String> {
        let store = self.store_snapshot()?;
        self.run_on_core(async move {
            store
                .save_draft(chat, draft)
                .await
                .map_err(service_message)
        })
        .await
    }

    pub async fn download_media(
        &self,
        request: MediaDownloadRequest,
    ) -> Result<CachedMedia, ServiceError> {
        if !self.commands_accepted() {
            return Err(ServiceError::new(ErrorKind::Cancelled, "shutting down"));
        }
        let session = self
            .session_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        let manager = self
            .media_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        let cancel = self
            .root_token
            .get()
            .map(CancellationToken::child_token)
            .ok_or_else(|| ServiceError::new(ErrorKind::Internal, "no cancellation root"))?;
        self.run_on_core_service(async move {
            let chat = request
                .chat
                .as_str()
                .parse::<whatsapp_rust::Jid>()
                .map_err(|error| {
                    ServiceError::new(ErrorKind::InvalidRequest, error.to_string())
                })?;
            let stored = session
                .chats
                .message(&chat, request.media.as_str())
                .await
                .map_err(|error| ServiceError::new(ErrorKind::Database, error.to_string()))?
                .ok_or_else(|| {
                    ServiceError::new(ErrorKind::InvalidRequest, "media message no longer exists")
                })?;
            let message = stored.message.ok_or_else(|| {
                ServiceError::new(ErrorKind::Unsupported, "media metadata is unavailable")
            })?;
            let downloadable = wasabi_media::media_downloadable(&message).ok_or_else(|| {
                ServiceError::new(ErrorKind::Unsupported, "this media cannot be downloaded")
            })?;
            let expected_sha = <[u8; 32]>::try_from(downloadable.file_sha256.as_slice()).ok();
            let path = manager
                .download(downloadable, expected_sha, None, cancel)
                .await
                .map_err(map_media_error)?;
            Ok(CachedMedia {
                media: request.media,
                path,
            })
        })
        .await
    }

    #[allow(dead_code)]
    pub async fn stage_attachment(
        &self,
        chat: ChatId,
        source: PathBuf,
    ) -> Result<StagedAttachment, ServiceError> {
        if !self.commands_accepted() {
            return Err(ServiceError::new(ErrorKind::Cancelled, "shutting down"));
        }
        let store = self
            .store_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        let manager = self
            .media_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        let cancel = self
            .root_token
            .get()
            .map(CancellationToken::child_token)
            .ok_or_else(|| ServiceError::new(ErrorKind::Internal, "no cancellation root"))?;
        let transfer = next_transfer_id();
        self.run_on_core_service(async move {
            let staged = manager
                .stage_upload(source, transfer.clone(), cancel)
                .await
                .map_err(map_media_error)?;
            let mut job = TransferJob::staged_upload(
                transfer,
                chat,
                staged.durable_path.clone(),
                staged.attachment.bytes_total,
            );
            job.payload = Some(staged.payload);
            if let Err(error) = store.save_transfer_job(job).await {
                // The database row is the owner record. If it cannot commit,
                // remove the otherwise orphaned plaintext immediately.
                let _ = manager.discard_staged_upload(staged.durable_path).await;
                return Err(error);
            }
            Ok(staged.attachment)
        })
        .await
    }

    pub async fn cancel_transfer(&self, transfer: TransferId) -> Result<(), ServiceError> {
        let store = self
            .store_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        let manager = self
            .media_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        self.run_on_core_service(async move {
            let job = store
                .transfer_job(transfer.clone())
                .await?
                .ok_or_else(|| {
                    ServiceError::new(ErrorKind::InvalidRequest, "attachment transfer not found")
                })?;
            if !job.state.is_terminal()
                && !store
                    .set_transfer_state(
                        transfer,
                        wasabi_domain::TransferState::Cancelled,
                        None,
                    )
                    .await?
            {
                return Err(ServiceError::new(
                    ErrorKind::InvalidRequest,
                    "attachment transfer could not be cancelled",
                ));
            }
            if let Some(source) = job.source_path {
                manager.discard_staged_upload(source).await.map_err(map_media_error)?;
            }
            Ok(())
        })
        .await
    }

    pub async fn recover_staged_attachments(
        &self,
    ) -> Result<Vec<(ChatId, StagedAttachment)>, ServiceError> {
        let store = self
            .store_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        self.run_on_core_service(async move {
            let jobs = store.transfer_jobs(false).await?;
            let mut recovered = Vec::new();
            for job in jobs {
                if job.direction != wasabi_domain::TransferDirection::OutgoingUpload {
                    continue;
                }
                let Some(source) = job.source_path.as_ref() else {
                    let _ = store
                        .set_transfer_state(
                            job.transfer,
                            wasabi_domain::TransferState::FailedPermanent,
                            Some(ErrorKind::MediaUnavailable),
                        )
                        .await;
                    continue;
                };
                if tokio::fs::metadata(source)
                    .await
                    .map(|metadata| !metadata.is_file())
                    .unwrap_or(true)
                {
                    let _ = store
                        .set_transfer_state(
                            job.transfer,
                            wasabi_domain::TransferState::FailedPermanent,
                            Some(ErrorKind::MediaUnavailable),
                        )
                        .await;
                    continue;
                }
                if matches!(
                    job.state,
                    wasabi_domain::TransferState::Queued | wasabi_domain::TransferState::Running
                ) {
                    let _ = store
                        .set_transfer_state(
                            job.transfer.clone(),
                            wasabi_domain::TransferState::FailedRetryable,
                            Some(ErrorKind::NotConnected),
                        )
                        .await;
                }
                let Some(payload) = job.payload else {
                    let _ = store
                        .set_transfer_state(
                            job.transfer,
                            wasabi_domain::TransferState::FailedPermanent,
                            Some(ErrorKind::InvalidRequest),
                        )
                        .await;
                    continue;
                };
                recovered.push((
                    job.chat,
                    StagedAttachment {
                        transfer: job.transfer,
                        kind: payload.kind,
                        display_name: payload.display_name,
                        mime_type: payload.mime_type,
                        bytes_total: job.bytes_total.unwrap_or_default(),
                    },
                ));
            }
            Ok(recovered)
        })
        .await
    }

    pub async fn set_typing(
        &self,
        chat: ChatId,
        composing: bool,
    ) -> Result<(), ServiceError> {
        if !self.commands_accepted() {
            return Err(ServiceError::new(ErrorKind::Cancelled, "shutting down"));
        }
        let session = self
            .session_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::NotPaired, detail))?;
        self.run_on_core_service(async move {
            let chat = chat
                .as_str()
                .parse::<whatsapp_rust::Jid>()
                .map_err(|error| {
                    ServiceError::new(ErrorKind::InvalidRequest, error.to_string())
                })?;
            let client = session.client().await.ok_or_else(|| {
                ServiceError::new(ErrorKind::NotConnected, "no live protocol client")
            })?;
            let state = client.chatstate();
            let result = if composing {
                state.send_composing(&chat).await
            } else {
                state.send_paused(&chat).await
            };
            result.map_err(|error| ServiceError::new(ErrorKind::Protocol, error.to_string()))
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
        let store = self
            .store_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        let manager = self
            .media_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        let cancel = self
            .root_token
            .get()
            .map(CancellationToken::child_token)
            .ok_or_else(|| ServiceError::new(ErrorKind::Internal, "no cancellation root"))?;
        self.run_on_core_service(async move {
            let to = request
                .chat
                .as_str()
                .parse::<whatsapp_rust::Jid>()
                .map_err(|e| {
                    ServiceError::new(ErrorKind::InvalidRequest, format!("bad chat id: {e}"))
                })?;
            let receipt = match request.content {
                SendContent::Text { body } => {
                    let client = session.client().await.ok_or_else(|| {
                        ServiceError::new(ErrorKind::NotConnected, "no live protocol client")
                    })?;
                    outbox.send_text(&client, to, body).await
                }
                SendContent::Attachment { transfer, caption } => {
                    let mut job = store
                        .transfer_job(transfer.clone())
                        .await?
                        .ok_or_else(|| {
                            ServiceError::new(
                                ErrorKind::InvalidRequest,
                                "attachment transfer does not exist",
                            )
                        })?;
                    if job.chat != request.chat {
                        return Err(ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "attachment transfer belongs to a different chat",
                        ));
                    }
                    if !matches!(
                        job.state,
                        wasabi_domain::TransferState::Staged
                            | wasabi_domain::TransferState::FailedRetryable
                    ) {
                        return Err(ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "attachment transfer is not sendable",
                        ));
                    }
                    let source = job.source_path.clone().ok_or_else(|| {
                        ServiceError::new(ErrorKind::InvalidRequest, "attachment source is missing")
                    })?;
                    let bytes_total = job.bytes_total;
                    let mut payload = job.payload.take().ok_or_else(|| {
                        ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "attachment metadata is missing",
                        )
                    })?;
                    payload.caption = normalized_caption(caption)?;
                    if payload.kind == wasabi_domain::AttachmentKind::Audio
                        && payload.caption.is_some()
                    {
                        return Err(ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "audio attachments do not support captions",
                        ));
                    }
                    if !store
                        .update_transfer_payload(transfer.clone(), payload.clone())
                        .await?
                    {
                        return Err(ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "attachment changed before send",
                        ));
                    }
                    if !store
                        .set_transfer_state(
                            transfer.clone(),
                            wasabi_domain::TransferState::Queued,
                            None,
                        )
                        .await?
                    {
                        return Err(ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "attachment could not be queued",
                        ));
                    }
                    let client = match session.client().await {
                        Some(client) => client,
                        None => {
                            let _ = store
                                .set_transfer_state(
                                    transfer,
                                    wasabi_domain::TransferState::FailedRetryable,
                                    Some(ErrorKind::NotConnected),
                                )
                                .await;
                            return Err(ServiceError::new(
                                ErrorKind::NotConnected,
                                "no live protocol client",
                            ));
                        }
                    };
                    if !store
                        .set_transfer_state(
                            transfer.clone(),
                            wasabi_domain::TransferState::Running,
                            None,
                        )
                        .await?
                    {
                        return Err(ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "attachment could not start",
                        ));
                    }
                    let upload = match manager.upload(source.clone(), payload.kind, cancel).await {
                        Ok(upload) => upload,
                        Err(error) => {
                            let service = map_media_error(error);
                            let state = transfer_failure_state(service.kind);
                            let persisted_kind = (state
                                != wasabi_domain::TransferState::Cancelled)
                                .then_some(service.kind);
                            let _ = store
                                .set_transfer_state(transfer, state, persisted_kind)
                                .await;
                            return Err(service);
                        }
                    };
                    if let Some(bytes_total) = bytes_total {
                        let _ = store
                            .update_transfer_progress(
                                transfer.clone(),
                                bytes_total,
                                Some(bytes_total),
                            )
                            .await;
                    }
                    let message = attachment_message(upload, &payload);
                    match outbox.send_message(&client, to, message).await {
                        Ok(receipt) => {
                            // The outbox owns a committed proto from here, so
                            // plaintext staging can be erased.
                            let _ = store
                                .set_transfer_state(
                                    transfer,
                                    wasabi_domain::TransferState::Succeeded,
                                    None,
                                )
                                .await;
                            let _ = manager.discard_staged_upload(source).await;
                            Ok(receipt)
                        }
                        Err(wasabi_whatsapp::outbox::OutboxError::Send {
                            message_id,
                            source: _,
                        }) => {
                            let _ = store
                                .set_transfer_state(
                                    transfer,
                                    wasabi_domain::TransferState::Succeeded,
                                    None,
                                )
                                .await;
                            let _ = manager.discard_staged_upload(source).await;
                            // Publication failed after the message proto was
                            // committed. Treat composer submission as accepted;
                            // the durable message row owns Retry/Delete now.
                            Ok(wasabi_whatsapp::outbox::SentReceipt { message_id })
                        }
                        Err(error) => {
                            let service = map_outbox_error_ref(&error);
                            let state = transfer_failure_state(service.kind);
                            let persisted_kind = (state
                                != wasabi_domain::TransferState::Cancelled)
                                .then_some(service.kind);
                            let _ = store
                                .set_transfer_state(transfer, state, persisted_kind)
                                .await;
                            Err(error)
                        }
                    }
                }
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

    pub async fn perform_message_action(
        &self,
        action: MessageAction,
    ) -> Result<(), ServiceError> {
        if !self.commands_accepted() {
            return Err(ServiceError::new(ErrorKind::Cancelled, "shutting down"));
        }
        let session = self
            .session_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        self.run_on_core_service(async move {
            let client = session.client().await.ok_or_else(|| {
                ServiceError::new(ErrorKind::NotConnected, "no live protocol client")
            })?;
            let target = action.target();
            let chat = target
                .chat
                .as_str()
                .parse::<whatsapp_rust::Jid>()
                .map_err(|error| {
                    ServiceError::new(ErrorKind::InvalidRequest, error.to_string())
                })?;
            let participant = (!target.from_me && target.chat.as_str().ends_with("@g.us"))
                .then(|| target.sender.parse::<whatsapp_rust::Jid>())
                .transpose()
                .map_err(|error| {
                    ServiceError::new(ErrorKind::InvalidRequest, error.to_string())
                })?;

            match action {
                MessageAction::Star { target, starred } => {
                    let actions = client.chat_actions();
                    let result = if starred {
                        actions
                            .star_message(
                                &chat,
                                participant.as_ref(),
                                target.message.as_str(),
                                target.from_me,
                            )
                            .await
                    } else {
                        actions
                            .unstar_message(
                                &chat,
                                participant.as_ref(),
                                target.message.as_str(),
                                target.from_me,
                            )
                            .await
                    };
                    result.map_err(|error| {
                        ServiceError::new(ErrorKind::Protocol, error.to_string())
                    })?;
                }
                MessageAction::React { target, emoji } => {
                    let key = whatsapp_rust::message_key(
                        target.message.as_str(),
                        &chat,
                        target.from_me,
                        participant.as_ref(),
                    );
                    client
                        .send_reaction(chat, key, &emoji)
                        .await
                        .map_err(|error| {
                            ServiceError::new(ErrorKind::Protocol, error.to_string())
                        })?;
                }
                MessageAction::DeleteForMe {
                    target,
                    delete_media,
                } => {
                    client
                        .chat_actions()
                        .delete_message_for_me(
                            &chat,
                            participant.as_ref(),
                            target.message.as_str(),
                            target.from_me,
                            delete_media,
                            Some(target.timestamp_ms),
                        )
                        .await
                        .map_err(|error| {
                            ServiceError::new(ErrorKind::Protocol, error.to_string())
                        })?;
                }
                MessageAction::RevokeForEveryone { target } => {
                    if !target.from_me {
                        return Err(ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "admin revocation requires an explicit permission-checked action",
                        ));
                    }
                    client
                        .revoke_message(
                            chat,
                            target.message.as_str(),
                            whatsapp_rust::RevokeType::Sender,
                        )
                        .await
                        .map_err(|error| {
                            ServiceError::new(ErrorKind::Protocol, error.to_string())
                        })?;
                }
            }
            Ok(())
        })
        .await
    }

    pub async fn perform_chat_action(&self, action: ChatAction) -> Result<(), ServiceError> {
        if !self.commands_accepted() {
            return Err(ServiceError::new(ErrorKind::Cancelled, "shutting down"));
        }
        let session = self
            .session_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        self.run_on_core_service(async move {
            let client = session.client().await.ok_or_else(|| {
                ServiceError::new(ErrorKind::NotConnected, "no live protocol client")
            })?;
            let chat = action
                .chat()
                .as_str()
                .parse::<whatsapp_rust::Jid>()
                .map_err(|error| {
                    ServiceError::new(ErrorKind::InvalidRequest, error.to_string())
                })?;
            let actions = client.chat_actions();
            let result = match action {
                ChatAction::Pin { pinned, .. } => {
                    if pinned {
                        actions.pin_chat(&chat).await
                    } else {
                        actions.unpin_chat(&chat).await
                    }
                }
                ChatAction::Mute { muted, .. } => {
                    if muted {
                        actions.mute_chat(&chat).await
                    } else {
                        actions.unmute_chat(&chat).await
                    }
                }
                ChatAction::Archive { archived, .. } => {
                    if archived {
                        actions.archive_chat(&chat, None).await
                    } else {
                        actions.unarchive_chat(&chat, None).await
                    }
                }
                ChatAction::MarkRead { read, .. } => {
                    actions.mark_chat_as_read(&chat, read, None).await
                }
            };
            result.map_err(|error| ServiceError::new(ErrorKind::Protocol, error.to_string()))
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

    fn media_snapshot(&self) -> Result<wasabi_media::MediaManager, String> {
        self.media
            .read()
            .expect("media lock")
            .clone()
            .ok_or_else(|| "media service not ready".to_string())
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
    map_outbox_error_ref(&error)
}

fn map_outbox_error_ref(error: &wasabi_whatsapp::outbox::OutboxError) -> ServiceError {
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

fn transfer_failure_state(kind: ErrorKind) -> wasabi_domain::TransferState {
    match kind {
        ErrorKind::Cancelled => wasabi_domain::TransferState::Cancelled,
        ErrorKind::InvalidRequest | ErrorKind::Unsupported => {
            wasabi_domain::TransferState::FailedPermanent
        }
        _ => wasabi_domain::TransferState::FailedRetryable,
    }
}

fn normalized_caption(caption: Option<String>) -> Result<Option<String>, ServiceError> {
    let caption = caption
        .map(|caption| caption.trim().to_string())
        .filter(|caption| !caption.is_empty());
    if caption.as_ref().is_some_and(|caption| caption.chars().count() > 1024) {
        return Err(ServiceError::new(
            ErrorKind::InvalidRequest,
            "attachment caption exceeds 1024 characters",
        ));
    }
    Ok(caption)
}

fn attachment_message(
    upload: wasabi_media::UploadResponse,
    payload: &wasabi_domain::TransferPayload,
) -> whatsapp_rust::waproto::whatsapp::Message {
    use wasabi_domain::AttachmentKind;
    match payload.kind {
        AttachmentKind::Image => whatsapp_rust::media::image_message(
            upload,
            whatsapp_rust::media::ImageOptions {
                caption: payload.caption.clone(),
                mimetype: Some(payload.mime_type.clone()),
                ..Default::default()
            },
        ),
        AttachmentKind::Video => whatsapp_rust::media::video_message(
            upload,
            whatsapp_rust::media::VideoOptions {
                caption: payload.caption.clone(),
                mimetype: Some(payload.mime_type.clone()),
                ..Default::default()
            },
        ),
        AttachmentKind::Audio => whatsapp_rust::media::audio_message(
            upload,
            whatsapp_rust::media::AudioOptions {
                mimetype: Some(payload.mime_type.clone()),
                ..Default::default()
            },
        ),
        AttachmentKind::Document => whatsapp_rust::media::document_message(
            upload,
            whatsapp_rust::media::DocumentOptions {
                mimetype: Some(payload.mime_type.clone()),
                file_name: Some(payload.display_name.clone()),
                caption: payload.caption.clone(),
                ..Default::default()
            },
        ),
    }
}

fn map_media_error(error: wasabi_media::MediaError) -> ServiceError {
    use wasabi_media::MediaError;

    let kind = match &error {
        MediaError::Overloaded => ErrorKind::Overloaded,
        MediaError::Cancelled => ErrorKind::Cancelled,
        MediaError::InvalidInput(_) => ErrorKind::InvalidRequest,
        MediaError::Unavailable
        | MediaError::Download(_)
        | MediaError::Upload(_)
        | MediaError::Encryption(_)
        | MediaError::Decode(_) => {
            ErrorKind::MediaUnavailable
        }
        MediaError::Io(_) => ErrorKind::Database,
    };
    ServiceError::new(kind, error.to_string())
}

#[allow(dead_code)]
static TRANSFER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[allow(dead_code)]
fn next_transfer_id() -> TransferId {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = TRANSFER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    TransferId::new(format!(
        "w{time:032x}{process:08x}{sequence:016x}",
        process = std::process::id()
    ))
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

#[async_trait::async_trait]
impl DesktopBackend for CoreBridge {
    fn store_ready(&self) -> bool {
        CoreBridge::store_ready(self)
    }

    fn commands_accepted(&self) -> bool {
        CoreBridge::commands_accepted(self)
    }

    fn invalidations(&self) -> &InvalidationPublisher {
        CoreBridge::invalidations(self)
    }

    fn subscribe_state(&self) -> Option<tokio::sync::watch::Receiver<SessionState>> {
        CoreBridge::subscribe_state(self)
    }

    fn subscribe_qr(&self) -> Option<tokio::sync::watch::Receiver<Option<QrState>>> {
        CoreBridge::subscribe_qr(self)
    }

    fn subscribe_typing(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<wasabi_domain::TypingUpdate>> {
        CoreBridge::subscribe_typing(self)
    }

    async fn connect_session(&self) -> Result<(), String> {
        CoreBridge::connect_session(self).await
    }

    async fn start_pairing(&self) -> Result<(), String> {
        CoreBridge::start_pairing(self).await
    }

    async fn start_phone_pairing(
        &self,
        phone: PairingPhoneNumber,
    ) -> Result<PhonePairCode, String> {
        CoreBridge::start_phone_pairing(self, phone).await
    }

    async fn cancel_phone_pairing(&self) -> Result<(), String> {
        CoreBridge::cancel_phone_pairing(self).await
    }

    async fn stop_session(&self) -> Result<(), String> {
        CoreBridge::stop_session(self).await
    }

    async fn logout(&self) -> Result<(), ServiceError> {
        CoreBridge::logout(self).await
    }

    async fn flush_storage(&self) -> Result<(), String> {
        CoreBridge::flush_storage(self).await
    }

    async fn media_cache_usage(&self) -> Result<u64, ServiceError> {
        CoreBridge::media_cache_usage(self).await
    }

    async fn set_media_cache_quota(&self, bytes: u64) -> Result<u64, ServiceError> {
        CoreBridge::set_media_cache_quota(self, bytes).await
    }

    async fn clear_media_cache(&self) -> Result<(), ServiceError> {
        CoreBridge::clear_media_cache(self).await
    }

    async fn load_chat_page(
        &self,
        scope: ChatScope,
        after: Option<wasabi_domain::ChatPageCursor>,
        limit: usize,
    ) -> Result<ChatPage, String> {
        CoreBridge::load_chat_page(self, scope, after, limit).await
    }

    async fn load_message_page(
        &self,
        chat: &str,
        before: Option<PageCursor>,
        limit: usize,
    ) -> Result<MessagePage, String> {
        CoreBridge::load_message_page(self, chat, before, limit).await
    }

    async fn load_message_context(
        &self,
        chat: String,
        anchor: MessageId,
        before: usize,
        after: usize,
    ) -> Result<MessageContext, String> {
        CoreBridge::load_message_context(self, chat, anchor, before, after).await
    }

    async fn notification_candidate(
        &self,
        chat: String,
    ) -> Result<Option<NotificationCandidate>, String> {
        CoreBridge::notification_candidate(self, chat).await
    }

    async fn search_messages(
        &self,
        query: String,
        chat_scope: Option<String>,
        page: usize,
    ) -> Result<SearchPage, String> {
        CoreBridge::search_messages(self, query, chat_scope, page).await
    }

    async fn direct_contact_details(&self, jid: String) -> Result<DirectContactDetails, String> {
        CoreBridge::direct_contact_details(self, jid).await
    }

    async fn group_details(&self, chat: String) -> Result<GroupDetails, String> {
        CoreBridge::group_details(self, chat).await
    }

    async fn set_favorite(&self, chat: ChatId, favorite: bool) -> Result<(), String> {
        CoreBridge::set_favorite(self, chat, favorite).await
    }

    async fn save_draft(
        &self,
        chat: ChatId,
        draft: Option<wasabi_domain::Draft>,
    ) -> Result<(), String> {
        CoreBridge::save_draft(self, chat, draft).await
    }

    async fn download_media(
        &self,
        request: MediaDownloadRequest,
    ) -> Result<CachedMedia, ServiceError> {
        CoreBridge::download_media(self, request).await
    }

    async fn stage_attachment(
        &self,
        chat: ChatId,
        source: PathBuf,
    ) -> Result<StagedAttachment, ServiceError> {
        CoreBridge::stage_attachment(self, chat, source).await
    }

    async fn cancel_transfer(&self, transfer: TransferId) -> Result<(), ServiceError> {
        CoreBridge::cancel_transfer(self, transfer).await
    }

    async fn recover_staged_attachments(
        &self,
    ) -> Result<Vec<(ChatId, StagedAttachment)>, ServiceError> {
        CoreBridge::recover_staged_attachments(self).await
    }

    async fn set_typing(&self, chat: ChatId, composing: bool) -> Result<(), ServiceError> {
        CoreBridge::set_typing(self, chat, composing).await
    }

    async fn send(&self, request: SendRequest) -> Result<SendReceipt, ServiceError> {
        CoreBridge::send(self, request).await
    }

    async fn perform_message_action(&self, action: MessageAction) -> Result<(), ServiceError> {
        CoreBridge::perform_message_action(self, action).await
    }

    async fn perform_chat_action(&self, action: ChatAction) -> Result<(), ServiceError> {
        CoreBridge::perform_chat_action(self, action).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_transfer_ids_are_unique_and_path_free() {
        let first = next_transfer_id();
        let second = next_transfer_id();
        assert_ne!(first, second);
        assert!(first.as_str().starts_with('w'));
        assert!(!first.as_str().contains('/'));
        assert_eq!(format!("{first:?}"), "TransferId(<opaque>)");
    }

    #[test]
    fn captions_are_trimmed_bounded_and_audio_is_classified_permanently() {
        assert_eq!(
            normalized_caption(Some("  hello  ".to_string())).unwrap(),
            Some("hello".to_string())
        );
        assert_eq!(normalized_caption(Some("   ".to_string())).unwrap(), None);
        assert_eq!(
            normalized_caption(Some("x".repeat(1025))).unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
        assert_eq!(
            transfer_failure_state(ErrorKind::InvalidRequest),
            wasabi_domain::TransferState::FailedPermanent
        );
        assert_eq!(
            transfer_failure_state(ErrorKind::NotConnected),
            wasabi_domain::TransferState::FailedRetryable
        );
    }

}
