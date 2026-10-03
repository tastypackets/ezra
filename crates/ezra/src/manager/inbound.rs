mod github;
mod senders;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ezra::agent::Agent;
use ezra::inbound::store::{EventStore, StoreError};
use ezra::inbound::{EventKey, MessageSender};
use time::OffsetDateTime;
use tokio::sync::{Mutex, RwLock, mpsc};

use github::GitHubFeedback;

use super::settings::Settings;
pub use senders::AgentSenders;

pub(super) enum RoutingWake {
    Event(EventKey, Agent),
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
        for (key, error) in store.fail_unknown_agents().await? {
            tracing::warn!(delivery_id = %key.delivery_id(), %error, "inbound request failed");
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
        senders: AgentSenders<impl MessageSender, impl MessageSender>,
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
            self.poll_github(&source, &projects),
            self.route_github(&source, &projects, &senders),
            self.delivery_feedback(&source, status_receiver),
        );
    }

    pub(super) fn enqueue_github_event(&self, event: EventKey, agent: Agent) {
        if let Err(error) = self
            .github_routing_sender
            .send(RoutingWake::Event(event, agent))
        {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ezra::inbound::store::{DeliveryOutcome, DeliveryState, InsertOutcome, SessionTarget};
    use ezra::inbound::{ConversationKey, EventKey, InboundEvent};

    trait InboundEventExt {
        fn maintenance_example(subject: &str) -> Self;
    }

    impl InboundEventExt for InboundEvent {
        fn maintenance_example(subject: &str) -> Self {
            Self {
                key: EventKey {
                    conversation: ConversationKey {
                        source: "github:github.com".to_owned(),
                        subject: subject.to_owned(),
                    },
                    id: "comment-1".to_owned(),
                },
                new_chat: false,
                options: Default::default(),
                chat_name: None,
                source_url: None,
                initial_context: None,
                actor: "author".to_owned(),
                created_at: OffsetDateTime::now_utc()
                    .checked_sub(time::Duration::days(100))
                    .expect("one hundred days ago fits in the supported date range"),
                message: "Continue this work".to_owned(),
            }
        }
    }

    #[tokio::test]
    async fn startup_recovers_attempted_deliveries_and_prunes_history_by_receipt_age() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("store opens");
        let settings = Settings::default();
        let completed = InboundEvent::maintenance_example("completed");
        let interrupted = InboundEvent::maintenance_example("interrupted");
        let pending = InboundEvent::maintenance_example("pending");
        for event in [&completed, &interrupted, &pending] {
            store
                .insert(event, settings.inbound.queue_limits())
                .await
                .expect("event inserts");
        }
        assert_eq!(
            store.claim_next(None).await.expect("completed claims"),
            Some(completed.clone())
        );
        store
            .finish_delivery(&completed.key, DeliveryOutcome::Delivered, None)
            .await
            .expect("delivery finishes");
        assert_eq!(
            store.claim_next(None).await.expect("interrupted claims"),
            Some(interrupted.clone())
        );
        let fixture_pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", path.display()))
            .await
            .expect("fixture database connects");
        let old_receipt = completed.created_at.unix_timestamp();
        sqlx::query("UPDATE inbound_events SET received_at = ?1 WHERE subject = 'completed'")
            .bind(old_receipt)
            .execute(&fixture_pool)
            .await
            .expect("completed history has an old receipt");
        sqlx::query(
            "UPDATE inbound_events SET attempted_at = unixepoch() WHERE subject = 'interrupted'",
        )
        .execute(&fixture_pool)
        .await
        .expect("interrupted message was actually submitted");
        fixture_pool.close().await;
        drop(store);
        let maintenance = InboundRuntime::open(&path, Arc::new(Mutex::new(settings)))
            .await
            .expect("startup succeeds");
        assert_eq!(
            maintenance
                .store
                .get(&completed.key)
                .await
                .expect("expired lookup"),
            None
        );
        assert_eq!(
            maintenance
                .store
                .delivery_state(&interrupted.key)
                .await
                .expect("interrupted state"),
            Some(DeliveryState::Uncertain)
        );
        assert_eq!(
            maintenance
                .store
                .delivery_state(&pending.key)
                .await
                .expect("pending state"),
            Some(DeliveryState::Pending)
        );
        assert_eq!(
            maintenance
                .store
                .insert(
                    &completed,
                    maintenance.settings.lock().await.inbound.queue_limits()
                )
                .await
                .expect("new trigger in old comment is accepted after history expires"),
            InsertOutcome::Inserted
        );
    }

    #[tokio::test]
    async fn later_sweeps_use_updated_settings_and_preserve_queued_sessions() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let settings = Arc::new(Mutex::new(Settings::default()));
        let maintenance = InboundRuntime::open(&path, Arc::clone(&settings))
            .await
            .expect("startup creates database");
        let mut delivered = InboundEvent::maintenance_example("delivered");
        delivered.created_at = OffsetDateTime::now_utc();
        let mut pending = InboundEvent::maintenance_example("pending");
        pending.created_at = OffsetDateTime::now_utc();
        let queue_limits = settings.lock().await.inbound.queue_limits();
        for event in [&delivered, &pending] {
            maintenance
                .store
                .bind_conversation(
                    &event.key.conversation,
                    &SessionTarget {
                        host_id: "host-a".to_owned(),
                        agent: "codex".to_owned(),
                        chat_id: event.key.conversation.subject.clone(),
                        workspace: "/home/dev/projects/repository".to_owned(),
                    },
                )
                .await
                .expect("session binds");
            maintenance
                .store
                .insert(event, queue_limits)
                .await
                .expect("event inserts");
        }
        maintenance
            .store
            .claim_next(None)
            .await
            .expect("first event claims");
        maintenance
            .store
            .finish_delivery(&delivered.key, DeliveryOutcome::Delivered, None)
            .await
            .expect("first event delivered");
        maintenance.sweep().await.expect("default sweep succeeds");
        assert!(
            maintenance
                .store
                .get(&delivered.key)
                .await
                .expect("recent history retained")
                .is_some()
        );
        {
            let mut updated = settings.lock().await;
            updated.inbound.max_history_events = 0;
            updated.inbound.max_idle_conversations = 0;
        }
        maintenance.sweep().await.expect("updated limits apply");
        assert_eq!(
            maintenance
                .store
                .get(&delivered.key)
                .await
                .expect("history removed"),
            None
        );
        assert_eq!(
            maintenance
                .store
                .find_binding(&delivered.key.conversation)
                .await
                .expect("idle binding removed"),
            None
        );
        assert!(
            maintenance
                .store
                .find_binding(&pending.key.conversation)
                .await
                .expect("queued binding retained")
                .is_some()
        );
        assert_eq!(
            maintenance
                .store
                .delivery_state(&pending.key)
                .await
                .expect("pending retained"),
            Some(DeliveryState::Pending)
        );
    }

    #[tokio::test]
    async fn startup_fails_waiting_requests_for_unknown_agents() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("store opens");
        let settings = Settings::default();
        let known = InboundEvent::maintenance_example("known");
        store
            .insert(&known, settings.inbound.queue_limits())
            .await
            .expect("known request inserts");
        drop(store);
        let fixture_pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", path.display()))
            .await
            .expect("fixture database connects");
        sqlx::query(
            "INSERT INTO inbound_events (source, subject, event_id, actor, created_at, message, requested_agent)
             VALUES ('github:github.com', 'unknown', 'comment-1', 'author', '2026-09-29T10:00:00Z', 'Continue', 'gemini')",
        )
        .execute(&fixture_pool)
        .await
        .expect("request from a newer version inserts");
        fixture_pool.close().await;
        let runtime = InboundRuntime::open(&path, Arc::new(Mutex::new(settings)))
            .await
            .expect("startup succeeds");
        let unknown = EventKey {
            conversation: ConversationKey {
                source: "github:github.com".to_owned(),
                subject: "unknown".to_owned(),
            },
            id: "comment-1".to_owned(),
        };
        assert_eq!(
            runtime
                .store
                .delivery_state(&unknown)
                .await
                .expect("unknown state"),
            Some(DeliveryState::Failed)
        );
        assert_eq!(
            runtime
                .store
                .delivery_state(&known.key)
                .await
                .expect("known state"),
            Some(DeliveryState::Pending)
        );
    }

    #[tokio::test]
    async fn an_invalid_database_fails_startup_without_replacing_it() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let contents = b"not a sqlite database";
        std::fs::write(&path, contents).expect("invalid database writes");
        assert!(
            InboundRuntime::open(&path, Arc::new(Mutex::new(Settings::default())))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).expect("original file reads"), contents);
    }
}
