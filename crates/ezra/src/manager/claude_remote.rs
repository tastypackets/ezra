mod models;

use ezra::inbound::{
    InboundEvent, MessageAttempt, MessageReceipt, MessageSendError, MessageSender, Shortcut,
};

pub struct ClaudeRemote;

impl ClaudeRemote {
    pub fn new() -> Self {
        Self
    }
}

impl MessageSender for ClaudeRemote {
    fn control_available(&self) -> bool {
        false
    }

    async fn create_chat(
        &self,
        _workspace: &str,
        _chat_name: Option<&str>,
        _options: &Shortcut,
    ) -> Result<String, MessageSendError> {
        Err(MessageSendError::Unavailable)
    }

    async fn queue_message(
        &self,
        _chat_id: &str,
        _event: &InboundEvent,
    ) -> Result<MessageReceipt, MessageSendError> {
        Err(MessageSendError::Unavailable)
    }

    async fn queue_message_tracked(
        &self,
        _chat_id: &str,
        _event: &InboundEvent,
        _attempt: &(impl MessageAttempt + Sync),
    ) -> Result<MessageReceipt, MessageSendError> {
        Err(MessageSendError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use ezra::inbound::{ConversationKey, EventKey};

    use super::*;

    struct Attempt(AtomicBool);

    impl MessageAttempt for Attempt {
        async fn mark_attempted(&self, _key: &EventKey) -> Result<(), MessageSendError> {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn the_placeholder_is_unavailable_and_never_marks_an_attempt() {
        let remote = ClaudeRemote::new();
        assert!(!remote.control_available());
        assert!(remote.control_changes().is_none());
        assert!(matches!(
            remote
                .create_chat("/home/dev/projects", None, &Shortcut::default())
                .await,
            Err(MessageSendError::Unavailable)
        ));
        let event = InboundEvent {
            key: EventKey {
                conversation: ConversationKey {
                    source: "github:github.com".to_owned(),
                    subject: "1/2".to_owned(),
                },
                id: "3".to_owned(),
            },
            new_chat: false,
            options: Shortcut::default(),
            chat_name: None,
            source_url: None,
            actor: "4".to_owned(),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            message: "Continue".to_owned(),
            initial_context: None,
        };
        assert!(matches!(
            remote.queue_message("session_1", &event).await,
            Err(MessageSendError::Unavailable)
        ));
        let attempt = Attempt(AtomicBool::new(false));
        assert!(matches!(
            remote
                .queue_message_tracked("session_1", &event, &attempt)
                .await,
            Err(MessageSendError::Unavailable)
        ));
        assert!(!attempt.0.load(Ordering::SeqCst));
    }
}
