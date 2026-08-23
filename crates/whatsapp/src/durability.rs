//! Inbound durability: persist-before-ACK via the library's
//! `InboundDurabilityHook`.
//!
//! The hook runs inside the receive pipeline; awaiting our commit here is the
//! backpressure that makes "persist before ACK" true. The commit path is
//! `ChatStore::apply_inbound` — a direct transaction reusing the store's own
//! idempotent appliers — NOT the write-behind queue.

use std::sync::Arc;

use async_trait::async_trait;
use whatsapp_rust::InboundDurabilityHook;
use whatsapp_rust::client::Client;
use whatsapp_rust::types::events::InboundMessage;
use whatsapp_rust_chat_store::ChatStore;

/// Commits every decrypted inbound batch to the chat store before the
/// transport ACKs. Idempotent by construction (upsert guards keyed
/// `(device_id, chat_jid, msg_id)`; tombstones and monotonicity enforced in
/// the appliers), so server redelivery after a crash-before-ACK converges.
pub struct RepositoryDurabilityHook {
    chats: Arc<ChatStore>,
}

impl RepositoryDurabilityHook {
    pub fn new(chats: Arc<ChatStore>) -> Self {
        Self { chats }
    }
}

#[async_trait]
impl InboundDurabilityHook for RepositoryDurabilityHook {
    async fn on_messages(
        &self,
        _client: Arc<Client>,
        batch: &[InboundMessage],
    ) -> anyhow::Result<()> {
        // Batch copy: live batches are size 1; drain batches are bounded by
        // the library's MessageProcessorCache granularity. Correctness over
        // micro-allocation here — this path gates delivery.
        self.chats.apply_inbound(batch.to_vec()).await?;
        Ok(())
    }
}
