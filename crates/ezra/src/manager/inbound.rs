mod github;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ezra::inbound::store::{EventStore, StoreError};
use ezra::inbound::{EventKey, MessageSender};
use time::OffsetDateTime;
use tokio::sync::{Mutex, RwLock, mpsc};

use github::GitHubFeedback;

use super::settings::Settings;

pub(super) enum RoutingWake {
    Event(EventKey),
    Sweep,
}

pub struct InboundRuntime {
    store: EventStore,
    host_id: String,
    settings: Arc<Mutex<Settings>>,
    github_feedback_sender: Mutex<Option<mpsc::Sender<GitHubFeedback>>>,
    github_scan: RwLock<()>,
    github_routing_sender: mpsc::UnboundedSender<RoutingWake>,
    github_routing_receiver: Mutex<Option<mpsc::UnboundedReceiver<RoutingWake>>>,
}

impl InboundRuntime {
    pub async fn open(path: &Path, settings: Arc<Mutex<Settings>>) -> Result<Self, StoreError> {
        let store = EventStore::open(path).await?;
        let host_id = store.host_id().await?;
        let recovered = store.recover_interrupted().await?;
        if recovered > 0 {
            tracing::warn!(recovered, "interrupted inbound operations recovered");
        }
        let (github_routing_sender, github_routing_receiver) = mpsc::unbounded_channel();
        let maintenance = Self {
            store,
            host_id,
            settings,
            github_feedback_sender: Mutex::new(None),
            github_scan: RwLock::new(()),
            github_routing_sender,
            github_routing_receiver: Mutex::new(Some(github_routing_receiver)),
        };
        maintenance.sweep_history().await?;
        tracing::info!(host_id = %maintenance.host_id, "inbound storage ready");
        Ok(maintenance)
    }

    pub async fn run(
        self,
        sender: Arc<impl MessageSender>,
        source: super::git::GitTools,
        projects: super::folders::ProjectsDirectory,
    ) {
        let (status_sender, status_receiver) = mpsc::channel(64);
        *self.github_feedback_sender.lock().await = Some(status_sender);
        if let Err(error) = self.sweep().await {
            tracing::warn!(%error, "could not clean up inbound history at startup");
        }
        tokio::join!(
            self.maintain(),
            self.poll_github(&source, &projects, sender.as_ref()),
            self.route_github(&source, &projects, sender.as_ref()),
            self.delivery_feedback(&source, status_receiver),
        );
    }

    pub(super) fn enqueue_github_event(&self, event: EventKey) {
        if let Err(error) = self.github_routing_sender.send(RoutingWake::Event(event)) {
            tracing::error!(%error, "GitHub routing receiver is closed");
        }
    }

    async fn delivery_feedback(
        &self,
        source: &super::git::GitTools,
        mut statuses: mpsc::Receiver<GitHubFeedback>,
    ) {
        while let Some(status) = statuses.recv().await {
            if let Err(error) = self.publish_github_feedback(source, &status).await {
                tracing::warn!(%error, "could not record GitHub status feedback");
            }
        }
    }

    async fn maintain(&self) {
        loop {
            tokio::time::sleep(Duration::from_hours(24)).await;
            if let Err(error) = self.sweep().await {
                tracing::warn!("could not clean up inbound history: {error}");
            }
        }
    }

    async fn sweep(&self) -> Result<(), StoreError> {
        let settings = self.settings.lock().await.inbound.clone();
        self.expire_waiting_requests(&settings).await?;
        self.sweep_history().await
    }

    async fn sweep_history(&self) -> Result<(), StoreError> {
        let settings = self.settings.lock().await.inbound.clone();
        let cutoff = settings.retention_cutoff(OffsetDateTime::now_utc());
        let events_removed = self
            .store
            .prune_history(cutoff, settings.history_limits())
            .await?;
        let sessions_removed = self
            .store
            .prune_sessions(cutoff, settings.max_idle_conversations)
            .await?;
        let checkpoints_removed = self
            .store
            .prune_source_checkpoints(cutoff, settings.max_idle_conversations)
            .await?;
        let feedback_removed = self
            .store
            .prune_feedback(cutoff, settings.max_history_events)
            .await?;
        if events_removed > 0
            || sessions_removed > 0
            || checkpoints_removed > 0
            || feedback_removed > 0
        {
            tracing::info!(
                events_removed,
                sessions_removed,
                checkpoints_removed,
                feedback_removed,
                "expired inbound history removed"
            );
        }
        Ok(())
    }
}
