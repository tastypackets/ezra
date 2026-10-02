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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::ConversationKey;

    #[tokio::test]
    async fn newly_observed_feedback_uses_local_time_after_retention_cleanup() {
        let directory = tempfile::tempdir().expect("directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store");
        let cutoff = OffsetDateTime::now_utc() - time::Duration::days(90);
        store
            .prune_feedback(cutoff, 100)
            .await
            .expect("retention floor");
        let key = EventKey {
            conversation: ConversationKey {
                source: "github:github.com".into(),
                subject: "7/42".into(),
            },
            id: "old-comment:new-status".into(),
        };
        assert!(
            store
                .claim_feedback(&key, 100)
                .await
                .expect("new status claim")
        );
        let received_at = sqlx::query_scalar!(
            "SELECT created_at_seconds FROM inbound_feedback_attempts WHERE event_id = ?1",
            key.id
        )
        .fetch_one(&store.pool)
        .await
        .expect("feedback receipt");
        assert!(received_at >= cutoff.unix_timestamp());
    }

    #[tokio::test]
    async fn feedback_claim_survives_restart_and_has_a_hard_capacity_limit() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("store");
        let key = EventKey {
            conversation: ConversationKey {
                source: "github:github.com".into(),
                subject: "7/42".into(),
            },
            id: "10".into(),
        };
        assert!(store.claim_feedback(&key, 1).await.expect("claim"));
        drop(store);
        let store = EventStore::open(&path).await.expect("reopen");
        assert!(!store.claim_feedback(&key, 1).await.expect("no repeat"));
        let another = EventKey {
            id: "11".into(),
            ..key.clone()
        };
        assert!(!store.claim_feedback(&another, 1).await.expect("capacity"));
        sqlx::query!("UPDATE inbound_feedback_attempts SET created_at_seconds = 0")
            .execute(&store.pool)
            .await
            .expect("old feedback receipt");
        assert_eq!(
            store
                .prune_feedback(OffsetDateTime::now_utc() - time::Duration::days(1), 1)
                .await
                .expect("age cleanup"),
            1
        );
        assert!(store.claim_feedback(&another, 1).await.expect("room"));
        assert_eq!(
            store
                .prune_feedback(OffsetDateTime::UNIX_EPOCH, 0)
                .await
                .expect("count cleanup"),
            1
        );
        assert_eq!(
            store
                .prune_feedback(OffsetDateTime::UNIX_EPOCH, 1)
                .await
                .expect("empty cleanup"),
            0
        );
    }
}
