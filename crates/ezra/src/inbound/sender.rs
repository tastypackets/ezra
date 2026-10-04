use std::future::Future;

use super::{EventKey, InboundEvent, Shortcut};

pub trait MessageAttempt {
    fn mark_attempted(
        &self,
        key: &EventKey,
    ) -> impl Future<Output = Result<(), MessageSendError>> + Send;

    fn mark_rejected(
        &self,
        _key: &EventKey,
    ) -> impl Future<Output = Result<(), MessageSendError>> + Send {
        async { Ok(()) }
    }
}

pub struct UntrackedAttempt;

impl MessageAttempt for UntrackedAttempt {
    async fn mark_attempted(&self, _key: &EventKey) -> Result<(), MessageSendError> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageReceipt {
    pub chat_name: Option<String>,
    pub native_message_id: String,
    pub delivery_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MessageSendError {
    /// No native send was attempted.
    #[error("the agent control connection is unavailable")]
    Unavailable,
    /// The agent confirmed that the destination cannot accept this message.
    #[error("chat needs replacement: {0}")]
    NeedsReplacement(String),
    #[error("message receipt is uncertain: {0}")]
    Uncertain(String),
}

pub trait MessageSender: Sync {
    fn control_available(&self) -> bool {
        true
    }

    fn control_changes(&self) -> Option<tokio::sync::watch::Receiver<bool>> {
        None
    }

    fn create_chat(
        &self,
        workspace: &str,
        chat_name: Option<&str>,
        options: &Shortcut,
    ) -> impl Future<Output = Result<String, MessageSendError>> + Send;

    fn queue_message(
        &self,
        chat_id: &str,
        event: &InboundEvent,
    ) -> impl Future<Output = Result<MessageReceipt, MessageSendError>> + Send;

    fn queue_message_tracked(
        &self,
        chat_id: &str,
        event: &InboundEvent,
        attempt: &(impl MessageAttempt + Sync),
    ) -> impl Future<Output = Result<MessageReceipt, MessageSendError>> + Send {
        async move {
            attempt.mark_attempted(&event.key).await?;
            self.queue_message(chat_id, event).await
        }
    }
}
