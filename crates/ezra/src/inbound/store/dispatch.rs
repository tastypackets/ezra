use super::{DeliveryOutcome, DeliveryScope, EventStore, StoreError};
use crate::inbound::{
    EventKey, InboundEvent, MessageAttempt, MessageReceipt, MessageSendError, MessageSender,
};
use time::OffsetDateTime;

struct DeliveryAttempt<'store> {
    store: &'store EventStore,
    waiting_duration: time::Duration,
}

impl MessageAttempt for DeliveryAttempt<'_> {
    async fn mark_attempted(&self, key: &EventKey) -> Result<(), MessageSendError> {
        let cutoff = OffsetDateTime::now_utc()
            .saturating_sub(self.waiting_duration)
            .unix_timestamp();
        let result = sqlx::query!(
            "UPDATE inbound_events SET attempted_at = COALESCE(attempted_at, unixepoch())
             WHERE source = ?1 AND subject = ?2 AND event_id = ?3 AND delivery_state = 'delivering'
               AND (attempted_at IS NOT NULL OR received_at > ?4)",
            key.conversation.source,
            key.conversation.subject,
            key.id,
            cutoff,
        )
        .execute(&self.store.pool)
        .await
        .map_err(|error| MessageSendError::Uncertain(error.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(MessageSendError::Uncertain(
                "request is no longer eligible for submission".to_owned(),
            ));
        }
        Ok(())
    }

    async fn mark_rejected(&self, key: &EventKey) -> Result<(), MessageSendError> {
        sqlx::query!(
            "UPDATE inbound_events SET attempted_at = NULL
             WHERE source = ?1 AND subject = ?2 AND event_id = ?3 AND delivery_state = 'delivering'",
            key.conversation.source, key.conversation.subject, key.id,
        ).execute(&self.store.pool).await
            .map_err(|error| MessageSendError::Uncertain(error.to_string()))?;
        Ok(())
    }
}

#[derive(Debug)]
pub enum DispatchOutcome {
    Idle,
    Delivered {
        event: EventKey,
        receipt: MessageReceipt,
    },
    Unavailable {
        event: EventKey,
    },
    Uncertain {
        event: EventKey,
        reason: String,
    },
    Failed {
        event: EventKey,
        reason: String,
    },
}

impl EventStore {
    #[tracing::instrument(
        skip_all,
        fields(host_id = scope.host_id, agent = scope.agent, delivery_id = tracing::field::Empty),
        err(level = "error")
    )]
    pub async fn dispatch_next(
        &self,
        scope: DeliveryScope<'_>,
        sender: &impl MessageSender,
    ) -> Result<DispatchOutcome, StoreError> {
        let Some(event) = self.claim_next(Some(scope)).await? else {
            return Ok(DispatchOutcome::Idle);
        };
        self.dispatch_claimed(event, sender, OffsetDateTime::UNIX_EPOCH)
            .await
    }

    pub async fn dispatch_event(
        &self,
        key: &EventKey,
        scope: DeliveryScope<'_>,
        sender: &impl MessageSender,
        cutoff: OffsetDateTime,
    ) -> Result<DispatchOutcome, StoreError> {
        let Some(event) = self.claim_event(key, scope).await? else {
            return Ok(DispatchOutcome::Idle);
        };
        self.dispatch_claimed(event, sender, cutoff).await
    }

    async fn dispatch_claimed(
        &self,
        event: InboundEvent,
        sender: &impl MessageSender,
        cutoff: OffsetDateTime,
    ) -> Result<DispatchOutcome, StoreError> {
        let attempt = DeliveryAttempt {
            store: self,
            waiting_duration: time::Duration::seconds(
                OffsetDateTime::now_utc()
                    .unix_timestamp()
                    .saturating_sub(cutoff.unix_timestamp()),
            ),
        };
        let delivery_id = event.key.delivery_id();
        tracing::Span::current().record("delivery_id", delivery_id.as_str());
        let target = self
            .find_binding(&event.key.conversation, event.options.agent)
            .await?
            .ok_or(StoreError::DeliveryChanged)?;
        let initial_context_pending = sqlx::query_scalar!(
            r#"SELECT initial_context_pending AS "initial_context_pending!: bool"
               FROM inbound_sessions WHERE host_id = ?1 AND agent = ?2 AND chat_id = ?3"#,
            target.host_id,
            target.agent,
            target.chat_id,
        )
        .fetch_one(&self.pool)
        .await?;
        let message = if initial_context_pending {
            event.with_initial_context()
        } else {
            event.clone()
        };
        let result = match sender
            .queue_message_tracked(&target.chat_id, &message, &attempt)
            .await
        {
            Err(MessageSendError::NeedsReplacement(reason)) => {
                attempt
                    .mark_rejected(&event.key)
                    .await
                    .map_err(|_| StoreError::DeliveryChanged)?;
                tracing::info!(%reason, "creating replacement chat for inbound delivery");
                match sender
                    .create_chat(
                        &target.workspace,
                        event.chat_name.as_deref(),
                        &event.options,
                    )
                    .await
                {
                    Ok(chat_id) => {
                        self.replace_chat_for_delivery(&event.key, &target, &chat_id)
                            .await?;
                        sender
                            .queue_message_tracked(
                                &chat_id,
                                &event.with_initial_context(),
                                &attempt,
                            )
                            .await
                    }
                    Err(error) => Err(error),
                }
            }
            result => result,
        };
        let attempted = sqlx::query_scalar!(
            r#"SELECT attempted_at IS NOT NULL AS "attempted!: bool" FROM inbound_events
               WHERE source = ?1 AND subject = ?2 AND event_id = ?3"#,
            event.key.conversation.source,
            event.key.conversation.subject,
            event.key.id,
        )
        .fetch_one(&self.pool)
        .await?;
        let (state, outcome) = match result {
            Ok(receipt)
                if receipt.delivery_id == event.key.delivery_id()
                    && !receipt.native_message_id.is_empty() =>
            {
                (
                    DeliveryOutcome::Delivered,
                    DispatchOutcome::Delivered {
                        event: event.key.clone(),
                        receipt,
                    },
                )
            }
            Ok(_) => (
                DeliveryOutcome::Uncertain,
                DispatchOutcome::Uncertain {
                    event: event.key.clone(),
                    reason: "agent receipt does not match the message".to_owned(),
                },
            ),
            Err(MessageSendError::Unavailable) if !attempted => (
                DeliveryOutcome::Pending,
                DispatchOutcome::Unavailable {
                    event: event.key.clone(),
                },
            ),
            Err(MessageSendError::Unavailable) => (
                DeliveryOutcome::Uncertain,
                DispatchOutcome::Uncertain {
                    event: event.key.clone(),
                    reason: "agent disconnected after the submission attempt".to_owned(),
                },
            ),
            Err(
                MessageSendError::Uncertain(reason) | MessageSendError::NeedsReplacement(reason),
            ) if !attempted => (
                DeliveryOutcome::Failed,
                DispatchOutcome::Failed {
                    event: event.key.clone(),
                    reason,
                },
            ),
            Err(
                MessageSendError::Uncertain(reason) | MessageSendError::NeedsReplacement(reason),
            ) => (
                DeliveryOutcome::Uncertain,
                DispatchOutcome::Uncertain {
                    event: event.key.clone(),
                    reason,
                },
            ),
        };
        let chat_name = match &outcome {
            DispatchOutcome::Delivered { receipt, .. } => receipt.chat_name.as_deref(),
            _ => None,
        };
        if !self.finish_delivery(&event.key, state, chat_name).await? {
            return Err(StoreError::DeliveryChanged);
        }
        match &outcome {
            DispatchOutcome::Delivered { receipt, .. } => {
                tracing::info!(native_message_id = %receipt.native_message_id, "inbound message delivered");
            }
            DispatchOutcome::Unavailable { .. } => {
                tracing::debug!("inbound message deferred because the agent is unavailable");
            }
            DispatchOutcome::Uncertain { reason, .. } => {
                tracing::warn!(%reason, "inbound submission could not be confirmed");
            }
            DispatchOutcome::Failed { reason, .. } => {
                tracing::warn!(%reason, "inbound request failed before submission");
            }
            DispatchOutcome::Idle => {}
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::agent::Agent;
    use crate::inbound::store::{DeliveryState, SessionTarget, TEST_QUEUE_LIMITS};
    use crate::inbound::{ConversationKey, InboundEvent, Shortcut};

    const SCOPE: DeliveryScope<'_> = DeliveryScope {
        host_id: "host-a",
        agent: "codex",
    };

    enum Reply {
        Accepted,
        Unavailable,
        Uncertain,
        Mismatched,
    }

    struct Sender {
        reply: Reply,
        chats: Mutex<Vec<String>>,
        messages: Mutex<Vec<String>>,
    }

    impl MessageSender for Sender {
        async fn create_chat(
            &self,
            _workspace: &str,
            _chat_name: Option<&str>,
            _options: &Shortcut,
        ) -> Result<String, MessageSendError> {
            panic!("these cases must not create chats")
        }

        async fn queue_message_tracked(
            &self,
            chat_id: &str,
            event: &InboundEvent,
            attempt: &(impl MessageAttempt + Sync),
        ) -> Result<MessageReceipt, MessageSendError> {
            if !matches!(self.reply, Reply::Unavailable) {
                attempt.mark_attempted(&event.key).await?;
            }
            self.queue_message(chat_id, event).await
        }

        async fn queue_message(
            &self,
            chat_id: &str,
            event: &InboundEvent,
        ) -> Result<MessageReceipt, MessageSendError> {
            self.chats
                .lock()
                .expect("chat log locks")
                .push(chat_id.to_owned());
            self.messages
                .lock()
                .expect("messages")
                .push(event.message.clone());
            match self.reply {
                Reply::Accepted => Ok(MessageReceipt {
                    chat_name: Some("Existing chat name".to_owned()),
                    native_message_id: "native-1".to_owned(),
                    delivery_id: event.key.delivery_id(),
                }),
                Reply::Unavailable => Err(MessageSendError::Unavailable),
                Reply::Uncertain => {
                    Err(MessageSendError::Uncertain("connection closed".to_owned()))
                }
                Reply::Mismatched => Ok(MessageReceipt {
                    chat_name: Some("Untrusted chat name".to_owned()),
                    native_message_id: "native-1".to_owned(),
                    delivery_id: "another-event".to_owned(),
                }),
            }
        }
    }

    impl InboundEvent {
        fn dispatch_example(subject: &str) -> Self {
            Self {
                key: EventKey {
                    conversation: ConversationKey {
                        source: "github:github.com".to_owned(),
                        subject: subject.to_owned(),
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

    impl SessionTarget {
        fn dispatch_example(host: &str, agent: &str, chat: &str) -> Self {
            Self {
                host_id: host.to_owned(),
                agent: agent.to_owned(),
                chat_id: chat.to_owned(),
                workspace: "/home/dev/projects/repository".to_owned(),
            }
        }
    }

    struct RecoverySender<'store> {
        store: &'store EventStore,
        creation_fails: bool,
        replacement_fails: bool,
        calls: Mutex<Vec<String>>,
    }

    impl MessageSender for RecoverySender<'_> {
        async fn create_chat(
            &self,
            workspace: &str,
            chat_name: Option<&str>,
            options: &Shortcut,
        ) -> Result<String, MessageSendError> {
            assert_eq!(workspace, "/home/dev/projects/repository");
            assert_eq!(chat_name, Some("repository#42: Fix crash"));
            assert_eq!(options.model.as_deref(), Some("chosen-model"));
            self.calls.lock().expect("calls").push("create".to_owned());
            if self.creation_fails {
                Err(MessageSendError::Uncertain("creation timed out".to_owned()))
            } else {
                Ok("new-chat".to_owned())
            }
        }

        async fn queue_message(
            &self,
            chat_id: &str,
            event: &InboundEvent,
        ) -> Result<MessageReceipt, MessageSendError> {
            self.calls.lock().expect("calls").push(chat_id.to_owned());
            assert_eq!(
                event.message.contains("Original description"),
                chat_id != "old-chat"
            );
            if chat_id == "old-chat" {
                return Err(MessageSendError::NeedsReplacement(
                    "chat deleted".to_owned(),
                ));
            }
            assert_eq!(
                self.store
                    .find_binding(&event.key.conversation, Agent::Codex)
                    .await
                    .expect("binding")
                    .expect("bound")
                    .chat_id,
                chat_id
            );
            if self.replacement_fails {
                return Err(MessageSendError::NeedsReplacement(
                    "replacement also deleted".to_owned(),
                ));
            }
            Ok(MessageReceipt {
                chat_name: None,
                native_message_id: "receipt".to_owned(),
                delivery_id: event.key.delivery_id(),
            })
        }
    }

    #[tokio::test]
    async fn recovery_persists_replacement_before_delivery_and_never_loops_on_failure() {
        for (creation_fails, replacement_fails) in [(false, false), (true, false), (false, true)] {
            let directory = tempfile::tempdir().expect("temporary directory");
            let store = EventStore::open(&directory.path().join("ezra.db"))
                .await
                .expect("store opens");
            let mut event = InboundEvent::dispatch_example("issue-1");
            event.chat_name = Some("repository#42: Fix crash".to_owned());
            event.initial_context = Some("\n\nOriginal description".to_owned());
            event.options.model = Some("chosen-model".to_owned());
            let target = SessionTarget::dispatch_example("host-a", "codex", "old-chat");
            store
                .bind_conversation(&event.key.conversation, &target)
                .await
                .expect("bind");
            store
                .insert(&event, TEST_QUEUE_LIMITS)
                .await
                .expect("insert");
            let sender = RecoverySender {
                store: &store,
                creation_fails,
                replacement_fails,
                calls: Mutex::default(),
            };
            let outcome = store.dispatch_next(SCOPE, &sender).await.expect("dispatch");
            let expected_state = if creation_fails {
                assert!(matches!(outcome, DispatchOutcome::Failed { .. }));
                DeliveryState::Failed
            } else if replacement_fails {
                assert!(matches!(outcome, DispatchOutcome::Uncertain { .. }));
                DeliveryState::Uncertain
            } else {
                assert!(matches!(outcome, DispatchOutcome::Delivered { .. }));
                DeliveryState::Delivered
            };
            assert_eq!(
                store.delivery_state(&event.key).await.expect("state"),
                Some(expected_state)
            );
            assert_eq!(
                store
                    .find_binding(&event.key.conversation, Agent::Codex)
                    .await
                    .expect("binding")
                    .expect("bound")
                    .chat_id,
                if creation_fails {
                    "old-chat"
                } else {
                    "new-chat"
                }
            );
            assert_eq!(
                *sender.calls.lock().expect("calls"),
                if creation_fails {
                    vec!["old-chat", "create"]
                } else {
                    vec!["old-chat", "create", "new-chat"]
                }
            );
        }
    }

    #[tokio::test]
    async fn dispatch_claims_only_the_selected_host_and_agent_and_uses_the_saved_chat() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store opens");
        let unbound = InboundEvent::dispatch_example("unbound");
        let other_host = InboundEvent::dispatch_example("other-host");
        let other_agent = InboundEvent::dispatch_example("other-agent");
        let ours = InboundEvent::dispatch_example("ours");
        for event in [&unbound, &other_host, &other_agent, &ours] {
            store
                .insert(event, TEST_QUEUE_LIMITS)
                .await
                .expect("event inserts");
        }
        for (event, target) in [
            (
                &other_host,
                SessionTarget::dispatch_example("host-b", "codex", "other-chat"),
            ),
            (
                &other_agent,
                SessionTarget::dispatch_example("host-a", "claude", "other-chat"),
            ),
            (
                &ours,
                SessionTarget::dispatch_example("host-a", "codex", "saved-chat"),
            ),
        ] {
            store
                .bind_conversation(&event.key.conversation, &target)
                .await
                .expect("session binds");
        }
        let sender = Sender {
            reply: Reply::Accepted,
            chats: Mutex::default(),
            messages: Mutex::default(),
        };
        let outcome = store
            .dispatch_next(SCOPE, &sender)
            .await
            .expect("message dispatches");
        let DispatchOutcome::Delivered { event, receipt } = outcome else {
            panic!("expected delivered outcome")
        };
        assert_eq!(event, ours.key);
        assert_eq!(receipt.delivery_id, ours.key.delivery_id());
        assert_eq!(
            *sender.chats.lock().expect("chat log locks"),
            ["saved-chat"]
        );
        assert!(matches!(
            store
                .dispatch_next(SCOPE, &sender)
                .await
                .expect("no more local work"),
            DispatchOutcome::Idle
        ));
        for event in [&unbound, &other_host, &other_agent] {
            assert_eq!(
                store
                    .delivery_state(&event.key)
                    .await
                    .expect("other event state"),
                Some(DeliveryState::Pending)
            );
        }
    }

    #[tokio::test]
    async fn dispatch_settles_outcomes_without_blocking_distinct_messages() {
        for (reply, expected_state) in [
            (Reply::Accepted, DeliveryState::Delivered),
            (Reply::Unavailable, DeliveryState::Pending),
            (Reply::Uncertain, DeliveryState::Uncertain),
            (Reply::Mismatched, DeliveryState::Uncertain),
        ] {
            let directory = tempfile::tempdir().expect("temporary directory");
            let path = directory.path().join("ezra.db");
            let store = EventStore::open(&path).await.expect("store opens");
            let issue = InboundEvent::dispatch_example("issue-1");
            let followup = InboundEvent::dispatch_example("pull-2");
            store
                .bind_conversation(
                    &issue.key.conversation,
                    &SessionTarget::dispatch_example("host-a", "codex", "shared-chat"),
                )
                .await
                .expect("issue binds");
            store
                .link_conversation(
                    &followup.key.conversation,
                    &issue.key.conversation,
                    Agent::Codex,
                )
                .await
                .expect("PR links");
            for event in [&issue, &followup] {
                store
                    .insert(event, TEST_QUEUE_LIMITS)
                    .await
                    .expect("event inserts");
            }
            let sender = Sender {
                reply,
                chats: Mutex::default(),
                messages: Mutex::default(),
            };
            let outcome = store
                .dispatch_next(SCOPE, &sender)
                .await
                .expect("dispatch outcome persists");
            match expected_state {
                DeliveryState::Delivered => {
                    assert!(matches!(outcome, DispatchOutcome::Delivered { .. }))
                }
                DeliveryState::Pending => {
                    assert!(matches!(outcome, DispatchOutcome::Unavailable { .. }))
                }
                DeliveryState::Uncertain => {
                    assert!(matches!(outcome, DispatchOutcome::Uncertain { .. }))
                }
                DeliveryState::Delivering => panic!("dispatch must settle its claim"),
                DeliveryState::Failed | DeliveryState::Expired => {
                    panic!("fixture expects no terminal rejection")
                }
            }
            assert_eq!(sender.chats.lock().expect("chat log locks").len(), 1);
            store.pool.close().await;
            let store = EventStore::open(&path).await.expect("store reopens");
            assert_eq!(
                store
                    .delivery_state(&issue.key)
                    .await
                    .expect("persisted delivery state"),
                Some(expected_state)
            );
            assert_eq!(
                store
                    .delivered_chat_name(&issue.key)
                    .await
                    .expect("saved name")
                    .as_deref(),
                if expected_state == DeliveryState::Delivered {
                    Some("Existing chat name")
                } else {
                    None
                },
            );
            let next = store
                .claim_next(Some(SCOPE))
                .await
                .expect("next eligible claim");
            match expected_state {
                DeliveryState::Delivered => assert_eq!(next, Some(followup)),
                DeliveryState::Pending => assert_eq!(next, Some(issue.clone())),
                DeliveryState::Uncertain => assert_eq!(next, Some(followup)),
                DeliveryState::Delivering => panic!("dispatch must settle its claim"),
                DeliveryState::Failed | DeliveryState::Expired => {
                    panic!("fixture expects no terminal rejection")
                }
            }
        }
    }

    #[tokio::test]
    async fn new_chat_context_survives_retries_and_confirmation_but_not_linked_followups() {
        for reply in [Reply::Accepted, Reply::Unavailable, Reply::Uncertain] {
            let directory = tempfile::tempdir().expect("directory");
            let database = directory.path().join("ezra.db");
            let store = EventStore::open(&database).await.expect("store");
            let mut event = InboundEvent::dispatch_example("issue-1");
            event.initial_context = Some("\n\nOriginal description".to_owned());
            store
                .insert(&event, TEST_QUEUE_LIMITS)
                .await
                .expect("insert");
            assert!(matches!(
                store
                    .claim_routing(&event.key, &[], SCOPE, "/home/dev/projects/repository")
                    .await
                    .expect("route"),
                super::super::RoutingOutcome::Create { .. }
            ));
            store
                .finish_routing(
                    &event.key,
                    &SessionTarget::dispatch_example("host-a", "codex", "fresh-chat"),
                )
                .await
                .expect("created");
            let sender = Sender {
                reply,
                chats: Mutex::default(),
                messages: Mutex::default(),
            };
            let outcome = store.dispatch_next(SCOPE, &sender).await.expect("dispatch");
            assert_eq!(
                *sender.messages.lock().expect("messages"),
                [event.with_initial_context().message]
            );
            store.pool.close().await;
            let store = EventStore::open(&database).await.expect("reopen");
            let accepted = Sender {
                reply: Reply::Accepted,
                chats: Mutex::default(),
                messages: Mutex::default(),
            };
            match outcome {
                DispatchOutcome::Unavailable { .. } => {
                    assert!(matches!(
                        store.dispatch_next(SCOPE, &accepted).await.expect("retry"),
                        DispatchOutcome::Delivered { .. }
                    ));
                    assert_eq!(
                        *accepted.messages.lock().expect("messages"),
                        [event.with_initial_context().message]
                    );
                    accepted.messages.lock().expect("messages").clear();
                }
                DispatchOutcome::Uncertain { .. } => {
                    assert!(store.confirm_delivery(&event.key).await.expect("confirm"));
                }
                DispatchOutcome::Delivered { .. } => {}
                DispatchOutcome::Idle | DispatchOutcome::Failed { .. } => {
                    panic!("first event must be attempted")
                }
            }
            let mut followup = event.clone();
            followup.key.conversation.subject = "pull-2".to_owned();
            followup.message = "Full followup\n\n```rust\nrun();\n```".to_owned();
            store
                .link_conversation(
                    &followup.key.conversation,
                    &event.key.conversation,
                    Agent::Codex,
                )
                .await
                .expect("link");
            store
                .insert(&followup, TEST_QUEUE_LIMITS)
                .await
                .expect("followup");
            assert!(matches!(
                store
                    .dispatch_next(SCOPE, &accepted)
                    .await
                    .expect("dispatch followup"),
                DispatchOutcome::Delivered { .. }
            ));
            assert_eq!(
                *accepted.messages.lock().expect("messages"),
                [followup.message]
            );
        }
    }
}
