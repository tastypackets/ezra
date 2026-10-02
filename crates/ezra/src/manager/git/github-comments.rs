use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

use ezra::inbound::github::{CommentAuthor, CommentQuery, CommentReader, IssueComment};
use serde::de::DeserializeOwned;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::process::Command;

use super::{GitError, GitHubSignIn, GitTools};
use crate::process_ext::OutputExt;

impl CommentReader for GitTools {
    type Error = GitError;

    async fn authenticated_user(&self) -> Result<CommentAuthor, GitError> {
        self.github_identity
            .lock()
            .await
            .get_or_fetch(&self.gh_config_directory.join("hosts.yml"), async {
                let mut command = self.identity_command();
                let output = tokio::time::timeout(Duration::from_secs(30), command.output())
                    .await
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::TimedOut, "GitHub identity request timed out")
                    })??;
                output.decode_github("read identity")
            })
            .await
    }

    async fn read_comments(&self, query: &CommentQuery) -> Result<Vec<IssueComment>, GitError> {
        let mut command = self.comment_command(query)?;
        let result = async {
            let output = tokio::time::timeout(Duration::from_secs(30), command.output())
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, "GitHub comment request timed out")
                })??;
            output.decode_github("read comments")
        }
        .await;
        if result.is_err() {
            self.github_identity.lock().await.user = None;
        }
        result
    }
}

#[derive(Debug, Default)]
pub(super) struct GitHubIdentity {
    pub(super) user: Option<CommentAuthor>,
    configuration: Option<(u64, i64, i64, u64)>,
}

impl GitHubIdentity {
    async fn get_or_fetch(
        &mut self,
        path: &Path,
        fetch: impl Future<Output = Result<CommentAuthor, GitError>>,
    ) -> Result<CommentAuthor, GitError> {
        let configuration = Self::configuration_stamp(path).await?;
        if configuration != self.configuration {
            self.user = None;
            self.configuration = configuration;
        }
        if let Some(user) = &self.user {
            return Ok(user.clone());
        }
        let user = fetch.await?;
        if Self::configuration_stamp(path).await? != configuration {
            return Err(
                io::Error::other("GitHub authentication changed during identity lookup").into(),
            );
        }
        self.user = Some(user.clone());
        Ok(user)
    }

    async fn configuration_stamp(path: &Path) -> io::Result<Option<(u64, i64, i64, u64)>> {
        match tokio::fs::metadata(path).await {
            Ok(metadata) => Ok(Some((
                metadata.ino(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.len(),
            ))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(super) fn observe(&mut self, sign_in: &GitHubSignIn) {
        if !sign_in.signed_in
            || self
                .user
                .as_ref()
                .is_some_and(|user| sign_in.account.as_deref() != Some(user.login.as_str()))
        {
            self.user = None;
        }
    }
}

impl GitTools {
    fn identity_command(&self) -> Command {
        let mut command = self.gh();
        command
            .args([
                "api",
                &self.hostname_argument(),
                "/user",
                "--method",
                "GET",
                "--header",
                "Accept: application/vnd.github+json",
            ])
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    pub(super) fn validate_repository(repository: &str) -> Result<(), GitError> {
        let parts: Vec<_> = repository.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|part| {
                part.is_empty()
                    || matches!(*part, "." | "..")
                    || !part.bytes().all(|character| {
                        character.is_ascii_alphanumeric() || b"-_.".contains(&character)
                    })
            })
        {
            return Err(GitError::GitHub {
                action: "read comments",
                output: "repository must be owner/name".to_owned(),
            });
        }
        Ok(())
    }

    fn comment_command(&self, query: &CommentQuery) -> Result<Command, GitError> {
        Self::validate_repository(&query.repository)?;
        let endpoint = format!(
            "/repos/{}/issues/{}/comments",
            query.repository, query.issue_number
        );
        let mut command = self.gh();
        command
            .args([
                "api",
                &self.hostname_argument(),
                &endpoint,
                "--method",
                "GET",
                "--header",
                "Accept: application/vnd.github.raw+json",
                "--raw-field",
                "per_page=100",
                "--raw-field",
                &format!("page={}", query.page),
            ])
            .stdin(Stdio::null())
            .kill_on_drop(true);
        if let Some(since) = query.since {
            let timestamp = since
                .to_offset(time::UtcOffset::UTC)
                .format(&Rfc3339)
                .map_err(io::Error::other)?;
            command.args(["--raw-field", &format!("since={timestamp}")]);
        }
        Ok(command)
    }
}

pub(super) trait GitHubOutputExt {
    fn decode_repository_snapshot(
        self,
    ) -> Result<ezra::inbound::github::RepositorySnapshot, GitError>;
    fn decode_github<Response: DeserializeOwned>(
        self,
        action: &'static str,
    ) -> Result<Response, GitError>;
}

impl GitHubOutputExt for Output {
    fn decode_repository_snapshot(
        mut self,
    ) -> Result<ezra::inbound::github::RepositorySnapshot, GitError> {
        if !self.status.success() {
            return Err(GitError::GitHub {
                action: "read repository",
                output: self.stderr_text(),
            });
        }
        let response = std::str::from_utf8(&self.stdout).map_err(std::io::Error::other)?;
        let (headers, body) = response
            .split_once("\r\n\r\n")
            .or_else(|| response.split_once("\n\n"))
            .ok_or_else(|| std::io::Error::other("GitHub response headers are missing"))?;
        let date = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("date").then_some(value.trim())
            })
            .ok_or_else(|| std::io::Error::other("GitHub response Date is missing"))?;
        let observed_at =
            OffsetDateTime::parse(date, &time::format_description::well_known::Rfc2822)
                .map_err(std::io::Error::other)?;
        self.stdout = body.as_bytes().to_vec();
        let repository = self.decode_github("read repository")?;
        Ok(ezra::inbound::github::RepositorySnapshot {
            repository,
            observed_at,
        })
    }

    fn decode_github<Response: DeserializeOwned>(
        self,
        action: &'static str,
    ) -> Result<Response, GitError> {
        if !self.status.success() {
            return Err(GitError::GitHub {
                action,
                output: self.stderr_text(),
            });
        }
        serde_json::from_slice(&self.stdout).map_err(|error| GitError::GitHub {
            action,
            output: format!("unexpected answer: {error}"),
        })
    }
}
