pub mod codex;
pub mod github;
mod receipt;
mod sender;
mod settings;
mod shortcuts;
pub mod store;

pub use sender::{
    MessageAttempt, MessageReceipt, MessageSendError, MessageSender, UntrackedAttempt,
};
pub use settings::InboundSettings;
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
    #[error("{field} starts with a hyphen")]
    LeadingHyphen { field: &'static str },
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

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    impl InboundEvent {
        fn example() -> Self {
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
                chat_name: None,
                source_url: None,
                initial_context: None,
                actor: "789".to_owned(),
                created_at: datetime!(2026-09-29 12:00 UTC),
                message: "Explain the failing test.\nKeep its existing assertions.".to_owned(),
            }
        }
    }

    #[test]
    fn legacy_events_default_options_and_invalid_options_are_rejected() {
        let event = InboundEvent::example();
        let mut json = serde_json::to_value(&event).expect("event JSON");
        json.as_object_mut().expect("object").remove("options");
        json.as_object_mut().expect("object").remove("chat_name");
        json.as_object_mut().expect("object").remove("source_url");
        json.as_object_mut()
            .expect("object")
            .remove("initial_context");
        assert_eq!(
            serde_json::from_value::<InboundEvent>(json).expect("legacy event"),
            event
        );
        for model in ["".to_owned(), "has whitespace".to_owned(), "x".repeat(513)] {
            let mut invalid = event.clone();
            invalid.options.model = Some(model);
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn event_context_is_optional_and_bounded() {
        for (name, source_url) in [
            (Some("".to_owned()), None),
            (Some("é".repeat(257)), None),
            (None, Some(" ".to_owned())),
            (None, Some("x".repeat(2049))),
        ] {
            let mut event = InboundEvent::example();
            event.chat_name = name;
            event.source_url = source_url;
            assert!(event.validate().is_err());
        }
    }

    #[test]
    fn identifiers_are_scoped_to_the_source_and_conversation() {
        let original_event = InboundEvent::example();
        let mut different_source = original_event.clone();
        different_source.key.conversation.source = "api:local".to_owned();
        assert_ne!(original_event.key, different_source.key);
        assert_ne!(
            original_event.key.conversation,
            different_source.key.conversation
        );

        let mut different_conversation = original_event.clone();
        different_conversation.key.conversation.subject = "1234/88".to_owned();
        assert_ne!(original_event.key, different_conversation.key);

        let mut different_event = original_event.clone();
        different_event.key.id = "457".to_owned();
        assert_ne!(original_event.key, different_event.key);
        assert_eq!(
            original_event.key.conversation,
            different_event.key.conversation
        );
    }

    #[test]
    fn validation_preserves_message_formatting() {
        let mut event = InboundEvent::example();
        event.message = "  Explain this:\n\n```rust\n  run();\n```\n".to_owned();
        let event_before_validation = event.clone();
        event.validate().expect("a formatted message is valid");
        assert_eq!(event, event_before_validation);
    }

    #[test]
    fn empty_messages_and_invalid_identifiers_are_rejected() {
        let mut event = InboundEvent::example();
        event.message = " \n\t".to_owned();
        assert_eq!(
            event.validate(),
            Err(InvalidEvent::Empty { field: "message" })
        );

        for actor in ["", " ", "a\nb", "a\0b", "a\u{2003}b"] {
            let mut event = InboundEvent::example();
            event.actor = actor.to_owned();
            assert!(event.validate().is_err(), "invalid actor: {actor:?}");
        }
    }

    #[test]
    fn limits_count_bytes_including_multibyte_text() {
        let mut event = InboundEvent::example();
        event.message = "é".repeat(MAX_MESSAGE_BYTES / 2);
        event.validate().expect("the byte limit is inclusive");
        event.message.push('é');
        assert_eq!(
            event.validate(),
            Err(InvalidEvent::TooLong {
                field: "message",
                limit: MAX_MESSAGE_BYTES,
            })
        );

        event = InboundEvent::example();
        event.key.id = "a".repeat(MAX_IDENTIFIER_BYTES);
        event.validate().expect("the identifier limit is inclusive");
        event.key.id.push('a');
        assert_eq!(
            event.validate(),
            Err(InvalidEvent::TooLong {
                field: "event id",
                limit: MAX_IDENTIFIER_BYTES,
            })
        );
    }

    #[test]
    fn initial_context_counts_toward_the_message_limit_and_is_appended_once() {
        let mut event = InboundEvent::example();
        event.initial_context = Some("\n\nDescription:\n🦦".to_owned());
        let expanded = event.with_initial_context();
        assert_eq!(expanded.message.len(), event.message_bytes());
        assert!(expanded.message.ends_with("\n\nDescription:\n🦦"));
        assert_eq!(expanded.with_initial_context(), expanded);
        assert!(event.initial_context.is_some());
        event.initial_context = Some("x".repeat(MAX_MESSAGE_BYTES - event.message.len()));
        event.validate().expect("combined byte limit is inclusive");
        event.initial_context.as_mut().expect("context").push('x');
        assert!(matches!(
            event.validate(),
            Err(InvalidEvent::TooLong {
                field: "message",
                ..
            })
        ));
    }
}
