use super::{EventStore, StoreError};
use crate::inbound::EventKey;

impl EventStore {
    pub async fn claim_for_binding(&self, key: &EventKey) -> Result<bool, StoreError> {
        let result = sqlx::query!(
            "UPDATE inbound_events SET delivery_state = 'delivering'
             WHERE source = ?1 AND subject = ?2 AND event_id = ?3 AND delivery_state = 'pending'
               AND NOT EXISTS (
                   SELECT 1 FROM inbound_conversations WHERE source = ?1 AND subject = ?2
               )
               AND NOT EXISTS (
                   SELECT 1 FROM inbound_events AS unresolved
                   WHERE unresolved.source = ?1 AND unresolved.subject = ?2
                     AND unresolved.delivery_state = 'delivering'
               )",
            key.conversation.source,
            key.conversation.subject,
            key.id,
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
}
