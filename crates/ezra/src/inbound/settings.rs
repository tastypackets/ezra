use std::collections::BTreeMap;
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use super::Shortcut;
use super::github::GitHubSettings;
use time::{Duration, OffsetDateTime};

use super::store::{HistoryLimits, QueueLimits};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct InboundSettings {
    pub github: GitHubSettings,
    pub shortcuts: BTreeMap<String, Shortcut>,
    pub retention_days: u32,
    #[schema(value_type = u32, minimum = 1, default = 24)]
    pub waiting_expiry_hours: NonZeroU32,
    pub max_queued_events: u32,
    pub max_queued_message_bytes: u64,
    pub max_history_events: u32,
    pub max_history_message_bytes: u64,
    pub max_idle_conversations: u32,
}

impl Default for InboundSettings {
    fn default() -> Self {
        Self {
            github: GitHubSettings::default(),
            shortcuts: BTreeMap::from([("/ezra".to_owned(), Shortcut::default())]),
            retention_days: 90,
            waiting_expiry_hours: NonZeroU32::new(24).expect("default waiting expiry is positive"),
            max_queued_events: 1_000,
            max_queued_message_bytes: 16 * 1024 * 1024,
            max_history_events: 100_000,
            max_history_message_bytes: 256 * 1024 * 1024,
            max_idle_conversations: 10_000,
        }
    }
}

impl InboundSettings {
    pub fn waiting_cutoff(&self, now: OffsetDateTime) -> OffsetDateTime {
        now.saturating_sub(Duration::hours(i64::from(self.waiting_expiry_hours.get())))
    }

    pub fn retention_cutoff(&self, now: OffsetDateTime) -> OffsetDateTime {
        now.saturating_sub(Duration::days(i64::from(self.retention_days)))
    }

    pub fn queue_limits(&self) -> QueueLimits {
        QueueLimits {
            max_events: self.max_queued_events,
            max_message_bytes: self.max_queued_message_bytes,
        }
    }

    pub fn history_limits(&self) -> HistoryLimits {
        HistoryLimits {
            max_events: self.max_history_events,
            max_message_bytes: self.max_history_message_bytes,
        }
    }
}
