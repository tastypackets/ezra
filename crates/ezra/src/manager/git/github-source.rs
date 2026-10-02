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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use ezra::inbound::github::CommentStatus;

    use super::*;

    struct StatusFixture {
        _directory: tempfile::TempDir,
        tools: GitTools,
    }

    impl StatusFixture {
        fn new(body: &str) -> Self {
            let directory = tempfile::tempdir().expect("fixture");
            let mut tools = GitTools::under(directory.path());
            std::fs::create_dir_all(&tools.gh_config_directory).expect("GitHub configuration");
            let executable = directory.path().join("fake-gh");
            std::fs::write(
                &executable,
                r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$GH_CONFIG_DIR/requests"
case "$*" in
  *" /user "*) printf '{"id":1,"login":"author","future":true}' ;;
  *"--method GET"*)
    if [ -f "$GH_CONFIG_DIR/read-error" ]; then exit 1; fi
    if [ -f "$GH_CONFIG_DIR/read-delay" ]; then exec sleep 10; fi
    cat "$GH_CONFIG_DIR/comment.json" ;;
  *"--method PATCH"*)
    cat > "$GH_CONFIG_DIR/payload.json"
    if [ -f "$GH_CONFIG_DIR/write-delay" ]; then exec sleep 10; fi
    if [ -f "$GH_CONFIG_DIR/write-error" ]; then exit 1; fi
    printf '{"future":{"result":true}}' ;;
  *) exit 1 ;;
esac
"#,
            )
            .expect("fake executable");
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
                .expect("executable permissions");
            tools.github_executable = Some(executable);
            let fixture = Self {
                _directory: directory,
                tools,
            };
            fixture.set_comment(serde_json::json!({
                "id": 10, "body": body, "user": {"id": 1, "login": "author"}, "future": [true]
            }));
            fixture
        }

        fn set_comment(&self, comment: serde_json::Value) {
            std::fs::write(
                self.tools.gh_config_directory.join("comment.json"),
                serde_json::to_vec(&comment).expect("comment JSON"),
            )
            .expect("comment response");
        }

        fn reference(&self) -> CommentReference<'_> {
            CommentReference {
                repository: "owner/repo",
                comment_id: NonZeroU64::new(10).expect("comment"),
                author_id: NonZeroU64::new(1).expect("author"),
            }
        }
    }

    #[tokio::test]
    async fn every_status_attempt_reads_the_latest_body_before_writing() {
        let fixture = StatusFixture::new("/ezra original");
        let failed_write = fixture.tools.gh_config_directory.join("write-error");
        std::fs::write(&failed_write, "").expect("failed PATCH");
        let footer = StatusFooter {
            status: CommentStatus::Delivered,
            chat_name: Some("owner/repo#42: Fix the crash"),
        };
        assert!(
            fixture
                .tools
                .update_comment_status(fixture.reference(), &footer)
                .await
                .is_err()
        );
        std::fs::remove_file(failed_write).expect("allow PATCH");
        fixture.set_comment(serde_json::json!({
            "id": 10, "body": "/ezra latest user edit", "user": {"id":1,"login":"renamed"}
        }));
        fixture
            .tools
            .update_comment_status(fixture.reference(), &footer)
            .await
            .expect("fresh attempt");
        let payload: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.tools.gh_config_directory.join("payload.json"))
                .expect("payload"),
        )
        .expect("payload JSON");
        assert_eq!(payload["body"], footer.apply("/ezra latest user edit"));
        let requests = std::fs::read_to_string(fixture.tools.gh_config_directory.join("requests"))
            .expect("requests");
        let comment_methods: Vec<_> = requests
            .lines()
            .filter(|line| !line.contains(" /user "))
            .map(|line| {
                if line.contains("--method GET") {
                    "GET"
                } else {
                    "PATCH"
                }
            })
            .collect();
        assert_eq!(comment_methods, ["GET", "PATCH", "GET", "PATCH"]);
    }

    #[tokio::test]
    async fn unchanged_status_reads_but_does_not_patch() {
        let footer = StatusFooter {
            status: CommentStatus::Received,
            chat_name: None,
        };
        let fixture = StatusFixture::new(&footer.apply("/ezra"));
        fixture
            .tools
            .update_comment_status(fixture.reference(), &footer)
            .await
            .expect("no-op");
        assert!(
            !fixture
                .tools
                .gh_config_directory
                .join("payload.json")
                .exists()
        );
    }

    #[tokio::test]
    async fn failed_reads_and_mismatched_comments_cannot_write() {
        for comment in [
            serde_json::json!({"id":11,"body":"/ezra","user":{"id":1,"login":"author"}}),
            serde_json::json!({"id":10,"body":"/ezra","user":{"id":2,"login":"other"}}),
            serde_json::json!({"id":10,"body":"/ezra","user":null}),
            serde_json::json!({"id":10,"body":null,"user":{"id":1,"login":"author"}}),
        ] {
            let fixture = StatusFixture::new("/ezra");
            fixture.set_comment(comment);
            assert!(
                fixture
                    .tools
                    .update_comment_status(
                        fixture.reference(),
                        &StatusFooter {
                            status: CommentStatus::Delivered,
                            chat_name: None,
                        }
                    )
                    .await
                    .is_err()
            );
            assert!(
                !fixture
                    .tools
                    .gh_config_directory
                    .join("payload.json")
                    .exists()
            );
        }
        let fixture = StatusFixture::new("/ezra");
        std::fs::write(fixture.tools.gh_config_directory.join("read-error"), "")
            .expect("failed GET");
        assert!(
            fixture
                .tools
                .update_comment_status(
                    fixture.reference(),
                    &StatusFooter {
                        status: CommentStatus::Delivered,
                        chat_name: None,
                    }
                )
                .await
                .is_err()
        );
        assert!(
            !fixture
                .tools
                .gh_config_directory
                .join("payload.json")
                .exists()
        );
    }

    #[tokio::test]
    async fn another_signed_in_account_cannot_read_or_update_the_status_comment() {
        let fixture = StatusFixture::new("/ezra");
        let reference = CommentReference {
            author_id: NonZeroU64::new(2).expect("another account"),
            ..fixture.reference()
        };
        assert!(
            fixture
                .tools
                .react(reference, CommentStatus::Delivered)
                .await
                .is_err()
        );
        assert!(
            fixture
                .tools
                .update_comment_status(
                    reference,
                    &StatusFooter {
                        status: CommentStatus::Delivered,
                        chat_name: None,
                    }
                )
                .await
                .is_err()
        );
        let requests = std::fs::read_to_string(fixture.tools.gh_config_directory.join("requests"))
            .expect("requests");
        assert!(requests.lines().all(|line| line.contains(" /user ")));
        assert!(
            !fixture
                .tools
                .gh_config_directory
                .join("payload.json")
                .exists()
        );
    }

    #[tokio::test]
    async fn status_update_has_an_overall_five_second_timeout() {
        for delayed_phase in ["read-delay", "write-delay"] {
            let fixture = StatusFixture::new("/ezra");
            std::fs::write(fixture.tools.gh_config_directory.join(delayed_phase), "")
                .expect("slow request");
            let error = fixture
                .tools
                .update_comment_status(
                    fixture.reference(),
                    &StatusFooter {
                        status: CommentStatus::Delivered,
                        chat_name: None,
                    },
                )
                .await
                .expect_err("overall timeout");
            assert!(
                matches!(error, GitError::Io(error) if error.kind() == std::io::ErrorKind::TimedOut)
            );
            assert_eq!(
                fixture
                    .tools
                    .gh_config_directory
                    .join("payload.json")
                    .exists(),
                delayed_phase == "write-delay"
            );
        }
    }

    #[test]
    fn source_requests_use_the_configured_host_and_explicit_get() {
        let directory = tempfile::tempdir().expect("configuration directory");
        let tools = GitTools::under(directory.path());
        let command = tools.source_command("/repos/owner/repository");
        assert_eq!(
            command.as_std().get_args().collect::<Vec<_>>(),
            [
                "api",
                "--hostname=github.com",
                "/repos/owner/repository",
                "--method",
                "GET",
                "--header",
                "Accept: application/vnd.github.raw+json",
            ]
        );
    }
}
