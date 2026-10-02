use std::num::NonZeroU64;
use std::process::Stdio;
use std::time::Duration;

use ezra::inbound::github::{
    CommentAuthor, CommentReader, CommentReference, CommentStatus, GitHubSource, IssueContext,
    RepositoryComment, RepositoryCommentsQuery, RepositorySnapshot, StatusFooter,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use time::format_description::well_known::Rfc3339;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::github_comments::GitHubOutputExt;
use super::{GitError, GitTools};

impl GitHubSource for GitTools {
    async fn update_comment_status(
        &self,
        reference: CommentReference<'_>,
        footer: &StatusFooter<'_>,
    ) -> Result<(), GitError> {
        Self::validate_repository(reference.repository)?;
        let endpoint = format!(
            "/repos/{}/issues/comments/{}",
            reference.repository, reference.comment_id
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            if self.authenticated_user().await?.id != reference.author_id {
                return Err(
                    std::io::Error::other("GitHub status author is no longer signed in").into(),
                );
            }
            let comment: StatusComment = self.read_github(self.source_command(&endpoint)).await?;
            if comment.id != reference.comment_id
                || comment
                    .user
                    .as_ref()
                    .is_none_or(|author| author.id != reference.author_id)
            {
                return Err(
                    std::io::Error::other("GitHub returned a mismatched status comment").into(),
                );
            }
            let body = comment
                .body
                .ok_or_else(|| std::io::Error::other("GitHub status comment has no body"))?;
            let updated = footer.apply(&body);
            if updated == body {
                return Ok(());
            }
            if self.authenticated_user().await?.id != reference.author_id {
                return Err(
                    std::io::Error::other("GitHub status author changed during the read").into(),
                );
            }
            self.write_github(&endpoint, "PATCH", serde_json::json!({"body": updated}))
                .await
        })
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "GitHub status update receipt is uncertain",
            )
        })?
    }

    async fn react(
        &self,
        reference: CommentReference<'_>,
        status: CommentStatus,
    ) -> Result<(), GitError> {
        Self::validate_repository(reference.repository)?;
        let content = match status {
            CommentStatus::Received => return Ok(()),
            CommentStatus::Delivered => "rocket",
            CommentStatus::Unconfirmed | CommentStatus::Failed => "confused",
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            if self.authenticated_user().await?.id != reference.author_id {
                return Err(
                    std::io::Error::other("GitHub reaction author is no longer signed in").into(),
                );
            }
            self.write_github(
                &format!(
                    "/repos/{}/issues/comments/{}/reactions",
                    reference.repository, reference.comment_id
                ),
                "POST",
                serde_json::json!({"content": content}),
            )
            .await
        })
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "GitHub reaction receipt is uncertain",
            )
        })?
    }

    async fn repository(&self, repository: &str) -> Result<RepositorySnapshot, GitError> {
        Self::validate_repository(repository)?;
        let mut command = self.source_command(&format!("/repos/{repository}"));
        command.arg("--include");
        let result = async {
            let output = tokio::time::timeout(Duration::from_secs(30), command.output())
                .await
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "GitHub repository request timed out",
                    )
                })??;
            output.decode_repository_snapshot()
        }
        .await;
        if result.is_err() {
            self.github_identity.lock().await.user = None;
        }
        result
    }

    async fn repository_comments(
        &self,
        query: &RepositoryCommentsQuery,
    ) -> Result<Vec<RepositoryComment>, GitError> {
        Self::validate_repository(&query.repository)?;
        let mut command =
            self.source_command(&format!("/repos/{}/issues/comments", query.repository));
        let since = query
            .since
            .to_offset(time::UtcOffset::UTC)
            .format(&Rfc3339)
            .map_err(std::io::Error::other)?;
        command.args([
            "--raw-field",
            "sort=updated",
            "--raw-field",
            "direction=asc",
            "--raw-field",
            "per_page=100",
            "--raw-field",
            &format!("page={}", query.page),
            "--raw-field",
            &format!("since={since}"),
        ]);
        self.read_github(command).await
    }

    async fn linked_discussions(
        &self,
        repository: &str,
        repository_id: NonZeroU64,
        number: NonZeroU64,
    ) -> Result<Vec<ezra::inbound::ConversationKey>, GitError> {
        Self::validate_repository(repository)?;
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            let mut links = std::collections::HashSet::new();
            let mut cursors = std::collections::HashSet::new();
            let mut cursor = None;
            for _ in 0..10 {
                let (nodes, next) = self
                    .discussion_links_page(repository, repository_id, number, cursor.as_deref())
                    .await?;
                for node in nodes {
                    links.insert(ezra::inbound::ConversationKey {
                        source: format!("github:{}", self.host().as_str().to_ascii_lowercase()),
                        subject: format!("{}/{}", node.repository.database_id, node.number),
                    });
                }
                let Some(next) = next else {
                    return Ok(links.into_iter().collect());
                };
                if !cursors.insert(next.clone()) {
                    return Err(std::io::Error::other(
                        "GitHub discussion links repeated a pagination cursor",
                    )
                    .into());
                }
                cursor = Some(next);
            }
            Err(
                std::io::Error::other("GitHub discussion links exceeded the pagination limit")
                    .into(),
            )
        })
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "GitHub discussion link lookup timed out",
            )
        })?;
        if result.is_err() {
            self.github_identity.lock().await.user = None;
        }
        result
    }

    async fn issue(&self, repository: &str, number: NonZeroU64) -> Result<IssueContext, GitError> {
        Self::validate_repository(repository)?;
        self.read_github(self.source_command(&format!("/repos/{repository}/issues/{number}")))
            .await
    }
}

#[derive(Deserialize)]
struct StatusComment {
    id: NonZeroU64,
    body: Option<String>,
    user: Option<CommentAuthor>,
}

impl GitTools {
    async fn write_github(
        &self,
        endpoint: &str,
        method: &str,
        body: serde_json::Value,
    ) -> Result<(), GitError> {
        let payload = serde_json::to_vec(&body).expect("GitHub feedback serializes");
        let mut command = self.gh();
        command
            .args([
                "api",
                &self.hostname_argument(),
                endpoint,
                "--method",
                method,
                "--input",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut child = command.spawn()?;
            child
                .stdin
                .take()
                .expect("piped comment input")
                .write_all(&payload)
                .await?;
            let output = child.wait_with_output().await?;
            output
                .decode_github::<serde::de::IgnoredAny>("feedback")
                .map(|_| ())
        })
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "GitHub feedback receipt is uncertain",
            )
        })?
    }

    fn source_command(&self, endpoint: &str) -> Command {
        let mut command = self.gh();
        command
            .args([
                "api",
                &self.hostname_argument(),
                endpoint,
                "--method",
                "GET",
                "--header",
                "Accept: application/vnd.github.raw+json",
            ])
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    async fn read_github<Response: DeserializeOwned>(
        &self,
        mut command: Command,
    ) -> Result<Response, GitError> {
        let result = async {
            let output = tokio::time::timeout(Duration::from_secs(30), command.output())
                .await
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "GitHub source request timed out",
                    )
                })??;
            output.decode_github("read source")
        }
        .await;
        if result.is_err() {
            self.github_identity.lock().await.user = None;
        }
        result
    }
}
