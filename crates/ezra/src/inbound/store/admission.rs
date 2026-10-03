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
        let requested_agent = event.options.agent.command_name();
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
                 SELECT session_id FROM inbound_conversations WHERE source = ?1 AND subject = ?2 AND agent = ?3
             )",
            event.key.conversation.source,
            event.key.conversation.subject,
            requested_agent,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::store::{DeliveryOutcome, TEST_QUEUE_LIMITS};
    use crate::inbound::{ConversationKey, EventKey};
    use time::macros::datetime;

    impl InboundEvent {
        fn admission_example(event_id: &str, message: &str) -> Self {
            Self {
                key: EventKey {
                    conversation: ConversationKey {
                        source: "github:github.com".to_owned(),
                        subject: "issue-1".to_owned(),
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
                message: message.to_owned(),
            }
        }
    }

    #[tokio::test]
    async fn concurrent_inserts_cannot_exceed_either_limit() {
        for limits in [
            QueueLimits {
                max_events: 1,
                ..TEST_QUEUE_LIMITS
            },
            QueueLimits {
                max_message_bytes: 6,
                ..TEST_QUEUE_LIMITS
            },
        ] {
            let directory = tempfile::tempdir().expect("temporary directory");
            let path = directory.path().join("ezra.db");
            let first_store = EventStore::open(&path).await.expect("first store opens");
            let second_store = EventStore::open(&path).await.expect("second store opens");
            let first_event = InboundEvent::admission_example("comment-1", "First");
            let mut second_event = InboundEvent::admission_example("comment-2", "Second");
            second_event.key.conversation.source = "api:local".to_owned();
            let (first_outcome, second_outcome) = tokio::join!(
                first_store.insert(&first_event, limits),
                second_store.insert(&second_event, limits),
            );
            let outcomes = [
                first_outcome.expect("first insert succeeds"),
                second_outcome.expect("second insert succeeds"),
            ];
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|outcome| **outcome == InsertOutcome::Inserted)
                    .count(),
                1
            );
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|outcome| **outcome == InsertOutcome::QueueFull)
                    .count(),
                1
            );
            let stored_events = [
                first_store
                    .get(&first_event.key)
                    .await
                    .expect("first lookup succeeds"),
                second_store
                    .get(&second_event.key)
                    .await
                    .expect("second lookup succeeds"),
            ];
            assert_eq!(stored_events.iter().flatten().count(), 1);
        }
    }

    #[tokio::test]
    async fn byte_limit_counts_utf8_and_nul_and_delivery_frees_capacity() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store opens");
        let first_event = InboundEvent::admission_example("comment-1", "🦦\0");
        let second_event = InboundEvent::admission_example("comment-2", "Next");
        let limits = QueueLimits {
            max_message_bytes: 5,
            ..TEST_QUEUE_LIMITS
        };
        assert_eq!(
            store
                .insert(
                    &first_event,
                    QueueLimits {
                        max_message_bytes: 4,
                        ..limits
                    }
                )
                .await
                .expect("oversized event is rejected"),
            InsertOutcome::QueueFull,
        );
        assert_eq!(
            store
                .insert(&first_event, limits)
                .await
                .expect("exact byte limit fits"),
            InsertOutcome::Inserted
        );
        assert_eq!(
            store
                .insert(&second_event, limits)
                .await
                .expect("full queue rejects"),
            InsertOutcome::QueueFull
        );
        let disabled_limits = QueueLimits {
            max_events: 0,
            max_message_bytes: 0,
        };
        assert_eq!(
            store
                .insert(&first_event, disabled_limits)
                .await
                .expect("duplicate ignores lowered limits"),
            InsertOutcome::Duplicate
        );
        store
            .claim_next(None)
            .await
            .expect("event claims")
            .expect("event exists");
        assert_eq!(
            store
                .insert(&second_event, limits)
                .await
                .expect("delivering counts toward capacity"),
            InsertOutcome::QueueFull
        );
        assert!(
            store
                .finish_delivery(&first_event.key, DeliveryOutcome::Delivered, None)
                .await
                .expect("delivery finishes")
        );
        assert_eq!(
            store
                .insert(&second_event, disabled_limits)
                .await
                .expect("zero capacity rejects new events"),
            InsertOutcome::QueueFull,
        );
        assert_eq!(
            store
                .insert(&second_event, limits)
                .await
                .expect("delivery frees capacity"),
            InsertOutcome::Inserted
        );
        assert_eq!(
            store
                .get(&first_event.key)
                .await
                .expect("delivered history remains"),
            Some(first_event)
        );
    }

    #[tokio::test]
    async fn uncertain_events_release_capacity_after_reopening() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("store opens");
        let first_event = InboundEvent::admission_example("comment-1", "First");
        let second_event = InboundEvent::admission_example("comment-2", "Second");
        let limits = QueueLimits {
            max_events: 1,
            ..TEST_QUEUE_LIMITS
        };
        store
            .insert(&first_event, limits)
            .await
            .expect("event inserts");
        store
            .claim_next(None)
            .await
            .expect("event claims")
            .expect("event exists");
        sqlx::query("UPDATE inbound_events SET attempted_at = unixepoch()")
            .execute(&store.pool)
            .await
            .expect("native submission was attempted before interruption");
        store.pool.close().await;
        let store = EventStore::open(&path).await.expect("store reopens");
        store
            .recover_interrupted()
            .await
            .expect("recovery succeeds");
        assert_eq!(
            store
                .insert(&second_event, limits)
                .await
                .expect("uncertain event releases capacity"),
            InsertOutcome::Inserted
        );
        assert_eq!(
            store
                .get(&second_event.key)
                .await
                .expect("new event lookup"),
            Some(second_event)
        );
    }

    #[tokio::test]
    async fn rejected_events_keep_dedupe_without_consuming_waiting_capacity() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("store opens");
        let rejected = InboundEvent::admission_example("rejected", "large request");
        store
            .reject_event(&rejected)
            .await
            .expect("rejection records");
        drop(store);
        let store = EventStore::open(&path).await.expect("store reopens");
        assert_eq!(
            store
                .insert(&rejected, TEST_QUEUE_LIMITS)
                .await
                .expect("dedupe"),
            InsertOutcome::Duplicate
        );
        let next = InboundEvent::admission_example("next", "request");
        assert_eq!(
            store
                .insert(
                    &next,
                    QueueLimits {
                        max_events: 1,
                        ..TEST_QUEUE_LIMITS
                    }
                )
                .await
                .expect("capacity remains"),
            InsertOutcome::Inserted
        );
        assert_eq!(
            store
                .get(&rejected.key)
                .await
                .expect("audit")
                .expect("record")
                .message,
            ""
        );
    }

    #[tokio::test]
    async fn newly_received_old_comments_are_not_rejected_by_history_cutoff() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store opens");
        store
            .prune_history(
                time::OffsetDateTime::now_utc(),
                super::super::HistoryLimits {
                    max_events: 100,
                    max_message_bytes: 1024 * 1024,
                },
            )
            .await
            .expect("history cleanup succeeds");
        let mut event = InboundEvent::admission_example("old-comment-new-trigger", "new request");
        event.created_at = time::OffsetDateTime::UNIX_EPOCH;
        assert_eq!(
            store
                .insert(&event, TEST_QUEUE_LIMITS)
                .await
                .expect("fresh admission"),
            InsertOutcome::Inserted
        );
    }
    #[tokio::test]
    async fn requested_options_survive_restart_and_delivery_claims() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("store");
        let mut event = InboundEvent::admission_example("options", "request");
        event.options = crate::inbound::Shortcut {
            agent: crate::agent::Agent::Claude,
            model: Some("future-model".to_owned()),
            effort: Some("future-effort".to_owned()),
        };
        event.initial_context = Some("\n\nOriginal description 🦦".to_owned());
        store
            .insert(&event, TEST_QUEUE_LIMITS)
            .await
            .expect("insert");
        drop(store);
        let store = EventStore::open(&path).await.expect("reopen");
        assert_eq!(
            store.get(&event.key).await.expect("read"),
            Some(event.clone())
        );
        assert_eq!(store.claim_next(None).await.expect("claim"), Some(event));
    }

    #[tokio::test]
    async fn queue_capacity_counts_initial_context_bytes() {
        let directory = tempfile::tempdir().expect("directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store");
        let mut event = InboundEvent::admission_example("context", "x");
        event.initial_context = Some("🦦".to_owned());
        let limits = QueueLimits {
            max_message_bytes: 5,
            ..TEST_QUEUE_LIMITS
        };
        assert_eq!(
            store
                .insert(
                    &event,
                    QueueLimits {
                        max_message_bytes: 4,
                        ..limits
                    }
                )
                .await
                .expect("reject"),
            InsertOutcome::QueueFull
        );
        assert_eq!(
            store.insert(&event, limits).await.expect("insert"),
            InsertOutcome::Inserted
        );
        let followup = InboundEvent::admission_example("followup", "x");
        assert_eq!(
            store.insert(&followup, limits).await.expect("full"),
            InsertOutcome::QueueFull
        );
    }
}
