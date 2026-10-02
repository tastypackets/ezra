use std::num::NonZeroU64;
use std::process::Stdio;

use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use super::github_comments::GitHubOutputExt;
use super::{GitError, GitTools};

const LINKS_QUERY: &str = r#"
query DiscussionLinks($owner: String!, $name: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    databaseId
    issueOrPullRequest(number: $number) {
      __typename
      ... on Issue {
        number
        links: closedByPullRequestsReferences(first: 100, after: $cursor, includeClosedPrs: true) {
          nodes { number repository { databaseId } }
          pageInfo { hasNextPage endCursor }
        }
      }
      ... on PullRequest {
        number
        links: closingIssuesReferences(first: 100, after: $cursor) {
          nodes { number repository { databaseId } }
          pageInfo { hasNextPage endCursor }
        }
      }
    }
  }
}
"#;

#[derive(Deserialize)]
struct LinksResponse {
    data: Option<LinksData>,
    #[serde(default)]
    errors: Vec<serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
struct LinksData {
    repository: Option<LinksRepository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinksRepository {
    database_id: NonZeroU64,
    issue_or_pull_request: Option<Discussion>,
}

#[derive(Deserialize)]
struct Discussion {
    #[serde(rename = "__typename")]
    kind: DiscussionKind,
    number: NonZeroU64,
    links: LinksConnection,
}

#[derive(Deserialize)]
enum DiscussionKind {
    Issue,
    PullRequest,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinksConnection {
    nodes: Vec<LinkedDiscussion>,
    page_info: LinksPage,
}

#[derive(Deserialize)]
pub(super) struct LinkedDiscussion {
    pub number: NonZeroU64,
    pub repository: LinkedRepository,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LinkedRepository {
    pub database_id: NonZeroU64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinksPage {
    has_next_page: bool,
    end_cursor: Option<String>,
}

impl LinksResponse {
    fn into_page(
        self,
        repository_id: NonZeroU64,
        number: NonZeroU64,
    ) -> Result<(Vec<LinkedDiscussion>, Option<String>), GitError> {
        if !self.errors.is_empty() {
            return Err(
                std::io::Error::other("GitHub discussion links returned GraphQL errors").into(),
            );
        }
        let repository = self
            .data
            .and_then(|data| data.repository)
            .ok_or_else(|| std::io::Error::other("GitHub discussion links have no repository"))?;
        let discussion = repository.issue_or_pull_request.ok_or_else(|| {
            std::io::Error::other("GitHub discussion links have no issue or pull request")
        })?;
        if repository.database_id != repository_id
            || discussion.number != number
            || matches!(discussion.kind, DiscussionKind::Unknown)
        {
            return Err(std::io::Error::other(
                "GitHub discussion links do not match the requested resource",
            )
            .into());
        }
        let page = discussion.links.page_info;
        let cursor = if page.has_next_page {
            Some(
                page.end_cursor
                    .filter(|cursor| !cursor.is_empty())
                    .ok_or_else(|| {
                        std::io::Error::other("GitHub discussion links have no next cursor")
                    })?,
            )
        } else {
            None
        };
        Ok((discussion.links.nodes, cursor))
    }
}

impl GitTools {
    pub(super) async fn discussion_links_page(
        &self,
        repository: &str,
        repository_id: NonZeroU64,
        number: NonZeroU64,
        cursor: Option<&str>,
    ) -> Result<(Vec<LinkedDiscussion>, Option<String>), GitError> {
        let (owner, name) = repository.split_once('/').expect("validated repository");
        let payload = serde_json::to_vec(&serde_json::json!({
            "query": LINKS_QUERY,
            "variables": {"owner": owner, "name": name, "number": number, "cursor": cursor}
        }))
        .expect("GitHub link query serializes");
        let mut command = self.gh();
        command
            .args([
                "api",
                &self.hostname_argument(),
                "graphql",
                "--method",
                "POST",
                "--input",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        child
            .stdin
            .take()
            .expect("piped query input")
            .write_all(&payload)
            .await?;
        let response: LinksResponse = child
            .wait_with_output()
            .await?
            .decode_github("read discussion links")?;
        response.into_page(repository_id, number)
    }
}
