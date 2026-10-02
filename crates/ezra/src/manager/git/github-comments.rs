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

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64};
    use std::os::unix::process::ExitStatusExt;

    use serde_json::json;
    use time::macros::datetime;

    use super::*;
    use crate::manager::github_host::GitHubHost;

    #[test]
    fn repository_snapshots_require_a_valid_github_date_and_tolerate_unknown_fields() {
        for separator in ["\r\n", "\n"] {
            let response = format!(
                "HTTP/2.0 200 OK{separator}dAtE: Wed, 30 Sep 2026 12:00:00 GMT{separator}Future-Header: value{separator}{separator}{{\"id\":7,\"full_name\":\"owner/repo\",\"futureField\":true}}"
            );
            let output = Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: response.into_bytes(),
                stderr: Vec::new(),
            };
            let snapshot = output.decode_repository_snapshot().expect("snapshot");
            assert_eq!(snapshot.observed_at, datetime!(2026-09-30 12:00 UTC));
            assert_eq!(snapshot.repository.id.get(), 7);
        }
        for response in [
            "HTTP/2.0 200 OK\r\n\r\n{\"id\":7,\"full_name\":\"owner/repo\"}",
            "HTTP/2.0 200 OK\r\nDate: invalid\r\n\r\n{\"id\":7,\"full_name\":\"owner/repo\"}",
            "{\"id\":7,\"full_name\":\"owner/repo\"}",
        ] {
            let output = Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: response.as_bytes().to_vec(),
                stderr: Vec::new(),
            };
            assert!(output.decode_repository_snapshot().is_err());
        }
    }

    #[tokio::test]
    async fn identity_is_shared_and_concurrent_reads_fetch_once() {
        let directory = tempfile::tempdir().expect("configuration directory");
        let tools = GitTools::under(directory.path());
        let cloned_tools = tools.clone();
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let lookup = async |tools: &GitTools| {
            tools
                .github_identity
                .lock()
                .await
                .get_or_fetch(&directory.path().join("hosts.yml"), async {
                    reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    Ok(CommentAuthor {
                        id: NonZeroU64::new(123).expect("user id"),
                        login: "author".to_owned(),
                    })
                })
                .await
                .expect("identity")
        };
        let (first, second) = tokio::join!(lookup(&tools), lookup(&cloned_tools));
        assert_eq!(first.id, second.id);
        assert_eq!(lookup(&tools).await.id, first.id);
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_lookup_can_retry_and_auth_changes_invalidate_identity() {
        let directory = tempfile::tempdir().expect("configuration directory");
        let mut identity = GitHubIdentity::default();
        assert!(
            identity
                .get_or_fetch(&directory.path().join("hosts.yml"), async {
                    Err(io::Error::other("identity unavailable").into())
                })
                .await
                .is_err()
        );
        assert!(identity.user.is_none());
        let user = identity
            .get_or_fetch(&directory.path().join("hosts.yml"), async {
                Ok(CommentAuthor {
                    id: NonZeroU64::new(123).expect("user id"),
                    login: "author".to_owned(),
                })
            })
            .await
            .expect("retry succeeds");
        let mut sign_in = GitHubSignIn {
            signed_in: true,
            account: Some("author".to_owned()),
            ..GitHubSignIn::default()
        };
        identity.observe(&sign_in);
        assert!(identity.user.is_some());
        sign_in.account = Some("different-author".to_owned());
        identity.observe(&sign_in);
        assert!(identity.user.is_none());
        identity.user = Some(user);
        identity.observe(&GitHubSignIn::default());
        assert!(identity.user.is_none());
    }

    #[tokio::test]
    async fn configuration_changes_invalidate_without_a_status_check() {
        let directory = tempfile::tempdir().expect("configuration directory");
        let path = directory.path().join("hosts.yml");
        let mut identity = GitHubIdentity::default();
        for (contents, user_id) in [("account-a", 1), ("account-b-with-new-token", 2)] {
            tokio::fs::write(&path, contents)
                .await
                .expect("configuration changes");
            let user = identity
                .get_or_fetch(&path, async {
                    Ok(CommentAuthor {
                        id: NonZeroU64::new(user_id).expect("user id"),
                        login: contents.to_owned(),
                    })
                })
                .await
                .expect("identity refreshes");
            assert_eq!(user.id.get(), user_id);
        }
        tokio::fs::remove_file(&path)
            .await
            .expect("configuration removed");
        assert!(
            identity
                .get_or_fetch(&path, async { Err(io::Error::other("signed out").into()) })
                .await
                .is_err()
        );
        assert!(identity.user.is_none());
    }

    #[tokio::test]
    async fn switching_accounts_during_lookup_does_not_cache_the_old_identity() {
        let directory = tempfile::tempdir().expect("configuration directory");
        let path = directory.path().join("hosts.yml");
        let mut identity = GitHubIdentity::default();
        let result = identity
            .get_or_fetch(&path, async {
                tokio::fs::write(&path, "new-account")
                    .await
                    .expect("account switches");
                Ok(CommentAuthor {
                    id: NonZeroU64::new(1).expect("old user id"),
                    login: "old".to_owned(),
                })
            })
            .await;
        assert!(result.is_err());
        assert!(identity.user.is_none());
        let refreshed = identity
            .get_or_fetch(&path, async {
                Ok(CommentAuthor {
                    id: NonZeroU64::new(2).expect("new user id"),
                    login: "new".to_owned(),
                })
            })
            .await
            .expect("next lookup succeeds");
        assert_eq!(refreshed.id.get(), 2);
    }

    trait CommentQueryExt {
        fn example() -> Self;
    }

    impl CommentQueryExt for CommentQuery {
        fn example() -> Self {
            Self {
                repository: "owner/repository".to_owned(),
                issue_number: NonZeroU64::new(87).expect("issue number"),
                page: NonZeroU32::new(2).expect("page number"),
                since: Some(datetime!(2026-09-30 12:00 +02:00)),
            }
        }
    }

    #[test]
    fn reads_one_page_with_the_existing_host_and_configuration() {
        let directory = tempfile::tempdir().expect("temporary configuration");
        let mut tools = GitTools::under(directory.path());
        tools.host = GitHubHost::from_value("github.example.com");
        let command = tools
            .comment_command(&CommentQuery::example())
            .expect("command builds");
        let command = command.as_std();
        assert_eq!(command.get_program(), "gh");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "api",
                "--hostname=github.example.com",
                "/repos/owner/repository/issues/87/comments",
                "--method",
                "GET",
                "--header",
                "Accept: application/vnd.github.raw+json",
                "--raw-field",
                "per_page=100",
                "--raw-field",
                "page=2",
                "--raw-field",
                "since=2026-09-30T10:00:00Z",
            ]
        );
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == "GH_CONFIG_DIR"
                    && value == Some(directory.path().join("gh").as_os_str()))
        );
    }

    #[test]
    fn identity_uses_the_same_authenticated_host_and_requires_a_valid_id() {
        let directory = tempfile::tempdir().expect("configuration directory");
        let mut tools = GitTools::under(directory.path());
        tools.host = GitHubHost::from_value("github.example.com");
        let command = tools.identity_command();
        assert_eq!(
            command.as_std().get_args().collect::<Vec<_>>(),
            [
                "api",
                "--hostname=github.example.com",
                "/user",
                "--method",
                "GET",
                "--header",
                "Accept: application/vnd.github+json"
            ]
        );
        for response in [
            json!({"login":"author"}),
            json!({"id":0,"login":"author"}),
            json!({"message":"not authenticated"}),
        ] {
            assert!(
                Output {
                    status: std::process::ExitStatus::from_raw(0),
                    stdout: serde_json::to_vec(&response).expect("response"),
                    stderr: Vec::new()
                }
                .decode_github::<CommentAuthor>("read identity")
                .is_err()
            );
        }
        let response = json!({"id":123,"login":"author","futureField":true});
        let identity: CommentAuthor = Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: serde_json::to_vec(&response).expect("response"),
            stderr: Vec::new(),
        }
        .decode_github("read identity")
        .expect("identity decodes");
        assert_eq!(identity.id.get(), 123);
    }

    #[test]
    fn endpoint_components_cannot_change_the_request_path_or_query() {
        let directory = tempfile::tempdir().expect("temporary configuration");
        let tools = GitTools::under(directory.path());
        for repository in [
            "",
            "owner",
            "owner/repo/extra",
            "owner/..",
            "owner/repo?other=1",
            "owner/{repo}",
            "owner/repo\n",
        ] {
            let query = CommentQuery {
                repository: repository.to_owned(),
                ..CommentQuery::example()
            };
            assert!(tools.comment_command(&query).is_err(), "{repository}");
        }
    }

    #[test]
    fn response_errors_are_not_mistaken_for_an_empty_page() {
        for (status, stdout) in [
            (256, b"[]".to_vec()),
            (0, b"invalid json".to_vec()),
            (0, b"{}".to_vec()),
            (0, b"[{\"id\":1}]".to_vec()),
        ] {
            assert!(
                Output {
                    status: std::process::ExitStatus::from_raw(status),
                    stdout,
                    stderr: b"request failed".to_vec()
                }
                .decode_github::<Vec<IssueComment>>("read comments")
                .is_err()
            );
        }
        let response = json!([{
            "id": 1, "body": "/ezra help", "user": {"id": 2, "login": "author", "future": true},
            "created_at": "2026-09-30T12:00:00Z", "updated_at": "2026-09-30T12:00:00Z", "future": true,
        }]);
        let comments = Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: serde_json::to_vec(&response).expect("response encodes"),
            stderr: Vec::new(),
        }
        .decode_github::<Vec<IssueComment>>("read comments")
        .expect("comments decode");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].body.as_deref(), Some("/ezra help"));
    }
}
