use super::{EventStore, SessionTarget, StoreError};
use crate::agent::Agent;
use crate::inbound::EventKey;

impl EventStore {
    /// Replace only after confirmed rejection, before sending to the new chat.
    pub async fn replace_chat_for_delivery(
        &self,
        event: &EventKey,
        expected: &SessionTarget,
        replacement_chat_id: &str,
    ) -> Result<(), StoreError> {
        let replacement = SessionTarget {
            chat_id: replacement_chat_id.to_owned(),
            ..expected.clone()
        };
        replacement.validate_binding(&event.conversation)?;
        let unrecorded_agent = Agent::UNRECORDED.command_name();
        let result = sqlx::query!(
            "UPDATE inbound_sessions
             SET chat_id = ?1, last_used_at = MAX(last_used_at, unixepoch()), initial_context_pending = 1
             WHERE host_id = ?2 AND agent = ?3 AND chat_id = ?4 AND workspace = ?5
               AND id = (
                   SELECT conversation.session_id FROM inbound_conversations AS conversation
                   JOIN inbound_events AS event
                     ON event.source = conversation.source AND event.subject = conversation.subject
                    AND COALESCE(event.requested_agent, ?9) = conversation.agent
                   WHERE event.source = ?6 AND event.subject = ?7 AND event.event_id = ?8
                     AND event.delivery_state = 'delivering'
               )
               AND NOT EXISTS (
                   SELECT 1 FROM inbound_conversations AS related
                   JOIN inbound_events AS unresolved
                     ON unresolved.source = related.source AND unresolved.subject = related.subject
                    AND COALESCE(unresolved.requested_agent, ?9) = related.agent
                   WHERE related.session_id = inbound_sessions.id
                     AND unresolved.delivery_state = 'delivering'
                     AND NOT (unresolved.source = ?6 AND unresolved.subject = ?7 AND unresolved.event_id = ?8)
               )",
            replacement_chat_id,
            expected.host_id,
            expected.agent,
            expected.chat_id,
            expected.workspace,
            event.conversation.source,
            event.conversation.subject,
            event.id,
            unrecorded_agent,
        )
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(StoreError::DeliveryChanged);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::store::{DeliveryOutcome, DeliveryState, TEST_QUEUE_LIMITS};
    use crate::inbound::{ConversationKey, InboundEvent};

    struct Fixture {
        directory: tempfile::TempDir,
        store: EventStore,
        event: InboundEvent,
        target: SessionTarget,
    }

    impl Fixture {
        async fn new() -> Self {
            let directory = tempfile::tempdir().expect("temporary database directory");
            let store = EventStore::open(&directory.path().join("ezra.db"))
                .await
                .expect("store opens");
            let event = InboundEvent {
                key: EventKey {
                    conversation: ConversationKey {
                        source: "github".to_owned(),
                        subject: "issue-1".to_owned(),
                    },
                    id: "comment-1".to_owned(),
                },
                new_chat: false,
                options: Default::default(),
                chat_name: None,
                source_url: None,
                initial_context: None,
                actor: "author".to_owned(),
                created_at: time::OffsetDateTime::UNIX_EPOCH,
                message: "Continue the work".to_owned(),
            };
            let target = SessionTarget {
                host_id: "host-1".to_owned(),
                agent: "codex".to_owned(),
                chat_id: "old-chat".to_owned(),
                workspace: "/workspace".to_owned(),
            };
            store
                .bind_conversation(&event.key.conversation, &target)
                .await
                .expect("chat binds");
            store
                .insert(&event, TEST_QUEUE_LIMITS)
                .await
                .expect("event inserts");
            Self {
                directory,
                store,
                event,
                target,
            }
        }
    }

    #[tokio::test]
    async fn replacement_preserves_shared_links_and_delivery_across_restart() {
        for attempted in [false, true] {
            let fixture = Fixture::new().await;
            let mut followup = fixture.event.clone();
            followup.key.conversation.subject = "pr-2".to_owned();
            fixture
                .store
                .link_conversation(
                    &followup.key.conversation,
                    &fixture.event.key.conversation,
                    Agent::Codex,
                )
                .await
                .expect("PR shares chat");
            fixture
                .store
                .insert(&followup, TEST_QUEUE_LIMITS)
                .await
                .expect("follow-up inserts");
            assert_eq!(
                fixture.store.claim_next(None).await.expect("claim"),
                Some(fixture.event.clone())
            );
            fixture
                .store
                .replace_chat_for_delivery(&fixture.event.key, &fixture.target, "new-chat")
                .await
                .expect("replacement persists");
            if attempted {
                sqlx::query!("UPDATE inbound_events SET attempted_at = unixepoch() WHERE delivery_state = 'delivering'")
                .execute(&fixture.store.pool).await.expect("replacement submission attempted");
            }
            fixture.store.pool.close().await;
            let reopened = EventStore::open(&fixture.directory.path().join("ezra.db"))
                .await
                .expect("reopen");
            let replacement = SessionTarget {
                chat_id: "new-chat".to_owned(),
                ..fixture.target
            };
            for conversation in [&fixture.event.key.conversation, &followup.key.conversation] {
                assert_eq!(
                    reopened
                        .find_binding(conversation, Agent::Codex)
                        .await
                        .expect("binding"),
                    Some(replacement.clone())
                );
            }
            assert_eq!(
                reopened
                    .get(&fixture.event.key)
                    .await
                    .expect("original message"),
                Some(fixture.event.clone())
            );
            assert_eq!(
                reopened
                    .delivery_state(&fixture.event.key)
                    .await
                    .expect("state"),
                Some(DeliveryState::Delivering)
            );
            assert!(
                reopened
                    .claim_next(None)
                    .await
                    .expect("follow-up remains blocked")
                    .is_none()
            );
            reopened
                .recover_interrupted()
                .await
                .expect("recover interrupted delivery");
            if attempted {
                assert_eq!(
                    reopened
                        .delivery_state(&fixture.event.key)
                        .await
                        .expect("consumed attempt"),
                    Some(DeliveryState::Uncertain)
                );
            } else {
                assert_eq!(
                    reopened
                        .claim_next(None)
                        .await
                        .expect("unsubmitted replacement resumes"),
                    Some(fixture.event.clone())
                );
                assert!(
                    reopened
                        .finish_delivery(
                            &fixture.event.key,
                            crate::inbound::store::DeliveryOutcome::Delivered,
                            None
                        )
                        .await
                        .expect("replacement delivered")
                );
            }
            assert_eq!(
                reopened
                    .claim_next(None)
                    .await
                    .expect("followup proceeds in replacement"),
                Some(followup)
            );
            assert!(
                reopened
                    .claim_next(None)
                    .await
                    .expect("no consumed attempt replay")
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn an_uncertain_alias_does_not_block_replacing_a_deleted_chat_for_a_followup() {
        let fixture = Fixture::new().await;
        let mut followup = fixture.event.clone();
        followup.key.conversation.subject = "pr-2".to_owned();
        followup.key.id = "comment-2".to_owned();
        fixture
            .store
            .link_conversation(
                &followup.key.conversation,
                &fixture.event.key.conversation,
                Agent::Codex,
            )
            .await
            .expect("shared chat");
        fixture
            .store
            .claim_next(None)
            .await
            .expect("original claim");
        fixture
            .store
            .finish_delivery(&fixture.event.key, DeliveryOutcome::Uncertain, None)
            .await
            .expect("original acceptance remains unknown");
        fixture
            .store
            .insert(&followup, TEST_QUEUE_LIMITS)
            .await
            .expect("followup inserts");
        assert_eq!(
            fixture
                .store
                .claim_next(None)
                .await
                .expect("followup claim"),
            Some(followup.clone())
        );
        fixture
            .store
            .replace_chat_for_delivery(&followup.key, &fixture.target, "replacement-chat")
            .await
            .expect("confirmed deleted chat can be replaced");
        for conversation in [&fixture.event.key.conversation, &followup.key.conversation] {
            assert_eq!(
                fixture
                    .store
                    .find_binding(conversation, Agent::Codex)
                    .await
                    .expect("binding")
                    .expect("shared replacement")
                    .chat_id,
                "replacement-chat"
            );
        }
        assert_eq!(
            fixture
                .store
                .delivery_state(&fixture.event.key)
                .await
                .expect("original state"),
            Some(DeliveryState::Uncertain)
        );
        fixture
            .store
            .finish_delivery(&followup.key, DeliveryOutcome::Delivered, None)
            .await
            .expect("followup delivered");
        assert!(
            fixture
                .store
                .claim_next(None)
                .await
                .expect("no uncertain replay")
                .is_none()
        );
    }

    #[tokio::test]
    async fn replacement_requires_the_active_claim_and_expected_destination() {
        let fixture = Fixture::new().await;
        assert!(matches!(
            fixture
                .store
                .replace_chat_for_delivery(&fixture.event.key, &fixture.target, "new-chat")
                .await,
            Err(StoreError::DeliveryChanged)
        ));
        fixture.store.claim_next(None).await.expect("claim");
        for stale_target in [
            SessionTarget {
                host_id: "other-host".to_owned(),
                ..fixture.target.clone()
            },
            SessionTarget {
                agent: "claude".to_owned(),
                ..fixture.target.clone()
            },
            SessionTarget {
                chat_id: "stale-chat".to_owned(),
                ..fixture.target.clone()
            },
            SessionTarget {
                workspace: "/other-workspace".to_owned(),
                ..fixture.target.clone()
            },
        ] {
            assert!(matches!(
                fixture
                    .store
                    .replace_chat_for_delivery(&fixture.event.key, &stale_target, "new-chat")
                    .await,
                Err(StoreError::DeliveryChanged)
            ));
        }
        for invalid_chat_id in ["", "invalid chat", &"x".repeat(513)] {
            assert!(matches!(
                fixture
                    .store
                    .replace_chat_for_delivery(&fixture.event.key, &fixture.target, invalid_chat_id)
                    .await,
                Err(StoreError::InvalidBinding(_))
            ));
        }
        fixture
            .store
            .finish_delivery(&fixture.event.key, DeliveryOutcome::Uncertain, None)
            .await
            .expect("mark uncertain");
        assert!(matches!(
            fixture
                .store
                .replace_chat_for_delivery(&fixture.event.key, &fixture.target, "new-chat")
                .await,
            Err(StoreError::DeliveryChanged)
        ));
        assert_eq!(
            fixture
                .store
                .find_binding(&fixture.event.key.conversation, Agent::Codex)
                .await
                .expect("unchanged binding"),
            Some(fixture.target)
        );
    }

    #[tokio::test]
    async fn competing_replacements_cannot_overwrite_each_other() {
        let fixture = Fixture::new().await;
        fixture.store.claim_next(None).await.expect("claim");
        let other_store = EventStore::open(&fixture.directory.path().join("ezra.db"))
            .await
            .expect("second connection");
        let (first, second) = tokio::join!(
            fixture.store.replace_chat_for_delivery(
                &fixture.event.key,
                &fixture.target,
                "first-chat"
            ),
            other_store.replace_chat_for_delivery(
                &fixture.event.key,
                &fixture.target,
                "second-chat"
            ),
        );
        let winning_chat = match (first, second) {
            (Ok(()), Err(StoreError::DeliveryChanged)) => "first-chat",
            (Err(StoreError::DeliveryChanged), Ok(())) => "second-chat",
            outcomes => panic!("one replacement must win: {outcomes:?}"),
        };
        assert_eq!(
            fixture
                .store
                .find_binding(&fixture.event.key.conversation, Agent::Codex)
                .await
                .expect("binding")
                .expect("bound")
                .chat_id,
            winning_chat
        );
    }

    #[tokio::test]
    async fn replacement_cannot_merge_an_unrelated_session() {
        let fixture = Fixture::new().await;
        let unrelated = ConversationKey {
            source: "github".to_owned(),
            subject: "issue-3".to_owned(),
        };
        let occupied = SessionTarget {
            chat_id: "occupied-chat".to_owned(),
            ..fixture.target.clone()
        };
        fixture
            .store
            .bind_conversation(&unrelated, &occupied)
            .await
            .expect("unrelated binding");
        fixture.store.claim_next(None).await.expect("claim");
        assert!(matches!(
            fixture
                .store
                .replace_chat_for_delivery(&fixture.event.key, &fixture.target, &occupied.chat_id)
                .await,
            Err(StoreError::Sqlite(_))
        ));
        assert_eq!(
            fixture
                .store
                .find_binding(&fixture.event.key.conversation, Agent::Codex)
                .await
                .expect("original binding"),
            Some(fixture.target)
        );
        assert_eq!(
            fixture
                .store
                .find_binding(&unrelated, Agent::Codex)
                .await
                .expect("unrelated binding"),
            Some(occupied)
        );
    }

    #[tokio::test]
    async fn replacement_changes_only_the_requesting_agents_chat() {
        let fixture = Fixture::new().await;
        let claude_target = SessionTarget {
            agent: "claude".to_owned(),
            chat_id: "claude-chat".to_owned(),
            ..fixture.target.clone()
        };
        fixture
            .store
            .bind_conversation(&fixture.event.key.conversation, &claude_target)
            .await
            .expect("Claude chat binds");
        fixture.store.claim_next(None).await.expect("claim");
        assert!(matches!(
            fixture
                .store
                .replace_chat_for_delivery(&fixture.event.key, &claude_target, "new-chat")
                .await,
            Err(StoreError::DeliveryChanged)
        ));
        fixture
            .store
            .replace_chat_for_delivery(&fixture.event.key, &fixture.target, "new-chat")
            .await
            .expect("Codex chat is replaced");
        for (agent, chat_id) in [(Agent::Codex, "new-chat"), (Agent::Claude, "claude-chat")] {
            assert_eq!(
                fixture
                    .store
                    .find_binding(&fixture.event.key.conversation, agent)
                    .await
                    .expect("binding reads")
                    .map(|target| target.chat_id),
                Some(chat_id.to_owned())
            );
        }
    }
}
