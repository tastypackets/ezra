use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};

use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};

use super::{CommentQuery, CommentReader, IssueComment};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct GitHubSettings {
    pub only_added_repositories: bool,
    pub edit_comment_status: bool,
    #[serde(alias = "react_on_delivery")]
    pub react_on_status: bool,
    #[schema(value_type = u32, minimum = 1, default = 30)]
    pub poll_interval_seconds: NonZeroU32,
    #[schema(value_type = usize, minimum = 1)]
    pub max_concurrent_requests: NonZeroUsize,
}

impl Default for GitHubSettings {
    fn default() -> Self {
        Self {
            only_added_repositories: true,
            edit_comment_status: false,
            react_on_status: true,
            poll_interval_seconds: NonZeroU32::new(30).expect("thirty seconds is nonzero"),
            max_concurrent_requests: NonZeroUsize::new(4).expect("four requests is nonzero"),
        }
    }
}

#[derive(Debug)]
pub struct CommentPageResult<Error> {
    pub authenticated_user_id: NonZeroU64,
    pub query: CommentQuery,
    pub comments: Result<Vec<IssueComment>, Error>,
}

impl GitHubSettings {
    pub async fn read_comment_pages<Reader: CommentReader + Sync>(
        &self,
        reader: &Reader,
        queries: Vec<CommentQuery>,
    ) -> Result<Vec<CommentPageResult<Reader::Error>>, Reader::Error> {
        if queries.is_empty() {
            return Ok(Vec::new());
        }
        let authenticated_user_id = reader.authenticated_user().await?.id;
        Ok(stream::iter(queries)
            .map(|query| async move {
                let comments = reader.read_comments(&query).await;
                CommentPageResult {
                    authenticated_user_id,
                    query,
                    comments,
                }
            })
            .buffer_unordered(self.max_concurrent_requests.get())
            .collect()
            .await)
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    #[derive(Default)]
    struct Reader {
        identity_reads: AtomicUsize,
        identity_fails: bool,
        active: AtomicUsize,
        peak: AtomicUsize,
        requested: Mutex<Vec<String>>,
    }

    impl CommentReader for Reader {
        type Error = &'static str;

        async fn authenticated_user(&self) -> Result<super::super::CommentAuthor, Self::Error> {
            let identity = self
                .identity_reads
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1);
            if self.identity_fails {
                return Err("identity unavailable");
            }
            Ok(super::super::CommentAuthor {
                id: NonZeroU64::new(u64::try_from(identity).expect("test identity fits"))
                    .expect("positive identity"),
                login: "signed-in-user".to_owned(),
            })
        }

        async fn read_comments(
            &self,
            query: &CommentQuery,
        ) -> Result<Vec<IssueComment>, Self::Error> {
            let previous = self.active.fetch_add(1, Ordering::SeqCst);
            self.peak
                .fetch_max(previous.saturating_add(1), Ordering::SeqCst);
            self.requested
                .lock()
                .expect("request log")
                .push(query.repository.clone());
            tokio::time::sleep(Duration::from_millis(10)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            if query.repository == "owner/failing" {
                Err("read failed")
            } else {
                Ok(Vec::new())
            }
        }
    }

    impl CommentQuery {
        fn polling_example(repository: &str) -> Self {
            Self {
                repository: repository.to_owned(),
                issue_number: NonZeroU64::new(1).expect("issue"),
                page: NonZeroU32::new(1).expect("page"),
                since: None,
            }
        }
    }

    #[tokio::test]
    async fn concurrent_reads_are_bounded_and_keep_individual_failures() {
        for concurrency in [1, 2] {
            let settings = GitHubSettings {
                max_concurrent_requests: NonZeroUsize::new(concurrency)
                    .expect("positive concurrency"),
                ..GitHubSettings::default()
            };
            let reader = Reader::default();
            let results = settings
                .read_comment_pages(
                    &reader,
                    ["Owner/First", "owner/second", "owner/failing", "owner/last"]
                        .map(CommentQuery::polling_example)
                        .into(),
                )
                .await
                .expect("identity confirmed");
            assert_eq!(reader.identity_reads.load(Ordering::SeqCst), 1);
            assert_eq!(results.len(), 4);
            assert_eq!(reader.peak.load(Ordering::SeqCst), concurrency);
            assert_eq!(reader.active.load(Ordering::SeqCst), 0);
            for result in results {
                assert_eq!(result.authenticated_user_id.get(), 1);
                assert_eq!(
                    result.comments.is_err(),
                    result.query.repository == "owner/failing"
                );
                assert_eq!(result.query.issue_number.get(), 1);
            }
        }
    }

    #[tokio::test]
    async fn every_batch_asks_the_reader_for_identity_and_failed_identity_prevents_reads() {
        let settings = GitHubSettings::default();
        let reader = Reader::default();
        for expected_id in [1, 2] {
            let pages = settings
                .read_comment_pages(&reader, vec![CommentQuery::polling_example("owner/repo")])
                .await
                .expect("identity confirmed");
            assert_eq!(pages[0].authenticated_user_id.get(), expected_id);
        }
        let failing = Reader {
            identity_fails: true,
            ..Reader::default()
        };
        assert!(
            settings
                .read_comment_pages(&failing, vec![CommentQuery::polling_example("owner/repo")])
                .await
                .is_err()
        );
        assert!(failing.requested.lock().expect("requests").is_empty());
    }

    #[test]
    fn concurrency_defaults_and_rejects_zero() {
        let settings: GitHubSettings =
            serde_json::from_str(r#"{"repositories":["owner/repo"]}"#).expect("partial settings");
        assert!(settings.only_added_repositories);
        assert_eq!(settings.max_concurrent_requests.get(), 4);
        assert_eq!(settings.poll_interval_seconds.get(), 30);
        assert!(settings.react_on_status);
        assert!(!settings.edit_comment_status);
        let legacy: GitHubSettings = serde_json::from_str(
            r#"{"disabled_repositories":["owner/repo"],"comment_on_trigger":false}"#,
        )
        .expect("legacy settings");
        assert!(legacy.only_added_repositories);
        assert!(!legacy.edit_comment_status);
        let legacy: GitHubSettings =
            serde_json::from_str(r#"{"comment_on_error":true,"react_on_delivery":false}"#)
                .expect("legacy feedback");
        assert!(legacy.only_added_repositories);
        assert!(!legacy.edit_comment_status);
        assert!(!legacy.react_on_status);
        for interval in ["0", "-1", "1.5", "4294967296"] {
            assert!(
                serde_json::from_str::<GitHubSettings>(&format!(
                    r#"{{"poll_interval_seconds":{interval}}}"#
                ))
                .is_err()
            );
        }
        assert!(
            serde_json::from_str::<GitHubSettings>(r#"{"max_concurrent_requests":0}"#).is_err()
        );
    }
}
