use time::OffsetDateTime;

use super::{EventStore, StoreError};
use crate::inbound::ConversationKey;

#[derive(Debug, PartialEq, Eq)]
pub struct SourceCheckpoint {
    pub scanned_through: OffsetDateTime,
    pub activated_at: OffsetDateTime,
}

impl EventStore {
    pub async fn source_checkpoint(
        &self,
        scope: &ConversationKey,
        initial: OffsetDateTime,
    ) -> Result<SourceCheckpoint, StoreError> {
        let initial_seconds = initial.unix_timestamp();
        let checkpoint = sqlx::query!(
            "INSERT INTO inbound_source_checkpoints (source, subject, scanned_through, activated_at)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT (source, subject) DO UPDATE SET updated_at = unixepoch(), active = 1
             RETURNING scanned_through, activated_at",
            scope.source,
            scope.subject,
            initial_seconds,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(SourceCheckpoint {
            scanned_through: OffsetDateTime::from_unix_timestamp(checkpoint.scanned_through)
                .map_err(|error| StoreError::Sqlite(sqlx::Error::Decode(Box::new(error))))?,
            activated_at: OffsetDateTime::from_unix_timestamp(checkpoint.activated_at)
                .map_err(|error| StoreError::Sqlite(sqlx::Error::Decode(Box::new(error))))?,
        })
    }

    pub async fn set_active_source_scopes(
        &self,
        source_prefix: &str,
        source: &str,
        subjects: &[String],
    ) -> Result<(), StoreError> {
        let subjects = serde_json::to_string(subjects).expect("strings serialize as JSON");
        sqlx::query!(
            "UPDATE inbound_source_checkpoints
             SET active = source = ?2 AND EXISTS (SELECT 1 FROM json_each(?3) WHERE value = subject)
             WHERE substr(source, 1, length(?1)) = ?1",
            source_prefix,
            source,
            subjects,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn advance_source_checkpoint(
        &self,
        scope: &ConversationKey,
        scanned_through: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let scanned_seconds = scanned_through.unix_timestamp();
        let result = sqlx::query!(
            "UPDATE inbound_source_checkpoints
             SET scanned_through = MAX(scanned_through, ?3), updated_at = unixepoch()
             WHERE source = ?1 AND subject = ?2",
            scope.source,
            scope.subject,
            scanned_seconds,
        )
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(StoreError::CheckpointMissing);
        }
        Ok(())
    }

    pub async fn clamp_source_checkpoint(
        &self,
        scope: &ConversationKey,
        ceiling: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let ceiling_seconds = ceiling.unix_timestamp();
        let result = sqlx::query!(
            "UPDATE inbound_source_checkpoints
             SET scanned_through = MIN(scanned_through, ?3), activated_at = MIN(activated_at, ?3), updated_at = unixepoch()
             WHERE source = ?1 AND subject = ?2",
            scope.source, scope.subject, ceiling_seconds,
        ).execute(&self.pool).await?;
        if result.rows_affected() != 1 {
            return Err(StoreError::CheckpointMissing);
        }
        Ok(())
    }

    pub async fn prune_source_checkpoints(
        &self,
        cutoff: OffsetDateTime,
        maximum: u32,
    ) -> Result<u64, StoreError> {
        let cutoff_seconds = cutoff.unix_timestamp();
        let removed = sqlx::query!(
            "DELETE FROM inbound_source_checkpoints
             WHERE rowid IN (
                 SELECT rowid FROM inbound_source_checkpoints
                 WHERE active = 0 AND updated_at < ?1
                 UNION
                 SELECT rowid FROM (
                     SELECT rowid FROM inbound_source_checkpoints
                     WHERE active = 0
                     ORDER BY updated_at DESC, rowid DESC LIMIT -1 OFFSET ?2
                 )
             )",
            cutoff_seconds,
            maximum,
        )
        .execute(&self.pool)
        .await?;
        Ok(removed.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[tokio::test]
    async fn switching_hosts_expires_old_checkpoints_without_touching_other_providers() {
        let directory = tempfile::tempdir().expect("directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store");
        let now = OffsetDateTime::now_utc();
        for source in ["github:old.example", "github:github.com", "api:custom"] {
            store
                .source_checkpoint(
                    &ConversationKey {
                        source: source.into(),
                        subject: "owner/repo".into(),
                    },
                    now,
                )
                .await
                .expect("checkpoint");
        }
        store
            .set_active_source_scopes("github:", "github:github.com", &["owner/repo".into()])
            .await
            .expect("reconcile current host");
        assert_eq!(
            store
                .prune_source_checkpoints(now - time::Duration::days(1), 0)
                .await
                .expect("prune obsolete host"),
            1
        );
        for source in ["github:github.com", "api:custom"] {
            store
                .advance_source_checkpoint(
                    &ConversationKey {
                        source: source.into(),
                        subject: "owner/repo".into(),
                    },
                    now,
                )
                .await
                .expect("active provider retained");
        }
        assert!(matches!(
            store
                .advance_source_checkpoint(
                    &ConversationKey {
                        source: "github:old.example".into(),
                        subject: "owner/repo".into(),
                    },
                    now
                )
                .await,
            Err(StoreError::CheckpointMissing)
        ));
    }

    #[tokio::test]
    async fn checkpoints_survive_restart_advance_monotonically_and_are_scoped() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("store");
        let scope = ConversationKey {
            source: "github:github.com".into(),
            subject: "owner/repo".into(),
        };
        let initial = datetime!(2026-09-30 12:00 UTC);
        assert_eq!(
            store
                .source_checkpoint(&scope, initial)
                .await
                .expect("initial")
                .scanned_through,
            initial
        );
        let later = initial + time::Duration::minutes(5);
        store
            .advance_source_checkpoint(&scope, later)
            .await
            .expect("advance");
        store
            .advance_source_checkpoint(&scope, initial)
            .await
            .expect("old scan");
        drop(store);
        let store = EventStore::open(&path).await.expect("reopen");
        assert_eq!(
            store
                .source_checkpoint(&scope, initial)
                .await
                .expect("resumed"),
            SourceCheckpoint {
                scanned_through: later,
                activated_at: initial
            }
        );
        let other = ConversationKey {
            subject: "owner/other".into(),
            ..scope
        };
        assert_eq!(
            store
                .source_checkpoint(&other, initial)
                .await
                .expect("other")
                .scanned_through,
            initial
        );
        assert_eq!(
            store
                .prune_source_checkpoints(OffsetDateTime::now_utc() + time::Duration::days(1), 0,)
                .await
                .expect("active sources survive zero cap"),
            0
        );
        store
            .advance_source_checkpoint(&other, later)
            .await
            .expect("active scan advances after cleanup");
        store
            .set_active_source_scopes("github:", &other.source, &[])
            .await
            .expect("disable sources");
        assert_eq!(
            store
                .prune_source_checkpoints(initial - time::Duration::days(365), 1)
                .await
                .expect("cap"),
            1
        );
        assert_eq!(
            store
                .prune_source_checkpoints(OffsetDateTime::now_utc() + time::Duration::days(1), 1)
                .await
                .expect("age"),
            1
        );
    }
}
