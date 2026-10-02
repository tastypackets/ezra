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
