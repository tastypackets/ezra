use time::OffsetDateTime;

use super::{EventStore, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimits {
    pub max_events: u32,
    pub max_message_bytes: u64,
}

impl EventStore {
    pub async fn prune_history(
        &self,
        cutoff: OffsetDateTime,
        limits: HistoryLimits,
    ) -> Result<u64, StoreError> {
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let age_floor = cutoff.unix_timestamp();
        let max_message_bytes = i64::try_from(limits.max_message_bytes).unwrap_or(i64::MAX);
        let expired_through = sqlx::query_scalar!(
            r#"WITH newest_history AS (
                   SELECT received_at, length(CAST(message AS BLOB)) + COALESCE(length(CAST(initial_context AS BLOB)), 0) AS message_bytes
                   FROM inbound_events
                   WHERE delivery_state IN ('delivered', 'uncertain', 'failed', 'expired') AND received_at >= ?1
                   ORDER BY received_at DESC LIMIT ?2 + 1
               ), ranked_history AS (
                   SELECT received_at,
                          ROW_NUMBER() OVER newest_first AS event_rank,
                          SUM(message_bytes) OVER newest_first AS message_bytes
                   FROM newest_history
                   WINDOW newest_first AS (
                       ORDER BY received_at DESC ROWS UNBOUNDED PRECEDING
                   )
               )
               SELECT MAX(received_at) AS "expired_through?: i64" FROM ranked_history
               WHERE event_rank > ?2 OR message_bytes > ?3"#,
            age_floor,
            limits.max_events,
            max_message_bytes,
        )
        .fetch_one(&mut *transaction)
        .await?;
        let receipt_floor = expired_through
            .map(|expired_timestamp| expired_timestamp.saturating_add(1).max(age_floor))
            .unwrap_or(age_floor);
        let result = sqlx::query!(
            "DELETE FROM inbound_events
             WHERE delivery_state IN ('delivered', 'uncertain', 'failed', 'expired') AND received_at < ?1",
            receipt_floor,
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(result.rows_affected())
    }
}
