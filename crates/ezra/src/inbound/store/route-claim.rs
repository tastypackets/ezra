use super::{EventStore, StoreError};
use crate::agent::Agent;
use crate::inbound::EventKey;

impl EventStore {
    pub async fn claim_for_binding(&self, key: &EventKey) -> Result<bool, StoreError> {
        let unrecorded_agent = Agent::UNRECORDED.command_name();
        let result = sqlx::query!(
            "UPDATE inbound_events AS claimed SET delivery_state = 'delivering'
             WHERE claimed.source = ?1 AND claimed.subject = ?2 AND claimed.event_id = ?3
               AND claimed.delivery_state = 'pending'
               AND NOT EXISTS (
                   SELECT 1 FROM inbound_conversations AS conversations
                   WHERE conversations.source = ?1 AND conversations.subject = ?2
                     AND conversations.agent = COALESCE(claimed.requested_agent, ?4)
               )
               AND NOT EXISTS (
                   SELECT 1 FROM inbound_events AS unresolved
                   WHERE unresolved.source = ?1 AND unresolved.subject = ?2
                     AND unresolved.delivery_state = 'delivering'
                     AND COALESCE(unresolved.requested_agent, ?4) = COALESCE(claimed.requested_agent, ?4)
               )",
            key.conversation.source,
            key.conversation.subject,
            key.id,
            unrecorded_agent,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::store::{DeliveryOutcome, DeliveryState, TEST_QUEUE_LIMITS};
    use crate::inbound::{ConversationKey, InboundEvent};
    use time::OffsetDateTime;

    #[tokio::test]
    async fn chat_creation_is_claimed_once_and_unsubmitted_creation_can_resume_after_restart() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store");
        let event = InboundEvent {
            key: EventKey {
                conversation: ConversationKey {
                    source: "source".into(),
                    subject: "discussion".into(),
                },
                id: "first".into(),
            },
            new_chat: false,
            options: Default::default(),
            chat_name: None,
            source_url: None,
            initial_context: None,
            actor: "author".into(),
            created_at: OffsetDateTime::now_utc(),
            message: "request".into(),
        };
        store
            .insert(&event, TEST_QUEUE_LIMITS)
            .await
            .expect("insert");
        let (first, second) = tokio::join!(
            store.claim_for_binding(&event.key),
            store.claim_for_binding(&event.key)
        );
        assert_ne!(first.expect("first claim"), second.expect("second claim"));
        store
            .finish_delivery(&event.key, DeliveryOutcome::Pending, None)
            .await
            .expect("unavailable");
        assert!(
            store
                .claim_for_binding(&event.key)
                .await
                .expect("safe retry")
        );
        store.recover_interrupted().await.expect("restart recovery");
        assert_eq!(
            store.delivery_state(&event.key).await.expect("state"),
            Some(DeliveryState::Pending)
        );
        assert!(
            store
                .claim_for_binding(&event.key)
                .await
                .expect("unsubmitted request can resume")
        );
        let next = InboundEvent {
            key: EventKey {
                id: "next".into(),
                ..event.key.clone()
            },
            ..event
        };
        store
            .insert(&next, TEST_QUEUE_LIMITS)
            .await
            .expect("next insert");
        assert!(
            !store
                .claim_for_binding(&next.key)
                .await
                .expect("active creation blocks overlapping creation")
        );
        assert!(
            !store
                .finish_delivery(&next.key, DeliveryOutcome::Delivered, None)
                .await
                .expect("unclaimed request ignored")
        );
        store
            .finish_delivery(
                &EventKey {
                    id: "first".into(),
                    ..next.key.clone()
                },
                DeliveryOutcome::Failed,
                None,
            )
            .await
            .expect("creation settles");
        assert!(
            store
                .claim_for_binding(&next.key)
                .await
                .expect("later creation can proceed")
        );
    }

    #[tokio::test]
    async fn binding_claims_check_only_the_requests_agent() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store");
        let codex = InboundEvent {
            key: EventKey {
                conversation: ConversationKey {
                    source: "source".into(),
                    subject: "discussion".into(),
                },
                id: "codex".into(),
            },
            new_chat: false,
            options: Default::default(),
            chat_name: None,
            source_url: None,
            initial_context: None,
            actor: "author".into(),
            created_at: OffsetDateTime::now_utc(),
            message: "request".into(),
        };
        let mut claude = codex.clone();
        claude.key.id = "claude".into();
        claude.options.agent = Agent::Claude;
        store
            .bind_conversation(
                &codex.key.conversation,
                &crate::inbound::store::SessionTarget {
                    host_id: "host".into(),
                    agent: "codex".into(),
                    chat_id: "codex-chat".into(),
                    workspace: "/repo".into(),
                },
            )
            .await
            .expect("discussion binds to Codex");
        for event in [&codex, &claude] {
            store
                .insert(event, TEST_QUEUE_LIMITS)
                .await
                .expect("insert");
        }
        assert!(
            !store
                .claim_for_binding(&codex.key)
                .await
                .expect("Codex request has a chat")
        );
        assert!(
            store
                .claim_for_binding(&claude.key)
                .await
                .expect("Claude request needs a chat")
        );
        let mut later_claude = claude.clone();
        later_claude.key.id = "later-claude".into();
        let mut unmapped_codex = codex.clone();
        unmapped_codex.key.conversation.subject = "unmapped".into();
        let mut unmapped_claude = claude.clone();
        unmapped_claude.key.conversation.subject = "unmapped".into();
        for event in [&later_claude, &unmapped_claude, &unmapped_codex] {
            store
                .insert(event, TEST_QUEUE_LIMITS)
                .await
                .expect("insert");
        }
        assert!(
            !store
                .claim_for_binding(&later_claude.key)
                .await
                .expect("Claude creation in flight")
        );
        assert!(
            store
                .claim_for_binding(&unmapped_claude.key)
                .await
                .expect("Claude creation starts")
        );
        assert!(
            store
                .claim_for_binding(&unmapped_codex.key)
                .await
                .expect("Codex creation does not wait for Claude")
        );
    }
}
