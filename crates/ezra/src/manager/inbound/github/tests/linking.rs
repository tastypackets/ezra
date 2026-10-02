use super::*;
use ezra::inbound::store::{DispatchOutcome, SessionTarget};

struct Fixture {
    directory: tempfile::TempDir,
    runtime: InboundRuntime,
    source: Source,
    sender: Sender,
    checkpoint: ezra::inbound::store::SourceCheckpoint,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().expect("directory");
        let runtime = InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime");
        let checkpoint = runtime
            .store
            .source_checkpoint(
                &Self::repository_scope(),
                OffsetDateTime::now_utc().saturating_sub(time::Duration::hours(1)),
            )
            .await
            .expect("initial checkpoint");
        Self {
            directory,
            runtime,
            source: Source::default(),
            sender: Sender {
                chat_ids: Mutex::new(["chat-1".into(), "chat-2".into(), "chat-3".into()].into()),
                ..Sender::default()
            },
            checkpoint,
        }
    }

    fn repository_scope() -> ConversationKey {
        ConversationKey {
            source: "github:github.com".into(),
            subject: "owner/repo".into(),
        }
    }

    fn conversation(number: u64) -> ConversationKey {
        ConversationKey {
            source: "github:github.com".into(),
            subject: format!("7/{number}"),
        }
    }

    fn comment(number: u64, identifier: u64, request: &str) -> serde_json::Value {
        let mut comment = Source::comment(identifier);
        comment["body"] = json!(request);
        comment["issue_url"] = json!(format!(
            "https://api.github.com/repos/owner/repo/issues/{number}"
        ));
        comment
    }

    fn target(&self, chat_id: &str) -> SessionTarget {
        SessionTarget {
            host_id: self.runtime.host_id.clone(),
            agent: "codex".into(),
            chat_id: chat_id.into(),
            workspace: "/home/dev/projects/repo".into(),
        }
    }

    async fn scan(&self, comments: Vec<serde_json::Value>) -> Result<(), PollError> {
        *self.source.comments.lock().expect("comments") = comments;
        self.runtime
            .scan_repository(
                &self.source,
                "github.com",
                &self.sender,
                &InboundSettings::default(),
                "owner/repo",
                "/home/dev/projects/repo",
            )
            .await
    }

    async fn drain(&self) -> Vec<(String, EventKey)> {
        loop {
            let outcome = self
                .runtime
                .store
                .dispatch_next(
                    DeliveryScope {
                        host_id: &self.runtime.host_id,
                        agent: "codex",
                    },
                    &self.sender,
                )
                .await
                .expect("dispatch");
            if matches!(outcome, DispatchOutcome::Idle) {
                break;
            }
            assert!(matches!(outcome, DispatchOutcome::Delivered { .. }));
        }
        self.sender.deliveries.lock().expect("deliveries").clone()
    }
}

#[tokio::test]
async fn a_pull_request_with_multiple_links_reuses_the_issue_chat_and_stays_sticky() {
    let fixture = Fixture::new().await;
    let target = fixture.target("chat-original");
    fixture
        .runtime
        .store
        .bind_conversation(&Fixture::conversation(12), &target)
        .await
        .expect("issue binds");
    fixture
        .runtime
        .store
        .link_conversation(&Fixture::conversation(13), &Fixture::conversation(12))
        .await
        .expect("second issue shares chat");
    fixture.source.links.lock().expect("links").insert(
        42,
        vec![Fixture::conversation(12), Fixture::conversation(13)],
    );
    fixture
        .scan(vec![Fixture::comment(42, 10, "/ezra fix this")])
        .await
        .expect("linked request routes");
    assert_eq!(
        fixture
            .runtime
            .store
            .find_binding(&Fixture::conversation(42))
            .await
            .expect("pull route"),
        Some(target)
    );
    assert_eq!(fixture.sender.creations.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.drain().await[0].0, "chat-original");
    fixture.source.link_error.store(true, Ordering::SeqCst);
    fixture
        .scan(vec![Fixture::comment(42, 11, "/ezra follow up")])
        .await
        .expect("sticky route skips lookup");
    assert_eq!(fixture.source.link_reads.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.drain().await[1].0, "chat-original");
    let messages = fixture.sender.messages.lock().expect("messages");
    assert!(
        messages
            .iter()
            .all(|message| !message.contains("Description:"))
    );
    assert!(messages[0].contains("Request:\nfix this"));
    assert!(messages[1].contains("Request:\nfollow up"));
    assert!(
        messages.iter().all(
            |message| message.contains("https://github.com/owner/repo/issues/42\nTitle: Issue")
        )
    );
}

#[tokio::test]
async fn conflicting_linked_chats_create_a_separate_repository_chat() {
    let fixture = Fixture::new().await;
    for (number, chat_id) in [(12, "chat-old-a"), (13, "chat-old-b")] {
        fixture
            .runtime
            .store
            .bind_conversation(&Fixture::conversation(number), &fixture.target(chat_id))
            .await
            .expect("issue binds");
    }
    fixture.source.links.lock().expect("links").insert(
        42,
        vec![Fixture::conversation(12), Fixture::conversation(13)],
    );
    fixture
        .scan(vec![Fixture::comment(42, 10, "/ezra")])
        .await
        .expect("separate chat routes");
    assert_eq!(fixture.sender.creations.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.drain().await[0].0, "chat-1");
}

#[tokio::test]
async fn lookup_errors_keep_the_event_and_checkpoint_until_the_links_can_be_read() {
    let fixture = Fixture::new().await;
    fixture.source.link_error.store(true, Ordering::SeqCst);
    let comment = Fixture::comment(42, 10, "/ezra fix this");
    assert!(matches!(
        fixture.scan(vec![comment.clone()]).await,
        Err(PollError::Incomplete)
    ));
    let key = EventKey {
        conversation: Fixture::conversation(42),
        id: "10".into(),
    };
    assert_eq!(
        fixture
            .runtime
            .store
            .delivery_state(&key)
            .await
            .expect("durable request"),
        Some(DeliveryState::Pending)
    );
    assert_eq!(
        fixture
            .runtime
            .store
            .source_checkpoint(&Fixture::repository_scope(), OffsetDateTime::now_utc())
            .await
            .expect("checkpoint"),
        fixture.checkpoint
    );
    assert_eq!(fixture.sender.creations.load(Ordering::SeqCst), 0);
    fixture.source.link_error.store(false, Ordering::SeqCst);
    fixture
        .runtime
        .store
        .bind_conversation(&Fixture::conversation(12), &fixture.target("chat-old"))
        .await
        .expect("issue binds");
    fixture
        .source
        .links
        .lock()
        .expect("links")
        .insert(42, vec![Fixture::conversation(12)]);
    fixture
        .scan(vec![comment])
        .await
        .expect("retry routes to issue");
    assert_eq!(fixture.drain().await[0].0, "chat-old");
}

#[tokio::test]
async fn account_changes_do_not_cancel_an_already_accepted_request() {
    let mut fixture = Fixture::new().await;
    fixture.source.switch_identity_after_links = true;
    fixture
        .source
        .links
        .lock()
        .expect("links")
        .insert(42, vec![Fixture::conversation(12)]);
    fixture
        .runtime
        .store
        .bind_conversation(&Fixture::conversation(12), &fixture.target("chat-old"))
        .await
        .expect("issue binds");
    assert!(matches!(
        fixture.scan(vec![Fixture::comment(42, 10, "/ezra")]).await,
        Err(PollError::IdentityChanged)
    ));
    assert_eq!(
        fixture
            .runtime
            .store
            .find_binding(&Fixture::conversation(42))
            .await
            .expect("accepted pull keeps its binding"),
        Some(fixture.target("chat-old"))
    );
    assert_eq!(fixture.sender.creations.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.drain().await[0].0, "chat-old");
}

#[tokio::test]
async fn new_chat_drains_older_requests_then_moves_only_this_discussion_once() {
    let mut fixture = Fixture::new().await;
    fixture
        .scan(vec![Fixture::comment(12, 10, "/ezra initial")])
        .await
        .expect("issue creates chat");
    fixture.drain().await;
    fixture
        .source
        .links
        .lock()
        .expect("links")
        .insert(42, vec![Fixture::conversation(12)]);
    fixture
        .scan(vec![Fixture::comment(42, 11, "/ezra older request")])
        .await
        .expect("pull shares issue chat");
    let reset = Fixture::comment(42, 12, "/ezra --new fresh request");
    let later = Fixture::comment(42, 13, "/ezra later request");
    assert!(matches!(
        fixture.scan(vec![reset.clone(), later.clone()]).await,
        Err(PollError::RoutingDeferred)
    ));
    assert_eq!(fixture.sender.creations.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.drain().await[1].0, "chat-1");
    fixture
        .scan(vec![reset.clone(), later.clone()])
        .await
        .expect("fresh chat starts");
    assert_eq!(fixture.sender.creations.load(Ordering::SeqCst), 2);
    let delivered = fixture.drain().await;
    assert_eq!(delivered[2].0, "chat-2");
    assert_eq!(delivered[2].1.id, "12");
    assert_eq!(delivered[3].0, "chat-2");
    assert_eq!(
        fixture
            .runtime
            .store
            .find_binding(&Fixture::conversation(12))
            .await
            .expect("issue route")
            .expect("issue binding")
            .chat_id,
        "chat-1"
    );
    fixture.runtime = InboundRuntime::open(
        &fixture.directory.path().join("ezra.db"),
        fixture.runtime.settings.clone(),
    )
    .await
    .expect("runtime reopens");
    let edited = Fixture::comment(42, 12, "/ezra --new an edit must not reset again");
    fixture
        .scan(vec![edited, later])
        .await
        .expect("duplicate after restart");
    assert_eq!(fixture.sender.creations.load(Ordering::SeqCst), 2);
    let new_reset = Fixture::comment(42, 14, "/ezra --new");
    fixture
        .scan(vec![new_reset.clone()])
        .await
        .expect("another fresh chat");
    let mut removed_option = new_reset;
    removed_option["body"] = json!("/ezra no longer has the option");
    fixture
        .scan(vec![removed_option])
        .await
        .expect("original directive persists");
    assert_eq!(fixture.sender.creations.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.drain().await[4].0, "chat-3");
    let messages = fixture.sender.messages.lock().expect("messages");
    assert_eq!(
        messages
            .iter()
            .map(|message| message.contains("Description:"))
            .collect::<Vec<_>>(),
        [true, false, true, false, true]
    );
}

#[tokio::test]
async fn an_uncertain_fresh_chat_creation_keeps_the_old_route_and_does_not_retry() {
    let fixture = Fixture::new().await;
    fixture
        .runtime
        .store
        .bind_conversation(&Fixture::conversation(42), &fixture.target("chat-old"))
        .await
        .expect("old chat binds");
    fixture.sender.uncertain.store(true, Ordering::SeqCst);
    let reset = Fixture::comment(42, 10, "/ezra --new");
    for _ in 0..2 {
        fixture
            .scan(vec![reset.clone()])
            .await
            .expect("uncertainty persists");
    }
    assert_eq!(fixture.sender.creations.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .runtime
            .store
            .find_binding(&Fixture::conversation(42))
            .await
            .expect("old route")
            .expect("binding")
            .chat_id,
        "chat-old"
    );
    assert!(fixture.drain().await.is_empty());
}

#[tokio::test]
async fn a_deferred_reset_retries_after_its_comment_is_deleted_or_the_trigger_removed() {
    for delete_comment in [false, true] {
        let fixture = Fixture::new().await;
        fixture
            .scan(vec![Fixture::comment(42, 10, "/ezra older request")])
            .await
            .expect("older request queues");
        let reset = Fixture::comment(42, 11, "/ezra --new original reset");
        let later = Fixture::comment(42, 12, "/ezra later request");
        assert!(matches!(
            fixture.scan(vec![reset.clone(), later.clone()]).await,
            Err(PollError::RoutingDeferred)
        ));
        assert_eq!(fixture.drain().await.len(), 1);
        let mut edited = reset;
        edited["body"] = json!("The trigger is gone");
        let comments = if delete_comment {
            vec![later]
        } else {
            vec![edited, later]
        };
        fixture
            .scan(comments)
            .await
            .expect("saved reset still routes");
        assert_eq!(fixture.sender.creations.load(Ordering::SeqCst), 2);
        let delivered = fixture.drain().await;
        assert_eq!(delivered.len(), 3);
        assert_eq!(delivered[1].0, "chat-2");
        assert_eq!(delivered[1].1.id, "11");
        assert_eq!(delivered[2].0, "chat-2");
        assert_eq!(delivered[2].1.id, "12");
    }
}

#[tokio::test]
async fn a_deleted_comment_after_lookup_failure_still_retries_the_saved_request() {
    let fixture = Fixture::new().await;
    fixture.source.link_error.store(true, Ordering::SeqCst);
    assert!(matches!(
        fixture
            .scan(vec![Fixture::comment(42, 10, "/ezra original request")])
            .await,
        Err(PollError::Incomplete)
    ));
    fixture.source.link_error.store(false, Ordering::SeqCst);
    fixture
        .source
        .links
        .lock()
        .expect("links")
        .insert(42, vec![Fixture::conversation(12)]);
    fixture
        .runtime
        .store
        .bind_conversation(&Fixture::conversation(12), &fixture.target("chat-old"))
        .await
        .expect("issue binds");
    fixture
        .scan(Vec::new())
        .await
        .expect("durable request retries without comment");
    assert_eq!(fixture.sender.creations.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.drain().await[0].0, "chat-old");
}
