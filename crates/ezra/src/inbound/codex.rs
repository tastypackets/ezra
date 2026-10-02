use serde::{Deserialize, Serialize};

use super::InboundEvent;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueMessage {
    thread_id: String,
    client_user_message_id: String,
    input: [TextInput; 1],
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TextInput {
    Text { text: String },
}

impl QueueMessage {
    pub fn for_event(thread_id: String, event: &InboundEvent) -> Self {
        Self {
            thread_id,
            client_user_message_id: event.key.delivery_id(),
            input: [TextInput::Text {
                text: event.message.clone(),
            }],
        }
    }

    pub fn accepts_receipt(&self, receipt: &QueueMessageResponse) -> bool {
        !receipt.queued_submission.id.is_empty()
            && receipt.queued_submission.client_user_message_id == self.client_user_message_id
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueMessageResponse {
    pub queued_submission: QueuedMessageReceipt,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedMessageReceipt {
    pub id: String,
    pub client_user_message_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateChatSettings {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatedChatSettings {}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeChat {
    pub thread_id: String,
    pub exclude_turns: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnarchiveChat {
    pub thread_id: String,
}

#[derive(Debug, Deserialize)]
pub struct UnarchivedChat {
    pub thread: ChatSnapshot,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChat {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub cwd: String,
    pub ephemeral: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NameChat {
    pub thread_id: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct ListProjects {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPage {
    pub data: Vec<Project>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Project {
    pub id: String,
    pub roots: Vec<ProjectRoot>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectRoot {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct CreatedChat {
    pub thread: ChatSnapshot,
    pub cwd: String,
}

#[derive(Debug, Deserialize)]
pub struct ResumedChat {
    pub thread: ChatSnapshot,
    pub cwd: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSnapshot {
    pub project_id: Option<String>,
    pub id: String,
    pub status: ChatStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChatStatus {
    NotLoaded,
    Idle,
    Active,
    SystemError,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartQueuedMessage {
    pub thread_id: String,
    pub queued_submission_id: String,
}

#[derive(Debug, Deserialize)]
pub struct StartedQueuedMessage {
    pub turn: StartedTurn,
}

#[derive(Debug, Deserialize)]
pub struct StartedTurn {
    pub id: String,
    pub status: TurnStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    InProgress,
    Completed,
    Interrupted,
    Failed,
    #[serde(other)]
    Unknown,
}
