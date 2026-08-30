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

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use tokio_util::sync::CancellationToken;
use wasabi_core::events::{Invalidation, InvalidationPublisher};
use wasabi_core::state::SessionState;
use wasabi_domain::{
    AvatarRef, CachedAvatar, CachedMedia, ChatAction, ChatId, ChatPage, ChatScope, ContactAction,
    ContactLookupResult, ContactPage, ContactPageCursor, ContactPhoneNumber, ContactSummary,
    CreateGroupRequest, DirectContactDetails, ErrorKind, GroupChange, GroupDetails, GroupPatch,
    GroupPatchResult, GroupPermissions, MediaDownloadRequest, MessageAction, MessageContext,
    MessageId, MessagePage, NotificationCandidate, PageCursor, PairingPhoneNumber, Participant,
    ParticipantRole, PendingMembershipRequest, PhonePairCode, ProfilePictureRequest, SearchPage,
    SendContent, SendReceipt, SendRequest, ServiceError, SharedGroup, StagedAttachment, TransferId,
    TransferJob,
};
use wasabi_repository::AccountStore;
use wasabi_whatsapp::lifecycle::QrState;
use wasabi_whatsapp::outbox::Outbox;
use wasabi_whatsapp::session::{AccountSession, SessionConfig};
use whatsapp_rust::wacore::proto_helpers::MessageBuilderExt;
use whatsapp_rust::wacore_binary::JidExt as _;

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
    async fn start_phone_pairing(&self, phone: PairingPhoneNumber)
    -> Result<PhonePairCode, String>;
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
    async fn contact_page(
        &self,
        query: String,
        after: Option<ContactPageCursor>,
        limit: usize,
    ) -> Result<ContactPage, String>;
    async fn lookup_contact(
        &self,
        phone: ContactPhoneNumber,
    ) -> Result<ContactLookupResult, ServiceError>;
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
    async fn groups_in_common(&self, jid: String) -> Result<Vec<SharedGroup>, ServiceError>;
    async fn group_details(&self, chat: String) -> Result<GroupDetails, String>;
    async fn create_group(&self, request: CreateGroupRequest)
    -> Result<GroupDetails, ServiceError>;
    async fn update_group(&self, patch: GroupPatch) -> Result<GroupPatchResult, ServiceError>;
    async fn membership_requests(
        &self,
        chat: ChatId,
    ) -> Result<Vec<PendingMembershipRequest>, ServiceError>;
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
    async fn cache_thumb_bytes(
        &self,
        key: String,
        source: PathBuf,
    ) -> Result<PathBuf, ServiceError>;
    async fn profile_picture(
        &self,
        request: ProfilePictureRequest,
    ) -> Result<Option<CachedAvatar>, ServiceError>;
    fn cached_avatar_path(&self, jid: &str, picture: &AvatarRef) -> Option<PathBuf>;
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
    async fn perform_contact_action(&self, action: ContactAction) -> Result<(), ServiceError>;
}

/// Longer-edge bound for still-image timeline thumbnails. The visual card is
/// ~150px tall; 360 covers typical HiDPI without decoding the original.
const IMAGE_THUMB_MAX_DIM: u32 = 360;

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
    thumbs: wasabi_media::ThumbnailService,
    media: Arc<RwLock<Option<wasabi_media::MediaManager>>>,
    left_groups: Arc<RwLock<HashSet<String>>>,
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
            thumbs: wasabi_media::ThumbnailService::new(),
            media: Arc::new(RwLock::new(None)),
            left_groups: Arc::new(RwLock::new(HashSet::new())),
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

    pub async fn contact_page(
        &self,
        query: String,
        after: Option<ContactPageCursor>,
        limit: usize,
    ) -> Result<ContactPage, String> {
        let store = self.store_snapshot()?;
        self.run_on_core(async move {
            store
                .contact_page(query, after, limit)
                .await
                .map_err(service_message)
        })
        .await
    }

    pub async fn lookup_contact(
        &self,
        phone: ContactPhoneNumber,
    ) -> Result<ContactLookupResult, ServiceError> {
        if !self.commands_accepted() {
            return Err(ServiceError::new(ErrorKind::Cancelled, "shutting down"));
        }
        let session = self
            .session_snapshot()
            .map_err(|_| ServiceError::new(ErrorKind::NotPaired, "session unavailable"))?;
        if !session.state().is_connected() {
            return Err(ServiceError::new(
                ErrorKind::NotConnected,
                "registration lookup requires a connected session",
            ));
        }
        self.run_on_core_service(async move {
            let client = session.client().await.ok_or_else(|| {
                ServiceError::new(ErrorKind::NotConnected, "protocol client unavailable")
            })?;
            let query = whatsapp_rust::Jid::pn(phone.as_str());
            let mut results = client
                .contacts()
                .is_on_whatsapp(&[query])
                .await
                .map_err(contact_lookup_error)?;
            let Some(result) = results.pop() else {
                return Err(ServiceError::new(
                    ErrorKind::Protocol,
                    "registration lookup returned no result",
                ));
            };
            if let Some(error) = result.contact_error.as_ref() {
                let kind = if error.code == Some(429) {
                    ErrorKind::RateLimited
                } else {
                    ErrorKind::Protocol
                };
                return Err(ServiceError::new(
                    kind,
                    format!(
                        "registration subprotocol rejected lookup: code={:?}",
                        error.code
                    ),
                ));
            }
            if !result.is_registered {
                return Ok(ContactLookupResult::NotRegistered);
            }

            let display_name = result
                .verified_name
                .and_then(|verified| verified.name)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format!("+{}", phone.as_str()));
            Ok(ContactLookupResult::Registered(ContactSummary {
                // Direct-chat persistence is PN-canonical. The upstream query
                // has already saved any returned PN/LID mapping for encryption.
                jid: ChatId::new(format!("{}@s.whatsapp.net", phone.as_str())),
                display_name,
                phone_number: Some(phone.as_str().to_string()),
                avatar: None,
            }))
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
        let session = self.session_snapshot()?;
        let store = Arc::clone(&session.store);
        self.run_on_core(async move {
            let cached = store
                .direct_contact_details(&jid)
                .await
                .map_err(service_message)?;
            if !session.state().is_connected() {
                return Ok(cached);
            }
            let contact: whatsapp_rust::Jid = jid
                .parse()
                .map_err(|error| format!("Invalid contact identity: {error}"))?;
            if !contact.is_pn() && !contact.is_lid() {
                return Err("This conversation is not a direct contact".to_string());
            }
            let Some(client) = session.client().await else {
                return Ok(cached);
            };
            let is_blocked = live_is_blocked(&client, &contact).await;
            let info = match client
                .contacts()
                .get_user_info(std::slice::from_ref(&contact))
                .await
            {
                Ok(info) => info,
                Err(error) => {
                    let error = contact_lookup_error(error);
                    tracing::warn!(kind = %error.kind, "live contact metadata refresh failed; using cache");
                    return Ok(DirectContactDetails {
                        is_blocked,
                        ..cached
                    });
                }
            };
            let Some(info) = info.get(&contact) else {
                tracing::warn!("live contact metadata response omitted requested contact; using cache");
                return Ok(DirectContactDetails {
                    is_blocked,
                    ..cached
                });
            };
            let about = optional_profile_text(info.status.as_deref());
            let avatar = avatar_ref_from_picture_id(info.picture_id.as_deref());
            let details = DirectContactDetails {
                about,
                avatar,
                is_blocked,
                ..cached
            };
            if let Err(error) = store.save_direct_contact_metadata(&details).await {
                tracing::warn!(kind = %error.kind, "failed to persist refreshed contact metadata");
            }
            Ok(details)
        })
        .await
    }

    /// Cached groups that include this direct contact. Local snapshots only.
    pub async fn groups_in_common(&self, jid: String) -> Result<Vec<SharedGroup>, ServiceError> {
        let store = self
            .store_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::NotPaired, detail))?;
        self.run_on_core_service(async move { store.groups_in_common(&jid).await })
            .await
    }

    /// Fetch complete group metadata, preferring the live client and falling
    /// back to the last truthful snapshot while disconnected. No protocol
    /// value crosses this method.
    pub async fn group_details(&self, chat: String) -> Result<GroupDetails, String> {
        let session = self.session_snapshot()?;
        let store = Arc::clone(&session.store);
        let left_groups = Arc::clone(&self.left_groups);
        let left_locally = left_groups.read().expect("left group lock").contains(&chat);
        self.run_on_core(async move {
            let jid: whatsapp_rust::Jid = chat
                .parse()
                .map_err(|error| format!("Invalid group identity: {error}"))?;
            if !jid.is_group() {
                return Err("This conversation is not a group".to_string());
            }
            let cached = store.cached_group_details(&chat).await;
            if !session.state().is_connected() {
                if left_locally {
                    return Err("You left this group".to_string());
                }
                return cached
                    .map_err(service_message)?
                    .ok_or_else(|| "Connect to load group information".to_string());
            }
            let client = match session.client().await {
                Some(client) => client,
                None => {
                    if !left_locally && let Ok(Some(cached)) = &cached {
                        return Ok(cached.clone());
                    }
                    return Err("Connect to load group information".to_string());
                }
            };
            let metadata = match client.groups().get_metadata(&jid).await {
                Ok(metadata) => metadata,
                Err(error) => {
                    let error = group_service_error(error);
                    if !left_locally && let Ok(Some(cached)) = cached {
                        tracing::warn!(kind = %error.kind, "live group refresh failed; using cache");
                        return Ok(cached);
                    }
                    return Err(service_message(error));
                }
            };
            left_groups.write().expect("left group lock").remove(&chat);
            let cached_avatar = cached
                .as_ref()
                .ok()
                .and_then(|cached| cached.as_ref())
                .and_then(|cached| cached.avatar.clone());
            let details = project_group_metadata(&session, metadata, false).await;
            let details = attach_group_avatar(client.as_ref(), &jid, details, cached_avatar).await;
            if let Err(error) = store
                .save_group_details(
                    details.clone(),
                    chrono::Utc::now().timestamp_millis(),
                )
                .await
            {
                tracing::warn!(kind = %error.kind, "group details cache write failed");
            }
            Ok(details)
        })
        .await
    }

    pub async fn create_group(
        &self,
        request: CreateGroupRequest,
    ) -> Result<GroupDetails, ServiceError> {
        if !self.commands_accepted() {
            return Err(ServiceError::new(ErrorKind::Cancelled, "shutting down"));
        }
        let session = self
            .session_snapshot()
            .map_err(|_| ServiceError::new(ErrorKind::NotPaired, "session unavailable"))?;
        if !session.state().is_connected() {
            return Err(ServiceError::new(
                ErrorKind::NotConnected,
                "group creation requires a connected session",
            ));
        }
        let store = Arc::clone(&session.store);
        let result = self
            .run_on_core_service(async move {
                let client = session.client().await.ok_or_else(|| {
                    ServiceError::new(ErrorKind::NotConnected, "protocol client unavailable")
                })?;
                let participants =
                    request
                        .participants()
                        .iter()
                        .map(|participant| {
                            let jid = participant.as_str().parse::<whatsapp_rust::Jid>().map_err(
                                |_| {
                                    ServiceError::new(
                                        ErrorKind::InvalidRequest,
                                        "invalid group participant identity",
                                    )
                                },
                            )?;
                            if !jid.is_pn() && !jid.is_lid() {
                                return Err(ServiceError::new(
                                    ErrorKind::InvalidRequest,
                                    "group participants must be direct contacts",
                                ));
                            }
                            Ok(whatsapp_rust::GroupParticipantOptions::new(jid))
                        })
                        .collect::<Result<Vec<_>, ServiceError>>()?;
                let options = whatsapp_rust::GroupCreateOptions::new(request.subject())
                    .with_participants(participants);
                let created = client
                    .groups()
                    .create_group(options)
                    .await
                    .map_err(group_service_error)?;
                let created_at_ms = created
                    .metadata
                    .creation_time
                    .and_then(|seconds| i64::try_from(seconds).ok())
                    .and_then(|seconds| seconds.checked_mul(1_000))
                    .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
                let details = project_group_metadata(&session, created.metadata, true).await;
                if let Err(error) = store
                    .record_created_group(
                        details.chat.clone(),
                        details.subject.clone(),
                        created_at_ms,
                    )
                    .await
                {
                    // The remote group already exists. Never turn a local
                    // cache failure into a retryable create operation.
                    tracing::warn!(kind = %error.kind, "created group cache write failed");
                }
                if let Err(error) = store
                    .save_group_details(details.clone(), chrono::Utc::now().timestamp_millis())
                    .await
                {
                    tracing::warn!(kind = %error.kind, "created group details cache write failed");
                }
                Ok(details)
            })
            .await?;
        self.invalidations.publish(Invalidation::Chats);
        Ok(result)
    }

    pub async fn update_group(&self, patch: GroupPatch) -> Result<GroupPatchResult, ServiceError> {
        if !self.commands_accepted() {
            return Err(ServiceError::new(ErrorKind::Cancelled, "shutting down"));
        }
        let session = self
            .session_snapshot()
            .map_err(|_| ServiceError::new(ErrorKind::NotPaired, "session unavailable"))?;
        if !session.state().is_connected() {
            return Err(ServiceError::new(
                ErrorKind::NotConnected,
                "group mutation requires a connected session",
            ));
        }
        let store = Arc::clone(&session.store);
        let left_groups = Arc::clone(&self.left_groups);
        let result = self
            .run_on_core_service(async move {
                let chat = patch
                    .chat()
                    .as_str()
                    .parse::<whatsapp_rust::Jid>()
                    .map_err(|_| {
                        ServiceError::new(ErrorKind::InvalidRequest, "invalid group identity")
                    })?;
                if !chat.is_group() {
                    return Err(ServiceError::new(
                        ErrorKind::InvalidRequest,
                        "conversation is not a group",
                    ));
                }
                let client = session.client().await.ok_or_else(|| {
                    ServiceError::new(ErrorKind::NotConnected, "protocol client unavailable")
                })?;
                let groups = client.groups();
                let (applied_participants, rejected_participants, left) = match patch.change() {
                    GroupChange::Subject(subject) => {
                        let subject = whatsapp_rust::GroupSubject::new(subject).map_err(|_| {
                            ServiceError::new(ErrorKind::InvalidRequest, "invalid group subject")
                        })?;
                        groups
                            .set_subject(chat.clone(), subject)
                            .await
                            .map_err(group_service_error)?;
                        (0, 0, false)
                    }
                    GroupChange::Description(description) => {
                        let description = description
                            .as_deref()
                            .map(whatsapp_rust::GroupDescription::new)
                            .transpose()
                            .map_err(|_| {
                                ServiceError::new(
                                    ErrorKind::InvalidRequest,
                                    "invalid group description",
                                )
                            })?;
                        groups
                            .set_description(
                                chat.clone(),
                                description,
                                whatsapp_rust::PreviousDescription::Resolve,
                            )
                            .await
                            .map_err(group_service_error)?;
                        (0, 0, false)
                    }
                    GroupChange::OnlyAdminsEdit(enabled) => {
                        groups
                            .set_locked(chat.clone(), *enabled)
                            .await
                            .map_err(group_service_error)?;
                        (0, 0, false)
                    }
                    GroupChange::OnlyAdminsSend(enabled) => {
                        groups
                            .set_announce(chat.clone(), *enabled)
                            .await
                            .map_err(group_service_error)?;
                        (0, 0, false)
                    }
                    GroupChange::MembershipApproval(enabled) => {
                        let mode = if *enabled {
                            whatsapp_rust::MembershipApprovalMode::On
                        } else {
                            whatsapp_rust::MembershipApprovalMode::Off
                        };
                        groups
                            .set_membership_approval(chat.clone(), mode)
                            .await
                            .map_err(group_service_error)?;
                        (0, 0, false)
                    }
                    GroupChange::AddParticipants(participants) => {
                        let participants = participant_jids(participants)?;
                        let responses = groups
                            .add_participants(chat.clone(), &participants)
                            .await
                            .map_err(group_service_error)?;
                        participant_result_counts(&responses)
                    }
                    GroupChange::RemoveParticipant(participant) => {
                        let participants = participant_jids(std::slice::from_ref(participant))?;
                        let responses = groups
                            .remove_participants(chat.clone(), &participants)
                            .await
                            .map_err(group_service_error)?;
                        participant_result_counts(&responses)
                    }
                    GroupChange::PromoteParticipant(participant) => {
                        let participants = participant_jids(std::slice::from_ref(participant))?;
                        let responses = groups
                            .promote_participants(chat.clone(), &participants)
                            .await
                            .map_err(group_service_error)?;
                        participant_result_counts(&responses)
                    }
                    GroupChange::DemoteParticipant(participant) => {
                        let participants = participant_jids(std::slice::from_ref(participant))?;
                        let responses = groups
                            .demote_participants(chat.clone(), &participants)
                            .await
                            .map_err(group_service_error)?;
                        participant_result_counts(&responses)
                    }
                    GroupChange::ApproveMembershipRequest(participant) => {
                        let participants = participant_jids(std::slice::from_ref(participant))?;
                        let responses = groups
                            .approve_membership_requests(chat.clone(), &participants)
                            .await
                            .map_err(group_service_error)?;
                        participant_result_counts(&responses)
                    }
                    GroupChange::RejectMembershipRequest(participant) => {
                        let participants = participant_jids(std::slice::from_ref(participant))?;
                        let responses = groups
                            .reject_membership_requests(chat.clone(), &participants)
                            .await
                            .map_err(group_service_error)?;
                        participant_result_counts(&responses)
                    }
                    GroupChange::Leave => {
                        if let Err(error) = groups.leave(chat.clone()).await {
                            let error = group_service_error(error);
                            if leave_outcome_uncertain(error.kind) {
                                left_groups
                                    .write()
                                    .expect("left group lock")
                                    .insert(patch.chat().as_str().to_string());
                            }
                            return Err(error);
                        }
                        (0, 0, true)
                    }
                };
                if left {
                    let group = patch.chat().as_str().to_string();
                    left_groups
                        .write()
                        .expect("left group lock")
                        .insert(group.clone());
                    if let Err(error) = store.remove_cached_group_details(&group).await {
                        // The remote mutation is already accepted; do not
                        // present a retryable leave action that could run
                        // twice. The UI closes the drawer and the next live
                        // group query remains authoritative.
                        tracing::error!(kind = %error.kind, "left group cache cleanup failed");
                    }
                    return Ok(GroupPatchResult {
                        details: None,
                        applied_participants,
                        rejected_participants,
                    });
                }
                let metadata = groups
                    .get_metadata(&chat)
                    .await
                    .map_err(group_service_error)?;
                let cached_avatar = store
                    .cached_group_details(patch.chat().as_str())
                    .await
                    .ok()
                    .flatten()
                    .and_then(|cached| cached.avatar);
                let details = project_group_metadata(&session, metadata, false).await;
                let details =
                    attach_group_avatar(client.as_ref(), &chat, details, cached_avatar).await;
                if let Err(error) = store
                    .save_group_details(details.clone(), chrono::Utc::now().timestamp_millis())
                    .await
                {
                    tracing::warn!(kind = %error.kind, "updated group details cache write failed");
                }
                Ok(GroupPatchResult {
                    details: Some(details),
                    applied_participants,
                    rejected_participants,
                })
            })
            .await?;
        self.invalidations.publish(Invalidation::Chats);
        Ok(result)
    }

    pub async fn membership_requests(
        &self,
        chat: ChatId,
    ) -> Result<Vec<PendingMembershipRequest>, ServiceError> {
        if !self.commands_accepted() {
            return Err(ServiceError::new(ErrorKind::Cancelled, "shutting down"));
        }
        let session = self
            .session_snapshot()
            .map_err(|_| ServiceError::new(ErrorKind::NotPaired, "session unavailable"))?;
        if !session.state().is_connected() {
            return Err(ServiceError::new(
                ErrorKind::NotConnected,
                "membership requests require a connected session",
            ));
        }
        self.run_on_core_service(async move {
            let jid = chat.as_str().parse::<whatsapp_rust::Jid>().map_err(|_| {
                ServiceError::new(ErrorKind::InvalidRequest, "invalid group identity")
            })?;
            if !jid.is_group() {
                return Err(ServiceError::new(
                    ErrorKind::InvalidRequest,
                    "conversation is not a group",
                ));
            }
            let client = session.client().await.ok_or_else(|| {
                ServiceError::new(ErrorKind::NotConnected, "protocol client unavailable")
            })?;
            let requests = client
                .groups()
                .get_membership_requests(jid)
                .await
                .map_err(group_service_error)?;
            let mut pending = Vec::with_capacity(requests.len());
            for request in requests {
                let contact = session.chats.contact(&request.jid).await.ok().flatten();
                let display_name = contact
                    .as_ref()
                    .and_then(|contact| contact.display_name())
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| request.jid.user.to_string());
                pending.push(PendingMembershipRequest {
                    jid: ChatId::new(request.jid.to_string()),
                    display_name,
                });
            }
            Ok(pending)
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
        self.run_on_core(
            async move { store.save_draft(chat, draft).await.map_err(service_message) },
        )
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
                .map_err(|error| ServiceError::new(ErrorKind::InvalidRequest, error.to_string()))?;
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
            let path = match static_url_media(&message) {
                Some(StaticUrlMedia::Image(downloadable)) => {
                    let expected_sha = expected_media_sha(downloadable.file_sha256.as_deref());
                    manager
                        .download(downloadable, expected_sha, None, cancel)
                        .await
                }
                Some(StaticUrlMedia::Video(downloadable))
                | Some(StaticUrlMedia::Ptv(downloadable)) => {
                    let expected_sha = expected_media_sha(downloadable.file_sha256.as_deref());
                    manager
                        .download(downloadable, expected_sha, None, cancel)
                        .await
                }
                None => {
                    let downloadable =
                        wasabi_media::media_downloadable(&message).ok_or_else(|| {
                            ServiceError::new(
                                ErrorKind::Unsupported,
                                "this media cannot be downloaded",
                            )
                        })?;
                    let expected_sha = expected_media_sha(Some(&downloadable.file_sha256));
                    manager
                        .download(downloadable, expected_sha, None, cancel)
                        .await
                }
            }
            .map_err(map_media_error)?;
            Ok(CachedMedia {
                media: request.media,
                path,
            })
        })
        .await
    }

    pub fn cached_avatar_path(&self, jid: &str, picture: &AvatarRef) -> Option<PathBuf> {
        self.media_cache
            .open_path(&wasabi_media::avatar_cache_key(jid, &picture.0))
    }

    pub async fn cache_thumb_bytes(
        &self,
        key: String,
        source: PathBuf,
    ) -> Result<PathBuf, ServiceError> {
        let thumbs = self.thumbs.clone();
        let cache = self.media_cache.clone();
        self.run_on_core_service(async move {
            if let Some(path) = cache.open_path(&key) {
                return Ok(path);
            }
            let bytes = thumbs
                .thumb(&source, IMAGE_THUMB_MAX_DIM)
                .await
                .map_err(map_media_error)?;
            cache
                .store_bytes(&key, bytes.as_ref())
                .await
                .map_err(|error| ServiceError::new(ErrorKind::Internal, error.to_string()))
        })
        .await
    }

    pub async fn profile_picture(
        &self,
        request: ProfilePictureRequest,
    ) -> Result<Option<CachedAvatar>, ServiceError> {
        if !self.commands_accepted() {
            return Err(ServiceError::new(ErrorKind::Cancelled, "shutting down"));
        }
        let session = self
            .session_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        let cache = self.media_cache.clone();
        self.run_on_core_service(async move {
            let jid: whatsapp_rust::Jid = request.jid.as_str().parse().map_err(|_| {
                ServiceError::new(
                    ErrorKind::InvalidRequest,
                    "invalid profile picture identity",
                )
            })?;
            if !jid.is_pn() && !jid.is_lid() && !jid.is_group() {
                return Err(ServiceError::new(
                    ErrorKind::InvalidRequest,
                    "profile pictures are only available for contacts and groups",
                ));
            }
            let store = Arc::clone(&session.store);
            let known = cached_avatar_ref(&store, &jid).await;
            if !request.refresh
                && let Some(picture) = known.as_ref()
                && let Some(path) = cache.open_path(&wasabi_media::avatar_cache_key(
                    request.jid.as_str(),
                    &picture.0,
                ))
            {
                return Ok(Some(CachedAvatar {
                    jid: request.jid,
                    path,
                }));
            }
            if !session.state().is_connected() {
                return cached_avatar_from_disk(&cache, request.jid, known.as_ref());
            }
            let Some(client) = session.client().await else {
                return cached_avatar_from_disk(&cache, request.jid, known.as_ref());
            };
            let picture = match client.contacts().get_profile_picture(&jid, true).await {
                Ok(picture) => picture,
                Err(error) => {
                    tracing::warn!(
                        kind = %contact_error_kind(&error),
                        "live profile picture request failed"
                    );
                    return cached_avatar_from_disk(&cache, request.jid, known.as_ref());
                }
            };
            let Some(picture) = picture else {
                if let Some(previous) = known.as_ref() {
                    let _ = cache
                        .remove(&wasabi_media::avatar_cache_key(
                            request.jid.as_str(),
                            &previous.0,
                        ))
                        .await;
                }
                persist_avatar_ref(&store, &jid, None).await;
                return Ok(None);
            };
            let Some(avatar) = avatar_ref_from_picture_id(Some(picture.id.as_str())) else {
                if let Some(previous) = known.as_ref() {
                    let _ = cache
                        .remove(&wasabi_media::avatar_cache_key(
                            request.jid.as_str(),
                            &previous.0,
                        ))
                        .await;
                }
                persist_avatar_ref(&store, &jid, None).await;
                return Ok(None);
            };
            let cache_key = wasabi_media::avatar_cache_key(request.jid.as_str(), &avatar.0);
            if let Some(path) = cache.open_path(&cache_key) {
                persist_avatar_ref(&store, &jid, Some(avatar)).await;
                return Ok(Some(CachedAvatar {
                    jid: request.jid,
                    path,
                }));
            }
            let url = picture.url;
            if !profile_picture_url_is_safe(&url) {
                tracing::warn!("profile picture URL rejected");
                return cached_avatar_from_disk(&cache, request.jid, known.as_ref());
            }
            let bytes = tokio::task::spawn_blocking(move || download_profile_picture_bytes(url))
                .await
                .map_err(|error| ServiceError::new(ErrorKind::Internal, error.to_string()))??;
            let path = cache
                .store_bytes(&cache_key, &bytes)
                .await
                .map_err(|error| ServiceError::new(ErrorKind::Internal, error.to_string()))?;
            persist_avatar_ref(&store, &jid, Some(avatar)).await;
            Ok(Some(CachedAvatar {
                jid: request.jid,
                path,
            }))
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
            let job = store.transfer_job(transfer.clone()).await?.ok_or_else(|| {
                ServiceError::new(ErrorKind::InvalidRequest, "attachment transfer not found")
            })?;
            if !job.state.is_terminal()
                && !store
                    .set_transfer_state(transfer, wasabi_domain::TransferState::Cancelled, None)
                    .await?
            {
                return Err(ServiceError::new(
                    ErrorKind::InvalidRequest,
                    "attachment transfer could not be cancelled",
                ));
            }
            if let Some(source) = job.source_path {
                manager
                    .discard_staged_upload(source)
                    .await
                    .map_err(map_media_error)?;
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

    pub async fn set_typing(&self, chat: ChatId, composing: bool) -> Result<(), ServiceError> {
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
                .map_err(|error| ServiceError::new(ErrorKind::InvalidRequest, error.to_string()))?;
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
                SendContent::Text { body, reply_to } => {
                    let client = session.client().await.ok_or_else(|| {
                        ServiceError::new(ErrorKind::NotConnected, "no live protocol client")
                    })?;
                    let context = load_reply_context(&session.chats, &to, reply_to).await?;
                    let mut message = whatsapp_rust::waproto::whatsapp::Message::text(body);
                    attach_reply_context(&mut message, context)?;
                    match outbox.send_message(&client, to, message).await {
                        Ok(receipt) => Ok(receipt),
                        Err(wasabi_whatsapp::outbox::OutboxError::Send {
                            message_id,
                            source: _,
                        }) => {
                            // The commit barrier passed and the durable failed
                            // row owns Retry. Returning an error here would
                            // restore the composer and invite a duplicate send.
                            Ok(wasabi_whatsapp::outbox::SentReceipt { message_id })
                        }
                        Err(error) => Err(error),
                    }
                }
                SendContent::Attachment {
                    transfer,
                    caption,
                    reply_to,
                } => {
                    let mut job = store.transfer_job(transfer.clone()).await?.ok_or_else(|| {
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
                    let reply_context = load_reply_context(&session.chats, &to, reply_to).await?;
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
                            let persisted_kind = (state != wasabi_domain::TransferState::Cancelled)
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
                    let mut message = attachment_message(upload, &payload);
                    attach_reply_context(&mut message, reply_context)?;
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
                            let persisted_kind = (state != wasabi_domain::TransferState::Cancelled)
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

    pub async fn perform_message_action(&self, action: MessageAction) -> Result<(), ServiceError> {
        if !self.commands_accepted() {
            return Err(ServiceError::new(ErrorKind::Cancelled, "shutting down"));
        }
        let session = self
            .session_snapshot()
            .map_err(|detail| ServiceError::new(ErrorKind::Internal, detail))?;
        let outbox = self
            .outbox_snapshot()
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
                .map_err(|error| ServiceError::new(ErrorKind::InvalidRequest, error.to_string()))?;
            let participant = (!target.from_me && target.chat.as_str().ends_with("@g.us"))
                .then(|| target.sender.parse::<whatsapp_rust::Jid>())
                .transpose()
                .map_err(|error| ServiceError::new(ErrorKind::InvalidRequest, error.to_string()))?;

            match action {
                MessageAction::Retry { target } => {
                    outbox
                        .retry_failed(&client, chat, target.message.as_str())
                        .await
                        .map_err(map_outbox_error)?;
                }
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
                    if emoji.chars().count() > 16 || emoji.chars().any(char::is_whitespace) {
                        return Err(ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "reaction must be one compact emoji sequence",
                        ));
                    }
                    let key = whatsapp_rust::message_key(
                        target.message.as_str(),
                        &chat,
                        target.from_me,
                        participant.as_ref(),
                    );
                    client
                        .send_reaction(chat.clone(), key.clone(), &emoji)
                        .await
                        .map_err(map_protocol_send_error)?;
                    session
                        .chats
                        .record_reaction(&chat, &key, &emoji, whatsapp_rust::chrono::Utc::now())
                        .map_err(|error| {
                            ServiceError::new(ErrorKind::Database, error.to_string())
                        })?;
                    session.chats.flush().await.map_err(|error| {
                        ServiceError::new(ErrorKind::Database, error.to_string())
                    })?;
                }
                MessageAction::Edit { target, body } => {
                    if !target.from_me {
                        return Err(ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "only your own messages can be edited",
                        ));
                    }
                    let body = body.trim().to_string();
                    if body.is_empty() || body.chars().count() > 65_536 {
                        return Err(ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "edited text must contain 1 to 65536 characters",
                        ));
                    }
                    let stored = session
                        .chats
                        .message(&chat, target.message.as_str())
                        .await
                        .map_err(|error| ServiceError::new(ErrorKind::Database, error.to_string()))?
                        .ok_or_else(|| {
                            ServiceError::new(ErrorKind::InvalidRequest, "message no longer exists")
                        })?;
                    if !stored.from_me
                        || stored.revoked
                        || !matches!(stored.kind, whatsapp_rust_chat_store::MessageKind::Text)
                    {
                        return Err(ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "message is not editable text",
                        ));
                    }
                    let now = whatsapp_rust::chrono::Utc::now();
                    if now.timestamp_millis() < stored.timestamp.timestamp_millis()
                        || now
                            .timestamp_millis()
                            .saturating_sub(stored.timestamp.timestamp_millis())
                            > wasabi_domain::MESSAGE_EDIT_WINDOW_MS
                    {
                        return Err(ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "message edit window has expired",
                        ));
                    }
                    let new_content = whatsapp_rust::waproto::whatsapp::Message::text(body);
                    client
                        .edit_message(chat.clone(), target.message.as_str(), new_content.clone())
                        .await
                        .map_err(map_protocol_send_error)?;
                    // Materialize only after the protocol accepts the edit.
                    // If this local write fails, the server echo/history sync
                    // can still converge the cache without showing an edit
                    // that never reached other devices.
                    session
                        .chats
                        .record_edit(&chat, target.message.as_str(), &new_content, now)
                        .map_err(|error| {
                            ServiceError::new(ErrorKind::Database, error.to_string())
                        })?;
                    session.chats.flush().await.map_err(|error| {
                        ServiceError::new(ErrorKind::Database, error.to_string())
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
                .map_err(|error| ServiceError::new(ErrorKind::InvalidRequest, error.to_string()))?;
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
                ChatAction::Clear {
                    delete_starred,
                    delete_media,
                    ..
                } => {
                    actions
                        .clear_chat(&chat, delete_starred, delete_media, None)
                        .await
                }
                ChatAction::Delete { delete_media, .. } => {
                    actions.delete_chat(&chat, delete_media, None).await
                }
            };
            result.map_err(|error| ServiceError::new(ErrorKind::Protocol, error.to_string()))
        })
        .await
    }

    pub async fn perform_contact_action(&self, action: ContactAction) -> Result<(), ServiceError> {
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
            let jid = action
                .jid()
                .as_str()
                .parse::<whatsapp_rust::Jid>()
                .map_err(|_| {
                    ServiceError::new(ErrorKind::InvalidRequest, "invalid contact identity")
                })?;
            match action {
                ContactAction::Block { .. } => client
                    .blocking()
                    .block(&jid)
                    .await
                    .map_err(map_blocking_error),
                ContactAction::Unblock { .. } => client
                    .blocking()
                    .unblock(&jid)
                    .await
                    .map_err(map_blocking_error),
                ContactAction::Remove { .. } => {
                    if !(jid.is_pn() && jid.device == 0) {
                        return Err(ServiceError::new(
                            ErrorKind::InvalidRequest,
                            "contact removal requires a bare phone-number identity",
                        ));
                    }
                    client
                        .chat_actions()
                        .remove_contact(&jid)
                        .await
                        .map_err(map_app_state_contact_error)?;
                    session.store.delete_local_contact(&jid.to_string()).await
                }
            }
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

fn map_protocol_send_error(error: whatsapp_rust::SendError) -> ServiceError {
    use whatsapp_rust::SendError;
    use whatsapp_rust::client::ClientError;

    let kind = match &error {
        SendError::NotLoggedIn => ErrorKind::NotPaired,
        SendError::Client(ClientError::NotConnected | ClientError::Socket(_)) => {
            ErrorKind::NotConnected
        }
        SendError::InvalidRequest(_) => ErrorKind::InvalidRequest,
        _ => ErrorKind::Protocol,
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
    if caption
        .as_ref()
        .is_some_and(|caption| caption.chars().count() > 1024)
    {
        return Err(ServiceError::new(
            ErrorKind::InvalidRequest,
            "attachment caption exceeds 1024 characters",
        ));
    }
    Ok(caption)
}

async fn load_reply_context(
    chats: &whatsapp_rust_chat_store::ChatStore,
    target_chat: &whatsapp_rust::Jid,
    reply_to: Option<MessageId>,
) -> Result<Option<whatsapp_rust::waproto::whatsapp::ContextInfo>, ServiceError> {
    use whatsapp_rust::wacore::proto_helpers::build_quote_context_with_info;

    let Some(reply_to) = reply_to else {
        return Ok(None);
    };
    let quoted = chats
        .message(target_chat, reply_to.as_str())
        .await
        .map_err(|error| ServiceError::new(ErrorKind::Database, error.to_string()))?
        .ok_or_else(|| {
            ServiceError::new(ErrorKind::InvalidRequest, "reply target no longer exists")
        })?;
    let quoted_message = quoted.message.as_deref().ok_or_else(|| {
        ServiceError::new(
            ErrorKind::InvalidRequest,
            "reply target content is unavailable",
        )
    })?;
    Ok(Some(build_quote_context_with_info(
        quoted.id,
        &quoted.sender_jid,
        &quoted.chat_jid,
        target_chat,
        quoted_message,
    )))
}

fn attach_reply_context(
    message: &mut whatsapp_rust::waproto::whatsapp::Message,
    context: Option<whatsapp_rust::waproto::whatsapp::ContextInfo>,
) -> Result<(), ServiceError> {
    use whatsapp_rust::wacore::proto_helpers::MessageExt;

    if context.is_some_and(|context| !message.set_context_info(context)) {
        return Err(ServiceError::new(
            ErrorKind::Unsupported,
            "this message type cannot carry reply context",
        ));
    }
    Ok(())
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
        | MediaError::Decode(_) => ErrorKind::MediaUnavailable,
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

fn participant_jids(participants: &[ChatId]) -> Result<Vec<whatsapp_rust::Jid>, ServiceError> {
    participants
        .iter()
        .map(|participant| {
            let jid = participant
                .as_str()
                .parse::<whatsapp_rust::Jid>()
                .map_err(|_| {
                    ServiceError::new(
                        ErrorKind::InvalidRequest,
                        "invalid group participant identity",
                    )
                })?;
            if !jid.is_pn() && !jid.is_lid() {
                return Err(ServiceError::new(
                    ErrorKind::InvalidRequest,
                    "group participants must be direct contacts",
                ));
            }
            Ok(jid)
        })
        .collect()
}

fn participant_result_counts(
    responses: &[whatsapp_rust::ParticipantChangeResponse],
) -> (usize, usize, bool) {
    let applied = responses.iter().filter(|response| response.is_ok()).count();
    (applied, responses.len().saturating_sub(applied), false)
}

/// Coarse, user-renderable message; diagnostics stay in logs.
fn service_message(e: ServiceError) -> String {
    tracing::warn!(kind = %e.kind, detail = %e.detail, "core query failed");
    e.ui_message().to_string()
}

fn optional_profile_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn avatar_ref_from_picture_id(picture_id: Option<&str>) -> Option<AvatarRef> {
    optional_profile_text(picture_id).map(AvatarRef)
}

fn profile_picture_url_is_safe(url: &str) -> bool {
    let url = url.trim();
    url.starts_with("https://") && !url.chars().any(char::is_whitespace)
}

fn cached_avatar_from_disk(
    cache: &wasabi_media::DiskCache,
    jid: ChatId,
    picture: Option<&AvatarRef>,
) -> Result<Option<CachedAvatar>, ServiceError> {
    let Some(picture) = picture else {
        return Ok(None);
    };
    Ok(cache
        .open_path(&wasabi_media::avatar_cache_key(jid.as_str(), &picture.0))
        .map(|path| CachedAvatar { jid, path }))
}

async fn cached_avatar_ref(
    store: &wasabi_repository::AccountStore,
    jid: &whatsapp_rust::Jid,
) -> Option<AvatarRef> {
    let identity = jid.to_string();
    if jid.is_group() {
        store
            .cached_group_details(&identity)
            .await
            .ok()
            .flatten()
            .and_then(|details| details.avatar)
    } else {
        store
            .direct_contact_details(&identity)
            .await
            .ok()
            .and_then(|details| details.avatar)
    }
}

async fn persist_avatar_ref(
    store: &wasabi_repository::AccountStore,
    jid: &whatsapp_rust::Jid,
    avatar: Option<AvatarRef>,
) {
    let identity = jid.to_string();
    if jid.is_group() {
        let Ok(Some(mut details)) = store.cached_group_details(&identity).await else {
            return;
        };
        details.avatar = avatar;
        if let Err(error) = store
            .save_group_details(details, chrono::Utc::now().timestamp_millis())
            .await
        {
            tracing::warn!(kind = %error.kind, "failed to persist group avatar metadata");
        }
        return;
    }
    let Ok(mut details) = store.direct_contact_details(&identity).await else {
        return;
    };
    details.avatar = avatar;
    if let Err(error) = store.save_direct_contact_metadata(&details).await {
        tracing::warn!(kind = %error.kind, "failed to persist contact avatar metadata");
    }
}

async fn attach_group_avatar(
    client: &whatsapp_rust::client::Client,
    jid: &whatsapp_rust::Jid,
    mut details: GroupDetails,
    cached_avatar: Option<AvatarRef>,
) -> GroupDetails {
    details.avatar = cached_avatar;
    match client.contacts().get_profile_picture(jid, true).await {
        Ok(Some(picture)) => {
            details.avatar = avatar_ref_from_picture_id(Some(picture.id.as_str()));
        }
        Ok(None) => details.avatar = None,
        Err(error) => {
            tracing::warn!(
                kind = %contact_error_kind(&error),
                "live group avatar refresh failed; using cache"
            );
        }
    }
    details
}

fn download_profile_picture_bytes(url: String) -> Result<Vec<u8>, ServiceError> {
    use std::io::Read;

    let unavailable = || {
        ServiceError::new(
            ErrorKind::MediaUnavailable,
            "profile picture download failed",
        )
    };
    if !profile_picture_url_is_safe(&url) {
        return Err(unavailable());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .build()
        .into();
    let mut response = agent.get(&url).call().map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(unavailable());
    }
    let limit = wasabi_media::MAX_PROFILE_PICTURE_BYTES;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.is_empty() || bytes.len() as u64 > limit {
        return Err(unavailable());
    }
    Ok(bytes)
}

fn contact_error_kind(error: &whatsapp_rust::ContactError) -> ErrorKind {
    use whatsapp_rust::ContactError;
    use whatsapp_rust::request::IqError;

    match error {
        ContactError::InvalidJid(_) => ErrorKind::InvalidRequest,
        ContactError::Iq(IqError::Timeout) => ErrorKind::Timeout,
        ContactError::Iq(IqError::NotConnected)
        | ContactError::Iq(IqError::Socket(_))
        | ContactError::Iq(IqError::EncryptSend(_))
        | ContactError::Iq(IqError::ClientState(_))
        | ContactError::Iq(IqError::Disconnected(_))
        | ContactError::Iq(IqError::InternalChannelClosed) => ErrorKind::NotConnected,
        ContactError::Iq(IqError::ServerError { code: 429, .. }) => ErrorKind::RateLimited,
        ContactError::Iq(_) => ErrorKind::Protocol,
        _ => ErrorKind::Protocol,
    }
}

fn contact_lookup_error(error: whatsapp_rust::ContactError) -> ServiceError {
    // Do not retain the upstream string: future variants may include the
    // queried JID. Phone numbers must stay out of normal diagnostics.
    ServiceError::new(contact_error_kind(&error), "registration lookup failed")
}

async fn live_is_blocked(
    client: &whatsapp_rust::client::Client,
    jid: &whatsapp_rust::Jid,
) -> Option<bool> {
    match client.blocking().is_blocked(jid).await {
        Ok(blocked) => Some(blocked),
        Err(_) => {
            tracing::warn!("live block state unavailable");
            None
        }
    }
}

fn map_blocking_error(error: whatsapp_rust::BlockingError) -> ServiceError {
    use whatsapp_rust::BlockingError;
    use whatsapp_rust::request::IqError;

    let kind = match &error {
        BlockingError::InvalidJid(_) => ErrorKind::InvalidRequest,
        BlockingError::Iq(IqError::NotConnected)
        | BlockingError::Iq(IqError::ClientState(_))
        | BlockingError::Iq(IqError::Socket(_))
        | BlockingError::Iq(IqError::EncryptSend(_))
        | BlockingError::Iq(IqError::Disconnected(_))
        | BlockingError::Iq(IqError::InternalChannelClosed) => ErrorKind::NotConnected,
        BlockingError::Iq(_) | BlockingError::Internal(_) => ErrorKind::Protocol,
        _ => ErrorKind::Protocol,
    };
    let detail = match kind {
        ErrorKind::InvalidRequest => "invalid blocklist target",
        ErrorKind::NotConnected => "not connected",
        _ => "blocklist operation failed",
    };
    ServiceError::new(kind, detail)
}

fn map_app_state_contact_error(error: whatsapp_rust::AppStateError) -> ServiceError {
    let kind = match error {
        whatsapp_rust::AppStateError::InvalidRequest(_) => ErrorKind::InvalidRequest,
        whatsapp_rust::AppStateError::NotConnected => ErrorKind::NotConnected,
        whatsapp_rust::AppStateError::Internal(_) => ErrorKind::Protocol,
        _ => ErrorKind::Protocol,
    };
    ServiceError::new(kind, "contact action failed")
}

fn group_service_error(error: whatsapp_rust::GroupError) -> ServiceError {
    use whatsapp_rust::GroupError;
    use whatsapp_rust::request::IqError;

    let kind = match &error {
        GroupError::InvalidRequest(_) => ErrorKind::InvalidRequest,
        GroupError::Iq(IqError::NotConnected) | GroupError::Iq(IqError::ClientState(_)) => {
            ErrorKind::NotConnected
        }
        // These may happen after bytes left the process. Surface them as an
        // ambiguous timeout so the UI blocks blind retry and asks the user to
        // check Chats after reconnecting.
        GroupError::Iq(IqError::Timeout)
        | GroupError::Iq(IqError::Socket(_))
        | GroupError::Iq(IqError::EncryptSend(_))
        | GroupError::Iq(IqError::Disconnected(_))
        | GroupError::Iq(IqError::InternalChannelClosed) => ErrorKind::Timeout,
        GroupError::Iq(IqError::ServerError { code: 429, .. }) => ErrorKind::RateLimited,
        GroupError::Internal(_) => ErrorKind::Internal,
        GroupError::Iq(_) | GroupError::Mex(_) | GroupError::DescriptionConflict => {
            ErrorKind::Protocol
        }
        _ => ErrorKind::Protocol,
    };
    ServiceError::new(kind, "group operation failed")
}

pub(crate) fn leave_outcome_uncertain(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::Timeout | ErrorKind::NotConnected)
}

async fn project_group_metadata(
    session: &AccountSession,
    metadata: whatsapp_rust::GroupMetadata,
    created_by_self: bool,
) -> GroupDetails {
    let mut own_identities = session
        .store
        .sqlite()
        .load_device_data_for_device(session.store.device_id())
        .await
        .unwrap_or_default()
        .into_iter()
        .flat_map(|device| [device.pn, device.lid])
        .flatten()
        .map(|jid| jid.to_non_ad().to_string())
        .collect::<std::collections::HashSet<_>>();
    if created_by_self {
        own_identities.extend(
            metadata
                .creator
                .iter()
                .chain(metadata.creator_pn.iter())
                .map(|jid| jid.to_non_ad().to_string()),
        );
    }
    let mut participants = Vec::with_capacity(metadata.participants.len());
    for participant in metadata.participants {
        let identity = participant
            .phone_number
            .as_ref()
            .unwrap_or(&participant.jid)
            .clone();
        let is_self = [
            Some(&participant.jid),
            participant.phone_number.as_ref(),
            participant.lid.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|jid| own_identities.contains(&jid.to_non_ad().to_string()));
        let contact = session.chats.contact(&identity).await.ok().flatten();
        let display_name = if is_self {
            "You".to_string()
        } else {
            contact
                .as_ref()
                .and_then(|contact| contact.display_name())
                .map(str::to_string)
                .or_else(|| participant.username.as_ref().map(ToString::to_string))
                .unwrap_or_else(|| identity.user.to_string())
        };
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
            is_self,
        });
    }

    let current_user_role = participants
        .iter()
        .find(|participant| participant.is_self)
        .map(|participant| participant.role)
        .or_else(|| created_by_self.then_some(ParticipantRole::SuperAdmin));
    participants.sort_by(|left, right| {
        right
            .is_self
            .cmp(&left.is_self)
            .then_with(|| role_rank(right.role).cmp(&role_rank(left.role)))
            .then_with(|| {
                left.display_name
                    .to_lowercase()
                    .cmp(&right.display_name.to_lowercase())
            })
    });
    let participant_count = metadata
        .size
        .map_or(participants.len(), |size| size as usize);
    GroupDetails {
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
            current_user_role,
        },
    }
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

    async fn contact_page(
        &self,
        query: String,
        after: Option<ContactPageCursor>,
        limit: usize,
    ) -> Result<ContactPage, String> {
        CoreBridge::contact_page(self, query, after, limit).await
    }

    async fn lookup_contact(
        &self,
        phone: ContactPhoneNumber,
    ) -> Result<ContactLookupResult, ServiceError> {
        CoreBridge::lookup_contact(self, phone).await
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

    async fn groups_in_common(&self, jid: String) -> Result<Vec<SharedGroup>, ServiceError> {
        CoreBridge::groups_in_common(self, jid).await
    }

    async fn group_details(&self, chat: String) -> Result<GroupDetails, String> {
        CoreBridge::group_details(self, chat).await
    }

    async fn create_group(
        &self,
        request: CreateGroupRequest,
    ) -> Result<GroupDetails, ServiceError> {
        CoreBridge::create_group(self, request).await
    }

    async fn update_group(&self, patch: GroupPatch) -> Result<GroupPatchResult, ServiceError> {
        CoreBridge::update_group(self, patch).await
    }

    async fn membership_requests(
        &self,
        chat: ChatId,
    ) -> Result<Vec<PendingMembershipRequest>, ServiceError> {
        CoreBridge::membership_requests(self, chat).await
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

    async fn cache_thumb_bytes(
        &self,
        key: String,
        source: PathBuf,
    ) -> Result<PathBuf, ServiceError> {
        CoreBridge::cache_thumb_bytes(self, key, source).await
    }

    async fn profile_picture(
        &self,
        request: ProfilePictureRequest,
    ) -> Result<Option<CachedAvatar>, ServiceError> {
        CoreBridge::profile_picture(self, request).await
    }

    fn cached_avatar_path(&self, jid: &str, picture: &AvatarRef) -> Option<PathBuf> {
        CoreBridge::cached_avatar_path(self, jid, picture)
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

    async fn perform_contact_action(&self, action: ContactAction) -> Result<(), ServiceError> {
        CoreBridge::perform_contact_action(self, action).await
    }
}

enum StaticUrlMedia {
    Image(whatsapp_rust::waproto::whatsapp::message::ImageMessage),
    Video(whatsapp_rust::waproto::whatsapp::message::VideoMessage),
    Ptv(whatsapp_rust::waproto::whatsapp::message::VideoMessage),
}

/// `DownloadParams` cannot retain the upstream `static_url` field. Keep these
/// message types intact so channel/newsletter media follows whatsapp-rust's
/// verbatim-URL download path instead of incorrectly rebuilding a CDN URL from
/// `direct_path`.
fn static_url_media(message: &whatsapp_rust::waproto::whatsapp::Message) -> Option<StaticUrlMedia> {
    let base = whatsapp_rust::wacore::proto_helpers::MessageExt::get_base_message(message);
    if let Some(image) = base.image_message.as_option()
        && image.static_url.is_some()
    {
        return Some(StaticUrlMedia::Image(image.clone()));
    }
    if let Some(video) = base.video_message.as_option()
        && video.static_url.is_some()
    {
        return Some(StaticUrlMedia::Video(video.clone()));
    }
    if let Some(ptv) = base.ptv_message.as_option()
        && ptv.static_url.is_some()
    {
        return Some(StaticUrlMedia::Ptv(ptv.clone()));
    }
    None
}

fn expected_media_sha(file_sha256: Option<&[u8]>) -> Option<[u8; 32]> {
    file_sha256.and_then(|sha| <[u8; 32]>::try_from(sha).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust::wacore::proto_helpers::MessageExt;
    use whatsapp_rust::waproto::buffa::MessageField;

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
    fn static_url_media_keeps_original_typed_downloadables() {
        let image = whatsapp_rust::waproto::whatsapp::Message {
            image_message: MessageField::some(
                whatsapp_rust::waproto::whatsapp::message::ImageMessage {
                    static_url: Some("https://static.example/image".to_string()),
                    file_sha256: Some(vec![1; 32]),
                    ..Default::default()
                },
            ),
            ..Default::default()
        };
        let Some(StaticUrlMedia::Image(image)) = static_url_media(&image) else {
            panic!("image static URL must stay on the typed path");
        };
        assert_eq!(
            image.static_url.as_deref(),
            Some("https://static.example/image")
        );
        assert_eq!(
            expected_media_sha(image.file_sha256.as_deref()),
            Some([1; 32])
        );

        let video = whatsapp_rust::waproto::whatsapp::Message {
            video_message: MessageField::some(
                whatsapp_rust::waproto::whatsapp::message::VideoMessage {
                    static_url: Some("https://static.example/video".to_string()),
                    file_sha256: Some(vec![2; 32]),
                    ..Default::default()
                },
            ),
            ..Default::default()
        };
        let Some(StaticUrlMedia::Video(video)) = static_url_media(&video) else {
            panic!("video static URL must stay on the typed path");
        };
        assert_eq!(
            video.static_url.as_deref(),
            Some("https://static.example/video")
        );

        let ptv = whatsapp_rust::waproto::whatsapp::Message {
            ptv_message: MessageField::some(
                whatsapp_rust::waproto::whatsapp::message::VideoMessage {
                    static_url: Some("https://static.example/ptv".to_string()),
                    file_sha256: Some(vec![3; 32]),
                    ..Default::default()
                },
            ),
            ..Default::default()
        };
        let Some(StaticUrlMedia::Ptv(ptv)) = static_url_media(&ptv) else {
            panic!("PTV static URL must stay on the typed path");
        };
        assert_eq!(
            ptv.static_url.as_deref(),
            Some("https://static.example/ptv")
        );

        let host_routed = whatsapp_rust::waproto::whatsapp::Message {
            video_message: MessageField::some(
                whatsapp_rust::waproto::whatsapp::message::VideoMessage {
                    direct_path: Some("/v/t62.7118-24/media".to_string()),
                    file_sha256: Some(vec![4; 32]),
                    ..Default::default()
                },
            ),
            ..Default::default()
        };
        assert!(static_url_media(&host_routed).is_none());
        assert!(wasabi_media::media_downloadable(&host_routed).is_some());
    }

    #[test]
    fn reply_context_promotes_plain_text_without_losing_its_body() {
        let mut message = whatsapp_rust::waproto::whatsapp::Message::text("answer");
        let context = whatsapp_rust::waproto::whatsapp::ContextInfo {
            stanza_id: Some("ORIGINAL".to_string()),
            ..Default::default()
        };

        attach_reply_context(&mut message, Some(context)).unwrap();

        assert_eq!(message.text_content(), Some("answer"));
        assert_eq!(
            message
                .extended_text_message
                .as_option()
                .and_then(|message| message.context_info.as_option())
                .and_then(|context| context.stanza_id.as_deref()),
            Some("ORIGINAL")
        );
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

    #[test]
    fn profile_metadata_mappers_treat_empty_and_unsafe_as_absent() {
        assert_eq!(
            optional_profile_text(Some("  hello  ")).as_deref(),
            Some("hello")
        );
        assert_eq!(optional_profile_text(Some("   ")), None);
        assert_eq!(optional_profile_text(None), None);
        assert_eq!(
            avatar_ref_from_picture_id(Some(" picture-1 ")),
            Some(AvatarRef("picture-1".to_string()))
        );
        assert_eq!(avatar_ref_from_picture_id(Some("")), None);
        assert!(profile_picture_url_is_safe("https://mmg.whatsapp.net/pic"));
        assert!(!profile_picture_url_is_safe("http://mmg.whatsapp.net/pic"));
        assert!(!profile_picture_url_is_safe(
            "https://example.com/pic with space"
        ));
        assert!(!profile_picture_url_is_safe("file:///tmp/pic"));
    }

    #[test]
    fn contact_lookup_errors_keep_actionable_typed_kinds() {
        use whatsapp_rust::request::IqError;

        assert_eq!(
            contact_lookup_error(whatsapp_rust::ContactError::Iq(IqError::Timeout)).kind,
            ErrorKind::Timeout
        );
        assert_eq!(
            contact_lookup_error(whatsapp_rust::ContactError::Iq(IqError::NotConnected)).kind,
            ErrorKind::NotConnected
        );
        assert_eq!(
            contact_lookup_error(whatsapp_rust::ContactError::InvalidJid(
                "redacted".to_string()
            ))
            .kind,
            ErrorKind::InvalidRequest
        );
    }

    #[test]
    fn blocking_errors_keep_actionable_typed_kinds_without_jids() {
        use whatsapp_rust::request::IqError;

        let invalid = map_blocking_error(whatsapp_rust::BlockingError::InvalidJid(
            "15551234567@s.whatsapp.net".to_string(),
        ));
        assert_eq!(invalid.kind, ErrorKind::InvalidRequest);
        assert!(!invalid.detail.contains("15551234567"));
        assert!(!invalid.detail.contains("@s.whatsapp.net"));

        let disconnected =
            map_blocking_error(whatsapp_rust::BlockingError::Iq(IqError::NotConnected));
        assert_eq!(disconnected.kind, ErrorKind::NotConnected);
        assert!(!disconnected.detail.contains('@'));
    }

    #[test]
    fn group_errors_keep_actionable_typed_kinds_without_content() {
        use whatsapp_rust::request::IqError;

        let timeout = group_service_error(whatsapp_rust::GroupError::Iq(IqError::Timeout));
        assert_eq!(timeout.kind, ErrorKind::Timeout);
        assert_eq!(timeout.detail, "group operation failed");
        assert_eq!(
            group_service_error(whatsapp_rust::GroupError::Iq(IqError::NotConnected)).kind,
            ErrorKind::NotConnected
        );
        assert_eq!(
            group_service_error(whatsapp_rust::GroupError::InvalidRequest(
                "private group subject".to_string()
            ))
            .kind,
            ErrorKind::InvalidRequest
        );
        assert!(leave_outcome_uncertain(ErrorKind::Timeout));
        assert!(leave_outcome_uncertain(ErrorKind::NotConnected));
        assert!(!leave_outcome_uncertain(ErrorKind::Protocol));
    }

    #[test]
    fn group_participant_mutations_accept_only_direct_identities() {
        let participants = [
            ChatId::new("15550000001@s.whatsapp.net"),
            ChatId::new("123456789012345@lid"),
        ];
        let parsed = participant_jids(&participants).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(participant_jids(&[ChatId::new("120363000000000001@g.us")]).is_err());
        assert_eq!(participant_result_counts(&[]), (0, 0, false));
    }
}
