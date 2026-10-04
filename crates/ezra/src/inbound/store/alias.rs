use super::{EventStore, SessionTarget, StoreError};
use crate::agent::Agent;
use crate::inbound::ConversationKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasOutcome {
    Created,
    AlreadyBound,
    MissingDestination,
    UnresolvedDelivery,
}

impl EventStore {
    pub async fn link_conversation(
        &self,
        conversation: &ConversationKey,
        destination: &ConversationKey,
        agent: Agent,
    ) -> Result<AliasOutcome, StoreError> {
        let agent = agent.command_name();
        let unrecorded_agent = Agent::UNRECORDED.command_name();
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let target = sqlx::query_as!(
            SessionTarget,
            "SELECT sessions.host_id, sessions.agent, sessions.chat_id, sessions.workspace
             FROM inbound_conversations AS conversations
             JOIN inbound_sessions AS sessions ON sessions.id = conversations.session_id
             WHERE conversations.source = ?1 AND conversations.subject = ?2 AND conversations.agent = ?3",
            destination.source,
            destination.subject,
            agent,
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(target) = target else {
            transaction.rollback().await?;
            return Ok(AliasOutcome::MissingDestination);
        };
        target.validate_binding(conversation)?;
        let already_bound = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM inbound_conversations WHERE source = ?1 AND subject = ?2 AND agent = ?3
               ) AS "already_bound!: bool""#,
            conversation.source,
            conversation.subject,
            agent,
        )
        .fetch_one(&mut *transaction)
        .await?;
        if already_bound {
            transaction.rollback().await?;
            return Ok(AliasOutcome::AlreadyBound);
        }
        let unresolved = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM inbound_events
                   WHERE source = ?1 AND subject = ?2
                     AND delivery_state IN ('delivering', 'uncertain')
                     AND COALESCE(requested_agent, ?4) = ?3
               ) AS "unresolved!: bool""#,
            conversation.source,
            conversation.subject,
            agent,
            unrecorded_agent,
        )
        .fetch_one(&mut *transaction)
        .await?;
        if unresolved {
            transaction.rollback().await?;
            return Ok(AliasOutcome::UnresolvedDelivery);
        }
        sqlx::query!(
            "INSERT INTO inbound_conversations (source, subject, agent, session_id)
             SELECT ?1, ?2, agent, session_id FROM inbound_conversations
             WHERE source = ?3 AND subject = ?4 AND agent = ?5",
            conversation.source,
            conversation.subject,
            destination.source,
            destination.subject,
            agent,
        )
        .execute(&mut *transaction)
        .await?;
        sqlx::query!(
            "UPDATE inbound_sessions SET last_used_at = MAX(last_used_at, unixepoch())
             WHERE id = (
                 SELECT session_id FROM inbound_conversations WHERE source = ?1 AND subject = ?2 AND agent = ?3
             )",
            conversation.source,
            conversation.subject,
            agent,
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(AliasOutcome::Created)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::store::{DeliveryOutcome, TEST_QUEUE_LIMITS};
    use crate::inbound::{EventKey, InboundEvent};
    use time::macros::datetime;

    impl InboundEvent {
        fn alias_example(subject: &str) -> Self {
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
        fn alias_target() -> Self {
            Self {
                host_id: "host-a".to_owned(),
                agent: "codex".to_owned(),
                chat_id: "chat-1".to_owned(),
                workspace: "/home/dev/projects/repository".to_owned(),
            }
        }
    }

    #[tokio::test]
    async fn aliases_survive_restart_and_do_not_replace_destinations() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("store opens");
        let issue = InboundEvent::alias_example("issue-1").key.conversation;
        let pull_request = InboundEvent::alias_example("pull-2").key.conversation;
        assert_eq!(
            store
                .link_conversation(&pull_request, &issue, Agent::Codex)
                .await
                .expect("missing destination"),
            AliasOutcome::MissingDestination
        );
        let target = SessionTarget::alias_target();
        store
            .bind_conversation(&issue, &target)
            .await
            .expect("issue binds");
        assert_eq!(
            store
                .link_conversation(&pull_request, &issue, Agent::Codex)
                .await
                .expect("alias creates"),
            AliasOutcome::Created
        );
        store.pool.close().await;
        let store = EventStore::open(&path).await.expect("store reopens");
        assert_eq!(
            store
                .find_binding(&pull_request, Agent::Codex)
                .await
                .expect("alias lookup"),
            Some(target.clone())
        );
        assert_eq!(
            store
                .link_conversation(&pull_request, &issue, Agent::Codex)
                .await
                .expect("alias repeats"),
            AliasOutcome::AlreadyBound
        );
        let other_issue = InboundEvent::alias_example("issue-3").key.conversation;
        let other_target = SessionTarget {
            chat_id: "chat-2".to_owned(),
            ..target.clone()
        };
        store
            .bind_conversation(&other_issue, &other_target)
            .await
            .expect("other issue binds");
        assert_eq!(
            store
                .link_conversation(&pull_request, &other_issue, Agent::Codex)
                .await
                .expect("replacement refused"),
            AliasOutcome::AlreadyBound
        );
        assert_eq!(
            store
                .find_binding(&pull_request, Agent::Codex)
                .await
                .expect("original alias lookup"),
            Some(target)
        );
        let invalid_alias = ConversationKey {
            subject: "bad subject".to_owned(),
            ..issue.clone()
        };
        assert!(matches!(
            store
                .link_conversation(&invalid_alias, &issue, Agent::Codex)
                .await,
            Err(StoreError::InvalidBinding(_))
        ));
    }

    #[tokio::test]
    async fn shared_session_serializes_active_claims_and_allows_followups_after_unknown_delivery() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("store opens");
        let other_store = EventStore::open(&path).await.expect("other store opens");
        let issue_event = InboundEvent::alias_example("issue-1");
        let pull_event = InboundEvent::alias_example("pull-2");
        let followup_event = InboundEvent {
            key: EventKey {
                id: "comment-2".to_owned(),
                ..issue_event.key.clone()
            },
            ..issue_event.clone()
        };
        let independent_event = InboundEvent::alias_example("issue-3");
        store
            .bind_conversation(
                &issue_event.key.conversation,
                &SessionTarget::alias_target(),
            )
            .await
            .expect("issue binds");
        store
            .link_conversation(
                &pull_event.key.conversation,
                &issue_event.key.conversation,
                Agent::Codex,
            )
            .await
            .expect("PR links");
        for event in [&issue_event, &pull_event, &followup_event] {
            store
                .insert(event, TEST_QUEUE_LIMITS)
                .await
                .expect("event inserts");
        }
        let (first_claim, second_claim) =
            tokio::join!(store.claim_next(None), other_store.claim_next(None));
        let claims: Vec<_> = [
            first_claim.expect("first claim"),
            second_claim.expect("second claim"),
        ]
        .into_iter()
        .flatten()
        .collect();
        assert_eq!(claims, vec![issue_event.clone()]);
        store
            .finish_delivery(&issue_event.key, DeliveryOutcome::Delivered, None)
            .await
            .expect("issue delivered");
        assert_eq!(
            store.claim_next(None).await.expect("PR claims next"),
            Some(pull_event.clone())
        );
        store
            .finish_delivery(&pull_event.key, DeliveryOutcome::Uncertain, None)
            .await
            .expect("PR uncertain");
        store
            .insert(&independent_event, TEST_QUEUE_LIMITS)
            .await
            .expect("independent event inserts");
        assert_eq!(
            store
                .claim_next(None)
                .await
                .expect("distinct followup is eligible"),
            Some(followup_event.clone())
        );
        assert_eq!(
            store
                .claim_next(None)
                .await
                .expect("independent event claims"),
            Some(independent_event)
        );
        assert_eq!(
            store
                .claim_next(None)
                .await
                .expect("no consumed message replays"),
            None
        );
    }

    #[tokio::test]
    async fn linking_refuses_unresolved_source_but_accepts_busy_destination() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store opens");
        let issue_event = InboundEvent::alias_example("issue-1");
        let pull_event = InboundEvent::alias_example("pull-2");
        store
            .bind_conversation(
                &issue_event.key.conversation,
                &SessionTarget::alias_target(),
            )
            .await
            .expect("issue binds");
        store
            .insert(&pull_event, TEST_QUEUE_LIMITS)
            .await
            .expect("PR event inserts");
        store.claim_next(None).await.expect("PR claims");
        assert_eq!(
            store
                .link_conversation(
                    &pull_event.key.conversation,
                    &issue_event.key.conversation,
                    Agent::Codex
                )
                .await
                .expect("in-flight PR refused"),
            AliasOutcome::UnresolvedDelivery
        );
        store
            .finish_delivery(&pull_event.key, DeliveryOutcome::Uncertain, None)
            .await
            .expect("PR uncertain");
        assert_eq!(
            store
                .link_conversation(
                    &pull_event.key.conversation,
                    &issue_event.key.conversation,
                    Agent::Codex
                )
                .await
                .expect("uncertain PR refused"),
            AliasOutcome::UnresolvedDelivery
        );
        assert_eq!(
            store
                .find_binding(&pull_event.key.conversation, Agent::Codex)
                .await
                .expect("PR unlinked"),
            None
        );
        store
            .insert(&issue_event, TEST_QUEUE_LIMITS)
            .await
            .expect("issue event inserts");
        assert_eq!(
            store.claim_next(None).await.expect("issue claims"),
            Some(issue_event.clone())
        );
        let new_alias_event = InboundEvent::alias_example("pull-3");
        assert_eq!(
            store
                .link_conversation(
                    &new_alias_event.key.conversation,
                    &issue_event.key.conversation,
                    Agent::Codex
                )
                .await
                .expect("busy destination links"),
            AliasOutcome::Created
        );
        store
            .insert(&new_alias_event, TEST_QUEUE_LIMITS)
            .await
            .expect("alias event inserts");
        assert_eq!(
            store
                .claim_next(None)
                .await
                .expect("alias waits for destination"),
            None
        );
        store
            .finish_delivery(&issue_event.key, DeliveryOutcome::Delivered, None)
            .await
            .expect("issue delivered");
        assert_eq!(
            store.claim_next(None).await.expect("alias claims"),
            Some(new_alias_event)
        );
    }

    #[tokio::test]
    async fn links_copy_only_the_requested_agents_chat() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store opens");
        let issue = InboundEvent::alias_example("issue-1").key.conversation;
        let pull_request = InboundEvent::alias_example("pull-2").key.conversation;
        let codex = SessionTarget::alias_target();
        let claude = SessionTarget {
            agent: "claude".to_owned(),
            chat_id: "claude-chat".to_owned(),
            ..codex.clone()
        };
        store
            .bind_conversation(&issue, &codex)
            .await
            .expect("issue binds to Codex");
        assert_eq!(
            store
                .link_conversation(&pull_request, &issue, Agent::Claude)
                .await
                .expect("issue has no Claude chat"),
            AliasOutcome::MissingDestination
        );
        store
            .bind_conversation(&issue, &claude)
            .await
            .expect("issue binds to Claude");
        assert_eq!(
            store
                .link_conversation(&pull_request, &issue, Agent::Claude)
                .await
                .expect("Claude alias creates"),
            AliasOutcome::Created
        );
        assert_eq!(
            store
                .find_binding(&pull_request, Agent::Claude)
                .await
                .expect("Claude alias lookup"),
            Some(claude.clone())
        );
        assert_eq!(
            store
                .find_binding(&pull_request, Agent::Codex)
                .await
                .expect("Codex lookup"),
            None
        );
        let mut busy = InboundEvent::alias_example("pull-3");
        busy.options.agent = Agent::Claude;
        store
            .insert(&busy, TEST_QUEUE_LIMITS)
            .await
            .expect("Claude request inserts");
        store.claim_next(None).await.expect("Claude request claims");
        let busy_pull = busy.key.conversation;
        assert_eq!(
            store
                .link_conversation(&busy_pull, &issue, Agent::Claude)
                .await
                .expect("in-flight Claude request refused"),
            AliasOutcome::UnresolvedDelivery
        );
        assert_eq!(
            store
                .link_conversation(&busy_pull, &issue, Agent::Codex)
                .await
                .expect("Codex alias creates"),
            AliasOutcome::Created
        );
        assert_eq!(
            store
                .find_binding(&busy_pull, Agent::Codex)
                .await
                .expect("Codex alias lookup"),
            Some(codex)
        );
    }
}
