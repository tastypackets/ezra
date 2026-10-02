use super::{EventStore, StoreError};
use crate::inbound::EventKey;

impl EventStore {
    /// Call only after confirming that the native session accepted this event's message.
    pub async fn confirm_delivery(&self, key: &EventKey) -> Result<bool, StoreError> {
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let result = sqlx::query!(
            "UPDATE inbound_events SET delivery_state = 'delivered'
             WHERE source = ?1 AND subject = ?2 AND event_id = ?3
               AND delivery_state = 'uncertain' AND (new_chat = 0 OR new_chat_applied = 1)",
            key.conversation.source,
            key.conversation.subject,
            key.id,
        )
        .execute(&mut *transaction)
        .await?;
        let confirmed = result.rows_affected() == 1;
        if confirmed {
            sqlx::query!(
                "UPDATE inbound_sessions SET last_used_at = MAX(last_used_at, unixepoch()), initial_context_pending = 0
                 WHERE id = (
                     SELECT session_id FROM inbound_conversations WHERE source = ?1 AND subject = ?2
                 )",
                key.conversation.source,
                key.conversation.subject,
            )
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(confirmed)
    }
}
