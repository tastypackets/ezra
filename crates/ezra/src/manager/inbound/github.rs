mod account;
mod linking;
use linking::GitHubDiscussion;

#[cfg(test)]
use std::num::NonZeroU32;
use std::num::NonZeroU64;
use std::time::Duration;

#[cfg(test)]
use ezra::inbound::github::RepositoryCommentsQuery;
use ezra::inbound::github::{CommentReference, CommentStatus, GitHubSource, StatusFooter};
use ezra::inbound::store::{DeliveryOutcome, InsertOutcome, SessionTarget};
use ezra::inbound::{
    ConversationKey, EventKey, InboundEvent, InboundSettings, MessageSendError, MessageSender,
};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::{InboundRuntime, RoutingWake};
use crate::manager::folders::ProjectsDirectory;
use crate::manager::git::GitTools;

#[derive(Debug, thiserror::Error)]
pub(super) enum PollError {
    #[error(transparent)]
    Git(#[from] crate::manager::git::GitError),
    #[error(transparent)]
    Store(#[from] ezra::inbound::store::StoreError),
    #[error("inbound queue is full")]
    QueueFull,
    #[error("agent is unavailable")]
    Unavailable,
    #[error("GitHub identity changed during polling")]
    IdentityChanged,
    #[cfg(test)]
    #[error("GitHub comment pagination exceeded the supported page number")]
    PageOverflow,
    #[error("GitHub comments changed during pagination")]
    ScanChanged,
    #[cfg(test)]
    #[error("some GitHub issue contexts or links could not be fetched")]
    Incomplete,
    #[cfg(test)]
    #[error("GitHub routing is waiting for earlier work")]
    RoutingDeferred,
}

pub(super) enum Admission {
    Accepted,
    Failed,
    Expired,
    Uncertain,
    Deferred,
}

#[derive(Clone)]
pub(super) struct GitHubFeedback {
    repository: String,
    key: EventKey,
    author_id: NonZeroU64,
    status: CommentStatus,
}

impl GitHubFeedback {
    fn from_event(event: &InboundEvent, host: &str) -> Option<Self> {
        if event.key.conversation.source != format!("github:{}", host.to_ascii_lowercase()) {
            return None;
        }
        let (resource_url, comment_id) =
            event.source_url.as_deref()?.rsplit_once("#issuecomment-")?;
        if comment_id != event.key.id || comment_id.parse::<NonZeroU64>().is_err() {
            return None;
        }
        let resource: axum::http::Uri = resource_url.parse().ok()?;
        if resource.scheme_str() != Some("https")
            || !resource.host()?.eq_ignore_ascii_case(host)
            || resource.port_u16().is_some()
            || resource.query().is_some()
        {
            return None;
        }
        let segments: Vec<_> = resource.path().trim_start_matches('/').split('/').collect();
        let [owner, repository, "issues" | "pull", number] = segments.as_slice() else {
            return None;
        };
        let number: NonZeroU64 = number.parse().ok()?;
        if event.key.conversation.subject.rsplit_once('/')?.1 != number.to_string() {
            return None;
        }
        Some(Self {
            repository: format!("{owner}/{repository}"),
            key: event.key.clone(),
            author_id: event.actor.parse().ok()?,
            status: CommentStatus::Received,
        })
    }
}

impl InboundRuntime {
    pub(super) async fn expire_waiting_requests(
        &self,
        settings: &ezra::inbound::InboundSettings,
    ) -> Result<(), ezra::inbound::store::StoreError> {
        let expired = self
            .store
            .expire_waiting(settings.waiting_cutoff(OffsetDateTime::now_utc()))
            .await?;
        if !expired.is_empty() {
            for key in &expired {
                if let Some(event) = self.store.get(key).await?
                    && let Some(host) = key.conversation.source.strip_prefix("github:")
                    && let Some(mut feedback) = GitHubFeedback::from_event(&event, host)
                {
                    feedback.status = ezra::inbound::github::CommentStatus::Failed;
                    self.queue_github_feedback(feedback).await;
                }
            }
            if let Err(error) = self.github_routing_sender.send(RoutingWake::Sweep) {
                tracing::error!(%error, "GitHub routing receiver is closed");
            }
            tracing::info!(expired = expired.len(), "expired waiting GitHub requests");
        }
        Ok(())
    }

    pub(super) async fn poll_github(
        &self,
        source: &GitTools,
        projects: &ProjectsDirectory,
        _sender: &impl MessageSender,
    ) {
        loop {
            let settings = self.settings.lock().await.inbound.clone();
            match projects
                .github_workspaces(source.host().as_str(), &settings.github)
                .await
            {
                Ok(workspaces) => {
                    if let Err(error) = self
                        .scan_account(source, source.host().as_str(), &settings, &workspaces)
                        .await
                    {
                        tracing::warn!(%error, "GitHub account trigger scan will retry");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "could not find local GitHub trigger workspaces")
                }
            }
            self.wait_for_github_scan().await;
        }
    }

    async fn wait_for_github_scan(&self) {
        let finished = tokio::time::Instant::now();
        loop {
            let interval = Duration::from_secs(u64::from(
                self.settings
                    .lock()
                    .await
                    .inbound
                    .github
                    .poll_interval_seconds
                    .get(),
            ));
            let remaining = interval.saturating_sub(finished.elapsed());
            if remaining.is_zero() {
                return;
            }
            tokio::time::sleep(remaining.min(Duration::from_secs(1))).await;
        }
    }

    #[cfg(test)]
    async fn scan_repository(
        &self,
        source: &(impl GitHubSource<Error = crate::manager::git::GitError> + Sync),
        host: &str,
        sender: &impl MessageSender,
        settings: &InboundSettings,
        repository_name: &str,
        workspace: &str,
    ) -> Result<(), PollError> {
        // Footer edits change the timestamps used by scan verification.
        let _scan = self.github_scan.read().await;
        let snapshot = source.repository(repository_name).await?;
        let scan_started = snapshot.observed_at;
        let repository = snapshot.repository;
        let scope = ConversationKey {
            source: format!("github:{}", host.to_ascii_lowercase()),
            subject: repository_name.to_owned(),
        };
        let mut checkpoint = self.store.source_checkpoint(&scope, scan_started).await?;
        if checkpoint.scanned_through > scan_started || checkpoint.activated_at > scan_started {
            self.store
                .clamp_source_checkpoint(&scope, scan_started)
                .await?;
            checkpoint.scanned_through = checkpoint.scanned_through.min(scan_started);
            checkpoint.activated_at = checkpoint.activated_at.min(scan_started);
            tracing::warn!(%repository_name, "GitHub polling checkpoint was ahead of the source clock and has been corrected");
        }
        let since = checkpoint
            .scanned_through
            .saturating_sub(time::Duration::seconds(5))
            .max(settings.retention_cutoff(scan_started))
            .max(checkpoint.activated_at);
        let identity = source.authenticated_user().await?;
        if !repository.full_name.eq_ignore_ascii_case(repository_name) {
            tracing::warn!(%repository_name, "GitHub repository was renamed, update its trigger setting");
            return Ok(());
        }
        let retries = self
            .retry_github_routing(source, sender, settings, &repository, host, workspace)
            .await?;
        let mut page = NonZeroU32::new(1).expect("first page");
        let mut fingerprint = Sha256::new();
        let mut incomplete = retries.incomplete;
        let mut routing_deferred = retries.deferred;
        loop {
            let comments = source
                .repository_comments(&RepositoryCommentsQuery {
                    repository: repository_name.to_owned(),
                    since,
                    page,
                })
                .await?;
            if source.authenticated_user().await?.id != identity.id {
                return Err(PollError::IdentityChanged);
            }
            let count = comments.len();
            let mut beyond_window = false;
            for fetched in comments {
                if fetched.comment.updated_at > scan_started {
                    beyond_window = true;
                    break;
                }
                fingerprint.update(fetched.comment.id.get().to_be_bytes());
                fingerprint.update(
                    fetched
                        .comment
                        .updated_at
                        .unix_timestamp_nanos()
                        .to_be_bytes(),
                );
                if fetched.comment.updated_at < checkpoint.activated_at
                    || fetched.comment.created_at < settings.retention_cutoff(scan_started)
                {
                    continue;
                }
                let Some(number) = fetched.issue_number() else {
                    tracing::warn!(comment_id = %fetched.comment.id, "GitHub comment has no usable issue number");
                    continue;
                };
                let mut feedback = GitHubFeedback {
                    repository: repository_name.to_owned(),
                    key: EventKey {
                        conversation: ConversationKey {
                            source: scope.source.clone(),
                            subject: format!("{}/{number}", repository.id),
                        },
                        id: fetched.comment.id.to_string(),
                    },
                    author_id: identity.id,
                    status: CommentStatus::Failed,
                };
                if retries.keys.contains(&feedback.key) {
                    continue;
                }
                let shortcut = match fetched.comment.matching_shortcut(settings, identity.id) {
                    Ok(Some(shortcut)) => shortcut,
                    Ok(None) => continue,
                    Err(ezra::inbound::ShortcutError::InvalidTrigger(trigger)) => {
                        tracing::warn!(%trigger, "invalid GitHub shortcut configuration");
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(comment_id = %fetched.comment.id, %error, "GitHub trigger rejected");
                        self.queue_github_feedback(feedback).await;
                        continue;
                    }
                };
                if shortcut.1.agent != ezra::agent::Agent::Codex {
                    tracing::warn!(comment_id = %fetched.comment.id, "GitHub trigger agent is not supported by this adapter");
                    self.queue_github_feedback(feedback).await;
                    continue;
                }
                let issue = match source.issue(repository_name, number).await {
                    Ok(issue) => issue,
                    Err(error) => {
                        incomplete = true;
                        tracing::warn!(%number, %error, "could not read GitHub trigger context");
                        continue;
                    }
                };
                if issue.number != number {
                    tracing::warn!(%number, "GitHub returned mismatched issue context");
                    continue;
                }
                let mut event = match issue.event(
                    fetched.comment,
                    host,
                    repository.id,
                    repository_name,
                    shortcut.0,
                ) {
                    Ok(event) => event,
                    Err(error) => {
                        tracing::warn!(%number, %error, "GitHub trigger context rejected");
                        self.queue_github_feedback(feedback).await;
                        continue;
                    }
                };
                event.options = shortcut.1.clone();
                let admitted = self
                    .admit_github_event(
                        source,
                        sender,
                        settings,
                        &event,
                        GitHubDiscussion {
                            repository: repository_name,
                            repository_id: repository.id,
                            number,
                            workspace,
                        },
                    )
                    .await;
                feedback.status = match &admitted {
                    Err(PollError::QueueFull) => CommentStatus::Failed,
                    Err(PollError::Unavailable | PollError::Git(_)) => CommentStatus::Received,
                    Err(_) => CommentStatus::Unconfirmed,
                    Ok(Admission::Expired) => continue,
                    Ok(Admission::Uncertain) => CommentStatus::Unconfirmed,
                    Ok(Admission::Failed) => CommentStatus::Failed,
                    Ok(Admission::Accepted | Admission::Deferred) => CommentStatus::Received,
                };
                self.queue_github_feedback(feedback).await;
                match admitted {
                    Ok(Admission::Deferred) => routing_deferred = true,
                    Err(PollError::Git(error)) => {
                        incomplete = true;
                        tracing::warn!(%number, %error, "could not resolve GitHub trigger routing");
                    }
                    Err(error) => return Err(error),
                    Ok(_) => {}
                }
            }
            if count < 100 || beyond_window {
                break;
            }
            page = page.checked_add(1).ok_or(PollError::PageOverflow)?;
        }
        if incomplete {
            return Err(PollError::Incomplete);
        }
        if routing_deferred {
            return Err(PollError::RoutingDeferred);
        }
        let observed: [u8; 32] = fingerprint.finalize().into();
        let verified = Self::scan_fingerprint(source, repository_name, since, scan_started).await?;
        if observed != verified {
            return Err(PollError::ScanChanged);
        }
        if source.authenticated_user().await?.id != identity.id {
            return Err(PollError::IdentityChanged);
        }
        self.store
            .advance_source_checkpoint(&scope, scan_started)
            .await?;
        Ok(())
    }

    async fn queue_github_feedback(&self, feedback: GitHubFeedback) {
        if let Some(sender) = self.github_feedback_sender.lock().await.as_ref()
            && let Err(error) = sender.try_send(feedback)
        {
            tracing::warn!(%error, "GitHub admission feedback was dropped");
        }
    }

    pub(super) async fn publish_github_feedback(
        &self,
        source: &(impl GitHubSource<Error = crate::manager::git::GitError> + Sync),
        feedback: &GitHubFeedback,
    ) -> Result<(), ezra::inbound::store::StoreError> {
        let mut scan = None;
        let settings = loop {
            let settings = self.settings.lock().await.inbound.clone();
            if !settings.github.edit_comment_status && !settings.github.react_on_status {
                return Ok(());
            }
            if settings.github.edit_comment_status && scan.is_none() {
                scan = Some(self.github_scan.write().await);
                continue;
            }
            if !settings.github.edit_comment_status {
                drop(scan.take());
            }
            break settings;
        };
        let mut status = feedback.status;
        if let Some(state) = self.store.delivery_state(&feedback.key).await? {
            use ezra::inbound::store::DeliveryState;
            status = match state {
                DeliveryState::Delivered => CommentStatus::Delivered,
                DeliveryState::Uncertain => CommentStatus::Unconfirmed,
                DeliveryState::Failed | DeliveryState::Expired => CommentStatus::Failed,
                DeliveryState::Pending | DeliveryState::Delivering
                    if status == CommentStatus::Failed =>
                {
                    CommentStatus::Received
                }
                _ => status,
            };
        }
        if !settings.github.edit_comment_status && status == CommentStatus::Received {
            return Ok(());
        }
        let status_name = match status {
            CommentStatus::Received => "received",
            CommentStatus::Delivered => "delivered",
            CommentStatus::Unconfirmed => "unconfirmed",
            CommentStatus::Failed => "failed",
        };
        let method = if settings.github.edit_comment_status {
            "footer"
        } else {
            "reaction"
        };
        let claim = EventKey {
            id: format!("{}:status:{status_name}:{method}", feedback.key.id),
            ..feedback.key.clone()
        };
        if !self
            .store
            .claim_feedback(&claim, settings.max_history_events)
            .await?
        {
            return Ok(());
        }
        let Ok(comment_id) = feedback.key.id.parse() else {
            return Ok(());
        };
        let reference = CommentReference {
            repository: &feedback.repository,
            comment_id,
            author_id: feedback.author_id,
        };
        let result = if settings.github.edit_comment_status {
            let chat_name = self.store.delivered_chat_name(&feedback.key).await?;
            source
                .update_comment_status(
                    reference,
                    &StatusFooter {
                        status,
                        chat_name: chat_name.as_deref(),
                    },
                )
                .await
        } else {
            source.react(reference, status).await
        };
        if let Err(error) = result {
            tracing::warn!(%error, delivery_id = %feedback.key.delivery_id(), "GitHub status feedback could not be confirmed, it will not be retried");
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) async fn github_delivery_feedback(
        &self,
        source: &(impl GitHubSource<Error = crate::manager::git::GitError> + Sync),
        host: &str,
        key: &EventKey,
        delivered: bool,
    ) -> Result<(), ezra::inbound::store::StoreError> {
        let Some(event) = self.store.get(key).await? else {
            return Ok(());
        };
        let Some(mut feedback) = GitHubFeedback::from_event(&event, host) else {
            return Ok(());
        };
        feedback.status = if delivered {
            CommentStatus::Delivered
        } else {
            CommentStatus::Unconfirmed
        };
        self.publish_github_feedback(source, &feedback).await
    }

    #[cfg(test)]
    async fn scan_fingerprint(
        source: &(impl GitHubSource<Error = crate::manager::git::GitError> + Sync),
        repository: &str,
        since: OffsetDateTime,
        through: OffsetDateTime,
    ) -> Result<[u8; 32], PollError> {
        let mut fingerprint = Sha256::new();
        let mut page = NonZeroU32::new(1).expect("first page");
        loop {
            let comments = source
                .repository_comments(&RepositoryCommentsQuery {
                    repository: repository.to_owned(),
                    since,
                    page,
                })
                .await?;
            let count = comments.len();
            for fetched in comments {
                if fetched.comment.updated_at > through {
                    return Ok(fingerprint.finalize().into());
                }
                fingerprint.update(fetched.comment.id.get().to_be_bytes());
                fingerprint.update(
                    fetched
                        .comment
                        .updated_at
                        .unix_timestamp_nanos()
                        .to_be_bytes(),
                );
            }
            if count < 100 {
                return Ok(fingerprint.finalize().into());
            }
            page = page.checked_add(1).ok_or(PollError::PageOverflow)?;
        }
    }
}

#[cfg(test)]
mod tests {
    mod account;
    mod linking;

    use std::num::NonZeroU64;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use ezra::inbound::github::{
        CommentAuthor, CommentQuery, CommentReader, IssueComment, IssueContext, Repository,
        RepositoryComment, RepositorySnapshot,
    };
    use ezra::inbound::store::{DeliveryScope, DeliveryState};
    use ezra::inbound::{EventKey, MessageReceipt};
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn checkpoints_use_github_time_and_five_second_overlap_and_leave_newer_comments_for_next_scan()
     {
        let directory = tempfile::tempdir().expect("directory");
        let runtime = InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime");
        let source_time = (OffsetDateTime::now_utc() - time::Duration::days(1))
            .replace_nanosecond(0)
            .expect("whole seconds");
        let scope = ConversationKey {
            source: "github:github.com".into(),
            subject: "owner/repo".into(),
        };
        runtime
            .store
            .source_checkpoint(&scope, source_time - time::Duration::minutes(10))
            .await
            .expect("activation");
        runtime
            .store
            .advance_source_checkpoint(&scope, source_time - time::Duration::seconds(30))
            .await
            .expect("last successful scan");
        let comments: Vec<_> = [(10, -1), (11, 1)]
            .into_iter()
            .map(|(identifier, seconds)| {
                let mut comment = Source::comment(identifier);
                let timestamp = (source_time + time::Duration::seconds(seconds))
                    .format(&time::format_description::well_known::Rfc3339)
                    .expect("source timestamp");
                comment["created_at"] = json!(timestamp);
                comment["updated_at"] = json!(timestamp);
                comment
            })
            .collect();
        let mut source = Source {
            comments: Mutex::new(comments),
            observed_at: Some(source_time),
            ..Source::default()
        };
        let sender = Sender::default();
        runtime
            .scan_repository(
                &source,
                "github.com",
                &sender,
                &InboundSettings::default(),
                "owner/repo",
                "/home/dev/projects/repo",
            )
            .await
            .expect("scan");
        assert!(
            source
                .query_times
                .lock()
                .expect("queries")
                .iter()
                .all(|since| *since == source_time - time::Duration::seconds(35))
        );
        assert_eq!(
            runtime
                .store
                .source_checkpoint(&scope, OffsetDateTime::now_utc())
                .await
                .expect("saved")
                .scanned_through,
            source_time
        );
        let newer = EventKey {
            conversation: ConversationKey {
                source: scope.source.clone(),
                subject: "7/42".into(),
            },
            id: "11".into(),
        };
        assert!(
            runtime
                .store
                .get(&newer)
                .await
                .expect("newer not scanned")
                .is_none()
        );
        source.observed_at = Some(source_time + time::Duration::seconds(30));
        runtime
            .scan_repository(
                &source,
                "github.com",
                &sender,
                &InboundSettings::default(),
                "owner/repo",
                "/home/dev/projects/repo",
            )
            .await
            .expect("next scan");
        assert!(
            runtime
                .store
                .get(&newer)
                .await
                .expect("newer recovered")
                .is_some()
        );
    }

    #[tokio::test]
    async fn checkpoints_ahead_of_github_time_are_corrected_without_replaying_before_activation() {
        let directory = tempfile::tempdir().expect("directory");
        let runtime = InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime");
        let source_time = OffsetDateTime::now_utc()
            .replace_nanosecond(0)
            .expect("whole seconds");
        let scope = ConversationKey {
            source: "github:github.com".into(),
            subject: "owner/repo".into(),
        };
        runtime
            .store
            .source_checkpoint(&scope, source_time + time::Duration::hours(1))
            .await
            .expect("old fast-clock activation");
        let source = Source {
            observed_at: Some(source_time),
            ..Source::default()
        };
        runtime
            .scan_repository(
                &source,
                "github.com",
                &Sender::default(),
                &InboundSettings::default(),
                "owner/repo",
                "/home/dev/projects/repo",
            )
            .await
            .expect("scan");
        let saved = runtime
            .store
            .source_checkpoint(&scope, OffsetDateTime::now_utc())
            .await
            .expect("corrected");
        assert_eq!(saved.scanned_through, source_time);
        assert_eq!(saved.activated_at, source_time);
        assert!(
            source
                .query_times
                .lock()
                .expect("queries")
                .iter()
                .all(|since| *since == source_time)
        );
    }

    #[tokio::test]
    async fn shortening_the_poll_interval_updates_an_existing_wait() {
        let directory = tempfile::tempdir().expect("directory");
        let mut settings = crate::manager::settings::Settings::default();
        settings.inbound.github.poll_interval_seconds = NonZeroU32::new(86400).expect("one day");
        let runtime = Arc::new(
            InboundRuntime::open(
                &directory.path().join("ezra.db"),
                Arc::new(tokio::sync::Mutex::new(settings)),
            )
            .await
            .expect("runtime"),
        );
        let waiting_runtime = runtime.clone();
        let waiting = tokio::spawn(async move { waiting_runtime.wait_for_github_scan().await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        runtime
            .settings
            .lock()
            .await
            .inbound
            .github
            .poll_interval_seconds = NonZeroU32::new(1).expect("one second");
        tokio::time::timeout(Duration::from_secs(3), waiting)
            .await
            .expect("new interval applied")
            .expect("wait task");
    }

    #[derive(Default)]
    struct Source {
        comments: Mutex<Vec<serde_json::Value>>,
        fail_second_page: bool,
        status_updates: Mutex<Vec<String>>,
        reactions: Mutex<Vec<(String, u64)>>,
        reply_fails: bool,
        pages: Mutex<std::collections::VecDeque<Vec<serde_json::Value>>>,
        observed_at: Option<OffsetDateTime>,
        query_times: Mutex<Vec<OffsetDateTime>>,
        links: Mutex<std::collections::HashMap<u64, Vec<ConversationKey>>>,
        link_error: AtomicBool,
        link_reads: AtomicUsize,
        switch_identity_after_links: bool,
    }

    impl Source {
        fn comment(identifier: u64) -> serde_json::Value {
            let now = OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .expect("timestamp");
            json!({
                "id": identifier, "body": "/ezra", "user": {"id": 1, "login": "author"},
                "created_at": now, "updated_at": now,
                "issue_url": "https://api.github.com/repos/owner/repo/issues/42"
            })
        }
    }

    impl CommentReader for Source {
        type Error = crate::manager::git::GitError;
        async fn authenticated_user(&self) -> Result<CommentAuthor, Self::Error> {
            Ok(CommentAuthor {
                id: NonZeroU64::new(
                    if self.switch_identity_after_links
                        && self.link_reads.load(Ordering::SeqCst) > 0
                    {
                        2
                    } else {
                        1
                    },
                )
                .expect("id"),
                login: "author".into(),
            })
        }
        async fn read_comments(&self, _: &CommentQuery) -> Result<Vec<IssueComment>, Self::Error> {
            panic!("repository polling uses repository comment pages")
        }
    }

    impl GitHubSource for Source {
        async fn update_comment_status(
            &self,
            reference: ezra::inbound::github::CommentReference<'_>,
            footer: &ezra::inbound::github::StatusFooter<'_>,
        ) -> Result<(), Self::Error> {
            let mut comments = self.comments.lock().expect("comments");
            let existing = comments
                .iter_mut()
                .find(|comment| comment["id"].as_u64() == Some(reference.comment_id.get()));
            let body = existing
                .as_ref()
                .and_then(|comment| comment["body"].as_str())
                .unwrap_or("/ezra");
            let updated = footer.apply(body);
            self.status_updates
                .lock()
                .expect("status updates")
                .push(updated.clone());
            if self.reply_fails {
                return Err(std::io::Error::other("PATCH timeout").into());
            }
            if let Some(comment) = existing {
                comment["body"] = json!(updated);
                comment["updated_at"] = json!(
                    OffsetDateTime::now_utc()
                        .format(&time::format_description::well_known::Rfc3339)
                        .expect("timestamp")
                );
            }
            Ok(())
        }

        async fn react(
            &self,
            reference: CommentReference<'_>,
            _: CommentStatus,
        ) -> Result<(), crate::manager::git::GitError> {
            self.reactions
                .lock()
                .expect("reactions")
                .push((reference.repository.to_owned(), reference.comment_id.get()));
            if self.reply_fails {
                return Err(std::io::Error::other("reaction lost").into());
            }
            Ok(())
        }
        async fn repository(&self, _: &str) -> Result<RepositorySnapshot, Self::Error> {
            Ok(RepositorySnapshot {
                repository: Repository {
                    id: NonZeroU64::new(7).expect("id"),
                    full_name: "owner/repo".into(),
                },
                observed_at: self.observed_at.unwrap_or_else(OffsetDateTime::now_utc),
            })
        }
        async fn repository_comments(
            &self,
            query: &RepositoryCommentsQuery,
        ) -> Result<Vec<RepositoryComment>, Self::Error> {
            self.query_times
                .lock()
                .expect("query times")
                .push(query.since);
            if self.fail_second_page && query.page.get() == 2 {
                return Err(std::io::Error::other("page unavailable").into());
            }
            if let Some(page) = self.pages.lock().expect("pages").pop_front() {
                return Ok(page
                    .into_iter()
                    .map(|comment| serde_json::from_value(comment).expect("comment"))
                    .collect());
            }
            Ok(self
                .comments
                .lock()
                .expect("comments")
                .iter()
                .cloned()
                .map(|comment| serde_json::from_value(comment).expect("comment"))
                .collect())
        }
        async fn linked_discussions(
            &self,
            _: &str,
            _: NonZeroU64,
            number: NonZeroU64,
        ) -> Result<Vec<ConversationKey>, Self::Error> {
            self.link_reads.fetch_add(1, Ordering::SeqCst);
            if self.link_error.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("links unavailable").into());
            }
            Ok(self
                .links
                .lock()
                .expect("links")
                .get(&number.get())
                .cloned()
                .unwrap_or_default())
        }
        async fn issue(&self, _: &str, number: NonZeroU64) -> Result<IssueContext, Self::Error> {
            Ok(IssueContext {
                number,
                title: "Issue".into(),
                body: Some("Description".into()),
                html_url: format!("https://github.com/owner/repo/issues/{number}"),
            })
        }
    }

    #[derive(Default)]
    struct Sender {
        expected_workspace: Option<String>,
        creations: AtomicUsize,
        unavailable: AtomicBool,
        uncertain: AtomicBool,
        delivery_uncertain: AtomicBool,
        chat_name: Option<String>,
        chat_ids: Mutex<std::collections::VecDeque<String>>,
        deliveries: Mutex<Vec<(String, EventKey)>>,
        messages: Mutex<Vec<String>>,
    }

    impl MessageSender for Sender {
        async fn create_chat(
            &self,
            workspace: &str,
            _chat_name: Option<&str>,
            _options: &ezra::inbound::Shortcut,
        ) -> Result<String, MessageSendError> {
            assert_eq!(
                workspace,
                self.expected_workspace
                    .as_deref()
                    .unwrap_or("/home/dev/projects/repo")
            );
            self.creations.fetch_add(1, Ordering::SeqCst);
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(MessageSendError::Unavailable);
            }
            if self.uncertain.load(Ordering::SeqCst) {
                return Err(MessageSendError::Uncertain("connection lost".into()));
            }
            Ok(self
                .chat_ids
                .lock()
                .expect("chat IDs")
                .pop_front()
                .unwrap_or_else(|| "chat-1".into()))
        }
        async fn queue_message(
            &self,
            chat_id: &str,
            event: &InboundEvent,
        ) -> Result<MessageReceipt, MessageSendError> {
            assert!(chat_id.starts_with("chat-"));
            self.deliveries
                .lock()
                .expect("deliveries")
                .push((chat_id.to_owned(), event.key.clone()));
            assert!(event.message.contains("Issue or pull request:"));
            self.messages
                .lock()
                .expect("messages")
                .push(event.message.clone());
            if self.delivery_uncertain.load(Ordering::SeqCst) {
                return Err(MessageSendError::Uncertain(
                    "delivery connection lost".into(),
                ));
            }
            Ok(MessageReceipt {
                chat_name: self.chat_name.clone(),
                native_message_id: event.key.id.clone(),
                delivery_id: event.key.delivery_id(),
            })
        }
    }

    #[tokio::test]
    async fn repeated_scans_and_followups_reuse_the_same_repository_chat() {
        let directory = tempfile::tempdir().expect("directory");
        let runtime = InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime");
        let activated_at = OffsetDateTime::now_utc() - time::Duration::minutes(1);
        runtime
            .store
            .source_checkpoint(
                &ConversationKey {
                    source: "github:github.com".into(),
                    subject: "owner/repo".into(),
                },
                activated_at,
            )
            .await
            .expect("activation");
        let mut before_activation = Source::comment(9);
        let old = (activated_at - time::Duration::minutes(1))
            .format(&time::format_description::well_known::Rfc3339)
            .expect("timestamp");
        before_activation["created_at"] = json!(old);
        before_activation["updated_at"] = json!(old);
        let source = Source {
            comments: Mutex::new(vec![Source::comment(10)]),
            fail_second_page: false,
            status_updates: Mutex::default(),
            reactions: Mutex::default(),
            reply_fails: true,
            pages: Mutex::default(),
            observed_at: None,
            query_times: Mutex::default(),
            links: Mutex::default(),
            link_error: AtomicBool::default(),
            link_reads: AtomicUsize::default(),
            switch_identity_after_links: false,
        };
        let sender = Sender::default();
        let settings = InboundSettings::default();
        for identifier in [10, 10, 11] {
            *source.comments.lock().expect("comments") =
                vec![before_activation.clone(), Source::comment(identifier)];
            runtime
                .scan_repository(
                    &source,
                    "github.com",
                    &sender,
                    &settings,
                    "owner/repo",
                    "/home/dev/projects/repo",
                )
                .await
                .expect("scan");
            let outcome = runtime
                .store
                .dispatch_next(
                    DeliveryScope {
                        host_id: &runtime.host_id,
                        agent: "codex",
                    },
                    &sender,
                )
                .await
                .expect("dispatch");
            if let ezra::inbound::store::DispatchOutcome::Delivered { event, .. } = outcome {
                runtime.settings.lock().await.inbound.github.react_on_status = false;
                let attempted = source.reactions.lock().expect("reactions").len();
                runtime
                    .github_delivery_feedback(&source, "github.com", &event, true)
                    .await
                    .expect("disabled reaction");
                assert_eq!(source.reactions.lock().expect("reactions").len(), attempted);
                runtime.settings.lock().await.inbound.github.react_on_status = true;
                for _ in 0..2 {
                    runtime
                        .github_delivery_feedback(&source, "github.com", &event, true)
                        .await
                        .expect("reaction");
                }
            }
        }
        assert_eq!(
            runtime
                .store
                .delivery_state(&EventKey {
                    conversation: ConversationKey {
                        source: "github:github.com".into(),
                        subject: "7/42".into()
                    },
                    id: "9".into(),
                })
                .await
                .expect("old comment skipped"),
            None
        );
        assert_eq!(sender.creations.load(Ordering::SeqCst), 1);
        {
            let messages = sender.messages.lock().expect("messages");
            assert_eq!(messages.len(), 2);
            assert!(messages[0].contains("Description:"));
            assert!(!messages[1].contains("Description:"));
        }
        assert!(
            source
                .status_updates
                .lock()
                .expect("status_updates")
                .is_empty()
        );
        assert_eq!(source.reactions.lock().expect("reactions").len(), 2);
        for identifier in ["10", "11"] {
            let key = EventKey {
                conversation: ConversationKey {
                    source: "github:github.com".into(),
                    subject: "7/42".into(),
                },
                id: identifier.into(),
            };
            assert_eq!(
                runtime.store.delivery_state(&key).await.expect("state"),
                Some(DeliveryState::Delivered)
            );
        }
    }

    #[tokio::test]
    async fn incomplete_scans_keep_the_checkpoint_and_uncertain_creation_is_not_repeated() {
        let directory = tempfile::tempdir().expect("directory");
        let runtime = InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime");
        let scope = ConversationKey {
            source: "github:github.com".into(),
            subject: "owner/repo".into(),
        };
        let initial = OffsetDateTime::now_utc() - time::Duration::hours(1);
        let initial = runtime
            .store
            .source_checkpoint(&scope, initial)
            .await
            .expect("checkpoint");
        let source = Source {
            comments: Mutex::new(vec![Source::comment(10); 100]),
            fail_second_page: true,
            status_updates: Mutex::default(),
            reactions: Mutex::default(),
            reply_fails: false,
            pages: Mutex::default(),
            observed_at: None,
            query_times: Mutex::default(),
            links: Mutex::default(),
            link_error: AtomicBool::default(),
            link_reads: AtomicUsize::default(),
            switch_identity_after_links: false,
        };
        let sender = Sender {
            uncertain: AtomicBool::new(true),
            ..Sender::default()
        };
        let settings = InboundSettings::default();
        for _ in 0..2 {
            assert!(
                runtime
                    .scan_repository(
                        &source,
                        "github.com",
                        &sender,
                        &settings,
                        "owner/repo",
                        "/home/dev/projects/repo"
                    )
                    .await
                    .is_err()
            );
        }
        assert_eq!(sender.creations.load(Ordering::SeqCst), 1);
        assert_eq!(
            runtime
                .store
                .source_checkpoint(&scope, OffsetDateTime::now_utc())
                .await
                .expect("checkpoint retained"),
            initial
        );
    }
    #[tokio::test]
    async fn moving_backlog_pages_do_not_advance_past_a_skipped_trigger() {
        let directory = tempfile::tempdir().expect("directory");
        let runtime = InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime");
        let scope = ConversationKey {
            source: "github:github.com".into(),
            subject: "owner/repo".into(),
        };
        let initial = runtime
            .store
            .source_checkpoint(&scope, OffsetDateTime::now_utc() - time::Duration::hours(1))
            .await
            .expect("checkpoint");
        let old = (OffsetDateTime::now_utc() - time::Duration::minutes(30))
            .format(&time::format_description::well_known::Rfc3339)
            .expect("old timestamp");
        let comment = |identifier| {
            let mut comment = Source::comment(identifier);
            comment["updated_at"] = json!(old);
            comment["created_at"] = json!(old);
            comment
        };
        let original: Vec<_> = (1..=100).map(comment).collect();
        let shifted: Vec<_> = (2..=101).map(comment).collect();
        let last = vec![comment(102)];
        let source = Source {
            comments: Mutex::new(Vec::new()),
            fail_second_page: false,
            status_updates: Mutex::default(),
            reactions: Mutex::default(),
            reply_fails: false,
            observed_at: None,
            query_times: Mutex::default(),
            links: Mutex::default(),
            link_error: AtomicBool::default(),
            link_reads: AtomicUsize::default(),
            switch_identity_after_links: false,
            pages: Mutex::new(
                [
                    original,
                    last.clone(),
                    shifted.clone(),
                    last.clone(),
                    shifted.clone(),
                    last.clone(),
                    shifted,
                    last,
                ]
                .into(),
            ),
        };
        let sender = Sender::default();
        let settings = InboundSettings::default();
        assert!(matches!(
            runtime
                .scan_repository(
                    &source,
                    "github.com",
                    &sender,
                    &settings,
                    "owner/repo",
                    "/home/dev/projects/repo"
                )
                .await,
            Err(PollError::ScanChanged)
        ));
        assert_eq!(
            runtime
                .store
                .source_checkpoint(&scope, OffsetDateTime::now_utc())
                .await
                .expect("retained"),
            initial
        );
        runtime
            .scan_repository(
                &source,
                "github.com",
                &sender,
                &settings,
                "owner/repo",
                "/home/dev/projects/repo",
            )
            .await
            .expect("stable scan");
        let missed = EventKey {
            conversation: ConversationKey {
                source: scope.source,
                subject: "7/42".into(),
            },
            id: "101".into(),
        };
        assert_eq!(
            runtime
                .store
                .delivery_state(&missed)
                .await
                .expect("missed trigger recovered"),
            Some(DeliveryState::Pending)
        );
    }
    #[tokio::test]
    async fn feedback_updates_and_failures_never_resend_a_delivered_comment_after_restart() {
        for (feedback_fails, chat_name) in [
            (false, Some("Add zvol creation API")),
            (true, Some("Add zvol creation API")),
            (false, None),
        ] {
            let directory = tempfile::tempdir().expect("directory");
            let database = directory.path().join("ezra.db");
            let settings = Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            ));
            settings.lock().await.inbound.github.edit_comment_status = true;
            let runtime = InboundRuntime::open(&database, Arc::clone(&settings))
                .await
                .expect("runtime");
            runtime
                .store
                .source_checkpoint(
                    &ConversationKey {
                        source: "github:github.com".into(),
                        subject: "owner/repo".into(),
                    },
                    OffsetDateTime::now_utc() - time::Duration::minutes(1),
                )
                .await
                .expect("activation");
            let (queued, mut statuses) = tokio::sync::mpsc::channel(64);
            *runtime.github_feedback_sender.lock().await = Some(queued);
            let source = Source {
                comments: Mutex::new(vec![Source::comment(10)]),
                reply_fails: feedback_fails,
                ..Source::default()
            };
            let sender = Sender {
                chat_name: chat_name.map(str::to_owned),
                ..Sender::default()
            };
            runtime
                .store
                .bind_conversation(
                    &ConversationKey {
                        source: "github:github.com".into(),
                        subject: "7/42".into(),
                    },
                    &SessionTarget {
                        host_id: runtime.host_id.clone(),
                        agent: "codex".into(),
                        chat_id: "chat-1".into(),
                        workspace: "/home/dev/projects/repo".into(),
                    },
                )
                .await
                .expect("existing chat binds");
            runtime
                .scan_repository(
                    &source,
                    "github.com",
                    &sender,
                    &settings.lock().await.inbound.clone(),
                    "owner/repo",
                    "/home/dev/projects/repo",
                )
                .await
                .expect("scan");
            let received = statuses.try_recv().expect("received feedback queued");
            assert_eq!(
                runtime
                    .store
                    .delivery_state(&received.key)
                    .await
                    .expect("persisted before feedback"),
                Some(DeliveryState::Pending)
            );
            assert!(source.status_updates.lock().expect("updates").is_empty());
            runtime
                .publish_github_feedback(&source, &received)
                .await
                .expect("best effort received");
            let ezra::inbound::store::DispatchOutcome::Delivered { event, .. } = runtime
                .store
                .dispatch_next(
                    DeliveryScope {
                        host_id: &runtime.host_id,
                        agent: "codex",
                    },
                    &sender,
                )
                .await
                .expect("dispatch")
            else {
                panic!("delivered")
            };
            runtime
                .github_delivery_feedback(&source, "github.com", &event, true)
                .await
                .expect("best effort delivered");
            runtime
                .publish_github_feedback(&source, &received)
                .await
                .expect("old received job cannot regress delivered");
            {
                let updates = source.status_updates.lock().expect("updates");
                assert_eq!(updates.len(), 2);
                assert!(updates[0].contains("👀 Ezra: received"));
                assert!(updates[1].contains("✅ Ezra: delivered"));
                assert!(!updates[0].contains("owner/repo#42:"));
                assert!(!updates[1].contains("owner/repo#42:"));
                if let Some(chat_name) = chat_name {
                    assert!(updates[1].contains(chat_name));
                } else {
                    assert!(!updates[1].contains(" · "));
                }
            }
            drop(runtime);
            {
                let mut comments = source.comments.lock().expect("comments");
                comments[0]["body"] = json!("/ezra edited after feedback");
                comments[0]["updated_at"] = json!(
                    OffsetDateTime::now_utc()
                        .format(&time::format_description::well_known::Rfc3339)
                        .expect("timestamp")
                );
            }
            let runtime = InboundRuntime::open(&database, Arc::clone(&settings))
                .await
                .expect("reopen");
            runtime
                .scan_repository(
                    &source,
                    "github.com",
                    &sender,
                    &settings.lock().await.inbound.clone(),
                    "owner/repo",
                    "/home/dev/projects/repo",
                )
                .await
                .expect("edited comment scan");
            assert!(matches!(
                runtime
                    .store
                    .dispatch_next(
                        DeliveryScope {
                            host_id: &runtime.host_id,
                            agent: "codex",
                        },
                        &sender
                    )
                    .await
                    .expect("dedup after restart"),
                ezra::inbound::store::DispatchOutcome::Idle
            ));
            assert_eq!(sender.creations.load(Ordering::SeqCst), 0);
            assert_eq!(
                runtime
                    .store
                    .delivered_chat_name(&event)
                    .await
                    .expect("persisted actual name")
                    .as_deref(),
                chat_name
            );
            assert_eq!(
                runtime
                    .store
                    .delivery_state(&event)
                    .await
                    .expect("delivery stays settled"),
                Some(DeliveryState::Delivered)
            );
        }
    }

    #[tokio::test]
    async fn footer_publication_waits_for_scan_verification() {
        for disable_while_waiting in [false, true] {
            let directory = tempfile::tempdir().expect("directory");
            let settings = Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            ));
            settings.lock().await.inbound.github.edit_comment_status = true;
            let runtime = Arc::new(
                InboundRuntime::open(&directory.path().join("ezra.db"), Arc::clone(&settings))
                    .await
                    .expect("runtime"),
            );
            let source = Arc::new(Source::default());
            let feedback = GitHubFeedback {
                repository: "owner/repo".into(),
                key: EventKey {
                    conversation: ConversationKey {
                        source: "github:github.com".into(),
                        subject: "7/42".into(),
                    },
                    id: "10".into(),
                },
                author_id: NonZeroU64::new(1).expect("author"),
                status: CommentStatus::Received,
            };
            let scan = runtime.github_scan.read().await;
            let running = Arc::clone(&runtime);
            let writer = Arc::clone(&source);
            let mut task = tokio::spawn(async move {
                running
                    .publish_github_feedback(writer.as_ref(), &feedback)
                    .await
            });
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut task)
                    .await
                    .is_err()
            );
            assert!(source.status_updates.lock().expect("updates").is_empty());
            if disable_while_waiting {
                let mut settings = settings.lock().await;
                settings.inbound.github.edit_comment_status = false;
                settings.inbound.github.react_on_status = false;
            }
            drop(scan);
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("scan released")
                .expect("writer")
                .expect("publication");
            assert_eq!(
                source.status_updates.lock().expect("updates").len(),
                usize::from(!disable_while_waiting)
            );
        }
    }

    #[tokio::test]
    async fn reaction_feedback_does_not_wait_for_a_comment_scan() {
        let directory = tempfile::tempdir().expect("directory");
        let runtime = InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime");
        let source = Source::default();
        let feedback = GitHubFeedback {
            repository: "owner/repo".into(),
            key: EventKey {
                conversation: ConversationKey {
                    source: "github:github.com".into(),
                    subject: "7/42".into(),
                },
                id: "10".into(),
            },
            author_id: NonZeroU64::new(1).expect("author"),
            status: CommentStatus::Delivered,
        };
        let _scan = runtime.github_scan.read().await;
        tokio::time::timeout(
            Duration::from_secs(1),
            runtime.publish_github_feedback(&source, &feedback),
        )
        .await
        .expect("reaction independent of scan")
        .expect("feedback");
        assert_eq!(source.reactions.lock().expect("reactions").len(), 1);
        assert!(source.status_updates.lock().expect("updates").is_empty());
    }

    #[tokio::test]
    async fn feedback_toggles_and_failed_updates_do_not_repeat_after_restart() {
        let directory = tempfile::tempdir().expect("directory");
        let database = directory.path().join("ezra.db");
        let settings = Arc::new(tokio::sync::Mutex::new(
            crate::manager::settings::Settings::default(),
        ));
        settings.lock().await.inbound.github.react_on_status = false;
        let runtime = InboundRuntime::open(&database, Arc::clone(&settings))
            .await
            .expect("runtime");
        let source = Source {
            reply_fails: true,
            ..Source::default()
        };
        let feedback = GitHubFeedback {
            repository: "owner/repo".into(),
            key: EventKey {
                conversation: ConversationKey {
                    source: "github:github.com".into(),
                    subject: "7/42".into(),
                },
                id: "10".into(),
            },
            author_id: NonZeroU64::new(1).expect("author"),
            status: CommentStatus::Delivered,
        };
        runtime
            .publish_github_feedback(&source, &feedback)
            .await
            .expect("disabled");
        assert!(source.status_updates.lock().expect("updates").is_empty());
        settings.lock().await.inbound.github.edit_comment_status = true;
        for _ in 0..2 {
            runtime
                .publish_github_feedback(&source, &feedback)
                .await
                .expect("best effort");
        }
        drop(runtime);
        let runtime = InboundRuntime::open(&database, settings)
            .await
            .expect("reopen");
        runtime
            .publish_github_feedback(&source, &feedback)
            .await
            .expect("no retry after restart");
        let updates = source.status_updates.lock().expect("updates");
        assert_eq!(updates.len(), 1);
        assert!(updates[0].contains("✅ Ezra: delivered"));
        assert!(!updates[0].contains(" · "));
    }

    #[test]
    fn delivery_feedback_rejects_unrelated_or_malformed_source_urls() {
        let mut event: InboundEvent = serde_json::from_value(json!({
            "key": {"conversation": {"source": "github:github.com", "subject": "7/42"}, "id": "10"},
            "actor": "1", "created_at": "2026-09-30T12:00:00Z", "message": "Request"
        }))
        .expect("event");
        for url in [
            "https://github.com/owner/repo/issues/42#issuecomment-10",
            "https://github.com/owner/repo/pull/42#issuecomment-10",
        ] {
            event.source_url = Some(url.to_owned());
            let feedback = GitHubFeedback::from_event(&event, "github.com").expect("valid source");
            assert_eq!(feedback.repository, "owner/repo");
        }
        for url in [
            "https://other.example/owner/repo/issues/42#issuecomment-10",
            "https://github.com/owner/repo/issues/43#issuecomment-10",
            "https://github.com/owner/repo/issues/42#issuecomment-11",
            "https://github.com/owner/repo/issues/42?query=value#issuecomment-10",
            "https://github.com/owner/repo/issues/42/extra#issuecomment-10",
            "malformed",
        ] {
            event.source_url = Some(url.to_owned());
            assert!(GitHubFeedback::from_event(&event, "github.com").is_none());
        }
        event.source_url = Some("https://github.com/owner/repo/issues/42#issuecomment-10".into());
        event.key.conversation.source = "api:local".into();
        assert!(GitHubFeedback::from_event(&event, "github.com").is_none());
    }

    #[tokio::test]
    async fn unconfirmed_delivery_footer_does_not_claim_failure_and_respects_toggles() {
        let directory = tempfile::tempdir().expect("directory");
        let runtime = InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime");
        runtime
            .store
            .source_checkpoint(
                &ConversationKey {
                    source: "github:github.com".into(),
                    subject: "owner/repo".into(),
                },
                OffsetDateTime::now_utc() - time::Duration::minutes(1),
            )
            .await
            .expect("activation");
        let source = Source {
            comments: Mutex::new(vec![Source::comment(10)]),
            ..Source::default()
        };
        let sender = Sender {
            delivery_uncertain: AtomicBool::new(true),
            ..Sender::default()
        };
        runtime
            .scan_repository(
                &source,
                "github.com",
                &sender,
                &InboundSettings::default(),
                "owner/repo",
                "/home/dev/projects/repo",
            )
            .await
            .expect("scan");
        assert!(
            source
                .status_updates
                .lock()
                .expect("status_updates")
                .is_empty()
        );
        let ezra::inbound::store::DispatchOutcome::Uncertain { event, .. } = runtime
            .store
            .dispatch_next(
                DeliveryScope {
                    host_id: &runtime.host_id,
                    agent: "codex",
                },
                &sender,
            )
            .await
            .expect("dispatch")
        else {
            panic!("expected uncertainty")
        };
        runtime.settings.lock().await.inbound.github.react_on_status = false;
        runtime
            .settings
            .lock()
            .await
            .inbound
            .github
            .edit_comment_status = false;
        runtime
            .github_delivery_feedback(&source, "github.com", &event, false)
            .await
            .expect("disabled");
        assert!(
            source
                .status_updates
                .lock()
                .expect("status_updates")
                .is_empty()
        );
        runtime
            .settings
            .lock()
            .await
            .inbound
            .github
            .edit_comment_status = true;
        for _ in 0..2 {
            runtime
                .github_delivery_feedback(&source, "github.com", &event, false)
                .await
                .expect("comment");
        }
        assert!(source.reactions.lock().expect("reactions").is_empty());
        assert_eq!(
            source.status_updates.lock().expect("status_updates").len(),
            1
        );
        let updates = source.status_updates.lock().expect("updates");
        assert!(updates[0].contains("delivery unconfirmed"));
        assert!(!updates[0].contains("❌"));
    }

    #[tokio::test]
    async fn invalid_configuration_does_not_reply_to_ordinary_comments() {
        let directory = tempfile::tempdir().expect("directory");
        let runtime = InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime");
        let mut comment = Source::comment(10);
        comment["body"] = json!("An ordinary discussion comment");
        let source = Source {
            comments: Mutex::new(vec![comment]),
            pages: Mutex::default(),
            observed_at: None,
            query_times: Mutex::default(),
            links: Mutex::default(),
            link_error: AtomicBool::default(),
            link_reads: AtomicUsize::default(),
            switch_identity_after_links: false,
            status_updates: Mutex::default(),
            reactions: Mutex::default(),
            fail_second_page: false,
            reply_fails: false,
        };
        let mut settings = InboundSettings::default();
        settings
            .shortcuts
            .insert("invalid trigger".into(), Default::default());
        runtime
            .scan_repository(
                &source,
                "github.com",
                &Sender::default(),
                &settings,
                "owner/repo",
                "/home/dev/projects/repo",
            )
            .await
            .expect("scan");
        assert!(
            source
                .status_updates
                .lock()
                .expect("status_updates")
                .is_empty()
        );
    }
}
