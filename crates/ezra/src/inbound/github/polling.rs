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
