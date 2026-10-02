use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;

use ezra::inbound::github::{AccountCommentSource, AccountCommentsPage};
use ezra::inbound::store::{DeliveryScope, DeliveryState, DispatchOutcome};
#[cfg(test)]
use futures_util::stream;
use futures_util::{StreamExt, stream::FuturesUnordered};

use super::*;
use crate::manager::inbound::RoutingWake;

impl InboundRuntime {
    pub(in crate::manager::inbound) async fn route_github(
        &self,
        source: &GitTools,
        projects: &ProjectsDirectory,
        sender: &impl MessageSender,
    ) {
        let Some(incoming) = self.github_routing_receiver.lock().await.take() else {
            return;
        };
        self.process_github_events(source.host().as_str(), sender, incoming, |key| {
            self.route_github_key(source, projects, sender, key)
        })
        .await;
    }

    pub(in crate::manager::inbound) async fn process_github_events<RouteFuture>(
        &self,
        host: &str,
        sender: &impl MessageSender,
        mut incoming: tokio::sync::mpsc::UnboundedReceiver<RoutingWake>,
        route: impl Fn(EventKey) -> RouteFuture,
    ) where
        RouteFuture: std::future::Future<Output = (EventKey, Result<Admission, PollError>)>,
    {
        let mut control_changes = sender.control_changes();
        self.load_github_routes(host).await;
        let mut queued = HashSet::new();
        let mut waiting: VecDeque<EventKey> = VecDeque::new();
        let mut deferred = Vec::new();
        let mut offline = Vec::new();
        let mut active = FuturesUnordered::new();
        let mut active_keys = HashSet::new();
        let mut availability_generation = 0u64;
        let mut progress_generation = 0u64;
        loop {
            while sender.control_available() && active.len() < 8 {
                let Some(key) = waiting.pop_front() else {
                    break;
                };
                active_keys.insert(key.clone());
                let operation = route(key);
                let generation = availability_generation;
                let started_progress = progress_generation;
                active.push(async move {
                    let (key, result) = operation.await;
                    (key, result, generation, started_progress)
                });
            }
            tokio::select! {
                Some(message) = incoming.recv() => {
                    match message {
                        RoutingWake::Event(key) => {
                            if queued.insert(key.clone()) {
                                waiting.push_back(key);
                            }
                        }
                        RoutingWake::Sweep => {
                            match self.store.waiting_events(&format!("github:{}", host.to_ascii_lowercase())).await {
                                Ok(events) => {
                                    let retained: HashSet<_> = events.into_iter().collect();
                                    waiting.retain(|key| retained.contains(key));
                                    deferred.retain(|key| retained.contains(key));
                                    offline.retain(|key| retained.contains(key));
                                    queued.retain(|key| retained.contains(key) || active_keys.contains(key));
                                    progress_generation = progress_generation.wrapping_add(1);
                                    waiting.extend(deferred.drain(..));
                                }
                                Err(error) => tracing::error!(%error, "could not trim expired GitHub requests"),
                            }
                        }
                    }
                }
                Some((key, result, generation, started_progress)) = active.next(), if !active.is_empty() => {
                    active_keys.remove(&key);
                    match result {
                        Ok(Admission::Accepted | Admission::Failed | Admission::Uncertain | Admission::Expired) => {
                            queued.remove(&key);
                            progress_generation = progress_generation.wrapping_add(1);
                            waiting.extend(deferred.drain(..));
                        }
                        Ok(Admission::Deferred) if started_progress != progress_generation => waiting.push_back(key),
                        Ok(Admission::Deferred) => deferred.push(key),
                        Err(PollError::Unavailable) if sender.control_available()
                            && (generation != availability_generation || control_changes.as_ref().is_some_and(|changes| changes.has_changed().unwrap_or(false))) => waiting.push_back(key),
                        Err(PollError::Unavailable) if !sender.control_available() => offline.push(key),
                        Err(error) => {
                            if let Err(storage_error) = self.store.fail_waiting_event(&key).await {
                                tracing::error!(%storage_error, "could not record failed GitHub request");
                            }
                            if let Ok(Some(event)) = self.store.get(&key).await
                                && let Some(mut feedback) = GitHubFeedback::from_event(&event, host) {
                                feedback.status = CommentStatus::Failed;
                                self.queue_github_feedback(feedback).await;
                            }
                            tracing::warn!(%error, "GitHub request failed before submission");
                            queued.remove(&key);
                            progress_generation = progress_generation.wrapping_add(1);
                            waiting.extend(deferred.drain(..));
                        }
                    }
                }
                changed = async {
                    match control_changes.as_mut() {
                        Some(changes) => changes.changed().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if changed.is_err() {
                        control_changes = None;
                    } else {
                        availability_generation = availability_generation.wrapping_add(1);
                        if !sender.control_available() { continue; }
                        waiting.extend(offline.drain(..));
                        waiting.extend(deferred.drain(..));
                        self.load_github_routes(host).await;
                    }
                }
            }
        }
    }

    async fn load_github_routes(&self, host: &str) {
        match self
            .store
            .waiting_events(&format!("github:{}", host.to_ascii_lowercase()))
            .await
        {
            Ok(events) => {
                for event in events {
                    self.enqueue_github_event(event);
                }
            }
            Err(error) => tracing::error!(%error, "could not restore waiting GitHub requests"),
        }
    }

    async fn route_github_key(
        &self,
        source: &GitTools,
        projects: &ProjectsDirectory,
        sender: &impl MessageSender,
        key: EventKey,
    ) -> (EventKey, Result<Admission, PollError>) {
        let result = async {
            let settings = self.settings.lock().await.inbound.clone();
            if !self.store.event_is_waiting(&key, settings.waiting_cutoff(OffsetDateTime::now_utc())).await? {
                self.expire_waiting_requests(&settings).await?;
                return Ok(Admission::Expired);
            }
            let Some(event) = self.store.get(&key).await? else {
                return Ok(Admission::Expired);
            };
            let Some(feedback) = GitHubFeedback::from_event(&event, source.host().as_str()) else {
                return Ok(Admission::Expired);
            };
            let Some((repository_id, number)) = event.key.conversation.subject.split_once('/') else {
                return Ok(Admission::Expired);
            };
            let (Ok(repository_id), Ok(number)) = (repository_id.parse(), number.parse()) else {
                return Ok(Admission::Expired);
            };
            let workspaces = projects.github_workspaces(source.host().as_str(), &settings.github).await.map_err(crate::manager::git::GitError::Io)?;
            let workspace = workspaces.get(&feedback.repository.to_ascii_lowercase())
                .and_then(|path| path.to_str()).unwrap_or("/home/dev");
            let result = self.admit_github_event(source, sender, &settings, &event, GitHubDiscussion {
                repository: &feedback.repository, repository_id, number, workspace,
            }).await;
            let result = match result {
                Ok(Admission::Accepted) => {
                    match self.store.dispatch_event(&event.key, DeliveryScope { host_id: &self.host_id, agent: "codex" }, sender, settings.waiting_cutoff(OffsetDateTime::now_utc())).await? {
                        DispatchOutcome::Delivered { .. } => {
                            let mut feedback = feedback.clone();
                            feedback.status = CommentStatus::Delivered;
                            self.queue_github_feedback(feedback).await;
                            Ok(Admission::Accepted)
                        }
                        DispatchOutcome::Uncertain { .. } => Ok(Admission::Uncertain),
                        DispatchOutcome::Failed { .. } => {
                            let mut feedback = feedback.clone();
                            feedback.status = CommentStatus::Failed;
                            self.queue_github_feedback(feedback).await;
                            Ok(Admission::Accepted)
                        }
                        DispatchOutcome::Unavailable { .. } => Err(PollError::Unavailable),
                        DispatchOutcome::Idle => {
                            if self.store.delivery_state(&event.key).await? == Some(DeliveryState::Pending) {
                                Ok(Admission::Deferred)
                            } else {
                                Ok(Admission::Accepted)
                            }
                        }
                    }
                }
                result => result,
            };
            match &result {
                Ok(Admission::Uncertain) => {
                    let mut feedback = feedback;
                    feedback.status = CommentStatus::Unconfirmed;
                    self.queue_github_feedback(feedback).await;
                }
                Err(error) => tracing::warn!(delivery_id = %event.key.delivery_id(), %error, "GitHub request routing failed"),
                _ => {},
            }
            result
        }.await;
        (key, result)
    }

    #[cfg(test)]
    pub(super) async fn route_saved_github(
        &self,
        source: &(impl GitHubSource<Error = crate::manager::git::GitError> + Sync),
        host: &str,
        sender: &impl MessageSender,
        settings: &InboundSettings,
        workspaces: &BTreeMap<String, PathBuf>,
    ) -> Result<(), PollError> {
        let author = source.authenticated_user().await?;
        let events = self
            .store
            .pending_routing(&format!("github:{}", host.to_ascii_lowercase()), "")
            .await?;
        stream::iter(events).for_each_concurrent(8, |event| {
            let author = &author;
            async move {
                if event.actor != author.id.to_string() { return; }
                let Some(feedback) = GitHubFeedback::from_event(&event, host) else { return; };
                let Some((repository_id, number)) = event.key.conversation.subject.split_once('/') else { return; };
                let (Ok(repository_id), Ok(number)) = (repository_id.parse(), number.parse()) else { return; };
                let workspace = workspaces.get(&feedback.repository.to_ascii_lowercase()).and_then(|path| path.to_str()).unwrap_or("/home/dev");
                let result = self.admit_github_event(source, sender, settings, &event, GitHubDiscussion {
                    repository: &feedback.repository, repository_id, number, workspace,
                }).await;
                match result {
                    Ok(Admission::Accepted) => {},
                    Ok(Admission::Uncertain) => {
                        let mut feedback = feedback;
                        feedback.status = CommentStatus::Unconfirmed;
                        self.queue_github_feedback(feedback).await;
                    }
                    Ok(_) => {},
                    Err(error) => tracing::debug!(delivery_id = %event.key.delivery_id(), %error, "GitHub request routing will retry"),
                }
            }
        }).await;
        Ok(())
    }

    pub(super) async fn scan_account(
        &self,
        source: &(impl AccountCommentSource<Error = crate::manager::git::GitError> + Sync),
        host: &str,
        settings: &InboundSettings,
        workspaces: &BTreeMap<String, PathBuf>,
    ) -> Result<(), PollError> {
        // Footer edits change the timestamps used by scan verification.
        let _scan = self.github_scan.read().await;
        let first = source.account_comments(None).await?;
        let scan_started = first.observed_at;
        let author = first.author.clone();
        if source.authenticated_user().await?.id != author.id {
            return Err(PollError::IdentityChanged);
        }
        let scope = ConversationKey {
            source: format!("github:{}", host.to_ascii_lowercase()),
            subject: format!("viewer/{}", author.id),
        };
        self.store
            .set_active_source_scopes(
                "github:",
                &scope.source,
                std::slice::from_ref(&scope.subject),
            )
            .await?;
        let mut checkpoint = self.store.source_checkpoint(&scope, scan_started).await?;
        if checkpoint.scanned_through > scan_started || checkpoint.activated_at > scan_started {
            self.store
                .clamp_source_checkpoint(&scope, scan_started)
                .await?;
            checkpoint.scanned_through = checkpoint.scanned_through.min(scan_started);
            checkpoint.activated_at = checkpoint.activated_at.min(scan_started);
        }
        let since = checkpoint
            .scanned_through
            .saturating_sub(time::Duration::seconds(10))
            .max(scan_started.saturating_sub(time::Duration::minutes(5)))
            .max(checkpoint.activated_at);
        let (comments, fingerprint, paginated) =
            Self::account_window(source, first, since, scan_started, author.id).await?;
        self.expire_waiting_requests(settings).await?;
        for fetched in comments {
            if settings.github.only_added_repositories
                && !workspaces.contains_key(&fetched.repository.full_name.to_ascii_lowercase())
            {
                continue;
            }
            let mut feedback = GitHubFeedback {
                repository: fetched.repository.full_name.clone(),
                key: EventKey {
                    conversation: ConversationKey {
                        source: scope.source.clone(),
                        subject: format!("{}/{}", fetched.repository.id, fetched.issue.number),
                    },
                    id: fetched.comment.id.to_string(),
                },
                author_id: author.id,
                status: CommentStatus::Received,
            };
            let shortcut = match fetched.comment.matching_shortcut(settings, author.id) {
                Ok(Some(shortcut)) => shortcut,
                Ok(None) => continue,
                Err(ezra::inbound::ShortcutError::InvalidTrigger(trigger)) => {
                    tracing::warn!(%trigger, "invalid GitHub shortcut configuration");
                    continue;
                }
                Err(error) => {
                    tracing::warn!(comment_id = %fetched.comment.id, %error, "GitHub trigger rejected");
                    feedback.status = CommentStatus::Failed;
                    self.queue_github_feedback(feedback).await;
                    continue;
                }
            };
            if shortcut
                .1
                .agent
                .as_deref()
                .is_some_and(|agent| agent != "codex")
            {
                feedback.status = CommentStatus::Failed;
                self.queue_github_feedback(feedback).await;
                continue;
            }
            let mut event = match fetched.issue.event(
                fetched.comment,
                host,
                fetched.repository.id,
                &fetched.repository.full_name,
                shortcut.0,
            ) {
                Ok(event) => event,
                Err(error) => {
                    tracing::warn!(%error, "GitHub trigger context rejected");
                    feedback.status = CommentStatus::Failed;
                    self.queue_github_feedback(feedback).await;
                    continue;
                }
            };
            event.options = shortcut.1.clone();
            match self.store.insert(&event, settings.queue_limits()).await? {
                InsertOutcome::QueueFull => {
                    self.store.reject_event(&event).await?;
                    tracing::warn!(delivery_id = %event.key.delivery_id(), "GitHub request rejected because the inbound queue is full");
                    feedback.status = CommentStatus::Failed;
                    self.queue_github_feedback(feedback).await;
                }
                InsertOutcome::Expired => continue,
                InsertOutcome::Inserted => {
                    self.enqueue_github_event(event.key.clone());
                    self.queue_github_feedback(feedback).await;
                }
                InsertOutcome::Duplicate => {}
            }
        }
        // A single page is one response. Cursor pagination needs a second stable pass.
        if paginated {
            let first = source.account_comments(None).await?;
            let (_, verified, _) =
                Self::account_window(source, first, since, scan_started, author.id).await?;
            if fingerprint != verified {
                return Err(PollError::ScanChanged);
            }
        }
        if source.authenticated_user().await?.id != author.id {
            return Err(PollError::IdentityChanged);
        }
        self.store
            .advance_source_checkpoint(&scope, scan_started)
            .await?;
        Ok(())
    }

    async fn account_window(
        source: &(impl AccountCommentSource<Error = crate::manager::git::GitError> + Sync),
        mut page: AccountCommentsPage,
        since: OffsetDateTime,
        through: OffsetDateTime,
        author_id: NonZeroU64,
    ) -> Result<(Vec<ezra::inbound::github::AccountComment>, [u8; 32], bool), PollError> {
        let mut fingerprint = Sha256::new();
        let mut comments = Vec::new();
        let mut cursors = HashSet::new();
        let mut identifiers = HashSet::new();
        let mut paginated = false;
        let mut previous_updated = None;
        loop {
            if page.author.id != author_id {
                return Err(PollError::IdentityChanged);
            }
            let mut reached_cutoff = false;
            for fetched in page.comments {
                let updated = fetched.comment.updated_at;
                if previous_updated.is_some_and(|previous| updated > previous) {
                    return Err(PollError::ScanChanged);
                }
                previous_updated = Some(updated);
                if updated < since {
                    reached_cutoff = true;
                    break;
                }
                if updated > through {
                    continue;
                }
                if !identifiers.insert(fetched.comment.id) {
                    return Err(PollError::ScanChanged);
                }
                fingerprint.update(fetched.comment.id.get().to_be_bytes());
                fingerprint.update(updated.unix_timestamp_nanos().to_be_bytes());
                comments.push(fetched);
            }
            if reached_cutoff {
                break;
            }
            let Some(cursor) = page.next_cursor else {
                break;
            };
            if !cursors.insert(cursor.clone()) || cursors.len() > 100 {
                return Err(PollError::ScanChanged);
            }
            paginated = true;
            page = source.account_comments(Some(&cursor)).await?;
        }
        comments.sort_by_key(|fetched| (fetched.comment.created_at, fetched.comment.id));
        Ok((comments, fingerprint.finalize().into(), paginated))
    }
}
