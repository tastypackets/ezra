use std::future::Future;
use std::num::{NonZeroU32, NonZeroU64};

use time::OffsetDateTime;

use super::{CommentAuthor, IssueComment};

#[derive(Debug)]
pub struct CommentQuery {
    pub repository: String,
    pub issue_number: NonZeroU64,
    pub page: NonZeroU32,
    pub since: Option<OffsetDateTime>,
}

pub trait CommentReader {
    type Error;

    fn authenticated_user(&self)
    -> impl Future<Output = Result<CommentAuthor, Self::Error>> + Send;

    fn read_comments(
        &self,
        query: &CommentQuery,
    ) -> impl Future<Output = Result<Vec<IssueComment>, Self::Error>> + Send;
}
