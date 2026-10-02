use std::path::Path;

use super::{EventStore, StoreError};
use crate::inbound::{ConversationKey, MAX_IDENTIFIER_BYTES};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTarget {
    pub host_id: String,
    pub agent: String,
    pub chat_id: String,
    /// Absolute path on the owning host.
    pub workspace: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindOutcome {
    Created,
    AlreadyBound,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidBinding {
    #[error("{field} is empty")]
    Empty { field: &'static str },
    #[error("{field} exceeds {limit} bytes")]
    TooLong { field: &'static str, limit: usize },
    #[error("{field} contains whitespace or control characters")]
    InvalidIdentifier { field: &'static str },
    #[error("workspace must be an absolute path without control characters")]
    InvalidWorkspace,
}

impl SessionTarget {
    pub(super) fn validate_binding(
        &self,
        conversation: &ConversationKey,
    ) -> Result<(), InvalidBinding> {
        for (field, identifier) in [
            ("source", conversation.source.as_str()),
            ("subject", conversation.subject.as_str()),
            ("host id", self.host_id.as_str()),
            ("agent", self.agent.as_str()),
            ("chat id", self.chat_id.as_str()),
        ] {
            if identifier.is_empty() {
                return Err(InvalidBinding::Empty { field });
            }
            if identifier.len() > MAX_IDENTIFIER_BYTES {
                return Err(InvalidBinding::TooLong {
                    field,
                    limit: MAX_IDENTIFIER_BYTES,
                });
            }
            if identifier
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
            {
                return Err(InvalidBinding::InvalidIdentifier { field });
            }
        }
        if self.workspace.len() > 4096 {
            return Err(InvalidBinding::TooLong {
                field: "workspace",
                limit: 4096,
            });
        }
        if !Path::new(&self.workspace).is_absolute() || self.workspace.chars().any(char::is_control)
        {
            return Err(InvalidBinding::InvalidWorkspace);
        }
        Ok(())
    }
}

impl EventStore {
    pub async fn bind_conversation(
        &self,
        conversation: &ConversationKey,
        target: &SessionTarget,
    ) -> Result<BindOutcome, StoreError> {
        target.validate_binding(conversation)?;
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let already_bound = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM inbound_conversations WHERE source = ?1 AND subject = ?2
               ) AS "already_bound!: bool""#,
            conversation.source,
            conversation.subject,
        )
        .fetch_one(&mut *transaction)
        .await?;
        if already_bound {
            transaction.rollback().await?;
            return Ok(BindOutcome::AlreadyBound);
        }
        let session_id = sqlx::query_scalar!(
            "INSERT INTO inbound_sessions (host_id, agent, chat_id, workspace)
             VALUES (?1, ?2, ?3, ?4) RETURNING id",
            target.host_id,
            target.agent,
            target.chat_id,
            target.workspace,
        )
        .fetch_one(&mut *transaction)
        .await?;
        sqlx::query!(
            "INSERT INTO inbound_conversations (source, subject, session_id) VALUES (?1, ?2, ?3)",
            conversation.source,
            conversation.subject,
            session_id,
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(BindOutcome::Created)
    }

    pub async fn find_binding(
        &self,
        conversation: &ConversationKey,
    ) -> Result<Option<SessionTarget>, StoreError> {
        let target = sqlx::query_as!(
            SessionTarget,
            "SELECT sessions.host_id, sessions.agent, sessions.chat_id, sessions.workspace
             FROM inbound_conversations AS conversations
             JOIN inbound_sessions AS sessions ON sessions.id = conversations.session_id
             WHERE conversations.source = ?1 AND conversations.subject = ?2",
            conversation.source,
            conversation.subject,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl SessionTarget {
        fn binding_example(host_id: &str, agent: &str) -> Self {
            Self {
                host_id: host_id.to_owned(),
                agent: agent.to_owned(),
                chat_id: "native-chat-1".to_owned(),
                workspace: "/home/dev/projects/work with spaces".to_owned(),
            }
        }
    }

    #[tokio::test]
    async fn binding_survives_restart_and_cannot_be_silently_replaced() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let conversation = ConversationKey {
            source: "github:github.com".to_owned(),
            subject: "repo/issue-1".to_owned(),
        };
        let original_target = SessionTarget::binding_example("host-a", "codex");
        let store = EventStore::open(&path).await.expect("store opens");
        assert_eq!(
            store
                .find_binding(&conversation)
                .await
                .expect("missing lookup"),
            None
        );
        assert_eq!(
            store
                .bind_conversation(&conversation, &original_target)
                .await
                .expect("binding creates"),
            BindOutcome::Created
        );
        store.pool.close().await;

        let store = EventStore::open(&path).await.expect("store reopens");
        let mut replacement = SessionTarget::binding_example("host-b", "claude");
        replacement.chat_id = "different-chat".to_owned();
        replacement.workspace = "/home/dev/projects/other".to_owned();
        for target in [&original_target, &replacement] {
            assert_eq!(
                store
                    .bind_conversation(&conversation, target)
                    .await
                    .expect("existing binding is retained"),
                BindOutcome::AlreadyBound
            );
        }
        assert_eq!(
            store
                .find_binding(&conversation)
                .await
                .expect("binding lookup"),
            Some(original_target)
        );
    }

    #[tokio::test]
    async fn concurrent_bindings_keep_the_winning_destination() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let first_store = EventStore::open(&path).await.expect("first store opens");
        let second_store = EventStore::open(&path).await.expect("second store opens");
        let conversation = ConversationKey {
            source: "github:github.com".to_owned(),
            subject: "repo/issue-1".to_owned(),
        };
        let first_target = SessionTarget::binding_example("host-a", "codex");
        let second_target = SessionTarget::binding_example("host-b", "claude");
        let (first_result, second_result) = tokio::join!(
            first_store.bind_conversation(&conversation, &first_target),
            second_store.bind_conversation(&conversation, &second_target)
        );
        let first_outcome = first_result.expect("first binding succeeds");
        let second_outcome = second_result.expect("second binding succeeds");
        let (expected_target, unused_target) = match (first_outcome, second_outcome) {
            (BindOutcome::Created, BindOutcome::AlreadyBound) => (first_target, second_target),
            (BindOutcome::AlreadyBound, BindOutcome::Created) => (second_target, first_target),
            outcomes => panic!("expected one winning binding, got {outcomes:?}"),
        };
        assert_eq!(
            first_store
                .find_binding(&conversation)
                .await
                .expect("first lookup"),
            Some(expected_target.clone())
        );
        assert_eq!(
            second_store
                .find_binding(&conversation)
                .await
                .expect("second lookup"),
            Some(expected_target)
        );
        let other_conversation = ConversationKey {
            subject: "repo/issue-2".to_owned(),
            ..conversation
        };
        assert_eq!(
            first_store
                .bind_conversation(&other_conversation, &unused_target)
                .await
                .expect("losing attempt did not reserve its target"),
            BindOutcome::Created,
        );
    }

    #[tokio::test]
    async fn sources_and_native_session_namespaces_remain_independent() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store opens");
        for (source, host_id, agent) in [
            ("github:github.com", "host-a", "codex"),
            ("github:enterprise.example", "host-b", "codex"),
            ("api:local", "host-a", "claude"),
        ] {
            let conversation = ConversationKey {
                source: source.to_owned(),
                subject: "repo/issue-1".to_owned(),
            };
            let target = SessionTarget::binding_example(host_id, agent);
            assert_eq!(
                store
                    .bind_conversation(&conversation, &target)
                    .await
                    .expect("independent binding creates"),
                BindOutcome::Created
            );
            assert_eq!(
                store
                    .find_binding(&conversation)
                    .await
                    .expect("binding lookup"),
                Some(target)
            );
        }
    }

    #[tokio::test]
    async fn invalid_destinations_are_not_persisted() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store opens");
        let conversation = ConversationKey {
            source: "github:github.com".to_owned(),
            subject: "repo/issue-1".to_owned(),
        };
        for target in [
            SessionTarget {
                host_id: String::new(),
                ..SessionTarget::binding_example("host-a", "codex")
            },
            SessionTarget {
                agent: "bad agent".to_owned(),
                ..SessionTarget::binding_example("host-a", "codex")
            },
            SessionTarget {
                chat_id: "x".repeat(513),
                ..SessionTarget::binding_example("host-a", "codex")
            },
            SessionTarget {
                workspace: "relative/path".to_owned(),
                ..SessionTarget::binding_example("host-a", "codex")
            },
            SessionTarget {
                workspace: "/home/dev/projects/bad\0path".to_owned(),
                ..SessionTarget::binding_example("host-a", "codex")
            },
        ] {
            assert!(matches!(
                store.bind_conversation(&conversation, &target).await,
                Err(StoreError::InvalidBinding(_))
            ));
            assert_eq!(
                store
                    .find_binding(&conversation)
                    .await
                    .expect("invalid binding lookup"),
                None
            );
        }
    }
}
