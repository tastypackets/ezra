use time::OffsetDateTime;

use super::{EventStore, StoreError};

impl EventStore {
    /// Caps idle conversation links, expiring each session's aliases together.
    /// Native chats and workspaces are not deleted.
    pub async fn prune_sessions(
        &self,
        cutoff: OffsetDateTime,
        max_idle_conversations: u32,
    ) -> Result<u64, StoreError> {
        let cutoff_seconds = cutoff.unix_timestamp();
        let result = sqlx::query!(
            "WITH idle_sessions AS (
                 SELECT sessions.id, sessions.last_used_at,
                        (SELECT COUNT(*) FROM inbound_conversations
                         WHERE session_id = sessions.id) AS conversation_count
                 FROM inbound_sessions AS sessions
                 WHERE NOT EXISTS (
                     SELECT 1 FROM inbound_conversations AS conversations
                     JOIN inbound_events AS events
                       ON events.source = conversations.source
                      AND events.subject = conversations.subject
                     WHERE conversations.session_id = sessions.id
                       AND events.delivery_state IN ('pending', 'delivering')
                 )
             ), ranked_sessions AS (
                 SELECT id, last_used_at,
                        SUM(conversation_count) OVER (
                            ORDER BY last_used_at DESC, id DESC ROWS UNBOUNDED PRECEDING
                        ) AS conversation_count
                 FROM idle_sessions
             )
             DELETE FROM inbound_sessions WHERE id IN (
                 SELECT id FROM ranked_sessions
                 WHERE last_used_at < ?1 OR conversation_count > ?2
             )",
            cutoff_seconds,
            max_idle_conversations,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}
