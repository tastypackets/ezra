use std::num::{NonZeroU32, NonZeroU64};

use serde::Deserialize;
use time::OffsetDateTime;

use super::{
    CommentReader, CommentReference, CommentStatus, InvalidComment, IssueComment, StatusFooter,
};
use crate::inbound::{ConversationKey, InboundEvent, MAX_MESSAGE_BYTES};

#[derive(Debug, Deserialize)]
pub struct Repository {
    pub id: NonZeroU64,
    pub full_name: String,
}

pub struct RepositorySnapshot {
    pub repository: Repository,
    pub observed_at: OffsetDateTime,
}

#[derive(Debug, Deserialize)]
pub struct RepositoryComment {
    #[serde(flatten)]
    pub comment: IssueComment,
    pub issue_url: String,
}

impl RepositoryComment {
    pub fn issue_number(&self) -> Option<NonZeroU64> {
        let (prefix, number) = self.issue_url.rsplit_once('/')?;
        prefix.ends_with("/issues").then_some(())?;
        number.parse().ok()
    }
}

#[derive(Debug, Deserialize)]
pub struct IssueContext {
    pub number: NonZeroU64,
    pub title: String,
    pub body: Option<String>,
    pub html_url: String,
}

impl IssueContext {
    pub fn event(
        &self,
        mut comment: IssueComment,
        host: &str,
        repository_id: NonZeroU64,
        repository_name: &str,
        trigger: &str,
    ) -> Result<InboundEvent, InvalidComment> {
        comment.body = comment
            .body
            .map(|body| StatusFooter::strip(&body).into_owned());
        let request = super::trigger::TriggerRequest::parse(
            comment.body.as_deref().unwrap_or_default(),
            trigger,
        );
        let mut event = comment.into_event(host, repository_id, self.number)?;
        event.new_chat = request.new_chat;
        let request = request.message;
        let title: String = self
            .title
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        let name = format!("{repository_name}#{}: {title}", self.number);
        event.chat_name = Some(name[..name.floor_char_boundary(512)].to_owned());
        event.source_url = Some(format!("{}#issuecomment-{}", self.html_url, event.key.id));
        let body = self.body.as_deref().unwrap_or_default();
        let title = &self.title[..self.title.floor_char_boundary(2048)];
        let request = if request.is_empty() {
            "Take action on this issue or pull request using the context below."
        } else {
            &request
        };
        let instruction = if event.new_chat {
            "Handle this request in a fresh conversation."
        } else {
            "Continue this discussion in this session."
        };
        event.message = format!(
            "GitHub request from the signed-in user. {instruction}\n\nRequest:\n{request}\n\nIssue or pull request: {}\nTitle: {title}",
            self.html_url,
        );
        let remaining = MAX_MESSAGE_BYTES
            .saturating_sub(event.message.len())
            .saturating_sub("\n\nDescription:\n".len())
            .min(32 * 1024);
        let body = &body[..body.floor_char_boundary(remaining)];
        event.initial_context = Some(format!("\n\nDescription:\n{body}"));
        event.validate()?;
        Ok(event)
    }
}

#[derive(Debug)]
pub struct RepositoryCommentsQuery {
    pub repository: String,
    pub since: OffsetDateTime,
    pub page: NonZeroU32,
}

pub struct AccountComment {
    pub repository: Repository,
    pub comment: IssueComment,
    pub issue: IssueContext,
}

pub struct AccountCommentsPage {
    pub author: super::CommentAuthor,
    pub observed_at: OffsetDateTime,
    pub comments: Vec<AccountComment>,
    pub next_cursor: Option<String>,
}

pub trait AccountCommentSource: GitHubSource {
    fn account_comments(
        &self,
        cursor: Option<&str>,
    ) -> impl Future<Output = Result<AccountCommentsPage, Self::Error>> + Send;
}

pub trait GitHubSource: CommentReader {
    fn update_comment_status(
        &self,
        reference: CommentReference<'_>,
        footer: &StatusFooter<'_>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn react(
        &self,
        reference: CommentReference<'_>,
        status: CommentStatus,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn repository(
        &self,
        repository: &str,
    ) -> impl Future<Output = Result<RepositorySnapshot, Self::Error>> + Send;

    fn repository_comments(
        &self,
        query: &RepositoryCommentsQuery,
    ) -> impl Future<Output = Result<Vec<RepositoryComment>, Self::Error>> + Send;

    fn linked_discussions(
        &self,
        repository: &str,
        repository_id: NonZeroU64,
        number: NonZeroU64,
    ) -> impl Future<Output = Result<Vec<ConversationKey>, Self::Error>> + Send;

    fn issue(
        &self,
        repository: &str,
        number: NonZeroU64,
    ) -> impl Future<Output = Result<IssueContext, Self::Error>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bare_trigger_includes_issue_context_and_keeps_the_comment_identity() {
        let comment: IssueComment = serde_json::from_value(json!({
            "id": 10, "body": "/ezra", "user": {"id": 2, "login": "author"},
            "created_at": "2026-09-30T12:00:00Z", "updated_at": "2026-09-30T12:00:00Z"
        }))
        .expect("comment");
        let issue = IssueContext {
            number: NonZeroU64::new(42).expect("issue"),
            title: "Fix the crash".to_owned(),
            body: Some("Reproduction steps".to_owned()),
            html_url: "https://github.com/owner/repo/issues/42".to_owned(),
        };
        let event = issue
            .event(
                comment,
                "github.com",
                NonZeroU64::new(3).expect("repo"),
                "owner/repo",
                "/ezra",
            )
            .expect("event");
        assert_eq!(event.key.id, "10");
        assert_eq!(event.key.conversation.subject, "3/42");
        assert_eq!(
            event.chat_name.as_deref(),
            Some("owner/repo#42: Fix the crash")
        );
        assert_eq!(
            event.source_url.as_deref(),
            Some("https://github.com/owner/repo/issues/42#issuecomment-10")
        );
        assert!(event.message.contains("Take action"));
        assert!(!event.message.contains("Description:"));
        assert!(
            event
                .with_initial_context()
                .message
                .contains("Reproduction steps")
        );
        assert!(!event.message.contains("/ezra"));
    }

    #[test]
    fn bare_fresh_chat_option_keeps_context_without_claiming_to_continue() {
        let issue = IssueContext {
            number: NonZeroU64::new(42).expect("issue"),
            title: "Fix the crash".into(),
            body: Some("Reproduction steps".into()),
            html_url: "https://github.com/owner/repo/issues/42".into(),
        };
        let body = StatusFooter {
            status: CommentStatus::Received,
            chat_name: Some("old chat --new"),
        }
        .apply("/ezra --new");
        let comment = serde_json::from_value(json!({
            "id":10, "body":body, "user":{"id":2,"login":"author"},
            "created_at":"2026-09-30T12:00:00Z", "updated_at":"2026-09-30T12:00:00Z"
        }))
        .expect("comment");
        let event = issue
            .event(
                comment,
                "github.com",
                NonZeroU64::new(3).expect("repo"),
                "owner/repo",
                "/ezra",
            )
            .expect("event");
        assert!(event.new_chat);
        assert!(event.message.contains("fresh conversation"));
        assert!(event.message.contains("Take action"));
        assert!(
            event
                .with_initial_context()
                .message
                .contains("Reproduction steps")
        );
        assert!(!event.message.contains("Continue"));
        assert!(!event.message.contains("--new"));
    }

    #[test]
    fn optional_context_fits_around_a_large_request_without_splitting_unicode() {
        let request = "x".repeat(40 * 1024);
        let comment: IssueComment = serde_json::from_value(json!({
            "id": 10, "body": format!("/ezra {request}"), "user": {"id": 2, "login": "author"},
            "created_at": "2026-09-30T12:00:00Z", "updated_at": "2026-09-30T12:00:00Z"
        }))
        .expect("comment");
        let issue = IssueContext {
            number: NonZeroU64::new(42).expect("issue"),
            title: "🦦".repeat(1000),
            body: Some("🦦".repeat(20 * 1024)),
            html_url: "https://github.com/owner/repo/issues/42".to_owned(),
        };
        let event = issue
            .event(
                comment,
                "github.com",
                NonZeroU64::new(3).expect("repo"),
                "owner/repo",
                "/ezra",
            )
            .expect("fits");
        assert!(event.message.contains(&request));
        assert!(event.message_bytes() <= MAX_MESSAGE_BYTES);
        assert!(event.with_initial_context().message.contains(&request));
        assert!(event.chat_name.expect("name").len() <= 512);
    }

    #[test]
    fn status_updates_preserve_event_identity_and_stay_out_of_agent_context() {
        let issue = IssueContext {
            number: NonZeroU64::new(42).expect("issue"),
            title: "Fix the crash".into(),
            body: None,
            html_url: "https://github.com/owner/repo/issues/42".into(),
        };
        let mut original = None;
        for status in [
            super::super::CommentStatus::Received,
            super::super::CommentStatus::Delivered,
        ] {
            let body = StatusFooter {
                status,
                chat_name: Some("Another /ezra chat"),
            }
            .apply("/ezra investigate");
            let comment = serde_json::from_value(json!({
                "id": 10, "body": body, "user": {"id": 2, "login": "author"},
                "created_at": "2026-09-30T12:00:00Z", "updated_at": "2026-09-30T13:00:00Z"
            }))
            .expect("comment");
            let event = issue
                .event(
                    comment,
                    "github.com",
                    NonZeroU64::new(3).expect("repo"),
                    "owner/repo",
                    "/ezra",
                )
                .expect("event");
            assert!(!event.message.contains("ezra:status"));
            assert!(!event.message.contains("Another"));
            assert!(event.message.contains("Request:\ninvestigate"));
            if let Some(previous) = original.as_ref() {
                assert_eq!(previous, &event);
            }
            original = Some(event);
        }
    }

    #[test]
    fn repository_comments_preserve_identity_and_decode_issue_numbers() {
        let mut value = json!({
            "id": 10, "body": "/ezra", "user": {"id": 2, "login": "author"},
            "created_at": "2026-09-30T12:00:00Z", "updated_at": "2026-09-30T12:00:00Z",
            "issue_url": "https://enterprise.example/api/v3/repos/owner/repo/issues/42",
            "future": true
        });
        let comment: RepositoryComment = serde_json::from_value(value.clone()).expect("comment");
        assert_eq!(comment.issue_number().map(NonZeroU64::get), Some(42));
        assert_eq!(comment.comment.id.get(), 10);
        for invalid in [
            "https://api.github.com/repos/owner/repo/issues/0",
            "https://api.github.com/repos/owner/repo/pulls/42",
            "https://api.github.com/repos/owner/repo/issues/42?query=true",
            "unknown",
        ] {
            value["issue_url"] = json!(invalid);
            let comment: RepositoryComment =
                serde_json::from_value(value.clone()).expect("comment");
            assert!(comment.issue_number().is_none());
        }
    }
}
