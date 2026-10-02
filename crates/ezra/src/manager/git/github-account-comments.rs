use std::num::NonZeroU64;
use std::process::Stdio;
use std::time::Duration;

use ezra::inbound::github::{
    AccountComment, AccountCommentSource, AccountCommentsPage, CommentAuthor, IssueComment,
    IssueContext, Repository,
};
use serde::Deserialize;
use time::OffsetDateTime;
use tokio::io::AsyncWriteExt;

use super::github_comments::GitHubOutputExt;
use super::{GitError, GitTools};

const COMMENTS_QUERY: &str = r#"
query AccountComments($cursor: String) {
  viewer {
    databaseId login
    issueComments(first: 100, after: $cursor, orderBy: {field: UPDATED_AT, direction: DESC}) {
      nodes {
        fullDatabaseId createdAt updatedAt body url
        repository { databaseId nameWithOwner }
        issue { number url title body }
        pullRequest { number url title body }
      }
      pageInfo { hasNextPage endCursor }
    }
  }
}
"#;

#[derive(Deserialize)]
struct Response {
    data: Option<Data>,
    #[serde(default)]
    errors: Vec<serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
struct Data {
    viewer: Viewer,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Viewer {
    database_id: NonZeroU64,
    login: String,
    issue_comments: Connection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    nodes: Vec<Comment>,
    page_info: PageInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comment {
    #[serde(deserialize_with = "Comment::deserialize_id")]
    full_database_id: NonZeroU64,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
    body: String,
    repository: CommentRepository,
    issue: Option<Discussion>,
    pull_request: Option<Discussion>,
}

impl Comment {
    fn deserialize_id<'de, Deserializer: serde::Deserializer<'de>>(
        deserializer: Deserializer,
    ) -> Result<NonZeroU64, Deserializer::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Identifier {
            Number(NonZeroU64),
            String(String),
        }
        match Identifier::deserialize(deserializer)? {
            Identifier::Number(identifier) => Ok(identifier),
            Identifier::String(identifier) => identifier.parse().map_err(serde::de::Error::custom),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentRepository {
    database_id: NonZeroU64,
    name_with_owner: String,
}

#[derive(Deserialize)]
struct Discussion {
    number: NonZeroU64,
    url: String,
    title: String,
    body: Option<String>,
}

impl Response {
    fn into_page(self, observed_at: OffsetDateTime) -> Result<AccountCommentsPage, GitError> {
        if !self.errors.is_empty() {
            return Err(
                std::io::Error::other("GitHub comment query returned partial errors").into(),
            );
        }
        let viewer = self
            .data
            .ok_or_else(|| std::io::Error::other("GitHub comment query has no data"))?
            .viewer;
        let author = CommentAuthor {
            id: viewer.database_id,
            login: viewer.login,
        };
        let page_info = viewer.issue_comments.page_info;
        let next_cursor = if page_info.has_next_page {
            Some(
                page_info
                    .end_cursor
                    .filter(|cursor| !cursor.is_empty())
                    .ok_or_else(|| {
                        std::io::Error::other("GitHub comment query has no next cursor")
                    })?,
            )
        } else {
            None
        };
        let mut comments = Vec::new();
        for node in viewer.issue_comments.nodes {
            let discussion = match (node.issue, node.pull_request) {
                (Some(issue), Some(pull_request))
                    if issue.number == pull_request.number && issue.url == pull_request.url =>
                {
                    pull_request
                }
                (Some(discussion), None) | (None, Some(discussion)) => discussion,
                _ => {
                    return Err(std::io::Error::other(
                        "GitHub comment has no unique parent discussion",
                    )
                    .into());
                }
            };
            GitTools::validate_repository(&node.repository.name_with_owner)?;
            comments.push(AccountComment {
                repository: Repository {
                    id: node.repository.database_id,
                    full_name: node.repository.name_with_owner,
                },
                comment: IssueComment {
                    id: node.full_database_id,
                    body: Some(node.body),
                    user: Some(author.clone()),
                    created_at: node.created_at,
                    updated_at: node.updated_at,
                },
                issue: IssueContext {
                    number: discussion.number,
                    title: discussion.title,
                    body: discussion.body,
                    html_url: discussion.url,
                },
            });
        }
        Ok(AccountCommentsPage {
            author,
            observed_at,
            comments,
            next_cursor,
        })
    }
}

impl AccountCommentSource for GitTools {
    async fn account_comments(
        &self,
        cursor: Option<&str>,
    ) -> Result<AccountCommentsPage, GitError> {
        let payload = serde_json::to_vec(&serde_json::json!({
            "query": COMMENTS_QUERY, "variables": {"cursor": cursor}
        }))
        .expect("GitHub comment query serializes");
        let mut command = self.gh();
        command
            .args([
                "api",
                &self.hostname_argument(),
                "graphql",
                "--method",
                "POST",
                "--include",
                "--input",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            let mut child = command.spawn()?;
            child
                .stdin
                .take()
                .expect("piped comment query")
                .write_all(&payload)
                .await?;
            let mut output = child.wait_with_output().await?;
            if !output.status.success() {
                return output
                    .decode_github::<serde::de::IgnoredAny>("read account comments")
                    .map(|_| unreachable!("failed command cannot decode"));
            }
            let response = std::str::from_utf8(&output.stdout).map_err(std::io::Error::other)?;
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
            output.stdout = body.as_bytes().to_vec();
            output
                .decode_github::<Response>("read account comments")?
                .into_page(observed_at)
        })
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "GitHub account comments timed out",
            )
        })?;
        if result.is_err() {
            self.github_identity.lock().await.user = None;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response() -> serde_json::Value {
        json!({"data":{"viewer":{"databaseId":1,"login":"author","future":true,
            "issueComments":{"nodes":[{"fullDatabaseId":"4294967297","body":"/ezra fix",
                "createdAt":"2026-10-01T10:00:00Z","updatedAt":"2026-10-01T11:00:00Z",
                "repository":{"databaseId":7,"nameWithOwner":"owner/repo"},
                "issue":{"number":42,"url":"https://github.com/owner/repo/pull/42","title":"Fix","body":"Description"},
                "pullRequest":{"number":42,"url":"https://github.com/owner/repo/pull/42","title":"Fix","body":"Description"},
                "future":{"unknown":true}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}})
    }

    #[test]
    fn pull_request_comments_accept_both_parent_fields_and_large_string_ids() {
        let parsed: Response = serde_json::from_value(response()).expect("response");
        let page = parsed.into_page(OffsetDateTime::UNIX_EPOCH).expect("page");
        assert_eq!(page.comments[0].comment.id.get(), 4294967297);
        assert_eq!(
            page.comments[0].issue.html_url,
            "https://github.com/owner/repo/pull/42"
        );
        assert_eq!(page.author.id.get(), 1);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn partial_errors_missing_cursors_and_inconsistent_parents_reject_entire_page() {
        for failure in ["errors", "cursor", "parent"] {
            let mut payload = response();
            match failure {
                "errors" => {
                    payload["errors"] = json!([{"message":"private repository unavailable"}])
                }
                "cursor" => {
                    payload["data"]["viewer"]["issueComments"]["pageInfo"]["hasNextPage"] =
                        json!(true)
                }
                "parent" => {
                    payload["data"]["viewer"]["issueComments"]["nodes"][0]["pullRequest"]["number"] =
                        json!(43)
                }
                _ => unreachable!("known fixture"),
            }
            let parsed: Response = serde_json::from_value(payload).expect("response");
            assert!(
                parsed.into_page(OffsetDateTime::UNIX_EPOCH).is_err(),
                "{failure}"
            );
        }
    }
}
