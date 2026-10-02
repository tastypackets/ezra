use std::num::NonZeroU64;

mod polling;
mod reader;
mod source;
mod status;
mod trigger;
pub use polling::{CommentPageResult, GitHubSettings};
pub use reader::{CommentQuery, CommentReader};
pub use source::{
    AccountComment, AccountCommentSource, AccountCommentsPage, GitHubSource, IssueContext,
    Repository, RepositoryComment, RepositoryCommentsQuery, RepositorySnapshot,
};
pub use status::{CommentReference, CommentStatus, StatusFooter};

use serde::Deserialize;
use time::OffsetDateTime;

use super::{
    ConversationKey, EventKey, InboundEvent, InboundSettings, InvalidEvent, Shortcut, ShortcutError,
};

pub const REPLY_MARKER: &str = "<!-- ezra:inbound -->";

#[derive(Debug, Deserialize)]
pub struct IssueComment {
    pub id: NonZeroU64,
    pub body: Option<String>,
    pub user: Option<CommentAuthor>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CommentAuthor {
    pub id: NonZeroU64,
    pub login: String,
}

#[derive(Debug, thiserror::Error)]
pub enum InvalidComment {
    #[error("GitHub host must be a hostname")]
    Host,
    #[error("GitHub comment has no author")]
    MissingAuthor,
    #[error(transparent)]
    Event(#[from] InvalidEvent),
}

impl IssueComment {
    pub fn matching_shortcut<'settings>(
        &self,
        settings: &'settings InboundSettings,
        authenticated_user_id: NonZeroU64,
    ) -> Result<Option<(&'settings str, &'settings Shortcut)>, ShortcutError> {
        if self
            .user
            .as_ref()
            .is_none_or(|author| author.id != authenticated_user_id)
        {
            return Ok(None);
        }
        let Some(body) = self.body.as_deref() else {
            return Ok(None);
        };
        if body.contains(REPLY_MARKER) {
            return Ok(None);
        }
        settings.match_shortcut(&StatusFooter::strip(body))
    }

    /// The caller supplies the repository and issue from which this comment was fetched.
    pub fn into_event(
        self,
        host: &str,
        repository_id: NonZeroU64,
        issue_number: NonZeroU64,
    ) -> Result<InboundEvent, InvalidComment> {
        let host = host.trim_end_matches('.');
        if host.is_empty()
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|character| character.is_ascii_alphanumeric() || character == b'-')
            })
        {
            return Err(InvalidComment::Host);
        }
        let author = self.user.ok_or(InvalidComment::MissingAuthor)?;
        let event = InboundEvent {
            key: EventKey {
                conversation: ConversationKey {
                    source: format!("github:{}", host.to_ascii_lowercase()),
                    subject: format!("{repository_id}/{issue_number}"),
                },
                id: self.id.to_string(),
            },
            new_chat: false,
            options: Default::default(),
            chat_name: None,
            source_url: None,
            initial_context: None,
            actor: author.id.to_string(),
            created_at: self.created_at,
            message: self.body.unwrap_or_default(),
        };
        event.validate()?;
        Ok(event)
    }
}
