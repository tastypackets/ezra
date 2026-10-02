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
