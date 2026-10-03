mod admission;
mod alias;
mod binding;
mod checkpoints;
mod delivery;
mod dispatch;
mod expiry;
mod feedback;
mod identity;
mod reconciliation;
#[path = "store/replace-chat.rs"]
mod replace_chat;
mod retention;
#[path = "store/route-claim.rs"]
mod route_claim;
mod routing;
#[path = "store/session-retention.rs"]
mod session_retention;

pub use admission::{InsertOutcome, QueueLimits};
pub use alias::AliasOutcome;
pub use binding::{BindOutcome, InvalidBinding, SessionTarget};
pub use checkpoints::SourceCheckpoint;
pub use delivery::{DeliveryOutcome, DeliveryScope, DeliveryState};
pub use dispatch::DispatchOutcome;
pub use retention::HistoryLimits;
pub use routing::RoutingOutcome;

use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use time::OffsetDateTime;

use super::{EventKey, InboundEvent, InvalidEvent, Shortcut};
use crate::agent::{Agent, UnknownAgent};

pub struct EventStore {
    pool: SqlitePool,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("source checkpoint no longer exists")]
    CheckpointMissing,
    #[error("claimed delivery no longer has its expected routing or state")]
    DeliveryChanged,
    #[error(transparent)]
    Sqlite(#[from] sqlx::Error),
    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error(transparent)]
    InvalidEvent(#[from] InvalidEvent),
    #[error(transparent)]
    InvalidBinding(#[from] InvalidBinding),
    #[error(transparent)]
    UnknownAgent(#[from] UnknownAgent),
}

impl Agent {
    /// Requests saved before shortcuts selected an agent have none.
    const UNRECORDED: Self = Self::Codex;

    fn stored(agent: Option<&str>) -> Result<Self, UnknownAgent> {
        agent.map_or(Ok(Self::UNRECORDED), str::parse)
    }
}

impl Shortcut {
    fn stored(
        agent: Option<String>,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<Self, StoreError> {
        let shortcut = Self {
            agent: Agent::stored(agent.as_deref())?,
            model,
            effort,
        };
        shortcut.validate()?;
        Ok(shortcut)
    }
}

impl EventStore {
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn get(&self, key: &EventKey) -> Result<Option<InboundEvent>, StoreError> {
        let record = sqlx::query!(
            r#"SELECT actor, created_at AS "created_at: OffsetDateTime", message, initial_context, requested_agent, requested_model, requested_effort, chat_name, source_url, new_chat AS "new_chat!: bool"
               FROM inbound_events
               WHERE source = ?1 AND subject = ?2 AND event_id = ?3"#,
            key.conversation.source,
            key.conversation.subject,
            key.id,
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(record) = record else {
            return Ok(None);
        };
        Ok(Some(InboundEvent {
            key: key.clone(),
            new_chat: record.new_chat,
            chat_name: record.chat_name,
            source_url: record.source_url,
            options: Shortcut::stored(
                record.requested_agent,
                record.requested_model,
                record.requested_effort,
            )?,
            actor: record.actor,
            created_at: record.created_at,
            message: record.message,
            initial_context: record.initial_context,
        }))
    }

    /// Run only during exclusive startup, before any delivery workers start.
    pub async fn fail_unknown_agents(&self) -> Result<Vec<(EventKey, UnknownAgent)>, StoreError> {
        let known = serde_json::to_string(&Agent::ALL).expect("agent names serialize");
        let records = sqlx::query!(
            r#"UPDATE inbound_events SET delivery_state = 'failed'
               WHERE delivery_state IN ('pending', 'delivering') AND attempted_at IS NULL
                 AND requested_agent IS NOT NULL
                 AND requested_agent NOT IN (SELECT value FROM json_each(?1))
               RETURNING source, subject, event_id, requested_agent AS "requested_agent!""#,
            known,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(records
            .into_iter()
            .map(|record| {
                let key = EventKey {
                    conversation: super::ConversationKey {
                        source: record.source,
                        subject: record.subject,
                    },
                    id: record.event_id,
                };
                (key, UnknownAgent(record.requested_agent))
            })
            .collect())
    }
}

#[cfg(test)]
const TEST_QUEUE_LIMITS: QueueLimits = QueueLimits {
    max_events: 100,
    max_message_bytes: 1024 * 1024,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::ConversationKey;
    use time::macros::datetime;

    impl InboundEvent {
        fn storage_example() -> Self {
            Self {
                key: EventKey {
                    conversation: ConversationKey {
                        source: "github:github.com".to_owned(),
                        subject: "1234/87".to_owned(),
                    },
                    id: "456".to_owned(),
                },
                new_chat: false,
                options: Default::default(),
                chat_name: Some("repository#87: Explain the failing test".to_owned()),
                source_url: Some(
                    "https://github.com/owner/repository/issues/87#issuecomment-456".to_owned(),
                ),
                initial_context: None,
                actor: "789".to_owned(),
                created_at: datetime!(2026-09-29 12:00:00.123456789 +02:00),
                message: "Explain 'this';\n\nKeep the formatting. 🦦".to_owned(),
            }
        }
    }

    #[tokio::test]
    async fn reopening_preserves_the_first_event_and_ignores_later_edits() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let original_event = InboundEvent::storage_example();
        let store = EventStore::open(&path).await.expect("new database opens");
        assert_eq!(
            store
                .get(&original_event.key)
                .await
                .expect("lookup succeeds"),
            None
        );
        assert_eq!(
            store
                .insert(&original_event, TEST_QUEUE_LIMITS)
                .await
                .expect("insert succeeds"),
            InsertOutcome::Inserted
        );
        drop(store);

        let store = EventStore::open(&path)
            .await
            .expect("existing database opens");
        let mut edited_event = original_event.clone();
        edited_event.message = "An edited request".to_owned();
        assert_eq!(
            store
                .insert(&edited_event, TEST_QUEUE_LIMITS)
                .await
                .expect("duplicate is handled"),
            InsertOutcome::Duplicate
        );
        assert_eq!(
            store.get(&original_event.key).await.expect("event reads"),
            Some(original_event.clone())
        );
        assert_eq!(
            store
                .claim_next(None)
                .await
                .expect("claim includes context"),
            Some(original_event)
        );
    }

    #[tokio::test]
    async fn identical_event_ids_in_other_sources_or_conversations_are_independent() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("database opens");
        for (source, subject) in [
            ("github:github.com", "1234/87"),
            ("api:local", "1234/87"),
            ("github:github.com", "1234/88"),
        ] {
            let mut event = InboundEvent::storage_example();
            event.key.conversation.source = source.to_owned();
            event.key.conversation.subject = subject.to_owned();
            assert_eq!(
                store
                    .insert(&event, TEST_QUEUE_LIMITS)
                    .await
                    .expect("insert succeeds"),
                InsertOutcome::Inserted
            );
            assert_eq!(
                store.get(&event.key).await.expect("event reads"),
                Some(event)
            );
        }
    }

    #[tokio::test]
    async fn invalid_events_are_not_persisted() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("database opens");
        let mut event = InboundEvent::storage_example();
        event.message.clear();
        assert!(matches!(
            store.insert(&event, TEST_QUEUE_LIMITS).await,
            Err(StoreError::InvalidEvent(_))
        ));
        assert_eq!(store.get(&event.key).await.expect("lookup succeeds"), None);
    }

    #[tokio::test]
    async fn concurrent_connections_accept_a_duplicate_only_once() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let first_store = EventStore::open(&path)
            .await
            .expect("first connection opens");
        let second_store = EventStore::open(&path)
            .await
            .expect("second connection opens");
        let event = InboundEvent::storage_example();
        let (first_outcome, second_outcome) = tokio::join!(
            first_store.insert(&event, TEST_QUEUE_LIMITS),
            second_store.insert(&event, TEST_QUEUE_LIMITS)
        );
        let outcomes = [
            first_outcome.expect("first insert succeeds"),
            second_outcome.expect("second insert succeeds"),
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == InsertOutcome::Inserted)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == InsertOutcome::Duplicate)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn reopening_preserves_requests_without_resetting_their_chat() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let pool = SqlitePoolOptions::new()
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("database opens");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("initial schema installs");
        sqlx::query!(
            "INSERT INTO inbound_events (source, subject, event_id, actor, created_at, message)
             VALUES ('github:github.com', '1234/87', '456', '789', '2026-09-29T10:00:00Z', 'Continue')",
        )
        .execute(&pool)
        .await
        .expect("existing request inserts");
        pool.close().await;
        let store = EventStore::open(&path).await.expect("database reopens");
        let waiting = store
            .waiting_events("github:github.com")
            .await
            .expect("old request waits");
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].1, Agent::Codex);
        let event = store
            .claim_next(None)
            .await
            .expect("old request claims")
            .expect("request remains");
        assert!(!event.new_chat);
        assert_eq!(event.message, "Continue");
        assert_eq!(event.options, Shortcut::default());
        assert_eq!(event.options.agent, Agent::Codex);
    }

    #[tokio::test]
    async fn stored_options_are_validated_when_read() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("database opens");
        sqlx::query(
            "INSERT INTO inbound_events (source, subject, event_id, actor, created_at, message, requested_agent, requested_model)
             VALUES ('github:github.com', '1234/87', '456', '789', '2026-09-29T10:00:00Z', 'Continue', 'claude', '--settings=x')",
        )
        .execute(&store.pool)
        .await
        .expect("tampered request inserts");
        let key = EventKey {
            conversation: ConversationKey {
                source: "github:github.com".to_owned(),
                subject: "1234/87".to_owned(),
            },
            id: "456".to_owned(),
        };
        assert!(matches!(
            store.get(&key).await,
            Err(StoreError::InvalidEvent(InvalidEvent::LeadingHyphen {
                field: "model"
            }))
        ));
    }

    #[tokio::test]
    async fn requests_for_unknown_agents_fail_without_blocking_known_ones() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("database opens");
        let known = InboundEvent::storage_example();
        store
            .insert(&known, TEST_QUEUE_LIMITS)
            .await
            .expect("known request inserts");
        for (event_id, state, attempted_at) in [
            ("unknown", "pending", None),
            ("unknown-attempted", "pending", Some(1)),
            ("unknown-delivered", "delivered", None),
        ] {
            sqlx::query(
                "INSERT INTO inbound_events (source, subject, event_id, actor, created_at, message, requested_agent, delivery_state, attempted_at)
                 VALUES ('github:github.com', 'unknown/1', ?1, '789', '2026-09-29T10:00:00Z', 'Continue', 'gemini', ?2, ?3)",
            )
            .bind(event_id)
            .bind(state)
            .bind(attempted_at)
            .execute(&store.pool)
            .await
            .expect("request from a newer version inserts");
        }
        let unknown = EventKey {
            conversation: ConversationKey {
                source: "github:github.com".to_owned(),
                subject: "unknown/1".to_owned(),
            },
            id: "unknown".to_owned(),
        };
        assert!(matches!(
            store.get(&unknown).await,
            Err(StoreError::UnknownAgent(agent)) if agent.0 == "gemini"
        ));
        assert_eq!(
            store
                .waiting_events("github:github.com")
                .await
                .expect("known requests still load"),
            [(known.key.clone(), Agent::Codex)]
        );
        assert_eq!(
            store.fail_unknown_agents().await.expect("sweep succeeds"),
            [(unknown.clone(), UnknownAgent("gemini".to_owned()))]
        );
        assert_eq!(
            store.delivery_state(&unknown).await.expect("state reads"),
            Some(DeliveryState::Failed)
        );
        for (event_id, state) in [
            ("unknown-attempted", DeliveryState::Pending),
            ("unknown-delivered", DeliveryState::Delivered),
        ] {
            let key = EventKey {
                id: event_id.to_owned(),
                ..unknown.clone()
            };
            assert_eq!(
                store.delivery_state(&key).await.expect("state reads"),
                Some(state)
            );
        }
        assert_eq!(
            store.delivery_state(&known.key).await.expect("state reads"),
            Some(DeliveryState::Pending)
        );
        assert!(
            store
                .fail_unknown_agents()
                .await
                .expect("sweep repeats")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn fresh_database_installs_one_schema_without_legacy_retry_state() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("initial schema installs");
        let migration_versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&store.pool)
                .await
                .expect("migration versions read");
        assert_eq!(migration_versions, vec![1]);
        let legacy_columns: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('inbound_events') WHERE name = 'next_delivery_at'",
        )
        .fetch_one(&store.pool)
        .await
        .expect("event columns read");
        assert_eq!(legacy_columns, 0);
        let legacy_tables: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'inbound_retention'",
        )
        .fetch_one(&store.pool)
        .await
        .expect("schema reads");
        assert_eq!(legacy_tables, 0);
    }

    #[tokio::test]
    async fn changed_migration_is_rejected_on_reopen() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra.db");
        let store = EventStore::open(&path).await.expect("database opens");
        sqlx::query!("UPDATE _sqlx_migrations SET checksum = X'00' WHERE version = 1")
            .execute(&store.pool)
            .await
            .expect("migration checksum changes");
        store.pool.close().await;
        assert!(matches!(
            EventStore::open(&path).await,
            Err(StoreError::Migration(
                sqlx::migrate::MigrateError::VersionMismatch(1)
            ))
        ));
    }
}
