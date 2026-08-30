//! Account session: one WhatsApp account's Bot/Client lifecycle owned by the
//! core domain.

use std::path::PathBuf;

use crate::lifecycle;
use std::sync::Arc;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use wasabi_core::state::SessionState;
use wasabi_repository::AccountStore;
use whatsapp_rust::TokioRuntime;
use whatsapp_rust::bot::{Bot, BotHandle, EventDelivery};
use whatsapp_rust::types::events::Event;
use whatsapp_rust_chat_store::ChatStore;

use crate::durability::RepositoryDurabilityHook;
use wasabi_domain::{PairingPhoneNumber, PhonePairCode, RATE_LIMITED_DEVICE};
use whatsapp_rust::pair_code::{PairCodeOptions, PairCodeRejection, PairError};

/// Assembly-time configuration for one account session.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Bounded event mailbox: ordered delivery with drops
    /// counted by the library. Durable message content bypasses this mailbox
    /// via the durability hook, so a drop here costs only re-derivable or
    /// self-healing signals.
    pub event_mailbox_capacity: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            event_mailbox_capacity: 4096,
        }
    }
}

/// Everything one live account owns. Dropping it cancels nothing implicitly —
/// use [`AccountSession::stop`]; the supervisor drives that during shutdown
///.
pub struct AccountSession {
    pub store: Arc<AccountStore>,
    pub chats: Arc<ChatStore>,
    state_tx: watch::Sender<SessionState>,
    qr_tx: watch::Sender<Option<lifecycle::QrState>>,
    typing_tx: tokio::sync::broadcast::Sender<wasabi_domain::TypingUpdate>,
    bot_handle: tokio::sync::Mutex<Option<BotHandle>>,
    pump: tokio::sync::Mutex<Option<lifecycle::Pump>>,
    outbox_recovery: tokio::sync::Mutex<Option<OutboxRecoveryTask>>,
    config: SessionConfig,
    db_path: PathBuf,
}

struct OutboxRecoveryTask {
    token: CancellationToken,
    join: tokio::task::JoinHandle<()>,
}

impl AccountSession {
    /// Open storage and prepare (not start) the session.
    pub async fn open(
        db_path: PathBuf,
        tuning: &wasabi_repository::StoreTuning,
        config: &SessionConfig,
    ) -> Result<Arc<Self>, wasabi_repository::OpenError> {
        let store = AccountStore::open(&db_path, tuning).await?;
        let chats = Arc::clone(store.chats());
        // The durability hook feeds this same store before the client emits
        // its hook-committed event. Skip that follow-up event so each inbound
        // batch is materialized exactly once.
        chats.skip_hook_committed_batches(true);
        let (state_tx, _) = watch::channel(SessionState::Stopped);
        let (qr_tx, _) = watch::channel(None);
        let (typing_tx, _) = tokio::sync::broadcast::channel(256);
        Ok(Arc::new(Self {
            store: Arc::new(store),
            chats,
            state_tx,
            qr_tx,
            typing_tx,
            bot_handle: tokio::sync::Mutex::new(None),
            pump: tokio::sync::Mutex::new(None),
            outbox_recovery: tokio::sync::Mutex::new(None),
            config: config.clone(),
            db_path,
        }))
    }

    pub fn db_path(&self) -> &std::path::Path {
        &self.db_path
    }

    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    pub fn store(&self) -> &Arc<wasabi_repository::AccountStore> {
        &self.store
    }

    pub fn chats(&self) -> &Arc<whatsapp_rust_chat_store::ChatStore> {
        &self.chats
    }

    /// Live protocol client while a Bot run loop is active; `None` between
    /// sessions (callers queue or surface "not connected").
    pub async fn client(&self) -> Option<std::sync::Arc<whatsapp_rust::client::Client>> {
        self.bot_handle.lock().await.as_ref().map(BotHandle::client)
    }

    pub fn subscribe_state(&self) -> watch::Receiver<SessionState> {
        self.state_tx.subscribe()
    }

    /// Latest pairing QR issued by the server; `None` once it is consumed
    /// (paired) or dead (rotated out / expired).
    pub fn subscribe_qr(&self) -> watch::Receiver<Option<lifecycle::QrState>> {
        self.qr_tx.subscribe()
    }

    pub fn subscribe_typing(
        &self,
    ) -> tokio::sync::broadcast::Receiver<wasabi_domain::TypingUpdate> {
        self.typing_tx.subscribe()
    }

    pub fn state(&self) -> SessionState {
        self.state_tx.borrow().clone()
    }

    fn set_state(&self, next: SessionState) {
        lifecycle::transition_to(&self.state_tx, next);
    }

    async fn is_running(&self) -> bool {
        self.bot_handle.lock().await.is_some()
    }

    /// Connect a paired (or pairable) account: assemble the Bot over the
    /// account database and start its run loop in the background. An already
    /// running session refuses rather than being replaced — use [`Self::stop`]
    /// first, or [`Self::start_pairing`] for relink semantics.
    pub async fn connect(
        self: &Arc<Self>,
        config: &SessionConfig,
        supervisor_token: CancellationToken,
    ) -> Result<(), lifecycle::LifecycleError> {
        self.launch(
            config.clone(),
            supervisor_token.child_token(),
            SessionState::Connecting,
        )
        .await
    }

    /// Begin (or join) QR pairing. Single-flight: while a pairing is already
    /// in flight this returns the existing QR feed instead of restarting.
    /// Any previous run is torn down first — linking anew replaces whatever
    /// ran before, and each attempt gets a fresh cancellation scope.
    ///
    /// The library flow needs no explicit connect call here: for an unpaired
    /// device the server pushes rotating `<pair-device>` refs as soon as the
    /// run loop connects, which surface as `PairingQrCode` events on the feed
    /// below (`pair_with_qr_code` is the opposite, primary-side flow).
    pub async fn start_pairing(
        self: &Arc<Self>,
        supervisor_token: CancellationToken,
    ) -> Result<watch::Receiver<Option<lifecycle::QrState>>, lifecycle::LifecycleError> {
        if matches!(self.state(), SessionState::Pairing) && self.is_running().await {
            return Ok(self.subscribe_qr());
        }
        // Replacement, not refusal: a stale or failed attempt must not block
        // the next one.
        self.stop().await;
        self.launch(
            self.config.clone(),
            supervisor_token.child_token(),
            SessionState::Pairing,
        )
        .await?;
        Ok(self.subscribe_qr())
    }

    /// Request a short-lived companion code for a validated phone number.
    /// Both the phone number and returned code remain memory-only and have
    /// redacted debug representations at the product boundary.
    pub async fn pair_with_phone(
        &self,
        phone: PairingPhoneNumber,
    ) -> Result<PhonePairCode, String> {
        let client = self
            .client()
            .await
            .ok_or_else(|| "Wait for the pairing connection, then try again".to_string())?;
        // A deliberate retry replaces the previous live code instead of
        // letting the dependency reject a competing request.
        client.cancel_pair_code().await;
        let code = client
            .pair_with_code(PairCodeOptions {
                phone_number: phone.as_str().to_string(),
                show_push_notification: true,
                ..PairCodeOptions::default()
            })
            .await
            .map_err(|error| {
                let copy =
                    pairing_request_error_copy(error.rejection(), pair_error_iq_code(&error));
                warn!(
                    rate_limited = copy == RATE_LIMITED_DEVICE,
                    "phone pairing request failed"
                );
                copy
            })?;
        Ok(PhonePairCode {
            code,
            expires_in: wacore::pair_code::PairCodeUtils::code_validity(),
        })
    }

    /// Cancel any in-flight or still-valid phone pairing code.
    pub async fn cancel_phone_pairing(&self) {
        if let Some(client) = self.client().await {
            client.cancel_pair_code().await;
        }
    }

    /// Assemble and start the bot stack under `run_token`, then announce
    /// `initial`. Serialized on `bot_handle` so concurrent launches/teardowns
    /// cannot interleave.
    async fn launch(
        &self,
        config: SessionConfig,
        run_token: CancellationToken,
        initial: SessionState,
    ) -> Result<(), lifecycle::LifecycleError> {
        let mut guard = self.bot_handle.lock().await;
        if guard.is_some() {
            return Err(lifecycle::LifecycleError::AlreadyRunning);
        }

        let backend = lifecycle::protocol_backend(&self.store);
        let (event_tx, event_rx) =
            whatsapp_rust::async_channel::bounded::<Arc<Event>>(lifecycle::PUMP_MAILBOX_CAPACITY);
        let bot = Bot::builder()
            .with_backend_arc(backend)
            // The workspace builds the library without default features, so no
            // runtime slot is pre-filled; supply Tokio explicitly.
            .with_runtime(TokioRuntime)
            .with_inbound_durability_hook(RepositoryDurabilityHook::new(Arc::clone(&self.chats)))
            .with_event_delivery(EventDelivery::Ordered {
                capacity: config.event_mailbox_capacity,
            })
            .on_event(move |event, _client| {
                let event_tx = event_tx.clone();
                // Awaited send: when the pump stalls, delivery backs up into
                // the ordered mailbox whose overflow is counted by the
                // library instead of growing without bound.
                async move {
                    let _ = event_tx.send(event).await;
                }
            })
            .build()
            .await?;

        // Chat materialization subscribes before the run loop starts so the
        // earliest events of a connection are not missed.
        bot.client()
            .subscribe_handler(self.chats.handler())
            .detach();

        let handle = bot.spawn();

        // A committed Pending outbox row is the crash window between the
        // record-before-send barrier and the server ack. Recovery used to be
        // implemented but never scheduled, so those rows could survive every
        // restart indefinitely. Arm one sweep per session run and wait until
        // the event pump reports a usable connection before touching the
        // network. Capture the cutoff before the bot starts so a new send that
        // races the first Connected event can never be mistaken for crash
        // residue. Ambiguous pre-launch publication is safe to repeat under
        // the original id.
        let recovery_cutoff = whatsapp_rust::chrono::Utc::now();
        let recovery_token = run_token.child_token();
        let recovery_task_token = recovery_token.clone();
        let recovery_wait_token = recovery_token.clone();
        let recovery_client = handle.client();
        let recovery_chats = Arc::clone(&self.chats);
        let recovery_state = self.subscribe_state();
        let recovery_join = tokio::spawn(run_once_after_first_connected(
            recovery_state,
            recovery_wait_token,
            move || async move {
                let stale_after = whatsapp_rust::chrono::Utc::now() - recovery_cutoff;
                crate::outbox::reconcile_stale_pending(
                    recovery_chats,
                    recovery_client,
                    stale_after,
                    recovery_token,
                )
                .await;
            },
        ));
        *self.outbox_recovery.lock().await = Some(OutboxRecoveryTask {
            token: recovery_task_token,
            join: recovery_join,
        });

        let pump = lifecycle::spawn_event_pump(
            event_rx,
            self.state_tx.clone(),
            self.qr_tx.clone(),
            self.typing_tx.clone(),
            run_token,
        );
        // Keep the pump under the same account owner as the bot. Dropping
        // this handle would detach the task, which makes reconnects and
        // shutdown unable to cancel and join the old event consumer.
        *self.pump.lock().await = Some(pump);
        *guard = Some(handle);
        drop(guard);

        self.set_state(initial);
        info!("session: bot started");
        Ok(())
    }

    /// Stop the session deterministically: disconnect transport, cancel bot
    /// loop and event pump, flush durable boundaries. Idempotent and safe to
    /// race with itself.
    pub async fn stop(&self) {
        // Pump first: it must not fold teardown-time events into the state
        // machine while shutdown is in progress.
        if let Some(pump) = self.pump.lock().await.take() {
            pump.token.cancel();
            let _ = tokio::time::timeout(lifecycle::PUMP_JOIN_TIMEOUT, pump.join).await;
        }
        if let Some(recovery) = self.outbox_recovery.lock().await.take() {
            recovery.token.cancel();
            let _ = tokio::time::timeout(lifecycle::PUMP_JOIN_TIMEOUT, recovery.join).await;
        }
        if let Some(bot) = self.bot_handle.lock().await.take() {
            info!("session: shutting down bot");
            bot.shutdown().await;
        }
        if let Err(e) = self.chats.flush().await {
            warn!("session: final flush failed: {e}");
        }
        let _ = self.qr_tx.send(None);
        // Route through the table: Connected has no direct edge to Stopped,
        // so record the transport loss first when that edge is missing.
        let current = self.state();
        if current.transition(SessionState::Stopped).is_err() {
            self.set_state(SessionState::Disconnected {
                reason: Some("session stopped".to_string()),
            });
        }
        self.set_state(SessionState::Stopped);
    }

    /// Unlink the device from the phone and stop the session. The client's
    /// logout is infallible by contract (best-effort deregistration IQ plus a
    /// local teardown), so there is nothing to propagate.
    pub async fn logout(self: &Arc<Self>) {
        self.set_state(SessionState::LoggingOut);
        let client = self.bot_handle.lock().await.as_ref().map(BotHandle::client);
        if let Some(client) = client {
            client.logout().await;
        } else {
            warn!("session: logout requested with no running bot");
        }
        self.stop().await;
    }
}

fn pair_rejection_is_rate_limited(rejection: PairCodeRejection) -> bool {
    matches!(rejection, PairCodeRejection::RateOverlimit) || rejection.code() == 429
}

fn pair_error_iq_code(error: &PairError) -> Option<u16> {
    match error {
        PairError::RequestFailed(whatsapp_rust::IqError::ServerError { code, .. }) => Some(*code),
        _ => None,
    }
}

fn pairing_request_error_copy(
    rejection: Option<PairCodeRejection>,
    iq_code: Option<u16>,
) -> String {
    if rejection.is_some_and(pair_rejection_is_rate_limited) || iq_code == Some(429) {
        RATE_LIMITED_DEVICE.to_string()
    } else {
        "Couldn’t request a pairing code. Try again.".to_string()
    }
}

async fn wait_for_first_connected(
    mut state_rx: watch::Receiver<SessionState>,
    token: CancellationToken,
) -> bool {
    loop {
        if state_rx.borrow().is_connected() {
            return true;
        }
        tokio::select! {
            _ = token.cancelled() => return false,
            changed = state_rx.changed() => {
                if changed.is_err() {
                    return false;
                }
            }
        }
    }
}

async fn run_once_after_first_connected<F, Fut>(
    state_rx: watch::Receiver<SessionState>,
    token: CancellationToken,
    recover: F,
) where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    if wait_for_first_connected(state_rx, token).await {
        recover().await;
    }
}

#[cfg(test)]
mod tests {
    use super::run_once_after_first_connected;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::watch;
    use tokio_util::sync::CancellationToken;
    use wasabi_core::state::SessionState;

    #[tokio::test]
    async fn outbox_recovery_waits_for_connected_state() {
        let (state_tx, state_rx) = watch::channel(SessionState::Connecting);
        let token = CancellationToken::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let task_calls = Arc::clone(&calls);
        let wait = tokio::spawn(run_once_after_first_connected(
            state_rx,
            token,
            move || async move {
                task_calls.fetch_add(1, Ordering::SeqCst);
            },
        ));

        tokio::task::yield_now().await;
        assert!(
            !wait.is_finished(),
            "recovery must not run while connecting"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        state_tx.send(SessionState::Reconnecting).unwrap();
        tokio::task::yield_now().await;
        assert!(!wait.is_finished(), "recovery must survive reconnecting");
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        state_tx.send(SessionState::Connected).unwrap();
        wait.await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn outbox_recovery_wait_is_cancelled_with_session_run() {
        let (_state_tx, state_rx) = watch::channel(SessionState::Connecting);
        let token = CancellationToken::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let task_calls = Arc::clone(&calls);
        let wait = tokio::spawn(run_once_after_first_connected(
            state_rx,
            token.clone(),
            move || async move {
                task_calls.fetch_add(1, Ordering::SeqCst);
            },
        ));

        token.cancel();
        wait.await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
mod pairing_error_tests {
    use super::{pair_rejection_is_rate_limited, pairing_request_error_copy};
    use wasabi_domain::RATE_LIMITED_DEVICE;
    use whatsapp_rust::pair_code::PairCodeRejection;

    #[test]
    fn pair_code_429_uses_device_rate_limit_copy() {
        assert!(pair_rejection_is_rate_limited(
            PairCodeRejection::RateOverlimit
        ));
        assert_eq!(PairCodeRejection::RateOverlimit.code(), 429);
        assert_eq!(
            pairing_request_error_copy(Some(PairCodeRejection::RateOverlimit), None),
            RATE_LIMITED_DEVICE
        );
        assert_eq!(
            pairing_request_error_copy(None, Some(429)),
            RATE_LIMITED_DEVICE
        );
        assert!(!RATE_LIMITED_DEVICE.contains("429"));
        assert!(!RATE_LIMITED_DEVICE.contains("Unknown"));
        assert!(!RATE_LIMITED_DEVICE.contains('@'));
    }

    #[test]
    fn other_pair_failures_do_not_dump_protocol_text() {
        let copy = pairing_request_error_copy(Some(PairCodeRejection::Forbidden), None);
        assert_ne!(copy, RATE_LIMITED_DEVICE);
        assert!(!copy.contains("Forbidden"));
        assert!(!copy.contains("403"));
        assert!(!copy.contains("phone"));
        assert!(!copy.contains("1555"));
    }
}
