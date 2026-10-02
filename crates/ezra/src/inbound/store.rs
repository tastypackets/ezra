mod admission;
mod alias;
mod binding;
mod delivery;
mod expiry;
mod identity;
#[path = "store/route-claim.rs"]
mod route_claim;
mod routing;

pub use admission::{InsertOutcome, QueueLimits};
pub use alias::AliasOutcome;
pub use binding::{BindOutcome, InvalidBinding, SessionTarget};
pub use delivery::{DeliveryOutcome, DeliveryScope, DeliveryState};
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
