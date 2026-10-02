pub mod codex;
mod receipt;
mod sender;
mod shortcuts;
pub mod store;

pub use sender::{
    MessageAttempt, MessageReceipt, MessageSendError, MessageSender, UntrackedAttempt,
};
pub use shortcuts::{Shortcut, ShortcutError};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

const MAX_IDENTIFIER_BYTES: usize = 512;
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConversationKey {
    /// Includes the integration and its server, such as github:github.com.
    pub source: String,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventKey {
    pub conversation: ConversationKey,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundEvent {
    pub key: EventKey,
    #[serde(default)]
    pub new_chat: bool,
    #[serde(default)]
    pub options: Shortcut,
    #[serde(default)]
    pub chat_name: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    /// Identifies the requester within the source. It does not grant authorization.
    pub actor: String,
    /// Original creation time from the source, unchanged by edits.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub message: String,
    /// Appended only when delivering the first message to a new native chat.
    #[serde(default)]
    pub initial_context: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidEvent {
    #[error("{field} is empty")]
    Empty { field: &'static str },
    #[error("{field} exceeds {limit} bytes")]
    TooLong { field: &'static str, limit: usize },
    #[error("{field} contains whitespace or control characters")]
    InvalidIdentifier { field: &'static str },
}

impl InboundEvent {
    pub fn message_bytes(&self) -> usize {
        self.message
            .len()
            .saturating_add(self.initial_context.as_ref().map_or(0, String::len))
    }

    pub fn with_initial_context(&self) -> Self {
        let mut event = self.clone();
        if let Some(context) = event.initial_context.take() {
            event.message.push_str(&context);
        }
        event
    }

    pub fn validate(&self) -> Result<(), InvalidEvent> {
        for (field, identifier) in [
            ("source", self.key.conversation.source.as_str()),
            ("subject", self.key.conversation.subject.as_str()),
            ("event id", self.key.id.as_str()),
            ("actor", self.actor.as_str()),
        ] {
            if identifier.is_empty() {
                return Err(InvalidEvent::Empty { field });
            }
            if identifier.len() > MAX_IDENTIFIER_BYTES {
                return Err(InvalidEvent::TooLong {
                    field,
                    limit: MAX_IDENTIFIER_BYTES,
                });
            }
            if identifier
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
            {
                return Err(InvalidEvent::InvalidIdentifier { field });
            }
        }
        self.options.validate()?;
        for (field, value, limit) in [
            ("chat name", self.chat_name.as_deref(), 512),
            ("source URL", self.source_url.as_deref(), 2048),
        ] {
            if let Some(value) = value {
                if value.trim().is_empty() {
                    return Err(InvalidEvent::Empty { field });
                }
                if value.len() > limit {
                    return Err(InvalidEvent::TooLong { field, limit });
                }
            }
        }
        if self.message_bytes() > MAX_MESSAGE_BYTES {
            return Err(InvalidEvent::TooLong {
                field: "message",
                limit: MAX_MESSAGE_BYTES,
            });
        }
        if self.message.trim().is_empty() {
            return Err(InvalidEvent::Empty { field: "message" });
        }
        Ok(())
    }
}
