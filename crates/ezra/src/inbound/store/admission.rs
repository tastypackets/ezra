use super::{EventStore, StoreError};
use crate::inbound::InboundEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueLimits {
    pub max_events: u32,
    pub max_message_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted,
    Duplicate,
    QueueFull,
    Expired,
}

impl EventStore {
    pub async fn insert(
        &self,
        event: &InboundEvent,
        limits: QueueLimits,
    ) -> Result<InsertOutcome, StoreError> {
        event.validate()?;
        let created_at_seconds = event.created_at.unix_timestamp();
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let exists = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM inbound_events
                   WHERE source = ?1 AND subject = ?2 AND event_id = ?3
               ) AS "exists!: bool""#,
            event.key.conversation.source,
            event.key.conversation.subject,
            event.key.id,
        )
        .fetch_one(&mut *transaction)
        .await?;
        if exists {
            transaction.rollback().await?;
            return Ok(InsertOutcome::Duplicate);
        }
        let usage = sqlx::query!(
            r#"SELECT COUNT(*) AS "event_count!: u64",
                      COALESCE(SUM(length(CAST(message AS BLOB)) + COALESCE(length(CAST(initial_context AS BLOB)), 0)), 0) AS "message_bytes!: u64"
               FROM inbound_events WHERE delivery_state IN ('pending', 'delivering')"#,
        )
        .fetch_one(&mut *transaction)
        .await?;
        let message_bytes =
            u64::try_from(event.message_bytes()).expect("a validated message length fits in u64");
        if usage.event_count >= u64::from(limits.max_events)
            || usage
                .message_bytes
                .checked_add(message_bytes)
                .is_none_or(|total_message_bytes| total_message_bytes > limits.max_message_bytes)
        {
            transaction.rollback().await?;
            return Ok(InsertOutcome::QueueFull);
        }
        let requested_agent = event.options.agent.as_deref();
        let requested_model = event.options.model.as_deref();
        let requested_effort = event.options.effort.as_deref();
        sqlx::query!(
            "INSERT INTO inbound_events (source, subject, event_id, actor, created_at, message, created_at_seconds, requested_agent, requested_model, requested_effort, chat_name, source_url, new_chat, initial_context)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            event.key.conversation.source,
            event.key.conversation.subject,
            event.key.id,
            event.actor,
            event.created_at,
            event.message,
            created_at_seconds, requested_agent, requested_model, requested_effort,
            event.chat_name, event.source_url, event.new_chat, event.initial_context,
        )
        .execute(&mut *transaction)
        .await?;
        sqlx::query!(
            "UPDATE inbound_sessions SET last_used_at = MAX(last_used_at, unixepoch())
             WHERE id = (
                 SELECT session_id FROM inbound_conversations WHERE source = ?1 AND subject = ?2
             )",
            event.key.conversation.source,
            event.key.conversation.subject,
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(InsertOutcome::Inserted)
    }

    pub async fn reject_event(&self, event: &InboundEvent) -> Result<(), StoreError> {
        event.validate()?;
        let created_at_seconds = event.created_at.unix_timestamp();
        sqlx::query!(
            "INSERT OR IGNORE INTO inbound_events
                (source, subject, event_id, actor, created_at, created_at_seconds, message, delivery_state, source_url)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', 'failed', ?7)",
            event.key.conversation.source,
            event.key.conversation.subject,
            event.key.id,
            event.actor,
            event.created_at,
            created_at_seconds,
            event.source_url,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
