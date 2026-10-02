use super::{DeliveryScope, DeliveryState, EventStore, SessionTarget, StoreError};
use crate::inbound::{ConversationKey, EventKey};

#[derive(Debug, PartialEq, Eq)]
pub enum RoutingOutcome {
    Ready,
    Deferred,
    Uncertain,
    Create { workspace: String },
}

impl EventStore {
    pub async fn pending_routing(
        &self,
        source: &str,
        subject_prefix: &str,
    ) -> Result<Vec<crate::inbound::InboundEvent>, StoreError> {
        let records = sqlx::query!(
            r#"SELECT events.subject, events.event_id, events.actor,
                      events.created_at AS "created_at: time::OffsetDateTime", events.message, events.initial_context,
                      events.requested_agent, events.requested_model, events.requested_effort,
                      events.chat_name, events.source_url, events.new_chat AS "new_chat!: bool"
               FROM inbound_events AS events
               WHERE events.delivery_state = 'pending' AND events.source = ?1
                 AND substr(events.subject, 1, length(?2)) = ?2
                 AND ((events.new_chat = 1 AND events.new_chat_applied = 0) OR NOT EXISTS (
                     SELECT 1 FROM inbound_conversations AS conversations
                     WHERE conversations.source = events.source AND conversations.subject = events.subject
                 ))
               ORDER BY events.rowid"#,
            source, subject_prefix,
        ).fetch_all(&self.pool).await?;
        Ok(records
            .into_iter()
            .map(|record| crate::inbound::InboundEvent {
                key: EventKey {
                    conversation: ConversationKey {
                        source: source.to_owned(),
                        subject: record.subject,
                    },
                    id: record.event_id,
                },
                new_chat: record.new_chat,
                options: crate::inbound::Shortcut {
                    agent: record.requested_agent,
                    model: record.requested_model,
                    effort: record.requested_effort,
                },
                actor: record.actor,
                created_at: record.created_at,
                message: record.message,
                initial_context: record.initial_context,
                chat_name: record.chat_name,
                source_url: record.source_url,
            })
            .collect())
    }

    pub async fn claim_routing(
        &self,
        key: &EventKey,
        related: &[ConversationKey],
        scope: DeliveryScope<'_>,
        workspace: &str,
    ) -> Result<RoutingOutcome, StoreError> {
        let related = serde_json::to_string(related).expect("conversation keys serialize");
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let event = sqlx::query!(
            r#"SELECT rowid AS "rowid!: i64", new_chat AS "new_chat!: bool",
                      new_chat_applied AS "new_chat_applied!: bool",
                      delivery_state AS "delivery_state: DeliveryState"
               FROM inbound_events WHERE source = ?1 AND subject = ?2 AND event_id = ?3"#,
            key.conversation.source,
            key.conversation.subject,
            key.id,
        )
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(StoreError::DeliveryChanged)?;
        if event.delivery_state == DeliveryState::Delivered {
            transaction.rollback().await?;
            return Ok(RoutingOutcome::Ready);
        }
        if event.delivery_state == DeliveryState::Uncertain {
            transaction.rollback().await?;
            return Ok(RoutingOutcome::Uncertain);
        }
        if event.delivery_state == DeliveryState::Delivering {
            transaction.rollback().await?;
            return Ok(RoutingOutcome::Deferred);
        }
        let existing = sqlx::query_as!(
            SessionTarget,
            "SELECT sessions.host_id, sessions.agent, sessions.chat_id, sessions.workspace
             FROM inbound_conversations AS conversations
             JOIN inbound_sessions AS sessions ON sessions.id = conversations.session_id
             WHERE conversations.source = ?1 AND conversations.subject = ?2",
            key.conversation.source,
            key.conversation.subject,
        )
        .fetch_optional(&mut *transaction)
        .await?;
        if existing.is_some() && (!event.new_chat || event.new_chat_applied) {
            transaction.rollback().await?;
            return Ok(RoutingOutcome::Ready);
        }
        let blocked = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                SELECT 1 FROM inbound_events AS older
                WHERE older.source = ?1 AND older.subject = ?2 AND older.rowid < ?3
                  AND older.delivery_state IN ('pending', 'delivering')
            ) AS "blocked!: bool""#,
            key.conversation.source,
            key.conversation.subject,
            event.rowid,
        )
        .fetch_one(&mut *transaction)
        .await?;
        if blocked {
            transaction.rollback().await?;
            return Ok(RoutingOutcome::Deferred);
        }
        if event.new_chat {
            if let Some(existing) = existing {
                if existing.host_id != scope.host_id || existing.agent != scope.agent {
                    transaction.rollback().await?;
                    return Ok(RoutingOutcome::Deferred);
                }
                let busy = sqlx::query_scalar!(
                    r#"SELECT EXISTS (
                        SELECT 1 FROM inbound_events AS events
                        JOIN inbound_conversations AS conversations
                          ON events.source = conversations.source AND events.subject = conversations.subject
                        JOIN inbound_sessions AS sessions ON sessions.id = conversations.session_id
                        WHERE sessions.host_id = ?1 AND sessions.agent = ?2 AND sessions.chat_id = ?3
                          AND events.delivery_state = 'delivering'
                    ) AS "busy!: bool""#,
                    existing.host_id, existing.agent, existing.chat_id,
                ).fetch_one(&mut *transaction).await?;
                if busy {
                    transaction.rollback().await?;
                    return Ok(RoutingOutcome::Deferred);
                }
            }
        } else {
            let creating_related = sqlx::query_scalar!(
                r#"SELECT EXISTS (
                    SELECT 1 FROM inbound_events AS events
                    JOIN json_each(?1) AS related
                      ON events.source = json_extract(related.value, '$.source')
                     AND events.subject = json_extract(related.value, '$.subject')
                    WHERE (events.delivery_state IN ('pending', 'delivering') AND events.new_chat = 1 AND events.new_chat_applied = 0 AND EXISTS (
                          SELECT 1 FROM inbound_conversations AS conversations
                          WHERE conversations.source = events.source AND conversations.subject = events.subject
                      ))
                       OR (events.delivery_state = 'delivering' AND NOT EXISTS (
                          SELECT 1 FROM inbound_conversations AS conversations
                          WHERE conversations.source = events.source AND conversations.subject = events.subject
                      ))
                ) AS "creating!: bool""#,
                related,
            ).fetch_one(&mut *transaction).await?;
            if creating_related {
                transaction.rollback().await?;
                return Ok(RoutingOutcome::Deferred);
            }
            let candidates = sqlx::query_as!(SessionTarget,
                "SELECT DISTINCT sessions.host_id, sessions.agent, sessions.chat_id, sessions.workspace
                 FROM inbound_conversations AS conversations
                 JOIN inbound_sessions AS sessions ON sessions.id = conversations.session_id
                 JOIN json_each(?1) AS related
                   ON conversations.source = json_extract(related.value, '$.source')
                  AND conversations.subject = json_extract(related.value, '$.subject')",
                related,
            ).fetch_all(&mut *transaction).await?;
            if let [target] = candidates.as_slice() {
                if target.host_id != scope.host_id || target.agent != scope.agent {
                    transaction.rollback().await?;
                    return Ok(RoutingOutcome::Deferred);
                }
                sqlx::query!(
                    "INSERT INTO inbound_conversations (source, subject, session_id)
                     SELECT ?1, ?2, id FROM inbound_sessions WHERE host_id = ?3 AND agent = ?4 AND chat_id = ?5",
                    key.conversation.source, key.conversation.subject, target.host_id, target.agent, target.chat_id,
                ).execute(&mut *transaction).await?;
                sqlx::query!(
                    "UPDATE inbound_sessions SET last_used_at = MAX(last_used_at, unixepoch())
                     WHERE host_id = ?1 AND agent = ?2 AND chat_id = ?3",
                    target.host_id,
                    target.agent,
                    target.chat_id,
                )
                .execute(&mut *transaction)
                .await?;
                transaction.commit().await?;
                return Ok(RoutingOutcome::Ready);
            }
        }
        sqlx::query!(
            "UPDATE inbound_events SET delivery_state = 'delivering'
             WHERE source = ?1 AND subject = ?2 AND event_id = ?3 AND delivery_state = 'pending'",
            key.conversation.source,
            key.conversation.subject,
            key.id,
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(RoutingOutcome::Create {
            workspace: workspace.to_owned(),
        })
    }

    pub async fn finish_routing(
        &self,
        key: &EventKey,
        target: &SessionTarget,
    ) -> Result<(), StoreError> {
        target.validate_binding(&key.conversation)?;
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let event = sqlx::query!(
            r#"SELECT new_chat AS "new_chat!: bool" FROM inbound_events
               WHERE source = ?1 AND subject = ?2 AND event_id = ?3 AND delivery_state = 'delivering'
                 AND (new_chat = 0 OR new_chat_applied = 0)"#,
            key.conversation.source, key.conversation.subject, key.id,
        ).fetch_optional(&mut *transaction).await?.ok_or(StoreError::DeliveryChanged)?;
        let session_id = sqlx::query_scalar!(
            "INSERT INTO inbound_sessions (host_id, agent, chat_id, workspace, initial_context_pending)
             VALUES (?1, ?2, ?3, ?4, 1) RETURNING id",
            target.host_id,
            target.agent,
            target.chat_id,
            target.workspace,
        )
        .fetch_one(&mut *transaction)
        .await?;
        if event.new_chat {
            sqlx::query!(
                "INSERT INTO inbound_conversations (source, subject, session_id) VALUES (?1, ?2, ?3)
                 ON CONFLICT (source, subject) DO UPDATE SET session_id = excluded.session_id",
                key.conversation.source, key.conversation.subject, session_id,
            )
            .execute(&mut *transaction)
            .await?;
        } else {
            sqlx::query!(
                "INSERT INTO inbound_conversations (source, subject, session_id) VALUES (?1, ?2, ?3)",
                key.conversation.source, key.conversation.subject, session_id,
            ).execute(&mut *transaction).await?;
        }
        sqlx::query!(
            "DELETE FROM inbound_sessions WHERE NOT EXISTS (
                SELECT 1 FROM inbound_conversations WHERE session_id = inbound_sessions.id
            )",
        )
        .execute(&mut *transaction)
        .await?;
        sqlx::query!(
            "UPDATE inbound_events SET delivery_state = 'pending', new_chat_applied = new_chat
             WHERE source = ?1 AND subject = ?2 AND event_id = ?3",
            key.conversation.source,
            key.conversation.subject,
            key.id,
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "routing-tests.rs"]
mod tests;
