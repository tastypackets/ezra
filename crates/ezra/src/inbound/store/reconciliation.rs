use super::{EventStore, StoreError};
use crate::agent::Agent;
use crate::inbound::EventKey;

impl EventStore {
    /// Call only after confirming that the native session accepted this event's message.
    pub async fn confirm_delivery(&self, key: &EventKey) -> Result<bool, StoreError> {
        let unrecorded_agent = Agent::UNRECORDED.command_name();
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let confirmed = sqlx::query_scalar!(
            r#"UPDATE inbound_events SET delivery_state = 'delivered'
               WHERE source = ?1 AND subject = ?2 AND event_id = ?3
                 AND delivery_state = 'uncertain' AND (new_chat = 0 OR new_chat_applied = 1)
               RETURNING COALESCE(requested_agent, ?4) AS "agent!: String""#,
            key.conversation.source,
            key.conversation.subject,
            key.id,
            unrecorded_agent,
        )
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(agent) = &confirmed {
            sqlx::query!(
                "UPDATE inbound_sessions SET last_used_at = MAX(last_used_at, unixepoch()), initial_context_pending = 0
                 WHERE id = (
                     SELECT session_id FROM inbound_conversations WHERE source = ?1 AND subject = ?2 AND agent = ?3
                 )",
                key.conversation.source,
                key.conversation.subject,
                agent,
            )
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(confirmed.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::store::{
        DeliveryOutcome, DeliveryState, InsertOutcome, SessionTarget, TEST_QUEUE_LIMITS,
    };
    use crate::inbound::{ConversationKey, InboundEvent};
    use time::macros::datetime;

    impl InboundEvent {
        fn reconciliation_example(subject: &str) -> Self {
            Self {
                key: EventKey {
                    conversation: ConversationKey {
                        source: "github:github.com".to_owned(),
                        subject: subject.to_owned(),
                    },
                    id: "comment-1".to_owned(),
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
    async fn attempted_delivery_never_replays_after_restart_and_does_not_block_distinct_followups()
    {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("store opens");
        let interrupted = InboundEvent::reconciliation_example("issue-1");
        let followup = InboundEvent::reconciliation_example("pull-2");
        let target = SessionTarget {
            host_id: "host-a".to_owned(),
            agent: "codex".to_owned(),
            chat_id: "chat-1".to_owned(),
            workspace: "/home/dev/projects/repository".to_owned(),
        };
        store
            .bind_conversation(&interrupted.key.conversation, &target)
            .await
            .expect("issue binds");
        store
            .link_conversation(
                &followup.key.conversation,
                &interrupted.key.conversation,
                Agent::Codex,
            )
            .await
            .expect("PR links");
        for event in [&interrupted, &followup] {
            store
                .insert(event, TEST_QUEUE_LIMITS)
                .await
                .expect("event inserts");
        }
        assert_eq!(
            store.claim_next(None).await.expect("first claims"),
            Some(interrupted.clone())
        );
        sqlx::query!("UPDATE inbound_events SET attempted_at = unixepoch() WHERE delivery_state = 'delivering'")
            .execute(&store.pool).await.expect("submission attempted");
        store.pool.close().await;
        let store = EventStore::open(&path).await.expect("store reopens");
        store
            .recover_interrupted()
            .await
            .expect("interrupted delivery recovers");
        assert_eq!(
            store
                .claim_next(None)
                .await
                .expect("distinct followup can proceed"),
            Some(followup.clone())
        );
        store
            .finish_delivery(&followup.key, DeliveryOutcome::Delivered, None)
            .await
            .expect("followup settles");
        assert!(
            store
                .confirm_delivery(&interrupted.key)
                .await
                .expect("receipt confirmed")
        );
        assert_eq!(
            store
                .delivery_state(&interrupted.key)
                .await
                .expect("confirmed state"),
            Some(DeliveryState::Delivered)
        );
        assert_eq!(
            store
                .insert(&interrupted, TEST_QUEUE_LIMITS)
                .await
                .expect("redelivery deduplicated"),
            InsertOutcome::Duplicate
        );
        store.pool.close().await;
        let store = EventStore::open(&path)
            .await
            .expect("confirmed store reopens");
        assert_eq!(
            store
                .claim_next(None)
                .await
                .expect("consumed messages never replay"),
            None
        );
        assert!(
            !store
                .confirm_delivery(&interrupted.key)
                .await
                .expect("confirmation is idempotent")
        );
    }

    #[tokio::test]
    async fn confirmation_ignores_other_states_and_keeps_the_idle_link_recent() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store opens");
        let event = InboundEvent::reconciliation_example("issue-1");
        assert!(
            !store
                .confirm_delivery(&event.key)
                .await
                .expect("missing event ignored")
        );
        store
            .insert(&event, TEST_QUEUE_LIMITS)
            .await
            .expect("event inserts");
        assert!(
            !store
                .confirm_delivery(&event.key)
                .await
                .expect("pending event ignored")
        );
        assert_eq!(
            store
                .delivery_state(&event.key)
                .await
                .expect("pending state"),
            Some(DeliveryState::Pending)
        );
        store.claim_next(None).await.expect("event claims");
        assert!(
            !store
                .confirm_delivery(&event.key)
                .await
                .expect("active delivery ignored")
        );
        assert_eq!(
            store
                .delivery_state(&event.key)
                .await
                .expect("active state"),
            Some(DeliveryState::Delivering)
        );
        store
            .bind_conversation(
                &event.key.conversation,
                &SessionTarget {
                    host_id: "host-a".to_owned(),
                    agent: "codex".to_owned(),
                    chat_id: "chat-1".to_owned(),
                    workspace: "/home/dev/projects/repository".to_owned(),
                },
            )
            .await
            .expect("session binds");
        store
            .finish_delivery(&event.key, DeliveryOutcome::Uncertain, None)
            .await
            .expect("uncertainty recorded");
        sqlx::query!("UPDATE inbound_sessions SET last_used_at = ?1", 0_i64)
            .execute(&store.pool)
            .await
            .expect("session backdated");
        assert!(
            store
                .confirm_delivery(&event.key)
                .await
                .expect("receipt confirmed")
        );
        assert_eq!(
            store
                .prune_sessions(datetime!(2026-01-01 00:00 UTC), u32::MAX)
                .await
                .expect("recent session retained"),
            0
        );
        assert!(
            store
                .find_binding(&event.key.conversation, Agent::Codex)
                .await
                .expect("binding lookup")
                .is_some()
        );
    }

    #[tokio::test]
    async fn concurrent_confirmations_only_apply_once() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let first_store = EventStore::open(&path).await.expect("first store opens");
        let second_store = EventStore::open(&path).await.expect("second store opens");
        let event = InboundEvent::reconciliation_example("issue-1");
        first_store
            .insert(&event, TEST_QUEUE_LIMITS)
            .await
            .expect("event inserts");
        first_store.claim_next(None).await.expect("event claims");
        first_store
            .finish_delivery(&event.key, DeliveryOutcome::Uncertain, None)
            .await
            .expect("uncertainty recorded");
        let (first_result, second_result) = tokio::join!(
            first_store.confirm_delivery(&event.key),
            second_store.confirm_delivery(&event.key)
        );
        assert_ne!(
            first_result.expect("first confirmation"),
            second_result.expect("second confirmation")
        );
        assert_eq!(
            first_store
                .delivery_state(&event.key)
                .await
                .expect("final state"),
            Some(DeliveryState::Delivered)
        );
    }
}
