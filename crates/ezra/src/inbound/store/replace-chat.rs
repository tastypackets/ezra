use super::{EventStore, SessionTarget, StoreError};
use crate::inbound::EventKey;

impl EventStore {
    /// Replace only after confirmed rejection, before sending to the new chat.
    pub async fn replace_chat_for_delivery(
        &self,
        event: &EventKey,
        expected: &SessionTarget,
        replacement_chat_id: &str,
    ) -> Result<(), StoreError> {
        let replacement = SessionTarget {
            chat_id: replacement_chat_id.to_owned(),
            ..expected.clone()
        };
        replacement.validate_binding(&event.conversation)?;
        let result = sqlx::query!(
            "UPDATE inbound_sessions
             SET chat_id = ?1, last_used_at = MAX(last_used_at, unixepoch()), initial_context_pending = 1
             WHERE host_id = ?2 AND agent = ?3 AND chat_id = ?4 AND workspace = ?5
               AND id = (
                   SELECT conversation.session_id FROM inbound_conversations AS conversation
                   JOIN inbound_events AS event
                     ON event.source = conversation.source AND event.subject = conversation.subject
                   WHERE event.source = ?6 AND event.subject = ?7 AND event.event_id = ?8
                     AND event.delivery_state = 'delivering'
               )
               AND NOT EXISTS (
                   SELECT 1 FROM inbound_conversations AS related
                   JOIN inbound_events AS unresolved
                     ON unresolved.source = related.source AND unresolved.subject = related.subject
                   WHERE related.session_id = inbound_sessions.id
                     AND unresolved.delivery_state = 'delivering'
                     AND NOT (unresolved.source = ?6 AND unresolved.subject = ?7 AND unresolved.event_id = ?8)
               )",
            replacement_chat_id,
            expected.host_id,
            expected.agent,
            expected.chat_id,
            expected.workspace,
            event.conversation.source,
            event.conversation.subject,
            event.id,
        )
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(StoreError::DeliveryChanged);
        }
        Ok(())
    }
}
