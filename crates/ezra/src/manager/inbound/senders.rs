use std::sync::Arc;

use ezra::agent::Agent;
use ezra::inbound::{
    InboundEvent, MessageAttempt, MessageReceipt, MessageSendError, MessageSender, Shortcut,
};
use tokio::sync::watch;

pub struct AgentSenders<Claude, Codex> {
    pub claude: Arc<Claude>,
    pub codex: Arc<Codex>,
}

pub enum AgentSender<'a, Claude, Codex> {
    Claude(&'a Claude),
    Codex(&'a Codex),
}

impl<Claude, Codex> AgentSenders<Claude, Codex> {
    pub fn for_agent(&self, agent: Agent) -> AgentSender<'_, Claude, Codex> {
        match agent {
            Agent::Claude => AgentSender::Claude(&self.claude),
            Agent::Codex => AgentSender::Codex(&self.codex),
        }
    }
}

impl<Claude: MessageSender, Codex: MessageSender> MessageSender for AgentSender<'_, Claude, Codex> {
    fn control_available(&self) -> bool {
        match self {
            Self::Claude(sender) => sender.control_available(),
            Self::Codex(sender) => sender.control_available(),
        }
    }

    fn control_changes(&self) -> Option<watch::Receiver<bool>> {
        match self {
            Self::Claude(sender) => sender.control_changes(),
            Self::Codex(sender) => sender.control_changes(),
        }
    }

    async fn create_chat(
        &self,
        workspace: &str,
        chat_name: Option<&str>,
        options: &Shortcut,
    ) -> Result<String, MessageSendError> {
        match self {
            Self::Claude(sender) => sender.create_chat(workspace, chat_name, options).await,
            Self::Codex(sender) => sender.create_chat(workspace, chat_name, options).await,
        }
    }

    async fn queue_message(
        &self,
        chat_id: &str,
        event: &InboundEvent,
    ) -> Result<MessageReceipt, MessageSendError> {
        match self {
            Self::Claude(sender) => sender.queue_message(chat_id, event).await,
            Self::Codex(sender) => sender.queue_message(chat_id, event).await,
        }
    }

    async fn queue_message_tracked(
        &self,
        chat_id: &str,
        event: &InboundEvent,
        attempt: &(impl MessageAttempt + Sync),
    ) -> Result<MessageReceipt, MessageSendError> {
        match self {
            Self::Claude(sender) => sender.queue_message_tracked(chat_id, event, attempt).await,
            Self::Codex(sender) => sender.queue_message_tracked(chat_id, event, attempt).await,
        }
    }
}
