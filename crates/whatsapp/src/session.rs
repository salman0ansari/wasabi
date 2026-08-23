//! Account session: one WhatsApp account's Bot/Client lifecycle owned by the
//! core domain (Phase 3 completes pairing/reconnect; this file establishes
//! assembly, state surfacing, and teardown ownership).

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::watch;
use tracing::{info, warn};

use wasabi_core::state::SessionState;
use wasabi_repository::AccountStore;
use whatsapp_rust::bot::BotHandle;
use whatsapp_rust_chat_store::ChatStore;

/// Assembly-time configuration for one account session.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Bounded event mailbox (charter §21/§53): ordered delivery with drops
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
/// (INV-16/17).
pub struct AccountSession {
    pub store: Arc<AccountStore>,
    pub chats: Arc<ChatStore>,
    state_tx: watch::Sender<SessionState>,
    bot_handle: tokio::sync::Mutex<Option<BotHandle>>,
    db_path: PathBuf,
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
        let _ = config; // consumed at connect() in Phase 3
        let (state_tx, _) = watch::channel(SessionState::Stopped);
        Ok(Arc::new(Self {
            store: Arc::new(store),
            chats,
            state_tx,
            bot_handle: tokio::sync::Mutex::new(None),
            db_path,
        }))
    }

    pub fn db_path(&self) -> &std::path::Path {
        &self.db_path
    }

    pub fn subscribe_state(&self) -> watch::Receiver<SessionState> {
        self.state_tx.subscribe()
    }

    fn set_state(&self, next: SessionState) {
        // Invalid transitions are programming errors; log loudly but never
        // wedge the pipeline on them mid-flight.
        let current = self.state_tx.borrow().clone();
        match current.transition(next.clone()) {
            Ok(next) => {
                let _ = self.state_tx.send(next);
            }
            Err(e) => warn!(from = %e.from, to = %e.to, "rejected invalid session transition"),
        }
    }

    /// Stop the session deterministically: disconnect transport, cancel bot
    /// loop, flush durable boundaries. Idempotent.
    pub async fn stop(&self) {
        if let Some(bot) = self.bot_handle.lock().await.take() {
            info!("session: shutting down bot");
            bot.shutdown().await;
        }
        if let Err(e) = self.chats.flush().await {
            warn!("session: final flush failed: {e}");
        }
        self.set_state(SessionState::Stopped);
    }
}
