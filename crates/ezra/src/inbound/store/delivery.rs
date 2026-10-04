use time::OffsetDateTime;

use super::{EventStore, StoreError};
use crate::agent::Agent;
use crate::inbound::{ConversationKey, EventKey, InboundEvent, Shortcut};

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
        let unrecorded_agent = Agent::UNRECORDED.command_name();
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
                     AND (?2 IS NULL OR COALESCE(pending.requested_agent, ?3) = ?2)
                     AND NOT EXISTS (
                         SELECT 1 FROM inbound_events AS unresolved
                         WHERE unresolved.source = pending.source
                           AND unresolved.subject = pending.subject
                           AND (unresolved.delivery_state = 'delivering'
                                OR (unresolved.delivery_state = 'pending' AND unresolved.rowid < pending.rowid))
                     )
                     AND NOT EXISTS (
                         SELECT 1 FROM inbound_conversations AS destination
                         JOIN inbound_sessions AS shared ON shared.id = destination.session_id
                         JOIN inbound_conversations AS related
                           ON related.session_id = destination.session_id
                         JOIN inbound_events AS unresolved
                           ON unresolved.source = related.source
                          AND unresolved.subject = related.subject
                         WHERE destination.source = pending.source
                           AND destination.subject = pending.subject
                           AND (unresolved.delivery_state = 'delivering'
                                OR (unresolved.delivery_state = 'pending' AND unresolved.rowid < pending.rowid
                                    AND COALESCE(unresolved.requested_agent, ?3) = shared.agent))
                     )
                   ORDER BY pending.received_at, pending.rowid LIMIT 1
               )
               RETURNING source, subject, event_id, actor,
                         created_at AS "created_at: OffsetDateTime", message, initial_context, requested_agent, requested_model, requested_effort, chat_name, source_url, new_chat AS "new_chat!: bool""#,
            host_id,
            agent,
            unrecorded_agent,
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(record) = record else {
            return Ok(None);
        };
        Ok(Some(InboundEvent {
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
            options: Shortcut::stored(
                record.requested_agent,
                record.requested_model,
                record.requested_effort,
            )?,
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
        let unrecorded_agent = Agent::UNRECORDED.command_name();
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
               AND COALESCE(pending.requested_agent, ?6) = ?5
               AND NOT EXISTS (
                   SELECT 1 FROM inbound_conversations AS destination
                   JOIN inbound_sessions AS shared ON shared.id = destination.session_id
                   JOIN inbound_conversations AS related ON related.session_id = destination.session_id
                   JOIN inbound_events AS older ON older.source = related.source AND older.subject = related.subject
                   WHERE destination.source = pending.source AND destination.subject = pending.subject
                     AND (older.delivery_state = 'delivering'
                          OR (older.delivery_state = 'pending' AND older.rowid < pending.rowid
                              AND (COALESCE(older.requested_agent, ?6) = shared.agent
                                   OR (older.source = pending.source AND older.subject = pending.subject))))
               )",
            key.conversation.source, key.conversation.subject, key.id, scope.host_id, scope.agent, unrecorded_agent,
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

    pub async fn waiting_events(&self, source: &str) -> Result<Vec<(EventKey, Agent)>, StoreError> {
        // The redundant state list lets SQLite use a partial index of queued requests.
        let records = sqlx::query!(
            "SELECT subject, event_id, requested_agent FROM inbound_events
             WHERE source = ?1 AND delivery_state IN ('pending', 'delivering')
               AND delivery_state = 'pending' AND attempted_at IS NULL ORDER BY rowid",
            source,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(records
            .into_iter()
            .filter_map(|record| {
                let key = EventKey {
                    conversation: ConversationKey {
                        source: source.to_owned(),
                        subject: record.subject,
                    },
                    id: record.event_id,
                };
                match Agent::stored(record.requested_agent.as_deref()) {
                    Ok(agent) => Some((key, agent)),
                    Err(error) => {
                        tracing::warn!(delivery_id = %key.delivery_id(), %error, "waiting inbound request skipped");
                        None
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::store::{InsertOutcome, TEST_QUEUE_LIMITS};
    use time::macros::datetime;

    #[tokio::test]
    async fn a_fresh_chat_request_is_persistent_and_holds_only_later_messages() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let database = directory.path().join("ezra.db");
        let store = EventStore::open(&database).await.expect("store");
        let older = InboundEvent::delivery_example("issue-1", "older");
        let mut reset = InboundEvent::delivery_example("issue-1", "reset");
        reset.new_chat = true;
        let later = InboundEvent::delivery_example("issue-1", "later");
        let unrelated = InboundEvent::delivery_example("issue-2", "unrelated");
        for event in [&older, &reset, &later, &unrelated] {
            store
                .insert(event, TEST_QUEUE_LIMITS)
                .await
                .expect("insert");
        }
        reset.new_chat = false;
        assert_eq!(
            store
                .insert(&reset, TEST_QUEUE_LIMITS)
                .await
                .expect("duplicate"),
            InsertOutcome::Duplicate
        );
        store.pool.close().await;
        let store = EventStore::open(&database).await.expect("reopen");
        assert!(
            store
                .get(&reset.key)
                .await
                .expect("request")
                .expect("saved")
                .new_chat
        );
        assert_eq!(
            store.claim_next(None).await.expect("older claim"),
            Some(older.clone())
        );
        store
            .finish_delivery(&older.key, DeliveryOutcome::Delivered, None)
            .await
            .expect("older finishes");
        assert_eq!(
            store.claim_next(None).await.expect("unrelated claim"),
            Some(unrelated)
        );
        assert!(
            store
                .claim_next(None)
                .await
                .expect("reset barrier")
                .is_none()
        );
        assert_eq!(
            store.delivery_state(&reset.key).await.expect("state"),
            Some(DeliveryState::Pending)
        );
        assert_eq!(
            store.delivery_state(&later.key).await.expect("state"),
            Some(DeliveryState::Pending)
        );
    }

    #[tokio::test]
    async fn delivered_names_are_bounded_and_set_only_with_a_finished_claim() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store");
        let event = InboundEvent::delivery_example("issue-1", "comment-1");
        store
            .insert(&event, TEST_QUEUE_LIMITS)
            .await
            .expect("insert");
        let name = format!("{}🦦", "x".repeat(510));
        assert!(
            !store
                .finish_delivery(&event.key, DeliveryOutcome::Delivered, Some(&name))
                .await
                .expect("unclaimed")
        );
        assert_eq!(
            store.delivered_chat_name(&event.key).await.expect("name"),
            None
        );
        store.claim_next(None).await.expect("claim");
        assert!(
            store
                .finish_delivery(&event.key, DeliveryOutcome::Delivered, Some(&name))
                .await
                .expect("finish")
        );
        assert_eq!(
            store
                .delivered_chat_name(&event.key)
                .await
                .expect("bounded name"),
            Some("x".repeat(510))
        );
        assert!(
            !store
                .finish_delivery(&event.key, DeliveryOutcome::Delivered, Some("overwrite"))
                .await
                .expect("already finished")
        );
        assert_eq!(
            store
                .delivered_chat_name(&event.key)
                .await
                .expect("retained name"),
            Some("x".repeat(510))
        );
    }

    impl InboundEvent {
        fn delivery_example(subject: &str, event_id: &str) -> Self {
            Self {
                key: EventKey {
                    conversation: ConversationKey {
                        source: "github:github.com".to_owned(),
                        subject: subject.to_owned(),
                    },
                    id: event_id.to_owned(),
                },
                new_chat: false,
                options: Default::default(),
                chat_name: None,
                source_url: None,
                initial_context: None,
                actor: "author".to_owned(),
                created_at: datetime!(2026-09-30 12:00 UTC),
                message: "Continue this work".to_owned(),
            }
        }
    }

    #[tokio::test]
    async fn unattempted_requests_survive_restart_and_serialize_only_their_chat() {
        let directory = tempfile::tempdir().expect("directory");
        let database = directory.path().join("ezra.db");
        let store = EventStore::open(&database).await.expect("store");
        let oldest = InboundEvent::delivery_example("issue-1", "first");
        let linked = InboundEvent::delivery_example("pull-2", "second");
        let unrelated = InboundEvent::delivery_example("issue-3", "third");
        let target = super::super::SessionTarget {
            host_id: "host-a".into(),
            agent: "codex".into(),
            chat_id: "shared".into(),
            workspace: "/repo".into(),
        };
        store
            .bind_conversation(&oldest.key.conversation, &target)
            .await
            .expect("bind");
        store
            .link_conversation(&linked.key.conversation, &oldest.key.conversation)
            .await
            .expect("link");
        store
            .bind_conversation(
                &unrelated.key.conversation,
                &super::super::SessionTarget {
                    chat_id: "independent".into(),
                    ..target
                },
            )
            .await
            .expect("independent binding");
        for event in [&oldest, &linked, &unrelated] {
            store
                .insert(event, TEST_QUEUE_LIMITS)
                .await
                .expect("insert");
        }
        let scope = DeliveryScope {
            host_id: "host-a",
            agent: "codex",
        };
        assert_eq!(
            store.claim_event(&oldest.key, scope).await.expect("claim"),
            Some(oldest.clone())
        );
        store
            .finish_delivery(&oldest.key, DeliveryOutcome::Pending, None)
            .await
            .expect("offline");
        store.pool.close().await;
        let store = EventStore::open(&database).await.expect("reopen");
        assert_eq!(
            store
                .waiting_events("github:github.com")
                .await
                .expect("recovery"),
            [
                (oldest.key.clone(), Agent::Codex),
                (linked.key.clone(), Agent::Codex),
                (unrelated.key.clone(), Agent::Codex)
            ]
        );
        assert_eq!(
            store
                .claim_event(&linked.key, scope)
                .await
                .expect("ordered"),
            None
        );
        assert_eq!(
            store
                .claim_event(&unrelated.key, scope)
                .await
                .expect("independent"),
            Some(unrelated.clone())
        );
        store
            .finish_delivery(&unrelated.key, DeliveryOutcome::Delivered, None)
            .await
            .expect("finish independent");
        assert_eq!(
            store
                .claim_event(&oldest.key, scope)
                .await
                .expect("resumed"),
            Some(oldest.clone())
        );
        store
            .finish_delivery(&oldest.key, DeliveryOutcome::Delivered, None)
            .await
            .expect("finish first");
        assert_eq!(
            store
                .claim_event(&linked.key, scope)
                .await
                .expect("followup"),
            Some(linked)
        );
    }

    #[tokio::test]
    async fn concurrent_claims_deliver_a_conversation_in_order() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let first_store = EventStore::open(&path).await.expect("first store opens");
        let second_store = EventStore::open(&path).await.expect("second store opens");
        let first_event = InboundEvent::delivery_example("issue-1", "comment-1");
        let followup_event = InboundEvent::delivery_example("issue-1", "comment-2");
        first_store
            .insert(&first_event, TEST_QUEUE_LIMITS)
            .await
            .expect("first event inserts");
        first_store
            .insert(&followup_event, TEST_QUEUE_LIMITS)
            .await
            .expect("follow-up inserts");

        let (first_claim, second_claim) =
            tokio::join!(first_store.claim_next(None), second_store.claim_next(None));
        let claims: Vec<_> = [
            first_claim.expect("first claim succeeds"),
            second_claim.expect("second claim succeeds"),
        ]
        .into_iter()
        .flatten()
        .collect();
        assert_eq!(claims, vec![first_event.clone()]);
        assert!(
            !first_store
                .finish_delivery(&followup_event.key, DeliveryOutcome::Delivered, None)
                .await
                .expect("pending event cannot finish")
        );
        assert!(
            first_store
                .finish_delivery(&first_event.key, DeliveryOutcome::Delivered, None)
                .await
                .expect("claimed event finishes")
        );
        assert_eq!(
            second_store
                .claim_next(None)
                .await
                .expect("follow-up claims"),
            Some(followup_event)
        );
        assert_eq!(
            first_store
                .delivery_state(&first_event.key)
                .await
                .expect("state reads"),
            Some(DeliveryState::Delivered)
        );
        assert_eq!(
            first_store
                .insert(&first_event, TEST_QUEUE_LIMITS)
                .await
                .expect("duplicate is handled"),
            InsertOutcome::Duplicate
        );
        assert_eq!(
            first_store
                .delivery_state(&first_event.key)
                .await
                .expect("duplicate keeps state"),
            Some(DeliveryState::Delivered)
        );
    }

    #[tokio::test]
    async fn restart_never_replays_attempted_messages_and_allows_followups() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("store opens");
        let interrupted_event = InboundEvent::delivery_example("issue-1", "comment-1");
        let followup_event = InboundEvent::delivery_example("issue-1", "comment-2");
        let independent_event = InboundEvent::delivery_example("issue-2", "comment-3");
        for event in [&interrupted_event, &followup_event, &independent_event] {
            store
                .insert(event, TEST_QUEUE_LIMITS)
                .await
                .expect("event inserts");
        }
        assert_eq!(
            store.claim_next(None).await.expect("event claims"),
            Some(interrupted_event.clone())
        );
        sqlx::query(
            "UPDATE inbound_events SET attempted_at = unixepoch() WHERE event_id = 'comment-1'",
        )
        .execute(&store.pool)
        .await
        .expect("native submission was attempted");
        store.pool.close().await;

        let store = EventStore::open(&path).await.expect("store reopens");
        assert_eq!(
            store
                .recover_interrupted()
                .await
                .expect("recovery succeeds"),
            1
        );
        assert_eq!(
            store
                .recover_interrupted()
                .await
                .expect("repeated recovery succeeds"),
            0
        );
        assert_eq!(
            store
                .delivery_state(&interrupted_event.key)
                .await
                .expect("state reads"),
            Some(DeliveryState::Uncertain)
        );
        assert!(
            !store
                .finish_delivery(&interrupted_event.key, DeliveryOutcome::Delivered, None)
                .await
                .expect("recovered event cannot finish")
        );
        assert_eq!(
            store.claim_next(None).await.expect("followup proceeds"),
            Some(followup_event.clone())
        );
        store
            .finish_delivery(&followup_event.key, DeliveryOutcome::Delivered, None)
            .await
            .expect("followup delivered");
        assert_eq!(
            store
                .claim_next(None)
                .await
                .expect("other conversation claims"),
            Some(independent_event.clone())
        );
        assert!(
            store
                .finish_delivery(&independent_event.key, DeliveryOutcome::Uncertain, None)
                .await
                .expect("uncertain outcome persists")
        );
        assert_eq!(
            store.claim_next(None).await.expect("no eligible events"),
            None
        );
        assert_eq!(
            store
                .delivery_state(&followup_event.key)
                .await
                .expect("follow-up delivered"),
            Some(DeliveryState::Delivered)
        );
    }
}
