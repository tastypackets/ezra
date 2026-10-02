use time::OffsetDateTime;

use super::{EventStore, StoreError};
use crate::inbound::{ConversationKey, EventKey};

impl EventStore {
    pub async fn expire_waiting(
        &self,
        cutoff: OffsetDateTime,
    ) -> Result<Vec<EventKey>, StoreError> {
        let cutoff_seconds = cutoff.unix_timestamp();
        let result = sqlx::query!(
            "UPDATE inbound_events SET delivery_state = 'expired'
             WHERE delivery_state IN ('pending', 'delivering') AND attempted_at IS NULL AND received_at <= ?1
             RETURNING source, subject, event_id",
            cutoff_seconds,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(result
            .into_iter()
            .map(|record| EventKey {
                conversation: ConversationKey {
                    source: record.source,
                    subject: record.subject,
                },
                id: record.event_id,
            })
            .collect())
    }
}
