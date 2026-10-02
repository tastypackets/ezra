use super::{EventStore, SessionTarget, StoreError};
use crate::inbound::ConversationKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasOutcome {
    Created,
    AlreadyBound,
    MissingDestination,
    UnresolvedDelivery,
}

impl EventStore {
    pub async fn link_conversation(
        &self,
        conversation: &ConversationKey,
        destination: &ConversationKey,
    ) -> Result<AliasOutcome, StoreError> {
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let target = sqlx::query_as!(
            SessionTarget,
            "SELECT sessions.host_id, sessions.agent, sessions.chat_id, sessions.workspace
             FROM inbound_conversations AS conversations
             JOIN inbound_sessions AS sessions ON sessions.id = conversations.session_id
             WHERE conversations.source = ?1 AND conversations.subject = ?2",
            destination.source,
            destination.subject,
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(target) = target else {
            transaction.rollback().await?;
            return Ok(AliasOutcome::MissingDestination);
        };
        target.validate_binding(conversation)?;
        let already_bound = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM inbound_conversations WHERE source = ?1 AND subject = ?2
               ) AS "already_bound!: bool""#,
            conversation.source,
            conversation.subject,
        )
        .fetch_one(&mut *transaction)
        .await?;
        if already_bound {
            transaction.rollback().await?;
            return Ok(AliasOutcome::AlreadyBound);
        }
        let unresolved = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM inbound_events
                   WHERE source = ?1 AND subject = ?2
                     AND delivery_state IN ('delivering', 'uncertain')
               ) AS "unresolved!: bool""#,
            conversation.source,
            conversation.subject,
        )
        .fetch_one(&mut *transaction)
        .await?;
        if unresolved {
            transaction.rollback().await?;
            return Ok(AliasOutcome::UnresolvedDelivery);
        }
        sqlx::query!(
            "INSERT INTO inbound_conversations (source, subject, session_id)
             SELECT ?1, ?2, session_id FROM inbound_conversations
             WHERE source = ?3 AND subject = ?4",
            conversation.source,
            conversation.subject,
            destination.source,
            destination.subject,
        )
        .execute(&mut *transaction)
        .await?;
        sqlx::query!(
            "UPDATE inbound_sessions SET last_used_at = MAX(last_used_at, unixepoch())
             WHERE id = (
                 SELECT session_id FROM inbound_conversations WHERE source = ?1 AND subject = ?2
             )",
            conversation.source,
            conversation.subject,
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(AliasOutcome::Created)
    }
}
