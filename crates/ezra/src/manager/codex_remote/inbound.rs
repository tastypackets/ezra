use std::collections::BTreeSet;
use std::path::Path;

use ezra::inbound::InboundEvent;
use ezra::inbound::codex::{
    CreateChat, ListProjects, NameChat, QueueMessage, QueueMessageResponse, ResumeChat,
    UnarchiveChat, UpdateChatSettings,
};
use ezra::inbound::{
    MessageAttempt, MessageReceipt, MessageSendError, MessageSender, UntrackedAttempt,
};

use super::CodexRemote;
use super::control::{ChatNameRead, ControlClient, ControlError, ThreadId};

impl ControlError {
    fn is_archive_rejection(&self) -> bool {
        matches!(self, Self::Codex { code: -32600, message }
            if message.contains("is archived"))
    }

    fn into_send_error(self, chat_id: &str) -> MessageSendError {
        let missing = match &self {
            Self::Codex {
                code: -32600,
                message,
            } => {
                message == &format!("no rollout found for thread id {chat_id}")
                    || message == &format!("no archived rollout found for thread id {chat_id}")
            }
            Self::Codex {
                code: -32603,
                message,
            } => {
                message
                    == &format!(
                        "failed to read thread: invalid thread-store request: no rollout found for thread id {chat_id}"
                    )
            }
            _ => false,
        };
        if missing {
            MessageSendError::NeedsReplacement(self.to_string())
        } else {
            MessageSendError::Uncertain(self.to_string())
        }
    }
}

impl ControlClient {
    async fn queue_event(
        &self,
        chat_id: &str,
        event: &InboundEvent,
        attempt: Option<&(impl MessageAttempt + Sync)>,
    ) -> Result<QueueMessageResponse, ControlError> {
        if event.options.model.is_some() || event.options.effort.is_some() {
            let update = UpdateChatSettings {
                thread_id: chat_id.to_owned(),
                model: event.options.model.clone(),
                effort: event.options.effort.clone(),
            };
            let updated = match self.request(update.clone()).await {
                Err(ControlError::Codex {
                    code: -32600,
                    message,
                }) if message == format!("thread not found: {chat_id}") => {
                    let resumed = self
                        .request(ResumeChat {
                            thread_id: chat_id.to_owned(),
                            exclude_turns: true,
                        })
                        .await?;
                    if resumed.thread.id != chat_id {
                        return Err(ControlError::Io(std::io::Error::other(
                            "Codex resumed a different chat",
                        )));
                    }
                    self.request(update).await
                }
                result => result,
            };
            match updated {
                Err(error @ ControlError::Codex { .. }) if !error.is_archive_rejection() => {
                    tracing::warn!(%error, "Codex rejected inbound settings, using the chat's current settings");
                }
                result => {
                    result?;
                }
            }
        }
        if let Some(attempt) = attempt {
            attempt
                .mark_attempted(&event.key)
                .await
                .map_err(|error| ControlError::Io(std::io::Error::other(error.to_string())))?;
        }
        let result = self
            .request(QueueMessage::for_event(chat_id.to_owned(), event))
            .await;
        if matches!(&result, Err(ControlError::Codex { .. }))
            && let Some(attempt) = attempt
        {
            attempt
                .mark_rejected(&event.key)
                .await
                .map_err(|error| ControlError::Io(std::io::Error::other(error.to_string())))?;
        }
        result
    }

    async fn deliver_event(
        &self,
        chat_id: &str,
        event: &InboundEvent,
        attempt: Option<&(impl MessageAttempt + Sync)>,
    ) -> Result<MessageReceipt, MessageSendError> {
        let client = self;
        let _chat_hold = client.hold_chat(ThreadId(chat_id.to_owned())).await;
        let request = QueueMessage::for_event(chat_id.to_owned(), event);
        let receipt = match client.queue_event(chat_id, event, attempt).await {
            Err(error) if error.is_archive_rejection() => {
                let restored = client
                    .request(UnarchiveChat {
                        thread_id: chat_id.to_owned(),
                    })
                    .await
                    .map_err(|error| error.into_send_error(chat_id))?;
                if restored.thread.id != chat_id {
                    return Err(MessageSendError::Uncertain(
                        "Codex unarchived a different chat".to_owned(),
                    ));
                }
                let resumed = client
                    .request(ResumeChat {
                        thread_id: chat_id.to_owned(),
                        exclude_turns: true,
                    })
                    .await
                    .map_err(|error| error.into_send_error(chat_id))?;
                if resumed.thread.id != chat_id {
                    return Err(MessageSendError::Uncertain(
                        "Codex resumed a different chat".to_owned(),
                    ));
                }
                tracing::info!("Codex chat unarchived for inbound delivery");
                client.queue_event(chat_id, event, attempt).await
            }
            result => result,
        }
        .map_err(|error| error.into_send_error(chat_id))?;
        if !request.accepts_receipt(&receipt) {
            return Err(MessageSendError::Uncertain(
                "Codex returned a receipt that does not match the message".to_owned(),
            ));
        }
        let chat_name = match client
            .request(ChatNameRead {
                thread_id: ThreadId(chat_id.to_owned()),
            })
            .await
        {
            Ok(snapshot) if snapshot.thread.id.0 == chat_id => snapshot.thread.name,
            Err(ControlError::Codex { code: -32601, .. }) => {
                tracing::debug!("Codex does not support chat metadata lookup");
                None
            }
            Ok(_) => {
                tracing::warn!("Codex returned metadata for a different chat");
                None
            }
            Err(error) => {
                tracing::warn!(%error, "could not confirm the delivered chat name");
                None
            }
        };
        Ok(MessageReceipt {
            chat_name,
            native_message_id: receipt.queued_submission.id,
            delivery_id: receipt.queued_submission.client_user_message_id,
        })
    }

    async fn project_for_workspace(&self, workspace: &str) -> Result<Option<String>, ControlError> {
        let mut cursor = None;
        let mut seen_cursors = BTreeSet::new();
        let mut matches = BTreeSet::new();
        let mut deepest_root = 0;
        loop {
            let page = match self.request(ListProjects { cursor, limit: 100 }).await {
                Ok(page) => page,
                Err(ControlError::Codex { code: -32601, .. }) => {
                    tracing::debug!("Codex does not support project lookup");
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            for project in page.data {
                for root in project.roots {
                    let root = Path::new(&root.path);
                    if root.is_absolute() && Path::new(workspace).starts_with(root) {
                        let depth = root.components().count();
                        if depth > deepest_root {
                            matches.clear();
                            deepest_root = depth;
                        }
                        if depth == deepest_root {
                            matches.insert(project.id.clone());
                        }
                    }
                }
            }
            let Some(next) = page.next_cursor else { break };
            if seen_cursors.len() >= 100 || !seen_cursors.insert(next.clone()) {
                return Err(ControlError::Io(std::io::Error::other(
                    "Codex project pagination did not finish",
                )));
            }
            cursor = Some(next);
        }
        if matches.len() > 1 || matches.contains("") {
            tracing::debug!("omitting ambiguous Codex project association");
            return Ok(None);
        }
        Ok(matches.into_iter().next())
    }
}

impl MessageSender for CodexRemote {
    fn control_available(&self) -> bool {
        self.client().is_some()
    }

    fn control_changes(&self) -> Option<tokio::sync::watch::Receiver<bool>> {
        Some(self.control_available.subscribe())
    }

    async fn create_chat(
        &self,
        workspace: &str,
        chat_name: Option<&str>,
    ) -> Result<String, MessageSendError> {
        let client = self.client().ok_or(MessageSendError::Unavailable)?;
        let project_id = client.project_for_workspace(workspace).await.map_err(|error| {
            tracing::warn!(%error, "could not resolve the Codex project before creating a chat");
            MessageSendError::Uncertain(error.to_string())
        })?;
        let created = client
            .request(CreateChat {
                project_id: project_id.clone(),
                cwd: workspace.to_owned(),
                ephemeral: false,
            })
            .await
            .map_err(|error| MessageSendError::Uncertain(error.to_string()))?;
        if created.thread.id.is_empty()
            || created.cwd != workspace
            || created.thread.project_id != project_id
        {
            return Err(MessageSendError::Uncertain(
                "Codex created a chat with unexpected identity or workspace".to_owned(),
            ));
        }
        if let Some(name) = chat_name {
            match client
                .request(NameChat {
                    thread_id: created.thread.id.clone(),
                    name: name.to_owned(),
                })
                .await
            {
                Err(ControlError::Codex { code: -32601, .. }) => {
                    tracing::debug!("Codex does not support chat naming")
                }
                Err(error) => tracing::warn!(%error, "could not name the new inbound chat"),
                Ok(_) => {}
            }
        }
        Ok(created.thread.id)
    }

    async fn queue_message(
        &self,
        chat_id: &str,
        event: &InboundEvent,
    ) -> Result<MessageReceipt, MessageSendError> {
        let client = self.client().ok_or(MessageSendError::Unavailable)?;
        client
            .deliver_event(chat_id, event, None::<&UntrackedAttempt>)
            .await
    }

    async fn queue_message_tracked(
        &self,
        chat_id: &str,
        event: &InboundEvent,
        attempt: &(impl MessageAttempt + Sync),
    ) -> Result<MessageReceipt, MessageSendError> {
        let client = self.client().ok_or(MessageSendError::Unavailable)?;
        client.deliver_event(chat_id, event, Some(attempt)).await
    }
}
