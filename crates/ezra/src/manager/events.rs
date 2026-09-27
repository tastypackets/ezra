use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::stream::{self, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, watch};
use utoipa::ToSchema;

const QUEUED_CHANGES: usize = 64;

/// A part of what the web app shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    Agents,
    ClaudeSettings,
    CodexSettings,
    Folders,
    Clones,
    RemoteControl,
    Git,
    Manager,
}

impl Topic {
    pub const ALL: [Self; 8] = [
        Self::Agents,
        Self::ClaudeSettings,
        Self::CodexSettings,
        Self::Folders,
        Self::Clones,
        Self::RemoteControl,
        Self::Git,
        Self::Manager,
    ];
}

/// One message on the event stream. Every change raises the revision by one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ManagerEvent {
    /// Sent first on every connection, with the current revision.
    Connected { revision: u64 },
    /// This part changed.
    Changed { topic: Topic, revision: u64 },
}

/// Tells connected web pages what changed. Revisions count up from the manager's start time in
/// milliseconds.
#[derive(Debug, Clone)]
pub struct Events {
    changes: broadcast::Sender<ManagerEvent>,
    revision: Arc<AtomicU64>,
    closed: Arc<watch::Sender<bool>>,
}

impl Default for Events {
    fn default() -> Self {
        Self {
            changes: broadcast::Sender::new(QUEUED_CHANGES),
            revision: Arc::new(AtomicU64::new(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |since| {
                        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
                    }),
            )),
            closed: Arc::new(watch::Sender::new(false)),
        }
    }
}

impl Events {
    pub fn publish(&self, topic: Topic) {
        let revision = self
            .revision
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let _nobody_listening = self.changes.send(ManagerEvent::Changed { topic, revision });
    }

    /// The revision of the latest change.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// Ends every stream.
    pub fn close(&self) {
        self.closed.send_replace(true);
    }

    /// `Connected`, then each change until closed. A stream that fell behind gets every topic.
    pub fn stream(&self) -> impl Stream<Item = ManagerEvent> + use<> {
        let listening = (
            Arc::clone(&self.revision),
            self.changes.subscribe(),
            self.closed.subscribe(),
        );
        let connected = ManagerEvent::Connected {
            revision: self.revision(),
        };
        let changes = stream::unfold(
            listening,
            |(revision, mut changes, mut closed)| async move {
                let received = tokio::select! {
                    received = changes.recv() => received,
                    _ = closed.wait_for(|closed| *closed) => return None,
                };
                let sent = match received {
                    Ok(event) => vec![event],
                    Err(RecvError::Lagged(_)) => {
                        changes = changes.resubscribe();
                        let revision = revision.load(Ordering::Relaxed);
                        Topic::ALL
                            .map(|topic| ManagerEvent::Changed { topic, revision })
                            .to_vec()
                    }
                    Err(RecvError::Closed) => return None,
                };
                Some((stream::iter(sent), (revision, changes, closed)))
            },
        )
        .flatten();
        stream::once(async move { connected }).chain(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_stream_starts_connected_then_sends_changes_until_closed() {
        let events = Events::default();
        let started = events.revision();
        events.publish(Topic::Agents);
        let mut stream = Box::pin(events.stream());
        assert_eq!(
            stream.next().await,
            Some(ManagerEvent::Connected {
                revision: started.wrapping_add(1)
            })
        );

        events.publish(Topic::Folders);
        assert_eq!(
            stream.next().await,
            Some(ManagerEvent::Changed {
                topic: Topic::Folders,
                revision: started.wrapping_add(2)
            })
        );

        events.close();
        assert_eq!(stream.next().await, None);
    }

    #[tokio::test]
    async fn a_stream_that_falls_behind_refetches_everything() {
        let events = Events::default();
        let mut stream = Box::pin(events.stream());
        stream.next().await;
        for _ in 0..=QUEUED_CHANGES {
            events.publish(Topic::Git);
        }
        let mut changed = Vec::new();
        for _ in Topic::ALL {
            changed.push(stream.next().await);
        }
        let revision = events.revision();
        assert_eq!(
            changed,
            Topic::ALL.map(|topic| Some(ManagerEvent::Changed { topic, revision }))
        );
    }

    #[test]
    fn events_are_sent_as_json() {
        assert_eq!(
            serde_json::to_string(&ManagerEvent::Connected { revision: 3 }).expect("serializes"),
            r#"{"event":"connected","revision":3}"#
        );
        assert_eq!(
            serde_json::to_string(&ManagerEvent::Changed {
                topic: Topic::RemoteControl,
                revision: 4
            })
            .expect("serializes"),
            r#"{"event":"changed","topic":"remote_control","revision":4}"#
        );
    }
}
