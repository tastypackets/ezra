use super::{EventStore, StoreError};
use crate::inbound::EventKey;

impl EventStore {
    pub async fn claim_for_binding(&self, key: &EventKey) -> Result<bool, StoreError> {
        let result = sqlx::query!(
            "UPDATE inbound_events SET delivery_state = 'delivering'
             WHERE source = ?1 AND subject = ?2 AND event_id = ?3 AND delivery_state = 'pending'
               AND NOT EXISTS (
                   SELECT 1 FROM inbound_conversations WHERE source = ?1 AND subject = ?2
               )
               AND NOT EXISTS (
                   SELECT 1 FROM inbound_events AS unresolved
                   WHERE unresolved.source = ?1 AND unresolved.subject = ?2
                     AND unresolved.delivery_state = 'delivering'
               )",
            key.conversation.source,
            key.conversation.subject,
            key.id,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }
}
