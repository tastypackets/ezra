use time::OffsetDateTime;

use super::{EventStore, StoreError};
use crate::inbound::{ConversationKey, EventKey};

impl EventStore {
    pub async fn expire_waiting(
        &self,
        cutoff: OffsetDateTime,
    ) -> Result<Vec<EventKey>, StoreError> {
        let cutoff_seconds = cutoff.unix_timestamp();
        let result = sqlx::query!(
            "UPDATE inbound_events SET delivery_state = 'expired'
             WHERE delivery_state IN ('pending', 'delivering') AND attempted_at IS NULL AND received_at <= ?1
             RETURNING source, subject, event_id",
            cutoff_seconds,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(result
            .into_iter()
            .map(|record| EventKey {
                conversation: ConversationKey {
                    source: record.source,
                    subject: record.subject,
                },
                id: record.event_id,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::store::{DeliveryState, InsertOutcome, TEST_QUEUE_LIMITS};
    use crate::inbound::{ConversationKey, EventKey, InboundEvent};
    use time::macros::datetime;

    impl InboundEvent {
        fn expiry_example(event_id: &str) -> Self {
            Self {
                key: EventKey {
                    conversation: ConversationKey {
                        source: "github:github.com".into(),
                        subject: "1/2".into(),
                    },
                    id: event_id.into(),
                },
                new_chat: false,
                options: Default::default(),
                chat_name: None,
                source_url: None,
                initial_context: None,
                actor: "author".into(),
                created_at: datetime!(2020-01-01 00:00 UTC),
                message: "New command in an edited old comment".into(),
            }
        }
    }

    #[tokio::test]
    async fn only_unattempted_waiting_requests_expire_and_keep_their_dedupe_record() {
        let directory = tempfile::tempdir().expect("directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store");
        let cutoff = datetime!(2026-10-01 00:00 UTC);
        for (event_id, state, recent, attempted) in [
            ("expired", "pending", false, false),
            ("recent", "pending", true, false),
            ("attempted", "pending", false, true),
            ("preparing", "delivering", false, false),
            ("delivered", "delivered", false, true),
        ] {
            let event = InboundEvent::expiry_example(event_id);
            store
                .insert(&event, TEST_QUEUE_LIMITS)
                .await
                .expect("insert");
            let received_at = cutoff.unix_timestamp() + i64::from(recent);
            let attempted_at = attempted.then_some(received_at);
            sqlx::query!("UPDATE inbound_events SET delivery_state = ?1, received_at = ?2, attempted_at = ?3 WHERE event_id = ?4", state, received_at, attempted_at, event_id)
                .execute(&store.pool).await.expect("waiting metadata");
        }
        let expired_keys = store.expire_waiting(cutoff).await.expect("expiry");
        assert_eq!(expired_keys.len(), 2);
        assert!(expired_keys.contains(&InboundEvent::expiry_example("expired").key));
        assert!(expired_keys.contains(&InboundEvent::expiry_example("preparing").key));
        let expired = InboundEvent::expiry_example("expired");
        assert_eq!(
            store.delivery_state(&expired.key).await.expect("state"),
            Some(DeliveryState::Expired)
        );
        assert_eq!(
            store
                .insert(&expired, TEST_QUEUE_LIMITS)
                .await
                .expect("duplicate"),
            InsertOutcome::Duplicate
        );
        assert_eq!(
            store
                .expire_waiting(cutoff)
                .await
                .expect("repeat expiry")
                .len(),
            0
        );
        assert_eq!(
            store
                .delivery_state(&InboundEvent::expiry_example("recent").key)
                .await
                .expect("recent state"),
            Some(DeliveryState::Pending)
        );
        assert_eq!(
            store
                .delivery_state(&InboundEvent::expiry_example("attempted").key)
                .await
                .expect("attempted state"),
            Some(DeliveryState::Pending)
        );
    }
}
