use time::OffsetDateTime;

use super::{EventStore, StoreError};
use crate::inbound::EventKey;

impl EventStore {
    pub async fn claim_feedback(&self, key: &EventKey, maximum: u32) -> Result<bool, StoreError> {
        let inserted = sqlx::query!(
            "INSERT INTO inbound_feedback_attempts (source, subject, event_id, created_at_seconds)
             SELECT ?1, ?2, ?3, unixepoch()
             WHERE (SELECT COUNT(*) FROM inbound_feedback_attempts) < ?4
               AND unixepoch() >= (SELECT created_at_floor FROM inbound_feedback_retention WHERE singleton = 1)
             ON CONFLICT (source, subject, event_id) DO NOTHING",
            key.conversation.source,
            key.conversation.subject,
            key.id,
            maximum,
        )
        .execute(&self.pool)
        .await?;
        Ok(inserted.rows_affected() == 1)
    }

    pub async fn prune_feedback(
        &self,
        cutoff: OffsetDateTime,
        maximum: u32,
    ) -> Result<u64, StoreError> {
        let cutoff_seconds = cutoff.unix_timestamp();
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let expired = sqlx::query_scalar!(
            r#"SELECT MAX(created_at_seconds) AS "expired?: i64" FROM (
                 SELECT created_at_seconds FROM inbound_feedback_attempts
                 ORDER BY created_at_seconds DESC LIMIT -1 OFFSET ?1
             )"#,
            maximum,
        )
        .fetch_one(&mut *transaction)
        .await?;
        let requested_floor = expired.map_or(cutoff_seconds, |timestamp| {
            timestamp.saturating_add(1).max(cutoff_seconds)
        });
        let floor = sqlx::query_scalar!(
            "UPDATE inbound_feedback_retention
             SET created_at_floor = MAX(created_at_floor, ?1)
             WHERE singleton = 1 RETURNING created_at_floor",
            requested_floor,
        )
        .fetch_one(&mut *transaction)
        .await?;
        let removed = sqlx::query!(
            "DELETE FROM inbound_feedback_attempts WHERE created_at_seconds < ?1",
            floor,
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(removed.rows_affected())
    }
}
