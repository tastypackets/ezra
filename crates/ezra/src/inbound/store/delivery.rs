use time::OffsetDateTime;

use super::{EventStore, StoreError};
use crate::inbound::{ConversationKey, EventKey, InboundEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
pub enum DeliveryState {
    Pending,
    Delivering,
    Delivered,
    Uncertain,
    Failed,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
pub enum DeliveryOutcome {
    /// No native send was attempted.
    Pending,
    Delivered,
    Uncertain,
    Failed,
}

#[derive(Debug, Clone, Copy)]
pub struct DeliveryScope<'a> {
    pub host_id: &'a str,
    pub agent: &'a str,
}

impl EventStore {
    pub async fn claim_next(
        &self,
        scope: Option<DeliveryScope<'_>>,
    ) -> Result<Option<InboundEvent>, StoreError> {
        let (host_id, agent) = scope.map_or((None, None), |scope| {
            (Some(scope.host_id), Some(scope.agent))
        });
        let record = sqlx::query!(
            r#"UPDATE inbound_events SET delivery_state = 'delivering'
               WHERE rowid = (
                   SELECT pending.rowid FROM inbound_events AS pending
                   WHERE pending.delivery_state = 'pending'
                     AND (pending.new_chat = 0 OR pending.new_chat_applied = 1)
                     AND NOT EXISTS (
                         SELECT 1 FROM inbound_events AS reset
                         WHERE reset.source = pending.source AND reset.subject = pending.subject
                           AND reset.new_chat = 1 AND reset.new_chat_applied = 0
                           AND reset.delivery_state IN ('pending', 'delivering')
                           AND reset.rowid < pending.rowid
                     )
                     AND (?1 IS NULL OR EXISTS (
                         SELECT 1 FROM inbound_conversations AS conversation
                         JOIN inbound_sessions AS session ON session.id = conversation.session_id
                         WHERE conversation.source = pending.source AND conversation.subject = pending.subject
                           AND session.host_id = ?1 AND session.agent = ?2
                     ))
                     AND NOT EXISTS (
                         SELECT 1 FROM inbound_events AS unresolved
                         WHERE unresolved.source = pending.source
                           AND unresolved.subject = pending.subject
                           AND (unresolved.delivery_state = 'delivering'
                                OR (unresolved.delivery_state = 'pending' AND unresolved.rowid < pending.rowid))
                     )
                     AND NOT EXISTS (
                         SELECT 1 FROM inbound_conversations AS destination
                         JOIN inbound_conversations AS related
                           ON related.session_id = destination.session_id
                         JOIN inbound_events AS unresolved
                           ON unresolved.source = related.source
                          AND unresolved.subject = related.subject
                         WHERE destination.source = pending.source
                           AND destination.subject = pending.subject
                           AND (unresolved.delivery_state = 'delivering'
                                OR (unresolved.delivery_state = 'pending' AND unresolved.rowid < pending.rowid))
                     )
                   ORDER BY pending.received_at, pending.rowid LIMIT 1
               )
               RETURNING source, subject, event_id, actor,
                         created_at AS "created_at: OffsetDateTime", message, initial_context, requested_agent, requested_model, requested_effort, chat_name, source_url, new_chat AS "new_chat!: bool""#,
            host_id,
            agent,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(record.map(|record| InboundEvent {
            new_chat: record.new_chat,
            chat_name: record.chat_name,
            source_url: record.source_url,
            key: EventKey {
                conversation: ConversationKey {
                    source: record.source,
                    subject: record.subject,
                },
                id: record.event_id,
            },
            options: crate::inbound::Shortcut {
                agent: record.requested_agent,
                model: record.requested_model,
                effort: record.requested_effort,
            },
            actor: record.actor,
            created_at: record.created_at,
            message: record.message,
            initial_context: record.initial_context,
        }))
    }

    pub async fn claim_event(
        &self,
        key: &EventKey,
        scope: DeliveryScope<'_>,
    ) -> Result<Option<InboundEvent>, StoreError> {
        let claimed = sqlx::query!(
            "UPDATE inbound_events AS pending SET delivery_state = 'delivering'
             WHERE pending.source = ?1 AND pending.subject = ?2 AND pending.event_id = ?3
               AND pending.delivery_state = 'pending' AND pending.attempted_at IS NULL
               AND (pending.new_chat = 0 OR pending.new_chat_applied = 1)
               AND EXISTS (
                   SELECT 1 FROM inbound_conversations AS conversation
                   JOIN inbound_sessions AS session ON session.id = conversation.session_id
                   WHERE conversation.source = pending.source AND conversation.subject = pending.subject
                     AND session.host_id = ?4 AND session.agent = ?5
               )
               AND NOT EXISTS (
                   SELECT 1 FROM inbound_conversations AS destination
                   JOIN inbound_conversations AS related ON related.session_id = destination.session_id
                   JOIN inbound_events AS older ON older.source = related.source AND older.subject = related.subject
                   WHERE destination.source = pending.source AND destination.subject = pending.subject
                     AND (older.delivery_state = 'delivering'
                          OR (older.delivery_state = 'pending' AND older.rowid < pending.rowid))
               )",
            key.conversation.source, key.conversation.subject, key.id, scope.host_id, scope.agent,
        ).execute(&self.pool).await?;
        if claimed.rows_affected() == 0 {
            return Ok(None);
        }
        self.get(key).await
    }

    pub async fn event_is_waiting(
        &self,
        key: &EventKey,
        cutoff: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        let cutoff = cutoff.unix_timestamp();
        let eligible = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                SELECT 1 FROM inbound_events WHERE source = ?1 AND subject = ?2 AND event_id = ?3
                  AND delivery_state = 'pending' AND attempted_at IS NULL AND received_at > ?4
            ) AS "eligible!: bool""#,
            key.conversation.source,
            key.conversation.subject,
            key.id,
            cutoff,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(eligible)
    }

    pub async fn fail_waiting_event(&self, key: &EventKey) -> Result<bool, StoreError> {
        let result = sqlx::query!(
            "UPDATE inbound_events SET delivery_state = 'failed'
             WHERE source = ?1 AND subject = ?2 AND event_id = ?3
               AND delivery_state IN ('pending', 'delivering') AND attempted_at IS NULL",
            key.conversation.source,
            key.conversation.subject,
            key.id,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn waiting_events(&self, source: &str) -> Result<Vec<EventKey>, StoreError> {
        let records = sqlx::query!(
            "SELECT subject, event_id FROM inbound_events
             WHERE source = ?1 AND delivery_state = 'pending' AND attempted_at IS NULL ORDER BY rowid",
            source,
        ).fetch_all(&self.pool).await?;
        Ok(records
            .into_iter()
            .map(|record| EventKey {
                conversation: ConversationKey {
                    source: source.to_owned(),
                    subject: record.subject,
                },
                id: record.event_id,
            })
            .collect())
    }

    pub async fn finish_delivery(
        &self,
        key: &EventKey,
        outcome: DeliveryOutcome,
        chat_name: Option<&str>,
    ) -> Result<bool, StoreError> {
        let chat_name = chat_name.map(|name| &name[..name.floor_char_boundary(512)]);
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let result = sqlx::query!(
            "UPDATE inbound_events SET delivery_state = ?1, delivered_chat_name = ?5
             WHERE source = ?2 AND subject = ?3 AND event_id = ?4
               AND delivery_state = 'delivering'",
            outcome,
            key.conversation.source,
            key.conversation.subject,
            key.id,
            chat_name,
        )
        .execute(&mut *transaction)
        .await?;
        let finished = result.rows_affected() == 1;
        if finished {
            sqlx::query!(
                "UPDATE inbound_sessions SET last_used_at = MAX(last_used_at, unixepoch()),
                     initial_context_pending = CASE WHEN ?3 = 'delivered' THEN 0 ELSE initial_context_pending END
                 WHERE id = (
                     SELECT session_id FROM inbound_conversations WHERE source = ?1 AND subject = ?2
                 )",
                key.conversation.source,
                key.conversation.subject,
                outcome,
            )
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(finished)
    }

    pub async fn delivered_chat_name(&self, key: &EventKey) -> Result<Option<String>, StoreError> {
        let record = sqlx::query!(
            "SELECT delivered_chat_name FROM inbound_events
             WHERE source = ?1 AND subject = ?2 AND event_id = ?3",
            key.conversation.source,
            key.conversation.subject,
            key.id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(record.and_then(|record| record.delivered_chat_name))
    }

    pub async fn delivery_state(
        &self,
        key: &EventKey,
    ) -> Result<Option<DeliveryState>, StoreError> {
        let record = sqlx::query!(
            r#"SELECT delivery_state AS "delivery_state: DeliveryState" FROM inbound_events
               WHERE source = ?1 AND subject = ?2 AND event_id = ?3"#,
            key.conversation.source,
            key.conversation.subject,
            key.id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(record.map(|record| record.delivery_state))
    }

    /// Run only during exclusive startup, before any delivery workers start.
    pub async fn recover_interrupted(&self) -> Result<u64, StoreError> {
        let result = sqlx::query!(
            "UPDATE inbound_events SET delivery_state = CASE WHEN attempted_at IS NULL THEN 'pending' ELSE 'uncertain' END
             WHERE delivery_state = 'delivering'",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}
