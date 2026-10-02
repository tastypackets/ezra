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

use super::{EventKey, InboundEvent, InvalidEvent};

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
        Ok(record.map(|record| InboundEvent {
            key: key.clone(),
            new_chat: record.new_chat,
            chat_name: record.chat_name,
            source_url: record.source_url,
            options: crate::inbound::Shortcut {
                agent: record.requested_agent,
                model: record.requested_model,
                effort: record.requested_effort,
            },
            actor: record.actor,
            created_at: record.created_at,
            message: record.message,
            initial_context: record.initial_context,
        }))
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
        let event = store
            .claim_next(None)
            .await
            .expect("old request claims")
            .expect("request remains");
        assert!(!event.new_chat);
        assert_eq!(event.message, "Continue");
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
