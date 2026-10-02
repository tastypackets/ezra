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
