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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::store::{InsertOutcome, TEST_QUEUE_LIMITS};
    use crate::inbound::{ConversationKey, EventKey, InboundEvent};
    use time::macros::datetime;

    const TEST_HISTORY_LIMITS: HistoryLimits = HistoryLimits {
        max_events: 100,
        max_message_bytes: 1024 * 1024,
    };

    impl InboundEvent {
        fn retention_example(event_id: &str) -> Self {
            Self {
                key: EventKey {
                    conversation: ConversationKey {
                        source: "github:github.com".to_owned(),
                        subject: event_id.to_owned(),
                    },
                    id: event_id.to_owned(),
                },
                new_chat: false,
                options: Default::default(),
                chat_name: None,
                source_url: None,
                initial_context: None,
                actor: "author".to_owned(),
                created_at: datetime!(2020-01-01 00:00 UTC),
                message: "Continue this work".to_owned(),
            }
        }
    }

    #[tokio::test]
    async fn history_uses_receipt_age_for_every_terminal_outcome_and_preserves_waiting_work() {
        let directory = tempfile::tempdir().expect("directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store");
        let cutoff = datetime!(2026-10-02 00:00 UTC);
        for state in [
            "delivered",
            "uncertain",
            "failed",
            "expired",
            "pending",
            "delivering",
        ] {
            for old_receipt in [false, true] {
                let event = InboundEvent::retention_example(&format!("{state}-{old_receipt}"));
                assert_eq!(
                    store
                        .insert(&event, TEST_QUEUE_LIMITS)
                        .await
                        .expect("insert old source comment"),
                    InsertOutcome::Inserted
                );
                let received_at = cutoff.unix_timestamp() - i64::from(old_receipt);
                sqlx::query!("UPDATE inbound_events SET delivery_state = ?1, received_at = ?2 WHERE event_id = ?3", state, received_at, event.key.id)
                    .execute(&store.pool).await.expect("receipt and outcome");
            }
        }
        assert_eq!(
            store
                .prune_history(cutoff, TEST_HISTORY_LIMITS)
                .await
                .expect("prune"),
            4
        );
        for state in ["delivered", "uncertain", "failed", "expired"] {
            assert!(
                store
                    .get(&InboundEvent::retention_example(&format!("{state}-true")).key)
                    .await
                    .expect("old history")
                    .is_none()
            );
            assert_eq!(
                store
                    .get(&InboundEvent::retention_example(&format!("{state}-false")).key)
                    .await
                    .expect("fresh history")
                    .expect("kept")
                    .created_at,
                datetime!(2020-01-01 00:00 UTC)
            );
        }
        for state in ["pending", "delivering"] {
            assert!(
                store
                    .get(&InboundEvent::retention_example(&format!("{state}-true")).key)
                    .await
                    .expect("waiting work")
                    .is_some()
            );
        }
    }

    #[tokio::test]
    async fn history_capacity_counts_initial_context_bytes_and_excludes_waiting_work() {
        let directory = tempfile::tempdir().expect("directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store");
        let mut event = InboundEvent::retention_example("context");
        event.message = "x".to_owned();
        event.initial_context = Some("🦦".to_owned());
        store
            .insert(&event, TEST_QUEUE_LIMITS)
            .await
            .expect("insert");
        let pending = InboundEvent::retention_example("pending");
        store
            .insert(&pending, TEST_QUEUE_LIMITS)
            .await
            .expect("waiting");
        sqlx::query!(
            "UPDATE inbound_events SET delivery_state = 'uncertain' WHERE event_id = 'context'"
        )
        .execute(&store.pool)
        .await
        .expect("terminal");
        assert_eq!(
            store
                .prune_history(
                    OffsetDateTime::UNIX_EPOCH,
                    HistoryLimits {
                        max_message_bytes: 5,
                        ..TEST_HISTORY_LIMITS
                    }
                )
                .await
                .expect("exact limit"),
            0
        );
        assert_eq!(
            store
                .prune_history(
                    OffsetDateTime::UNIX_EPOCH,
                    HistoryLimits {
                        max_message_bytes: 4,
                        ..TEST_HISTORY_LIMITS
                    }
                )
                .await
                .expect("prune context"),
            1
        );
        assert!(store.get(&pending.key).await.expect("waiting").is_some());
    }

    #[tokio::test]
    async fn count_capacity_keeps_newest_receipts_and_zero_removes_terminal_history() {
        let directory = tempfile::tempdir().expect("directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store");
        for (event_id, receipt) in [("older", 100), ("newer", 101)] {
            let event = InboundEvent::retention_example(event_id);
            store
                .insert(&event, TEST_QUEUE_LIMITS)
                .await
                .expect("insert");
            sqlx::query!("UPDATE inbound_events SET delivery_state = 'failed', received_at = ?1 WHERE event_id = ?2", receipt, event_id)
                .execute(&store.pool).await.expect("terminal receipt");
        }
        let limits = HistoryLimits {
            max_events: 1,
            ..TEST_HISTORY_LIMITS
        };
        assert_eq!(
            store
                .prune_history(OffsetDateTime::UNIX_EPOCH, limits)
                .await
                .expect("count cap"),
            1
        );
        assert!(
            store
                .get(&InboundEvent::retention_example("newer").key)
                .await
                .expect("newest")
                .is_some()
        );
        assert_eq!(
            store
                .prune_history(
                    OffsetDateTime::UNIX_EPOCH,
                    HistoryLimits {
                        max_events: 0,
                        ..limits
                    }
                )
                .await
                .expect("zero cap"),
            1
        );
    }
}
