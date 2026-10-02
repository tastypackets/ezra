use std::os::unix::fs::PermissionsExt;

use ezra::inbound::github::GitHubSource;
use serde_json::{Value, json};

use super::*;

impl LinksResponse {
    fn example(kind: &str, number: u64, nodes: Value, next: Option<&str>) -> Value {
        json!({"data":{"repository":{"databaseId":7,"issueOrPullRequest":{
            "__typename":kind,"number":number,"links":{"nodes":nodes,
                "pageInfo":{"hasNextPage":next.is_some(),"endCursor":next},"future":true}
        },"future":true}},"future":true})
    }
}

#[test]
fn complete_empty_results_and_unknown_fields_are_valid() {
    for kind in ["Issue", "PullRequest"] {
        let response: LinksResponse =
            serde_json::from_value(LinksResponse::example(kind, 42, json!([]), None))
                .expect("response");
        let (links, cursor) = response
            .into_page(
                NonZeroU64::new(7).expect("repo"),
                NonZeroU64::new(42).expect("number"),
            )
            .expect("complete links");
        assert!(links.is_empty());
        assert!(cursor.is_none());
    }
}

#[test]
fn partial_errors_missing_resources_unknown_kinds_and_bad_cursors_are_not_empty_results() {
    let original = LinksResponse::example("Issue", 42, json!([]), None);
    let mut invalid = Vec::new();
    let mut value = original.clone();
    value["errors"] = json!([{"message":"Unavailable"}]);
    invalid.push(value);
    let mut value = original.clone();
    value["data"]["repository"] = Value::Null;
    invalid.push(value);
    let mut value = original.clone();
    value["data"]["repository"]["databaseId"] = json!(8);
    invalid.push(value);
    let mut value = original.clone();
    value["data"]["repository"]["issueOrPullRequest"] = Value::Null;
    invalid.push(value);
    invalid.push(LinksResponse::example(
        "FutureDiscussion",
        42,
        json!([]),
        None,
    ));
    invalid.push(LinksResponse::example("Issue", 43, json!([]), None));
    for cursor in [Value::Null, json!("")] {
        let mut value = original.clone();
        value["data"]["repository"]["issueOrPullRequest"]["links"]["pageInfo"] =
            json!({"hasNextPage":true,"endCursor":cursor});
        invalid.push(value);
    }
    for value in invalid {
        let response: LinksResponse = serde_json::from_value(value).expect("response decodes");
        assert!(
            response
                .into_page(
                    NonZeroU64::new(7).expect("repo"),
                    NonZeroU64::new(42).expect("number")
                )
                .is_err()
        );
    }
    for nodes in [
        json!([null]),
        json!([{"number":1,"repository":null}]),
        json!([{"number":0,"repository":{"databaseId":7}}]),
    ] {
        assert!(
            serde_json::from_value::<LinksResponse>(LinksResponse::example(
                "Issue", 42, nodes, None
            ))
            .is_err()
        );
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    tools: GitTools,
}

impl Fixture {
    fn new(pages: &[Value]) -> Self {
        let directory = tempfile::tempdir().expect("fixture");
        let mut tools = GitTools::under(directory.path());
        std::fs::create_dir_all(&tools.gh_config_directory).expect("configuration directory");
        tools.host = crate::manager::github_host::GitHubHost::from_value("enterprise.example");
        let executable = directory.path().join("fake-gh");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$GH_CONFIG_DIR/requests"
if [ -f "$GH_CONFIG_DIR/delay" ]; then exec sleep 60; fi
count=0
if [ -f "$GH_CONFIG_DIR/count" ]; then count=$(cat "$GH_CONFIG_DIR/count"); fi
cat > "$GH_CONFIG_DIR/query-$count.json"
cat "$GH_CONFIG_DIR/page-$count.json"
printf '%s' "$((count + 1))" > "$GH_CONFIG_DIR/count"
"#,
        )
        .expect("fake executable");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("permissions");
        tools.github_executable = Some(executable);
        for (index, page) in pages.iter().enumerate() {
            std::fs::write(
                tools.gh_config_directory.join(format!("page-{index}.json")),
                serde_json::to_vec(page).expect("page JSON"),
            )
            .expect("page response");
        }
        Self {
            _directory: directory,
            tools,
        }
    }
}

#[tokio::test]
async fn pagination_collects_and_deduplicates_direct_links_on_the_configured_host() {
    let node = json!({"number":12,"repository":{"databaseId":7},"future":true});
    let fixture = Fixture::new(&[
        LinksResponse::example("PullRequest", 42, json!([node.clone()]), Some("next")),
        LinksResponse::example(
            "PullRequest",
            42,
            json!([node, {"number":13,"repository":{"databaseId":8}}]),
            None,
        ),
    ]);
    let mut links = fixture
        .tools
        .linked_discussions(
            "owner/repo",
            NonZeroU64::new(7).expect("repo"),
            NonZeroU64::new(42).expect("number"),
        )
        .await
        .expect("all links");
    links.sort_by(|left, right| left.subject.cmp(&right.subject));
    assert_eq!(
        links
            .iter()
            .map(|link| link.subject.as_str())
            .collect::<Vec<_>>(),
        ["7/12", "8/13"]
    );
    assert!(
        links
            .iter()
            .all(|link| link.source == "github:enterprise.example")
    );
    let query: Value = serde_json::from_slice(
        &std::fs::read(fixture.tools.gh_config_directory.join("query-1.json")).expect("query"),
    )
    .expect("query JSON");
    assert_eq!(query["variables"]["cursor"], "next");
    assert_eq!(query["variables"]["number"], 42);
    assert!(
        query["query"]
            .as_str()
            .expect("query text")
            .contains("includeClosedPrs: true")
    );
    let requests = std::fs::read_to_string(fixture.tools.gh_config_directory.join("requests"))
        .expect("requests");
    assert!(requests.lines().all(|request| {
        request.contains("--hostname=enterprise.example graphql --method POST --input -")
    }));
}

#[tokio::test]
async fn repeated_cursors_page_limits_and_late_failures_discard_partial_links() {
    let node = json!({"number":12,"repository":{"databaseId":7}});
    let repeated = LinksResponse::example("Issue", 42, json!([node.clone()]), Some("repeat"));
    let fixture = Fixture::new(&[repeated.clone(), repeated]);
    assert!(
        fixture
            .tools
            .linked_discussions(
                "owner/repo",
                NonZeroU64::new(7).expect("repo"),
                NonZeroU64::new(42).expect("number")
            )
            .await
            .is_err()
    );
    let pages: Vec<_> = (0..10)
        .map(|page| {
            LinksResponse::example(
                "Issue",
                42,
                json!([node.clone()]),
                Some(&format!("page-{page}")),
            )
        })
        .collect();
    let fixture = Fixture::new(&pages);
    assert!(
        fixture
            .tools
            .linked_discussions(
                "owner/repo",
                NonZeroU64::new(7).expect("repo"),
                NonZeroU64::new(42).expect("number")
            )
            .await
            .is_err()
    );
    let mut partial = LinksResponse::example("Issue", 42, json!([]), None);
    partial["errors"] = json!([{"message":"Unavailable"}]);
    let fixture = Fixture::new(&[
        LinksResponse::example("Issue", 42, json!([node]), Some("next")),
        partial,
    ]);
    assert!(
        fixture
            .tools
            .linked_discussions(
                "owner/repo",
                NonZeroU64::new(7).expect("repo"),
                NonZeroU64::new(42).expect("number")
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_complete_tenth_page_is_accepted() {
    let pages: Vec<_> = (0..10)
        .map(|page| {
            let cursor = format!("page-{page}");
            LinksResponse::example(
                "Issue",
                42,
                json!([]),
                (page != 9).then_some(cursor.as_str()),
            )
        })
        .collect();
    let fixture = Fixture::new(&pages);
    let links = fixture
        .tools
        .linked_discussions(
            "owner/repo",
            NonZeroU64::new(7).expect("repo"),
            NonZeroU64::new(42).expect("number"),
        )
        .await
        .expect("tenth page completes");
    assert!(links.is_empty());
}

#[tokio::test]
async fn a_slow_subprocess_cannot_hold_the_lookup_past_its_timeout() {
    let fixture = Fixture::new(&[]);
    std::fs::write(fixture.tools.gh_config_directory.join("delay"), "").expect("delay marker");
    let outcome = fixture
        .tools
        .linked_discussions(
            "owner/repo",
            NonZeroU64::new(7).expect("repo"),
            NonZeroU64::new(42).expect("number"),
        )
        .await;
    assert!(
        matches!(outcome, Err(GitError::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut)
    );
}
