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

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use time::macros::datetime;

    use super::*;

    impl IssueComment {
        fn example_json() -> Value {
            json!({
                "id": 456, "body": "@codex fix this\n\n```rust\nrun();\n```\n🦦",
                "user": {"id": 789, "login": "author", "type": "FutureUserKind", "futureField": true},
                "created_at": "2026-09-30T12:00:00Z", "updated_at": "2026-09-30T13:00:00Z",
                "issue_url": "https://api.github.com/repos/owner/repository/issues/87",
                "futureField": {"nested": [1, 2]},
            })
        }
    }

    #[test]
    fn shortcuts_match_raw_comments_but_ignore_generated_replies() {
        let settings = InboundSettings::default();
        for body in [
            "Please /ezra fix it",
            "> /ezra quoted",
            "`/ezra`",
            "```\n/ezra\n```",
        ] {
            let mut input = IssueComment::example_json();
            input["body"] = json!(body);
            let comment: IssueComment = serde_json::from_value(input).expect("comment");
            assert_eq!(
                comment
                    .matching_shortcut(&settings, NonZeroU64::new(789).expect("signed-in user"))
                    .expect("match")
                    .expect("shortcut")
                    .0,
                "/ezra"
            );
            assert_eq!(comment.body.as_deref(), Some(body));
        }
        for body in [
            "/ezra done\n<!-- ezra:inbound -->",
            "<!-- ezra:inbound -->\n/ezra done",
        ] {
            let mut input = IssueComment::example_json();
            input["body"] = json!(body);
            let comment: IssueComment = serde_json::from_value(input).expect("comment");
            assert!(
                comment
                    .matching_shortcut(&settings, NonZeroU64::new(789).expect("signed-in user"))
                    .expect("match")
                    .is_none()
            );
        }
    }

    #[test]
    fn authorization_uses_user_ids_and_requires_an_author() {
        let mut input = IssueComment::example_json();
        input["body"] = json!("/ezra help");
        let mut comment: IssueComment = serde_json::from_value(input).expect("comment");
        let settings = InboundSettings::default();
        let signed_in = NonZeroU64::new(789).expect("signed-in user");
        comment.user.as_mut().expect("author").login = "renamed-user".to_owned();
        assert!(
            comment
                .matching_shortcut(&settings, signed_in)
                .expect("match")
                .is_some()
        );
        comment.user.as_mut().expect("author").id = NonZeroU64::new(790).expect("another user");
        assert!(
            comment
                .matching_shortcut(&settings, signed_in)
                .expect("match")
                .is_none()
        );
        comment.user = None;
        assert!(
            comment
                .matching_shortcut(&settings, signed_in)
                .expect("match")
                .is_none()
        );
    }

    #[test]
    fn footer_chat_names_do_not_match_shortcuts() {
        let mut settings = InboundSettings::default();
        settings
            .shortcuts
            .insert("/other".into(), Default::default());
        for request in ["/ezra fix it", "An ordinary comment"] {
            let body = StatusFooter {
                status: CommentStatus::Received,
                chat_name: Some("owner/repo#42: /other /ezra"),
            }
            .apply(request);
            let mut input = IssueComment::example_json();
            input["body"] = json!(body);
            let comment: IssueComment = serde_json::from_value(input).expect("comment");
            let shortcut = comment
                .matching_shortcut(&settings, NonZeroU64::new(789).expect("user"))
                .expect("footer excluded");
            assert_eq!(
                shortcut.map(|matched| matched.0),
                request.starts_with("/ezra").then_some("/ezra")
            );
        }
    }

    #[test]
    fn normalization_preserves_markdown_and_uses_ids_and_original_creation_time() {
        let input = IssueComment::example_json();
        let comment: IssueComment = serde_json::from_value(input.clone()).expect("comment decodes");
        assert_eq!(comment.updated_at, datetime!(2026-09-30 13:00 UTC));
        let event = comment
            .into_event(
                "GitHub.COM.",
                NonZeroU64::new(1234).expect("repository"),
                NonZeroU64::new(87).expect("issue"),
            )
            .expect("event normalizes");
        assert_eq!(
            event.key.conversation,
            ConversationKey {
                source: "github:github.com".to_owned(),
                subject: "1234/87".to_owned()
            }
        );
        assert_eq!(event.key.id, "456");
        assert_eq!(event.actor, "789");
        assert_eq!(event.created_at, datetime!(2026-09-30 12:00 UTC));
        assert_eq!(event.message, input["body"].as_str().expect("body"));
    }

    #[test]
    fn edits_and_renames_keep_event_identity_but_other_hosts_do_not() {
        let repository_id = NonZeroU64::new(1234).expect("repository");
        let issue_number = NonZeroU64::new(87).expect("issue");
        let mut input = IssueComment::example_json();
        let original = serde_json::from_value::<IssueComment>(input.clone())
            .expect("decode")
            .into_event("github.com", repository_id, issue_number)
            .expect("normalize");
        input["body"] = json!("edited message");
        input["updated_at"] = json!("2026-10-01T12:00:00Z");
        input["user"]["login"] = json!("renamed-author");
        input["issue_url"] = json!("https://api.github.com/repos/owner/renamed/issues/87");
        let edited = serde_json::from_value::<IssueComment>(input.clone())
            .expect("decode")
            .into_event("github.com", repository_id, issue_number)
            .expect("normalize");
        assert_eq!(original.key, edited.key);
        assert_eq!(original.actor, edited.actor);
        assert_eq!(original.created_at, edited.created_at);
        let enterprise = serde_json::from_value::<IssueComment>(input)
            .expect("decode")
            .into_event("github.example.com", repository_id, issue_number)
            .expect("normalize");
        assert_ne!(original.key, enterprise.key);
    }

    #[test]
    fn invalid_comments_are_rejected_without_panicking() {
        let repository_id = NonZeroU64::new(1234).expect("repository");
        let issue_number = NonZeroU64::new(87).expect("issue");
        for (field, value) in [
            ("body", Value::Null),
            ("body", json!(" ")),
            (
                "body",
                json!("x".repeat(super::super::MAX_MESSAGE_BYTES + 1)),
            ),
            ("user", Value::Null),
        ] {
            let mut input = IssueComment::example_json();
            input[field] = value;
            assert!(
                serde_json::from_value::<IssueComment>(input)
                    .expect("nullable fields decode")
                    .into_event("github.com", repository_id, issue_number)
                    .is_err()
            );
        }
        for host in [
            "",
            "https://github.com",
            "github.com/repo",
            "user@github.com",
            "bad host",
            "bad..host",
        ] {
            let comment: IssueComment =
                serde_json::from_value(IssueComment::example_json()).expect("decode");
            assert!(matches!(
                comment.into_event(host, repository_id, issue_number),
                Err(InvalidComment::Host)
            ));
        }
        for (field, value) in [("id", json!(0)), ("created_at", json!("bad timestamp"))] {
            let mut input = IssueComment::example_json();
            input[field] = value;
            assert!(serde_json::from_value::<IssueComment>(input).is_err());
        }
    }
}
