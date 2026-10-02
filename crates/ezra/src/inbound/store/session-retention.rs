use time::OffsetDateTime;

use super::{EventStore, StoreError};

impl EventStore {
    /// Caps idle conversation links, expiring each session's aliases together.
    /// Native chats and workspaces are not deleted.
    pub async fn prune_sessions(
        &self,
        cutoff: OffsetDateTime,
        max_idle_conversations: u32,
    ) -> Result<u64, StoreError> {
        let cutoff_seconds = cutoff.unix_timestamp();
        let result = sqlx::query!(
            "WITH idle_sessions AS (
                 SELECT sessions.id, sessions.last_used_at,
                        (SELECT COUNT(*) FROM inbound_conversations
                         WHERE session_id = sessions.id) AS conversation_count
                 FROM inbound_sessions AS sessions
                 WHERE NOT EXISTS (
                     SELECT 1 FROM inbound_conversations AS conversations
                     JOIN inbound_events AS events
                       ON events.source = conversations.source
                      AND events.subject = conversations.subject
                     WHERE conversations.session_id = sessions.id
                       AND events.delivery_state IN ('pending', 'delivering')
                 )
             ), ranked_sessions AS (
                 SELECT id, last_used_at,
                        SUM(conversation_count) OVER (
                            ORDER BY last_used_at DESC, id DESC ROWS UNBOUNDED PRECEDING
                        ) AS conversation_count
                 FROM idle_sessions
             )
             DELETE FROM inbound_sessions WHERE id IN (
                 SELECT id FROM ranked_sessions
                 WHERE last_used_at < ?1 OR conversation_count > ?2
             )",
            cutoff_seconds,
            max_idle_conversations,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::store::{
        DeliveryOutcome, DeliveryState, InsertOutcome, SessionTarget, TEST_QUEUE_LIMITS,
    };
    use crate::inbound::{ConversationKey, EventKey, InboundEvent};
    use time::macros::datetime;

    impl InboundEvent {
        fn session_retention_example(subject: &str) -> Self {
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

    impl SessionTarget {
        fn retention_target(chat_id: &str) -> Self {
            Self {
                host_id: "host-a".to_owned(),
                agent: "codex".to_owned(),
                chat_id: chat_id.to_owned(),
                workspace: "/home/dev/projects/repository".to_owned(),
            }
        }
    }

    impl EventStore {
        async fn set_test_session_activity(&self, timestamp: i64) {
            sqlx::query!("UPDATE inbound_sessions SET last_used_at = ?1", timestamp)
                .execute(&self.pool)
                .await
                .expect("session activity sets");
        }

        async fn test_session_activity(&self) -> i64 {
            sqlx::query_scalar!("SELECT last_used_at FROM inbound_sessions")
                .fetch_one(&self.pool)
                .await
                .expect("session activity reads")
        }
    }

    #[tokio::test]
    async fn expiry_removes_all_aliases_and_keeps_boundary_sessions_and_event_history() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("store opens");
        let issue_event = InboundEvent::session_retention_example("issue-1");
        let pull_request = InboundEvent::session_retention_example("pull-2")
            .key
            .conversation;
        let target = SessionTarget::retention_target("chat-1");
        store
            .bind_conversation(&issue_event.key.conversation, &target)
            .await
            .expect("issue binds");
        store
            .link_conversation(&pull_request, &issue_event.key.conversation)
            .await
            .expect("PR links");
        store
            .insert(&issue_event, TEST_QUEUE_LIMITS)
            .await
            .expect("event inserts");
        store.claim_next(None).await.expect("event claims");
        store
            .finish_delivery(&issue_event.key, DeliveryOutcome::Delivered, None)
            .await
            .expect("event delivered");
        store.set_test_session_activity(1000).await;
        let cutoff = OffsetDateTime::from_unix_timestamp(1000).expect("cutoff timestamp");
        assert_eq!(
            store
                .prune_sessions(cutoff, u32::MAX)
                .await
                .expect("boundary kept"),
            0
        );
        store.pool.close().await;
        let store = EventStore::open(&path).await.expect("store reopens");
        assert_eq!(
            store
                .prune_sessions(cutoff + time::Duration::seconds(1), u32::MAX)
                .await
                .expect("idle session expires"),
            1
        );
        for conversation in [&issue_event.key.conversation, &pull_request] {
            assert_eq!(
                store
                    .find_binding(conversation)
                    .await
                    .expect("expired lookup"),
                None
            );
        }
        assert_eq!(
            store.get(&issue_event.key).await.expect("history lookup"),
            Some(issue_event.clone())
        );
        assert_eq!(
            store
                .insert(&issue_event, TEST_QUEUE_LIMITS)
                .await
                .expect("history prevents replay"),
            InsertOutcome::Duplicate
        );
        let fresh_target = SessionTarget::retention_target("chat-2");
        store
            .bind_conversation(&pull_request, &fresh_target)
            .await
            .expect("expired discussion can bind again");
        assert_eq!(
            store
                .find_binding(&pull_request)
                .await
                .expect("fresh lookup"),
            Some(fresh_target)
        );
    }

    #[tokio::test]
    async fn any_queued_alias_protects_the_entire_session() {
        for state in [DeliveryState::Pending, DeliveryState::Delivering] {
            let directory = tempfile::tempdir().expect("temporary directory");
            let store = EventStore::open(&directory.path().join("ezra.db"))
                .await
                .expect("store opens");
            let issue = InboundEvent::session_retention_example("issue-1")
                .key
                .conversation;
            let pull_event = InboundEvent::session_retention_example("pull-2");
            let target = SessionTarget::retention_target("chat-1");
            store
                .bind_conversation(&issue, &target)
                .await
                .expect("issue binds");
            store
                .link_conversation(&pull_event.key.conversation, &issue)
                .await
                .expect("PR links");
            store
                .insert(&pull_event, TEST_QUEUE_LIMITS)
                .await
                .expect("PR event inserts");
            if state != DeliveryState::Pending {
                assert_eq!(
                    store.claim_next(None).await.expect("PR claims"),
                    Some(pull_event.clone())
                );
            }
            store.set_test_session_activity(0).await;
            assert_eq!(
                store
                    .prune_sessions(OffsetDateTime::now_utc(), 0)
                    .await
                    .expect("queue protects session"),
                0
            );
            for conversation in [&issue, &pull_event.key.conversation] {
                assert_eq!(
                    store
                        .find_binding(conversation)
                        .await
                        .expect("protected lookup"),
                    Some(target.clone())
                );
            }
        }
    }

    #[tokio::test]
    async fn terminal_audit_records_do_not_protect_idle_session_links() {
        for state in ["delivered", "uncertain", "failed", "expired"] {
            let directory = tempfile::tempdir().expect("directory");
            let store = EventStore::open(&directory.path().join("ezra.db"))
                .await
                .expect("store");
            let event = InboundEvent::session_retention_example("issue-1");
            store
                .bind_conversation(
                    &event.key.conversation,
                    &SessionTarget::retention_target("chat-1"),
                )
                .await
                .expect("binding");
            store
                .insert(&event, TEST_QUEUE_LIMITS)
                .await
                .expect("insert");
            sqlx::query!(
                "UPDATE inbound_events SET delivery_state = ?1 WHERE event_id = ?2",
                state,
                event.key.id
            )
            .execute(&store.pool)
            .await
            .expect("terminal outcome");
            store.set_test_session_activity(0).await;
            assert_eq!(
                store
                    .prune_sessions(OffsetDateTime::now_utc(), 0)
                    .await
                    .expect("prune idle session"),
                1
            );
            assert!(
                store
                    .find_binding(&event.key.conversation)
                    .await
                    .expect("binding lookup")
                    .is_none()
            );
            assert!(store.get(&event.key).await.expect("audit lookup").is_some());
        }
    }

    #[tokio::test]
    async fn only_new_activity_refreshes_sessions_and_time_never_moves_backwards() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store opens");
        let issue_event = InboundEvent::session_retention_example("issue-1");
        let pull_request = InboundEvent::session_retention_example("pull-2")
            .key
            .conversation;
        store
            .bind_conversation(
                &issue_event.key.conversation,
                &SessionTarget::retention_target("chat-1"),
            )
            .await
            .expect("issue binds");
        store.set_test_session_activity(0).await;
        store
            .insert(&issue_event, TEST_QUEUE_LIMITS)
            .await
            .expect("event inserts");
        assert!(store.test_session_activity().await > 0);
        store.set_test_session_activity(0).await;
        assert_eq!(
            store
                .insert(&issue_event, TEST_QUEUE_LIMITS)
                .await
                .expect("duplicate ignored"),
            InsertOutcome::Duplicate
        );
        assert_eq!(store.test_session_activity().await, 0);
        assert!(
            !store
                .finish_delivery(&issue_event.key, DeliveryOutcome::Delivered, None)
                .await
                .expect("unclaimed finish ignored")
        );
        assert_eq!(store.test_session_activity().await, 0);
        store.claim_next(None).await.expect("event claims");
        store
            .finish_delivery(&issue_event.key, DeliveryOutcome::Delivered, None)
            .await
            .expect("event delivered");
        assert!(store.test_session_activity().await > 0);
        store.set_test_session_activity(0).await;
        store
            .link_conversation(&pull_request, &issue_event.key.conversation)
            .await
            .expect("PR links");
        assert!(store.test_session_activity().await > 0);
        store.set_test_session_activity(0).await;
        store
            .link_conversation(&pull_request, &issue_event.key.conversation)
            .await
            .expect("repeated link ignored");
        assert_eq!(store.test_session_activity().await, 0);
        let future_timestamp =
            (OffsetDateTime::now_utc() + time::Duration::days(1)).unix_timestamp();
        store.set_test_session_activity(future_timestamp).await;
        let followup = InboundEvent {
            key: EventKey {
                id: "comment-2".to_owned(),
                ..issue_event.key.clone()
            },
            ..issue_event
        };
        store
            .insert(&followup, TEST_QUEUE_LIMITS)
            .await
            .expect("follow-up inserts");
        assert_eq!(store.test_session_activity().await, future_timestamp);
    }

    #[tokio::test]
    async fn count_caps_keep_newest_sessions_and_count_every_alias() {
        for (max_idle_conversations, expected_deleted, keep_oldest, keep_middle, keep_newest) in [
            (u32::MAX, 0, true, true, true),
            (4, 0, true, true, true),
            (3, 1, false, true, true),
            (2, 2, false, false, true),
            (1, 2, false, false, true),
            (0, 3, false, false, false),
        ] {
            let directory = tempfile::tempdir().expect("temporary directory");
            let path = directory.path().join("ezra.db");
            let store = EventStore::open(&path).await.expect("store opens");
            let oldest = InboundEvent::session_retention_example("oldest")
                .key
                .conversation;
            let middle = InboundEvent::session_retention_example("middle")
                .key
                .conversation;
            let newest = InboundEvent::session_retention_example("newest")
                .key
                .conversation;
            let middle_alias = InboundEvent::session_retention_example("middle-pr")
                .key
                .conversation;
            for conversation in [&oldest, &middle, &newest] {
                store
                    .bind_conversation(
                        conversation,
                        &SessionTarget::retention_target(&conversation.subject),
                    )
                    .await
                    .expect("session binds");
            }
            store
                .link_conversation(&middle_alias, &middle)
                .await
                .expect("middle PR links");
            store.set_test_session_activity(1000).await;
            let cutoff = OffsetDateTime::from_unix_timestamp(1000).expect("cutoff timestamp");
            assert_eq!(
                store
                    .prune_sessions(cutoff, max_idle_conversations)
                    .await
                    .expect("count cap applies"),
                expected_deleted
            );
            store.pool.close().await;
            let store = EventStore::open(&path).await.expect("store reopens");
            for (conversation, retained) in [
                (&oldest, keep_oldest),
                (&middle, keep_middle),
                (&middle_alias, keep_middle),
                (&newest, keep_newest),
            ] {
                assert_eq!(
                    store
                        .find_binding(conversation)
                        .await
                        .expect("retained lookup")
                        .is_some(),
                    retained
                );
            }
            assert_eq!(
                store
                    .prune_sessions(cutoff, u32::MAX)
                    .await
                    .expect("larger cap does not restore links"),
                0
            );
        }
    }

    #[tokio::test]
    async fn refreshed_sessions_take_priority_over_newer_session_ids() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store opens");
        let first_event = InboundEvent::session_retention_example("first");
        let second = InboundEvent::session_retention_example("second")
            .key
            .conversation;
        store
            .bind_conversation(
                &first_event.key.conversation,
                &SessionTarget::retention_target("first"),
            )
            .await
            .expect("first binds");
        store
            .bind_conversation(&second, &SessionTarget::retention_target("second"))
            .await
            .expect("second binds");
        store.set_test_session_activity(0).await;
        store
            .insert(&first_event, TEST_QUEUE_LIMITS)
            .await
            .expect("first refreshed");
        store.claim_next(None).await.expect("first claims");
        store
            .finish_delivery(&first_event.key, DeliveryOutcome::Delivered, None)
            .await
            .expect("first delivered");
        assert_eq!(
            store
                .prune_sessions(OffsetDateTime::UNIX_EPOCH, 1)
                .await
                .expect("cap applies"),
            1
        );
        assert!(
            store
                .find_binding(&first_event.key.conversation)
                .await
                .expect("first retained")
                .is_some()
        );
        assert_eq!(
            store.find_binding(&second).await.expect("second expires"),
            None
        );
    }
}
