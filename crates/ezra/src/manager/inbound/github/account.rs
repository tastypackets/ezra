use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;

use ezra::agent::Agent;
use ezra::inbound::github::{AccountCommentSource, AccountCommentsPage};
use ezra::inbound::store::{DeliveryScope, DeliveryState, DispatchOutcome};
#[cfg(test)]
use futures_util::stream;
use futures_util::{StreamExt, stream::FuturesUnordered};
use tokio::sync::watch;

use super::*;
use crate::manager::inbound::{AgentSenders, RoutingWake};

const ROUTES_PER_AGENT: usize = 8;

struct AgentQueue {
    changes: Option<watch::Receiver<bool>>,
    generation: u64,
    waiting: VecDeque<EventKey>,
    offline: VecDeque<EventKey>,
    active: usize,
}

impl AgentQueue {
    fn watching(changes: Option<watch::Receiver<bool>>) -> Self {
        Self {
            changes,
            generation: 0,
            waiting: VecDeque::new(),
            offline: VecDeque::new(),
            active: 0,
        }
    }

    async fn changed(&mut self) -> Result<(), watch::error::RecvError> {
        match self.changes.as_mut() {
            Some(changes) => changes.changed().await,
            None => std::future::pending().await,
        }
    }

    fn resume(&mut self) {
        let offline = std::mem::take(&mut self.offline);
        self.waiting.extend(offline);
    }

    fn changed_since(&self, generation: u64) -> bool {
        generation != self.generation
            || self
                .changes
                .as_ref()
                .is_some_and(|changes| changes.has_changed().unwrap_or(false))
    }
}

struct AgentQueues {
    claude: AgentQueue,
    codex: AgentQueue,
}

impl AgentQueues {
    fn watching(senders: &AgentSenders<impl MessageSender, impl MessageSender>) -> Self {
        Self {
            claude: AgentQueue::watching(senders.claude.control_changes()),
            codex: AgentQueue::watching(senders.codex.control_changes()),
        }
    }

    fn get_mut(&mut self, agent: Agent) -> &mut AgentQueue {
        match agent {
            Agent::Claude => &mut self.claude,
            Agent::Codex => &mut self.codex,
        }
    }

    async fn next_change(&mut self) -> (Agent, Result<(), watch::error::RecvError>) {
        tokio::select! {
            changed = self.claude.changed() => (Agent::Claude, changed),
            changed = self.codex.changed() => (Agent::Codex, changed),
        }
    }

    fn requeue(&mut self, keys: impl IntoIterator<Item = (EventKey, Agent)>) {
        for (key, agent) in keys {
            self.get_mut(agent).waiting.push_back(key);
        }
    }

    fn retain(&mut self, retained: &HashSet<EventKey>) {
        for queue in [&mut self.claude, &mut self.codex] {
            queue.waiting.retain(|key| retained.contains(key));
            queue.offline.retain(|key| retained.contains(key));
        }
    }
}

pub(super) trait RoutesExt<Route: std::future::Future> {
    /// Keeps polling routes while `work` runs, because a suspended route can hold the store's
    /// only connection that `work` waits for.
    async fn draining<Output>(
        &mut self,
        work: impl std::future::Future<Output = Output>,
        finished: &mut VecDeque<Route::Output>,
    ) -> Output;
}

impl<Route: std::future::Future> RoutesExt<Route> for FuturesUnordered<Route> {
    async fn draining<Output>(
        &mut self,
        work: impl std::future::Future<Output = Output>,
        finished: &mut VecDeque<Route::Output>,
    ) -> Output {
        tokio::pin!(work);
        loop {
            tokio::select! {
                output = &mut work => return output,
                Some(result) = self.next(), if !self.is_empty() => finished.push_back(result),
            }
        }
    }
}

impl InboundRuntime {
    pub(in crate::manager::inbound) async fn route_github(
        &self,
        source: &GitTools,
        projects: &ProjectsDirectory,
        senders: &AgentSenders<impl MessageSender, impl MessageSender>,
    ) {
        let Some(incoming) = self.github_routing_receiver.lock().await.take() else {
            return;
        };
        self.process_github_events(source.host().as_str(), senders, incoming, |key| {
            self.route_github_key(source, projects, senders, key)
        })
        .await;
    }

    pub(in crate::manager::inbound) async fn process_github_events<RouteFuture>(
        &self,
        host: &str,
        senders: &AgentSenders<impl MessageSender, impl MessageSender>,
        mut incoming: tokio::sync::mpsc::UnboundedReceiver<RoutingWake>,
        route: impl Fn(EventKey) -> RouteFuture,
    ) where
        RouteFuture: std::future::Future<Output = (EventKey, Result<Admission, PollError>)>,
    {
        let mut agents = AgentQueues::watching(senders);
        self.load_github_routes(host).await;
        let mut queued = HashSet::new();
        let mut deferred = Vec::new();
        let mut active = FuturesUnordered::new();
        let mut active_keys = HashSet::new();
        let mut finished = VecDeque::new();
        let mut progress_generation = 0u64;
        loop {
            while let Some((key, agent, result, generation, started_progress)) =
                finished.pop_front()
            {
                active_keys.remove(&key);
                let queue = agents.get_mut(agent);
                queue.active = queue.active.saturating_sub(1);
                match result {
                    Ok(
                        Admission::Accepted
                        | Admission::Failed
                        | Admission::Uncertain
                        | Admission::Expired,
                    ) => {
                        queued.remove(&key);
                        progress_generation = progress_generation.wrapping_add(1);
                        agents.requeue(deferred.drain(..));
                    }
                    Ok(Admission::Deferred) if started_progress != progress_generation => {
                        queue.waiting.push_back(key);
                    }
                    Ok(Admission::Deferred) => deferred.push((key, agent)),
                    Err(PollError::Unavailable)
                        if senders.for_agent(agent).control_available()
                            && queue.changed_since(generation) =>
                    {
                        queue.waiting.push_back(key);
                    }
                    Err(PollError::Unavailable)
                        if !senders.for_agent(agent).control_available() =>
                    {
                        queue.offline.push_back(key);
                    }
                    Err(error) => {
                        let recorded = async {
                            if let Err(storage_error) = self.store.fail_waiting_event(&key).await {
                                tracing::error!(%storage_error, "could not record failed GitHub request");
                            }
                            if let Ok(Some(event)) = self.store.get(&key).await
                                && let Some(mut feedback) = GitHubFeedback::from_event(&event, host)
                            {
                                feedback.status = CommentStatus::Failed;
                                self.queue_github_feedback(feedback).await;
                            }
                        };
                        active.draining(recorded, &mut finished).await;
                        tracing::warn!(%error, "GitHub request failed before submission");
                        queued.remove(&key);
                        progress_generation = progress_generation.wrapping_add(1);
                        agents.requeue(deferred.drain(..));
                    }
                }
            }
            for agent in Agent::ALL {
                let queue = agents.get_mut(agent);
                while senders.for_agent(agent).control_available()
                    && queue.active < ROUTES_PER_AGENT
                {
                    let Some(key) = queue.waiting.pop_front() else {
                        break;
                    };
                    queue.active = queue.active.saturating_add(1);
                    active_keys.insert(key.clone());
                    let operation = route(key);
                    let generation = queue.generation;
                    let started_progress = progress_generation;
                    active.push(async move {
                        let (key, result) = operation.await;
                        (key, agent, result, generation, started_progress)
                    });
                }
            }
            tokio::select! {
                Some(message) = incoming.recv() => {
                    match message {
                        RoutingWake::Event(key, agent) => {
                            if queued.insert(key.clone()) {
                                agents.get_mut(agent).waiting.push_back(key);
                            }
                        }
                        RoutingWake::Sweep => {
                            let source = format!("github:{}", host.to_ascii_lowercase());
                            match active.draining(self.store.waiting_events(&source), &mut finished).await {
                                Ok(events) => {
                                    let retained: HashSet<_> = events.into_iter().map(|(key, _)| key).collect();
                                    agents.retain(&retained);
                                    deferred.retain(|(key, _)| retained.contains(key));
                                    queued.retain(|key| retained.contains(key) || active_keys.contains(key));
                                    progress_generation = progress_generation.wrapping_add(1);
                                    agents.requeue(deferred.drain(..));
                                }
                                Err(error) => tracing::error!(%error, "could not trim expired GitHub requests"),
                            }
                        }
                    }
                }
                Some(result) = active.next(), if !active.is_empty() => finished.push_back(result),
                (agent, changed) = agents.next_change() => {
                    let queue = agents.get_mut(agent);
                    if changed.is_err() {
                        tracing::warn!(%agent, "agent availability changes stopped");
                        queue.changes = None;
                    } else {
                        queue.generation = queue.generation.wrapping_add(1);
                        if !senders.for_agent(agent).control_available() { continue; }
                        queue.resume();
                        let (resumed, others) = deferred
                            .drain(..)
                            .partition::<Vec<_>, _>(|(_, deferred_agent)| *deferred_agent == agent);
                        deferred = others;
                        agents.requeue(resumed);
                        active.draining(self.load_github_routes(host), &mut finished).await;
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
                for (event, agent) in events {
                    self.enqueue_github_event(event, agent);
                }
            }
            Err(error) => tracing::error!(%error, "could not restore waiting GitHub requests"),
        }
    }

    async fn route_github_key(
        &self,
        source: &GitTools,
        projects: &ProjectsDirectory,
        senders: &AgentSenders<impl MessageSender, impl MessageSender>,
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
            let sender = senders.for_agent(event.options.agent);
            let result = self.admit_github_event(source, &sender, &settings, &event, GitHubDiscussion {
                repository: &feedback.repository, repository_id, number, workspace,
            }).await;
            let result = match result {
                Ok(Admission::Accepted) => {
                    let scope = DeliveryScope { host_id: &self.host_id, agent: event.options.agent.command_name() };
                    match self.store.dispatch_event(&event.key, scope, &sender, settings.waiting_cutoff(OffsetDateTime::now_utc())).await? {
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
                Err(PollError::Unavailable) => tracing::debug!(delivery_id = %event.key.delivery_id(), "GitHub request waits for its agent"),
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
            if let Err(error) = shortcut.1.validate() {
                tracing::warn!(trigger = %shortcut.0, %error, "invalid GitHub shortcut configuration");
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
                    self.enqueue_github_event(event.key.clone(), event.options.agent);
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
