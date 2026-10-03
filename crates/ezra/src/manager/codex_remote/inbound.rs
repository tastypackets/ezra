use std::collections::BTreeSet;
use std::path::Path;

use ezra::inbound::InboundEvent;
use ezra::inbound::codex::{
    CreateChat, ListProjects, NameChat, QueueMessage, QueueMessageResponse, ResumeChat,
    UnarchiveChat, UpdateChatSettings,
};
use ezra::inbound::{
    MessageAttempt, MessageReceipt, MessageSendError, MessageSender, Shortcut, UntrackedAttempt,
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

impl CodexRemote {
    fn connected_client(&self) -> Result<std::sync::Arc<ControlClient>, MessageSendError> {
        self.client().ok_or_else(|| {
            let reason = "ezra is not connected to Codex's remote control server".to_owned();
            if self.pauses.is_paused(&()) {
                MessageSendError::Paused(format!("{reason}, which ezra paused"))
            } else {
                MessageSendError::Unavailable(reason)
            }
        })
    }
}

impl MessageSender for CodexRemote {
    fn resumes(&self) -> Option<tokio::sync::watch::Receiver<()>> {
        Some(self.pauses.resumes())
    }

    async fn create_chat(
        &self,
        workspace: &str,
        chat_name: Option<&str>,
        _options: &Shortcut,
    ) -> Result<String, MessageSendError> {
        let client = self.connected_client()?;
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
        let client = self.connected_client()?;
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
        let client = self.connected_client()?;
        client.deliver_event(chat_id, event, Some(attempt)).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ezra::inbound::{ConversationKey, EventKey};
    use nix::unistd::Pid;
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::manager::codex_remote::control::{ControlClient, ControlEvent, ControlSocket};
    use crate::manager::codex_remote::fake::{FakeControlServer, Reply};
    use crate::manager::codex_remote::{ExpectedPeer, ServerBudget};
    use crate::manager::events::Events;
    use crate::manager::supervision::ServerLog;

    struct Fixture {
        remote: CodexRemote,
        fake: FakeControlServer,
        _events: mpsc::UnboundedReceiver<ControlEvent>,
        _home: TempDir,
    }

    struct AttemptProbe<'fixture> {
        fixture: &'fixture Fixture,
        calls: std::sync::atomic::AtomicUsize,
        rejections: std::sync::atomic::AtomicUsize,
        reject: bool,
    }

    impl MessageAttempt for AttemptProbe<'_> {
        async fn mark_attempted(&self, key: &EventKey) -> Result<(), MessageSendError> {
            assert_eq!(key, &Fixture::event().key);
            assert!(
                self.fixture
                    .fake
                    .requests_after_initialize()
                    .iter()
                    .all(|(method, _)| method != "thread/queue/add")
            );
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.reject {
                Err(MessageSendError::Unavailable(
                    "the attempt was refused".to_owned(),
                ))
            } else {
                Ok(())
            }
        }
        async fn mark_rejected(&self, _key: &EventKey) -> Result<(), MessageSendError> {
            self.rejections
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn attempt_is_recorded_after_settings_and_before_message_submission() {
        let fixture = Fixture::connected().await;
        let mut event = Fixture::event();
        event.options.effort = Some("low".into());
        fixture
            .fake
            .reply("thread/settings/update", [Reply::Result(json!({}))]);
        fixture.fake.reply("thread/queue/add", [Reply::Result(json!({"queuedSubmission":{"id":"submission","clientUserMessageId":event.key.delivery_id()}}))]);
        let attempt = AttemptProbe {
            fixture: &fixture,
            calls: Default::default(),
            rejections: Default::default(),
            reject: false,
        };
        fixture
            .remote
            .queue_message_tracked("chat-1", &event, &attempt)
            .await
            .expect("message accepted");
        assert_eq!(attempt.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let methods: Vec<_> = fixture
            .fake
            .requests_after_initialize()
            .into_iter()
            .map(|(method, _)| method)
            .collect();
        assert_eq!(methods[..2], ["thread/settings/update", "thread/queue/add"]);
    }

    #[tokio::test]
    async fn settings_timeout_does_not_record_a_message_attempt() {
        let fixture = Fixture::connected().await;
        let mut event = Fixture::event();
        event.options.effort = Some("low".into());
        fixture.fake.reply("thread/settings/update", [Reply::Never]);
        let attempt = AttemptProbe {
            fixture: &fixture,
            calls: Default::default(),
            rejections: Default::default(),
            reject: false,
        };
        assert!(
            fixture
                .remote
                .queue_message_tracked("chat-1", &event, &attempt)
                .await
                .is_err()
        );
        assert_eq!(attempt.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            attempt.rejections.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(fixture.fake.requests_after_initialize().len(), 1);
    }

    #[tokio::test]
    async fn failed_attempt_record_prevents_message_submission() {
        let fixture = Fixture::connected().await;
        let attempt = AttemptProbe {
            fixture: &fixture,
            calls: Default::default(),
            rejections: Default::default(),
            reject: true,
        };
        assert!(
            fixture
                .remote
                .queue_message_tracked("chat-1", &Fixture::event(), &attempt)
                .await
                .is_err()
        );
        assert_eq!(attempt.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(fixture.fake.requests_after_initialize().is_empty());
    }

    #[tokio::test]
    async fn only_explicit_submission_rejections_clear_the_attempt_marker() {
        for (reply, expected_rejections) in [
            (
                Reply::Error {
                    code: -32600,
                    message: "request rejected".to_owned(),
                },
                1,
            ),
            (Reply::Never, 0),
        ] {
            let fixture = Fixture::connected().await;
            fixture.fake.reply("thread/queue/add", [reply]);
            let attempt = AttemptProbe {
                fixture: &fixture,
                calls: Default::default(),
                rejections: Default::default(),
                reject: false,
            };
            assert!(
                fixture
                    .remote
                    .queue_message_tracked("chat-1", &Fixture::event(), &attempt)
                    .await
                    .is_err()
            );
            assert_eq!(attempt.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(
                attempt.rejections.load(std::sync::atomic::Ordering::SeqCst),
                expected_rejections
            );
            assert_eq!(fixture.fake.requests_after_initialize().len(), 1);
        }
    }

    impl Fixture {
        async fn connected() -> Self {
            let home = tempfile::tempdir_in("/tmp").expect("temporary Codex home");
            let fake = FakeControlServer::bind(home.path());
            let budget = ServerBudget {
                request: Duration::from_secs(1),
                ..ServerBudget::default()
            };
            let (client, events) = ControlClient::connect(
                &ControlSocket::of(home.path()),
                Pid::this(),
                home.path(),
                &budget,
            )
            .await
            .expect("fake control connects");
            let remote = CodexRemote::new(
                Events::default(),
                ServerLog(home.path().join("logs")),
                budget,
                ExpectedPeer::Child,
            );
            remote.with_control(|control| control.client = Some(Arc::new(client)));
            Self {
                remote,
                fake,
                _events: events,
                _home: home,
            }
        }

        fn event() -> InboundEvent {
            InboundEvent {
                key: EventKey {
                    conversation: ConversationKey {
                        source: "github:github.com".to_owned(),
                        subject: "issue-1".to_owned(),
                    },
                    id: "comment-1".to_owned(),
                },
                new_chat: false,
                options: Default::default(),
                chat_name: None,
                source_url: None,
                initial_context: None,
                actor: "author".to_owned(),
                created_at: time::OffsetDateTime::UNIX_EPOCH,
                message: "Continue this work".to_owned(),
            }
        }
    }

    #[tokio::test]
    async fn requested_settings_are_applied_before_the_message_and_unloaded_chats_resume() {
        for unloaded in [false, true] {
            let fixture = Fixture::connected().await;
            let mut event = Fixture::event();
            event.options.model = Some("requested-model".into());
            event.options.effort = Some("future-effort".into());
            let mut replies = Vec::new();
            if unloaded {
                replies.push(Reply::Error {
                    code: -32600,
                    message: "thread not found: chat-1".into(),
                });
                fixture.fake.reply(
                    "thread/resume",
                    [Reply::Result(json!({
                        "thread": {"id":"chat-1","status":{"type":"idle"}}, "cwd":"/workspace"
                    }))],
                );
            }
            replies.push(Reply::Result(json!({"futureField": true})));
            fixture.fake.reply("thread/settings/update", replies);
            fixture.fake.reply("thread/queue/add", [Reply::Result(json!({
                "queuedSubmission": {"id":"submission", "clientUserMessageId": event.key.delivery_id()}
            }))]);
            fixture
                .remote
                .queue_message("chat-1", &event)
                .await
                .expect("queued");
            let requests = fixture.fake.requests_after_initialize();
            assert_eq!(requests[0].0, "thread/settings/update");
            assert_eq!(
                requests[0].1,
                json!({"threadId":"chat-1","model":"requested-model","effort":"future-effort"})
            );
            assert_eq!(requests[requests.len() - 2].0, "thread/queue/add");
            assert_eq!(requests.len(), if unloaded { 5 } else { 3 });
        }
    }

    #[tokio::test]
    async fn archived_settings_requests_restore_before_sending() {
        let fixture = Fixture::connected().await;
        let mut event = Fixture::event();
        event.options.effort = Some("low".into());
        fixture.fake.reply(
            "thread/settings/update",
            [
                Reply::Error {
                    code: -32600,
                    message: "thread not found: chat-1".into(),
                },
                Reply::Result(json!({})),
            ],
        );
        fixture.fake.reply(
            "thread/resume",
            [
                Fixture::archived_reply(),
                Reply::Result(
                    json!({"thread":{"id":"chat-1","status":{"type":"idle"}}, "cwd":"/workspace"}),
                ),
            ],
        );
        fixture.fake.reply(
            "thread/unarchive",
            [Reply::Result(
                json!({"thread":{"id":"chat-1","status":{"type":"idle"}}}),
            )],
        );
        fixture.fake.reply(
            "thread/queue/add",
            [Reply::Result(json!({
                "queuedSubmission":{"id":"submission","clientUserMessageId":event.key.delivery_id()}
            }))],
        );
        fixture
            .remote
            .queue_message("chat-1", &event)
            .await
            .expect("queued after restore");
        assert_eq!(
            fixture
                .fake
                .requests_after_initialize()
                .into_iter()
                .map(|request| request.0)
                .collect::<Vec<_>>(),
            [
                "thread/settings/update",
                "thread/resume",
                "thread/unarchive",
                "thread/resume",
                "thread/settings/update",
                "thread/queue/add",
                "thread/read"
            ]
        );
    }

    #[tokio::test]
    async fn settings_rejections_queue_the_full_message_in_the_available_chat() {
        for unloaded in [false, true] {
            let fixture = Fixture::connected().await;
            let mut event = Fixture::event();
            event.options.model = Some("unsupported".into());
            event.options.effort = Some("future-effort".into());
            let mut settings_replies = Vec::new();
            if unloaded {
                settings_replies.push(Reply::Error {
                    code: -32600,
                    message: "thread not found: chat-1".into(),
                });
                fixture.fake.reply(
                    "thread/resume",
                    [Reply::Result(json!({
                        "thread":{"id":"chat-1","status":{"type":"idle"}}, "cwd":"/workspace"
                    }))],
                );
            }
            settings_replies.push(Reply::Error {
                code: -32600,
                message: "unsupported model".into(),
            });
            fixture
                .fake
                .reply("thread/settings/update", settings_replies);
            fixture.fake.reply(
                "thread/read",
                [Reply::Result(json!({
                    "thread":{"id":"chat-1","name":"Existing chat"}
                }))],
            );
            fixture.fake.reply("thread/queue/add", [Reply::Result(json!({
                "queuedSubmission":{"id":"submission","clientUserMessageId":event.key.delivery_id()}
            }))]);
            let receipt = fixture
                .remote
                .queue_message("chat-1", &event)
                .await
                .expect("queued with current settings");
            assert_eq!(receipt.native_message_id, "submission");
            assert_eq!(receipt.delivery_id, event.key.delivery_id());
            let requests = fixture.fake.requests_after_initialize();
            let submissions: Vec<_> = requests
                .iter()
                .filter(|(method, _)| method == "thread/queue/add")
                .collect();
            assert_eq!(submissions.len(), 1);
            assert_eq!(
                submissions[0].1,
                json!({
                    "threadId":"chat-1", "clientUserMessageId":event.key.delivery_id(),
                    "input":[{"type":"text","text":event.message}]
                })
            );
            assert!(!requests.iter().any(|(method, _)| method == "thread/start"));
        }
    }

    #[tokio::test]
    async fn settings_rejection_preserves_missing_chat_submission_errors() {
        for submission in [
            Reply::Error {
                code: -32600,
                message: "no rollout found for thread id chat-1".into(),
            },
            Reply::Error {
                code: -32603,
                message: "failed to read thread: invalid thread-store request: no rollout found for thread id chat-1".into(),
            },
        ] {
            let fixture = Fixture::connected().await;
            let mut event = Fixture::event();
            event.options.model = Some("unsupported".into());
            fixture.fake.reply("thread/settings/update", [Reply::Error {
                code: -32603,
                message: "failed to read thread: invalid thread-store request: no rollout found for thread id chat-1".into(),
            }]);
            fixture.fake.reply("thread/queue/add", [submission]);
            assert!(matches!(
                fixture.remote.queue_message("chat-1", &event).await,
                Err(MessageSendError::NeedsReplacement(_))
            ));
            let requests = fixture.fake.requests_after_initialize();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[1].0, "thread/queue/add");
        }
    }

    #[tokio::test]
    async fn settings_timeouts_still_hold_the_message_without_submitting() {
        let fixture = Fixture::connected().await;
        let mut event = Fixture::event();
        event.options.effort = Some("low".into());
        fixture.fake.reply("thread/settings/update", [Reply::Never]);
        assert!(matches!(
            fixture.remote.queue_message("chat-1", &event).await,
            Err(MessageSendError::Uncertain(_))
        ));
        let requests = fixture.fake.requests_after_initialize();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "thread/settings/update");
    }

    #[tokio::test]
    async fn archived_settings_rejections_restore_the_chat_before_submission() {
        let fixture = Fixture::connected().await;
        let mut event = Fixture::event();
        event.options.effort = Some("low".into());
        fixture.fake.reply(
            "thread/settings/update",
            [Fixture::archived_reply(), Reply::Result(json!({}))],
        );
        fixture.fake.reply(
            "thread/unarchive",
            [Reply::Result(
                json!({"thread":{"id":"chat-1","status":{"type":"idle"}}}),
            )],
        );
        fixture.fake.reply(
            "thread/resume",
            [Reply::Result(
                json!({"thread":{"id":"chat-1","status":{"type":"idle"}},"cwd":"/workspace"}),
            )],
        );
        fixture.fake.reply("thread/queue/add", [Reply::Result(json!({"queuedSubmission":{"id":"submission","clientUserMessageId":event.key.delivery_id()}}))]);
        fixture
            .remote
            .queue_message("chat-1", &event)
            .await
            .expect("restored and queued");
        assert_eq!(
            fixture
                .fake
                .requests_after_initialize()
                .into_iter()
                .map(|(method, _)| method)
                .collect::<Vec<_>>(),
            [
                "thread/settings/update",
                "thread/unarchive",
                "thread/resume",
                "thread/settings/update",
                "thread/queue/add",
                "thread/read"
            ]
        );
    }

    #[tokio::test]
    async fn active_connection_returns_the_verified_native_receipt() {
        let fixture = Fixture::connected().await;
        let event = Fixture::event();
        fixture.fake.reply(
            "thread/queue/add",
            [Reply::Result(json!({"queuedSubmission": {
                "id": "submission-1", "clientUserMessageId": event.key.delivery_id(),
            }}))],
        );
        let receipt = fixture
            .remote
            .queue_message("chat-1", &event)
            .await
            .expect("receipt confirmed");
        assert_eq!(
            receipt,
            MessageReceipt {
                chat_name: None,
                native_message_id: "submission-1".to_owned(),
                delivery_id: event.key.delivery_id()
            }
        );
        assert_eq!(fixture.fake.requests_after_initialize().len(), 2);
    }

    impl Fixture {
        fn archived_reply() -> Reply {
            Reply::Error { code: -32600, message: "session chat-1 is archived. Run `codex unarchive chat-1` to unarchive it first.".to_owned() }
        }
    }

    #[tokio::test]
    async fn delivery_reports_the_existing_chat_name_without_renaming_or_resuming() {
        for name in [Some("Add zvol creation API"), None] {
            let fixture = Fixture::connected().await;
            let mut event = Fixture::event();
            event.chat_name = Some("owner/repo#42: Requested issue title".to_owned());
            fixture.fake.reply("thread/queue/add", [Reply::Result(json!({
                "queuedSubmission": {"id": "submission", "clientUserMessageId": event.key.delivery_id()}
            }))]);
            let mut thread = json!({
                "id": "chat-1", "status": {"type": "futureStatus"}, "futureField": true
            });
            if let Some(name) = name {
                thread["name"] = json!(name);
            }
            fixture
                .fake
                .reply("thread/read", [Reply::Result(json!({"thread": thread}))]);
            let receipt = fixture
                .remote
                .queue_message("chat-1", &event)
                .await
                .expect("accepted");
            assert_eq!(receipt.chat_name.as_deref(), name);
            let requests = fixture.fake.requests_after_initialize();
            assert_eq!(
                requests
                    .iter()
                    .map(|request| request.0.as_str())
                    .collect::<Vec<_>>(),
                ["thread/queue/add", "thread/read"]
            );
            assert_eq!(requests[1].1, json!({"threadId": "chat-1"}));
        }
    }

    #[tokio::test]
    async fn metadata_failures_never_invalidate_or_resend_an_accepted_message() {
        for reply in [
            Reply::Error {
                code: -32601,
                message: "unsupported method".into(),
            },
            Reply::Error {
                code: -32603,
                message: "metadata unavailable".into(),
            },
            Reply::Result(
                json!({"thread": {"id": "other-chat", "name": "Wrong name", "status": {"type": "idle"}}}),
            ),
            Reply::Result(
                json!({"thread": {"id": "chat-1", "name": 123, "status": {"type": "idle"}}}),
            ),
            Reply::Result(
                json!({"thread": {"id": "chat-1", "name": null, "status": {"type": "active"}}}),
            ),
            Reply::Never,
        ] {
            let fixture = Fixture::connected().await;
            let event = Fixture::event();
            fixture.fake.reply("thread/queue/add", [Reply::Result(json!({
                "queuedSubmission": {"id": "submission", "clientUserMessageId": event.key.delivery_id()}
            }))]);
            fixture.fake.reply("thread/read", [reply]);
            let receipt = fixture
                .remote
                .queue_message("chat-1", &event)
                .await
                .expect("delivery stays accepted");
            assert_eq!(receipt.native_message_id, "submission");
            assert_eq!(receipt.delivery_id, event.key.delivery_id());
            assert_eq!(receipt.chat_name, None);
            assert_eq!(fixture.fake.requests_after_initialize().len(), 2);
        }
    }

    #[tokio::test]
    async fn archived_chat_is_restored_before_retrying_the_same_message() {
        for rejection in [
            "session chat-1 is archived. Run `codex unarchive chat-1` to unarchive it first.",
            "This thread is archived. Restore it to continue.",
        ] {
            let fixture = Fixture::connected().await;
            let event = Fixture::event();
            fixture.fake.reply("thread/queue/add", [Reply::Error { code: -32600, message: rejection.to_owned() }, Reply::Result(json!({
            "queuedSubmission": {"id": "submission-1", "clientUserMessageId": event.key.delivery_id()}
        }))]);
            fixture.fake.reply(
                "thread/unarchive",
                [Reply::Result(
                    json!({"thread": {"id": "chat-1", "status": {"type": "notLoaded"}}}),
                )],
            );
            fixture.fake.reply("thread/resume", [Reply::Result(json!({"thread": {"id": "chat-1", "status": {"type": "idle"}}, "cwd": "/workspace"}))]);
            fixture
                .remote
                .queue_message("chat-1", &event)
                .await
                .expect("restored chat accepts message");
            let requests = fixture.fake.requests_after_initialize();
            assert_eq!(
                requests
                    .iter()
                    .map(|request| request.0.as_str())
                    .collect::<Vec<_>>(),
                [
                    "thread/queue/add",
                    "thread/unarchive",
                    "thread/resume",
                    "thread/queue/add",
                    "thread/read"
                ]
            );
            assert_eq!(requests[0].1, requests[3].1);
        }
    }

    #[test]
    fn archive_detection_excludes_transport_failures_and_other_rejections() {
        for error in [
            ControlError::TimedOut,
            ControlError::Closed,
            ControlError::Io(std::io::Error::other("connection is archived")),
            ControlError::Codex {
                code: -32600,
                message: "unknown rejection".to_owned(),
            },
            ControlError::Codex {
                code: -32603,
                message: "thread is archived".to_owned(),
            },
        ] {
            assert!(!error.is_archive_rejection());
        }
    }

    #[tokio::test]
    async fn missing_chats_request_replacement_but_unknown_recovery_failures_do_not() {
        for (reply, replacement) in [
            (
                Reply::Error {
                    code: -32600,
                    message: "no archived rollout found for thread id chat-1".to_owned(),
                },
                true,
            ),
            (
                Reply::Error {
                    code: -32600,
                    message: "no archived rollout found for thread id other-chat".to_owned(),
                },
                false,
            ),
            (
                Reply::Error {
                    code: -32600,
                    message: "unknown failure".to_owned(),
                },
                false,
            ),
            (
                Reply::Result(json!({"thread": {"id": "other-chat", "status": {"type": "idle"}}})),
                false,
            ),
            (Reply::Never, false),
        ] {
            let fixture = Fixture::connected().await;
            fixture
                .fake
                .reply("thread/queue/add", [Fixture::archived_reply()]);
            fixture.fake.reply("thread/unarchive", [reply]);
            let error = fixture
                .remote
                .queue_message("chat-1", &Fixture::event())
                .await
                .expect_err("recovery failed");
            assert_eq!(
                matches!(error, MessageSendError::NeedsReplacement(_)),
                replacement
            );
            if !replacement {
                assert!(matches!(error, MessageSendError::Uncertain(_)));
            }
            assert_eq!(fixture.fake.requests_after_initialize().len(), 2);
        }
        let fixture = Fixture::connected().await;
        fixture.fake.reply("thread/queue/add", [Reply::Error { code: -32603, message: "failed to read thread: invalid thread-store request: no rollout found for thread id chat-1".to_owned() }]);
        assert!(matches!(
            fixture
                .remote
                .queue_message("chat-1", &Fixture::event())
                .await,
            Err(MessageSendError::NeedsReplacement(_))
        ));
        assert_eq!(fixture.fake.requests_after_initialize().len(), 1);
    }

    #[tokio::test]
    async fn chat_creation_requires_the_requested_workspace_and_is_not_retried() {
        for (reply, succeeds) in [
            (
                Reply::Result(
                    json!({"thread": {"id": "new-chat", "status": {"type": "idle"}}, "cwd": "/workspace"}),
                ),
                true,
            ),
            (
                Reply::Result(
                    json!({"thread": {"id": "new-chat", "status": {"type": "idle"}}, "cwd": "/other"}),
                ),
                false,
            ),
            (Reply::Never, false),
        ] {
            let fixture = Fixture::connected().await;
            fixture
                .fake
                .reply("project/list", [Reply::Result(json!({"data": []}))]);
            fixture.fake.reply("thread/start", [reply]);
            assert_eq!(
                fixture
                    .remote
                    .create_chat("/workspace", None, &Shortcut::default())
                    .await
                    .is_ok(),
                succeeds
            );
            assert_eq!(fixture.fake.requests_after_initialize().len(), 2);
        }
    }

    #[tokio::test]
    async fn creation_uses_the_nearest_project_and_checks_assignment() {
        let fixture = Fixture::connected().await;
        fixture.fake.reply(
            "project/list",
            [
                Reply::Result(json!({"data": [
                {"id": "parent", "roots": [{"path": "/home/dev/projects"}]}
            ], "nextCursor": "second"})),
                Reply::Result(json!({"data": [
                    {"id": "repository", "roots": [{"path": "/home/dev/projects/repo"}]}
                ]})),
            ],
        );
        fixture.fake.reply(
            "thread/start",
            [Reply::Result(json!({
                "thread": {"id": "new-chat", "projectId": "repository", "status": {"type": "idle"}},
                "cwd": "/home/dev/projects/repo/worktree"
            }))],
        );
        assert_eq!(
            fixture
                .remote
                .create_chat(
                    "/home/dev/projects/repo/worktree",
                    None,
                    &Shortcut::default()
                )
                .await
                .expect("created"),
            "new-chat"
        );
        let requests = fixture.fake.requests_after_initialize();
        assert_eq!(requests[2].1["projectId"], "repository");
    }

    #[tokio::test]
    async fn creation_names_the_chat_and_naming_errors_do_not_repeat_creation() {
        for reply in [
            Reply::Result(json!({"futureField": true})),
            Reply::Error {
                code: -32601,
                message: "method unavailable".into(),
            },
            Reply::Error {
                code: -32603,
                message: "name could not be saved".into(),
            },
            Reply::Never,
        ] {
            let fixture = Fixture::connected().await;
            fixture
                .fake
                .reply("project/list", [Reply::Result(json!({"data": []}))]);
            fixture.fake.reply(
                "thread/start",
                [Reply::Result(json!({
                    "thread": {"id": "new-chat", "status": {"type": "idle"}}, "cwd": "/workspace"
                }))],
            );
            fixture.fake.reply("thread/name/set", [reply]);
            assert_eq!(
                fixture
                    .remote
                    .create_chat(
                        "/workspace",
                        Some("owner/repo#42: Fix crash"),
                        &Shortcut {
                            model: Some("chosen-model".into()),
                            effort: Some("high".into()),
                            ..Shortcut::default()
                        }
                    )
                    .await
                    .expect("chat survives naming failure"),
                "new-chat"
            );
            let requests = fixture.fake.requests_after_initialize();
            assert_eq!(requests.len(), 3);
            assert_eq!(requests[2].0, "thread/name/set");
            assert_eq!(
                requests[2].1,
                json!({"threadId": "new-chat", "name": "owner/repo#42: Fix crash"})
            );
        }
    }

    #[tokio::test]
    async fn ambiguous_projects_and_old_agents_omit_project_association() {
        let fixture = Fixture::connected().await;
        fixture.fake.reply(
            "project/list",
            [Reply::Result(json!({"data": [
                {"id": "first", "roots": [{"path": "/workspace"}]},
                {"id": "second", "roots": [{"path": "/workspace"}]}
            ]}))],
        );
        fixture.fake.reply(
            "thread/start",
            [Reply::Result(json!({
                "thread": {"id": "new-chat", "status": {"type": "idle"}}, "cwd": "/workspace"
            }))],
        );
        assert!(
            fixture
                .remote
                .create_chat("/workspace", None, &Shortcut::default())
                .await
                .is_ok()
        );
        let requests = fixture.fake.requests_after_initialize();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].1["cwd"], "/workspace");
        assert!(requests[1].1["projectId"].is_null());
        fixture.fake.reply(
            "project/list",
            [Reply::Error {
                code: -32601,
                message: "method unavailable".into(),
            }],
        );
        fixture.fake.reply(
            "thread/start",
            [Reply::Result(json!({
                "thread": {"id": "new-chat", "status": {"type": "idle"}}, "cwd": "/workspace"
            }))],
        );
        assert!(
            fixture
                .remote
                .create_chat("/workspace", None, &Shortcut::default())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn repeated_project_pages_and_wrong_assignments_do_not_retry_creation() {
        let fixture = Fixture::connected().await;
        fixture.fake.reply(
            "project/list",
            [
                Reply::Result(json!({"data": [], "nextCursor": "same"})),
                Reply::Result(json!({"data": [], "nextCursor": "same"})),
            ],
        );
        assert!(matches!(
            fixture
                .remote
                .create_chat("/workspace", None, &Shortcut::default())
                .await,
            Err(MessageSendError::Uncertain(_))
        ));
        assert_eq!(fixture.fake.requests_after_initialize().len(), 2);
        fixture.fake.reply(
            "project/list",
            [Reply::Result(json!({"data": [
                {"id": "expected", "roots": [{"path": "/workspace"}]}
            ]}))],
        );
        fixture.fake.reply("thread/start", [Reply::Result(json!({
            "thread": {"id": "new-chat", "projectId": "wrong", "status": {"type": "idle"}}, "cwd": "/workspace"
        }))]);
        assert!(matches!(
            fixture
                .remote
                .create_chat("/workspace", None, &Shortcut::default())
                .await,
            Err(MessageSendError::Uncertain(_))
        ));
        assert_eq!(fixture.fake.requests_after_initialize().len(), 4);
    }

    #[tokio::test]
    async fn missing_connection_does_not_send_and_waits_only_while_paused() {
        let fixture = Fixture::connected().await;
        fixture.remote.with_control(|control| control.client = None);
        assert!(matches!(
            fixture
                .remote
                .queue_message("chat-1", &Fixture::event())
                .await,
            Err(MessageSendError::Paused(_))
        ));
        fixture.remote.pauses.resume(&());
        assert!(matches!(
            fixture
                .remote
                .queue_message("chat-1", &Fixture::event())
                .await,
            Err(MessageSendError::Unavailable(_))
        ));
        assert!(matches!(
            fixture
                .remote
                .create_chat("/workspace", None, &Shortcut::default())
                .await,
            Err(MessageSendError::Unavailable(_))
        ));
        assert!(fixture.fake.requests_after_initialize().is_empty());
    }

    #[tokio::test]
    async fn unconfirmed_receipts_and_errors_are_not_retried() {
        for reply in [
            Reply::Result(
                json!({"queuedSubmission": {"id": "submission-1", "clientUserMessageId": "another-event"}}),
            ),
            Reply::Result(json!({"queuedSubmission": {"id": "submission-1"}})),
            Reply::Error {
                code: -32601,
                message: "unsupported method".to_owned(),
            },
            Reply::Never,
        ] {
            let fixture = Fixture::connected().await;
            fixture.fake.reply("thread/queue/add", [reply]);
            assert!(matches!(
                fixture
                    .remote
                    .queue_message("chat-1", &Fixture::event())
                    .await,
                Err(MessageSendError::Uncertain(_))
            ));
            assert_eq!(fixture.fake.requests_after_initialize().len(), 1);
        }
    }
}
