mod account;
mod linking;
use linking::GitHubDiscussion;

use std::num::NonZeroU64;
use std::time::Duration;

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
    #[error("GitHub comments changed during pagination")]
    ScanChanged,
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
}
