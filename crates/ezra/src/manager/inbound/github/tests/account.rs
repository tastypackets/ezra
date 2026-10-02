use std::collections::BTreeMap;
use std::path::PathBuf;

use ezra::inbound::github::{AccountComment, AccountCommentSource, AccountCommentsPage};

use super::*;

#[tokio::test]
async fn expired_waiting_requests_report_failure_and_keep_dedupe() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database_path = directory.path().join("ezra.db");
    let runtime = InboundRuntime::open(
        &database_path,
        Arc::new(tokio::sync::Mutex::new(
            crate::manager::settings::Settings::default(),
        )),
    )
    .await
    .expect("runtime opens");
    let settings = InboundSettings::default();
    let event: InboundEvent = serde_json::from_value(json!({
        "key": {"conversation": {"source": "github:github.com", "subject": "7/42"}, "id": "10"},
        "actor": "1", "created_at": "2026-09-30T12:00:00Z", "message": "request",
        "source_url": "https://github.com/owner/repo/issues/42#issuecomment-10"
    }))
    .expect("event");
    runtime
        .store
        .insert(&event, settings.queue_limits())
        .await
        .expect("request persists");
    let fixture_pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", database_path.display()))
        .await
        .expect("fixture database connects");
    sqlx::query("UPDATE inbound_events SET received_at = unixepoch() - 90000")
        .execute(&fixture_pool)
        .await
        .expect("request waited more than one day");
    fixture_pool.close().await;
    let (status_sender, mut statuses) = tokio::sync::mpsc::channel(8);
    *runtime.github_feedback_sender.lock().await = Some(status_sender);
    runtime
        .expire_waiting_requests(&settings)
        .await
        .expect("expiry reports status");
    let feedback = statuses.try_recv().expect("failure status is queued");
    assert_eq!(feedback.key, event.key);
    assert_eq!(feedback.status, CommentStatus::Failed);
    assert_eq!(
        runtime
            .store
            .delivery_state(&event.key)
            .await
            .expect("state"),
        Some(ezra::inbound::store::DeliveryState::Expired)
    );
    assert_eq!(
        runtime
            .store
            .insert(&event, settings.queue_limits())
            .await
            .expect("dedupe persists"),
        InsertOutcome::Duplicate
    );
    runtime
        .expire_waiting_requests(&settings)
        .await
        .expect("second sweep succeeds");
    assert!(statuses.try_recv().is_err());
}

impl AccountCommentSource for Source {
    async fn account_comments(
        &self,
        cursor: Option<&str>,
    ) -> Result<AccountCommentsPage, Self::Error> {
        if self.fail_second_page && cursor.is_some() {
            return Err(std::io::Error::other("page unavailable").into());
        }
        let queued = self.pages.lock().expect("pages").pop_front();
        let values = queued.unwrap_or_else(|| self.comments.lock().expect("comments").clone());
        let next_cursor = values
            .first()
            .and_then(|value| value["next"].as_str())
            .map(str::to_owned);
        let mut comments = Vec::new();
        for value in values {
            let comment: IssueComment = serde_json::from_value(value.clone()).expect("comment");
            comments.push(AccountComment {
                repository: Repository {
                    id: NonZeroU64::new(7).expect("repository"),
                    full_name: value["repository"]
                        .as_str()
                        .unwrap_or("owner/repo")
                        .to_owned(),
                },
                comment,
                issue: IssueContext {
                    number: NonZeroU64::new(42).expect("issue"),
                    html_url: "https://github.com/owner/repo/issues/42".into(),
                    title: "Fix it".into(),
                    body: Some("Description".into()),
                },
            });
        }
        Ok(AccountCommentsPage {
            author: self.authenticated_user().await?,
            observed_at: self.observed_at.unwrap_or_else(OffsetDateTime::now_utc),
            comments,
            next_cursor,
        })
    }
}

#[tokio::test]
async fn discovery_uses_ten_second_overlap_and_caps_downtime_lookback_at_five_minutes() {
    for (last_scan_seconds, included_seconds, excluded_seconds) in
        [(-2, -11, -13), (-3600, -299, -301)]
    {
        let directory = tempfile::tempdir().expect("directory");
        let runtime = InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime");
        let source_time = OffsetDateTime::now_utc()
            .replace_nanosecond(0)
            .expect("time");
        let scope = ConversationKey {
            source: "github:github.com".into(),
            subject: "viewer/1".into(),
        };
        runtime
            .store
            .source_checkpoint(&scope, source_time - time::Duration::hours(2))
            .await
            .expect("activate");
        runtime
            .store
            .advance_source_checkpoint(
                &scope,
                source_time + time::Duration::seconds(last_scan_seconds),
            )
            .await
            .expect("last successful scan");
        let mut comments = Vec::new();
        for (identifier, seconds) in [(11, included_seconds), (10, excluded_seconds)] {
            let mut comment = Source::comment(identifier);
            let timestamp = (source_time + time::Duration::seconds(seconds))
                .format(&time::format_description::well_known::Rfc3339)
                .expect("time");
            comment["created_at"] = json!(timestamp);
            comment["updated_at"] = json!(timestamp);
            comments.push(comment);
        }
        let source = Source {
            comments: Mutex::new(comments),
            observed_at: Some(source_time),
            ..Source::default()
        };
        let mut settings = InboundSettings::default();
        settings.github.only_added_repositories = false;
        runtime
            .scan_account(&source, "github.com", &settings, &BTreeMap::new())
            .await
            .expect("scan");
        let pending = runtime
            .store
            .pending_routing("github:github.com", "")
            .await
            .expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].key.id, "11");
        assert_eq!(
            runtime
                .store
                .source_checkpoint(&scope, source_time)
                .await
                .expect("checkpoint")
                .scanned_through,
            source_time
        );
    }
}

#[tokio::test]
async fn an_oversized_request_reports_failure_without_blocking_later_requests_or_the_checkpoint() {
    let directory = tempfile::tempdir().expect("directory");
    let runtime = InboundRuntime::open(
        &directory.path().join("ezra.db"),
        Arc::new(tokio::sync::Mutex::new(
            crate::manager::settings::Settings::default(),
        )),
    )
    .await
    .expect("runtime");
    let source_time = OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .expect("time");
    let scope = ConversationKey {
        source: "github:github.com".into(),
        subject: "viewer/1".into(),
    };
    runtime
        .store
        .source_checkpoint(&scope, source_time - time::Duration::minutes(1))
        .await
        .expect("activate");
    let mut comments = Vec::new();
    for (identifier, seconds, body) in [
        (11, -10, "/ezra followup".to_owned()),
        (10, -20, format!("/ezra {}", "x".repeat(2048))),
    ] {
        let mut comment = Source::comment(identifier);
        let timestamp = (source_time + time::Duration::seconds(seconds))
            .format(&time::format_description::well_known::Rfc3339)
            .expect("time");
        comment["body"] = json!(body);
        comment["created_at"] = json!(timestamp);
        comment["updated_at"] = json!(timestamp);
        comments.push(comment);
    }
    let source = Source {
        comments: Mutex::new(comments),
        observed_at: Some(source_time),
        ..Source::default()
    };
    let mut settings = InboundSettings {
        max_queued_message_bytes: 1024,
        ..Default::default()
    };
    settings.github.only_added_repositories = false;
    let (feedback_sender, mut feedback_receiver) = tokio::sync::mpsc::channel(4);
    *runtime.github_feedback_sender.lock().await = Some(feedback_sender);
    runtime
        .scan_account(&source, "github.com", &settings, &BTreeMap::new())
        .await
        .expect("scan continues after rejection");
    let pending = runtime
        .store
        .pending_routing("github:github.com", "")
        .await
        .expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].key.id, "11");
    let rejected = feedback_receiver.try_recv().expect("rejection feedback");
    assert_eq!(rejected.key.id, "10");
    assert_eq!(rejected.status, CommentStatus::Failed);
    assert_eq!(
        runtime
            .store
            .source_checkpoint(&scope, source_time)
            .await
            .expect("checkpoint")
            .scanned_through,
        source_time
    );
}

#[tokio::test]
async fn edited_fresh_chat_commands_are_admitted_before_followups_with_numeric_id_ties() {
    let directory = tempfile::tempdir().expect("directory");
    let runtime = InboundRuntime::open(
        &directory.path().join("ezra.db"),
        Arc::new(tokio::sync::Mutex::new(
            crate::manager::settings::Settings::default(),
        )),
    )
    .await
    .expect("runtime");
    let source_time = OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .expect("time");
    runtime
        .store
        .source_checkpoint(
            &ConversationKey {
                source: "github:github.com".into(),
                subject: "viewer/1".into(),
            },
            source_time - time::Duration::minutes(1),
        )
        .await
        .expect("activate");
    let mut comments = Vec::new();
    for (identifier, created_seconds, updated_seconds, body) in [
        (1, -20, -1, "/ezra --new edited original request"),
        (2, -10, -2, "/ezra first followup"),
        (10, -10, -3, "/ezra second followup"),
    ] {
        let mut comment = Source::comment(identifier);
        comment["body"] = json!(body);
        for (field, seconds) in [
            ("created_at", created_seconds),
            ("updated_at", updated_seconds),
        ] {
            comment[field] = json!(
                (source_time + time::Duration::seconds(seconds))
                    .format(&time::format_description::well_known::Rfc3339)
                    .expect("time")
            );
        }
        comments.push(comment);
    }
    let source = Source {
        comments: Mutex::new(comments),
        observed_at: Some(source_time),
        ..Source::default()
    };
    let workspaces = BTreeMap::from([(
        "owner/repo".to_owned(),
        PathBuf::from("/home/dev/projects/repo"),
    )]);
    runtime
        .scan_account(
            &source,
            "github.com",
            &InboundSettings::default(),
            &workspaces,
        )
        .await
        .expect("scan");
    let pending = runtime
        .store
        .pending_routing("github:github.com", "")
        .await
        .expect("persisted");
    assert_eq!(
        pending
            .iter()
            .map(|event| (event.key.id.as_str(), event.new_chat))
            .collect::<Vec<_>>(),
        [("1", true), ("2", false), ("10", false)]
    );
}

#[tokio::test]
async fn discovery_persists_without_native_routing_and_ignores_history_retention_cutoff() {
    for only_added in [true, false] {
        let directory = tempfile::tempdir().expect("directory");
        let runtime = InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime");
        let source_time = OffsetDateTime::now_utc()
            .replace_nanosecond(0)
            .expect("time");
        let scope = ConversationKey {
            source: "github:github.com".into(),
            subject: "viewer/1".into(),
        };
        runtime
            .store
            .source_checkpoint(&scope, source_time - time::Duration::minutes(1))
            .await
            .expect("activate");
        let mut values = Vec::new();
        for (identifier, repository) in [(11, "owner/elsewhere"), (10, "owner/repo")] {
            let mut value = Source::comment(identifier);
            value["repository"] = json!(repository);
            value["created_at"] = json!(
                (source_time - time::Duration::days(1))
                    .format(&time::format_description::well_known::Rfc3339)
                    .expect("time")
            );
            value["updated_at"] = json!(
                (source_time - time::Duration::seconds(1))
                    .format(&time::format_description::well_known::Rfc3339)
                    .expect("time")
            );
            values.push(value);
        }
        let source = Source {
            comments: Mutex::new(values),
            observed_at: Some(source_time),
            link_error: AtomicBool::new(true),
            ..Source::default()
        };
        let mut settings = InboundSettings {
            retention_days: 0,
            ..Default::default()
        };
        settings.github.only_added_repositories = only_added;
        let workspaces = BTreeMap::from([(
            "owner/repo".to_owned(),
            PathBuf::from("/home/dev/projects/repo"),
        )]);
        runtime
            .scan_account(&source, "github.com", &settings, &workspaces)
            .await
            .expect("scan independently of failed linking");
        let pending = runtime
            .store
            .pending_routing("github:github.com", "")
            .await
            .expect("persisted");
        assert_eq!(pending.len(), if only_added { 1 } else { 2 });
        assert_eq!(source.link_reads.load(Ordering::SeqCst), 0);
        assert_eq!(
            runtime
                .store
                .source_checkpoint(&scope, source_time)
                .await
                .expect("checkpoint")
                .scanned_through,
            source_time
        );
        runtime
            .scan_account(&source, "github.com", &settings, &workspaces)
            .await
            .expect("overlap");
        assert_eq!(
            runtime
                .store
                .pending_routing("github:github.com", "")
                .await
                .expect("dedupe")
                .len(),
            pending.len()
        );
    }
}

#[tokio::test]
async fn pagination_failure_and_changed_verification_leave_account_checkpoint_unchanged() {
    for page_failure in [false, true] {
        let directory = tempfile::tempdir().expect("directory");
        let runtime = InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime");
        let source_time = OffsetDateTime::now_utc()
            .replace_nanosecond(0)
            .expect("time");
        let initial = source_time - time::Duration::minutes(1);
        let scope = ConversationKey {
            source: "github:github.com".into(),
            subject: "viewer/1".into(),
        };
        runtime
            .store
            .source_checkpoint(&scope, initial)
            .await
            .expect("activate");
        let mut value = Source::comment(10);
        value["updated_at"] = json!(
            source_time
                .format(&time::format_description::well_known::Rfc3339)
                .expect("time")
        );
        value["next"] = json!("next-page");
        let source = Source {
            pages: Mutex::new(std::collections::VecDeque::from([
                vec![value],
                vec![],
                vec![],
            ])),
            fail_second_page: page_failure,
            observed_at: Some(source_time),
            ..Source::default()
        };
        let mut settings = InboundSettings::default();
        settings.github.only_added_repositories = false;
        assert!(
            runtime
                .scan_account(&source, "github.com", &settings, &BTreeMap::new())
                .await
                .is_err()
        );
        assert_eq!(
            runtime
                .store
                .source_checkpoint(&scope, source_time)
                .await
                .expect("checkpoint")
                .scanned_through,
            initial
        );
    }
}

#[tokio::test]
async fn newer_comments_wait_for_next_scan_and_unadded_repositories_route_to_home() {
    let directory = tempfile::tempdir().expect("directory");
    let runtime = InboundRuntime::open(
        &directory.path().join("ezra.db"),
        Arc::new(tokio::sync::Mutex::new(
            crate::manager::settings::Settings::default(),
        )),
    )
    .await
    .expect("runtime");
    let source_time = OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .expect("time");
    let scope = ConversationKey {
        source: "github:github.com".into(),
        subject: "viewer/1".into(),
    };
    runtime
        .store
        .source_checkpoint(&scope, source_time - time::Duration::minutes(1))
        .await
        .expect("activate");
    let mut comments = Vec::new();
    for (identifier, seconds) in [(11, 1), (10, -1)] {
        let mut comment = Source::comment(identifier);
        comment["updated_at"] = json!(
            (source_time + time::Duration::seconds(seconds))
                .format(&time::format_description::well_known::Rfc3339)
                .expect("time")
        );
        comments.push(comment);
    }
    let source = Source {
        comments: Mutex::new(comments),
        observed_at: Some(source_time),
        ..Source::default()
    };
    let mut settings = InboundSettings::default();
    settings.github.only_added_repositories = false;
    runtime
        .scan_account(&source, "github.com", &settings, &BTreeMap::new())
        .await
        .expect("scan");
    let pending = runtime
        .store
        .pending_routing("github:github.com", "")
        .await
        .expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].key.id, "10");
    let sender = Sender {
        expected_workspace: Some("/home/dev".into()),
        ..Default::default()
    };
    runtime
        .route_saved_github(&source, "github.com", &sender, &settings, &BTreeMap::new())
        .await
        .expect("route");
    assert_eq!(
        runtime
            .store
            .find_binding(&pending[0].key.conversation)
            .await
            .expect("binding")
            .expect("bound")
            .workspace,
        "/home/dev"
    );
    assert_eq!(sender.creations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn channel_routing_reuses_freed_slots_without_waiting_for_the_old_batch() {
    let directory = tempfile::tempdir().expect("directory");
    let runtime = Arc::new(
        InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime"),
    );
    let gates: Vec<_> = (0..9)
        .map(|_| Arc::new(tokio::sync::Notify::new()))
        .collect();
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let incoming = runtime
        .github_routing_receiver
        .lock()
        .await
        .take()
        .expect("receiver");
    let running = Arc::clone(&runtime);
    let operation_gates = gates.clone();
    let worker = tokio::spawn(async move {
        let sender = Sender::default();
        running
            .process_github_events("github.com", &sender, incoming, |key| {
                let gate =
                    Arc::clone(&operation_gates[key.id.parse::<usize>().expect("numeric ID")]);
                let started = started.clone();
                async move {
                    let released = gate.notified();
                    tokio::pin!(released);
                    released.as_mut().enable();
                    started.send(key.id.clone()).expect("receiver");
                    released.await;
                    (key, Ok(Admission::Accepted))
                }
            })
            .await;
    });
    let request = |identifier: usize| EventKey {
        conversation: ConversationKey {
            source: "github:github.com".into(),
            subject: format!("1/{identifier}"),
        },
        id: identifier.to_string(),
    };
    for identifier in 0..8 {
        runtime.enqueue_github_event(request(identifier));
    }
    for _ in 0..8 {
        tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .expect("initial task starts")
            .expect("worker");
    }
    runtime.enqueue_github_event(request(0));
    runtime.enqueue_github_event(request(8));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), starts.recv())
            .await
            .is_err()
    );
    gates[0].notify_waiters();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .expect("freed slot starts new request")
            .expect("worker"),
        "8"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), starts.recv())
            .await
            .is_err()
    );
    worker.abort();
    assert!(worker.await.expect_err("worker cancelled").is_cancelled());
}

struct AvailabilitySender(tokio::sync::watch::Sender<bool>);

impl MessageSender for AvailabilitySender {
    fn control_available(&self) -> bool {
        *self.0.borrow()
    }
    fn control_changes(&self) -> Option<tokio::sync::watch::Receiver<bool>> {
        Some(self.0.subscribe())
    }
    async fn create_chat(
        &self,
        _workspace: &str,
        _name: Option<&str>,
    ) -> Result<String, MessageSendError> {
        panic!("scheduler fixture supplies the operation")
    }
    async fn queue_message(
        &self,
        _chat: &str,
        _event: &InboundEvent,
    ) -> Result<MessageReceipt, MessageSendError> {
        panic!("scheduler fixture supplies the operation")
    }
}

#[tokio::test]
async fn offline_requests_start_once_on_reconnect_and_a_reconnect_during_work_is_not_lost() {
    let directory = tempfile::tempdir().expect("directory");
    let runtime = Arc::new(
        InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime"),
    );
    let availability = tokio::sync::watch::Sender::new(false);
    let sender = AvailabilitySender(availability.clone());
    let incoming = runtime
        .github_routing_receiver
        .lock()
        .await
        .take()
        .expect("receiver");
    let request = EventKey {
        conversation: ConversationKey {
            source: "github:github.com".into(),
            subject: "1/2".into(),
        },
        id: "1".into(),
    };
    runtime.enqueue_github_event(request.clone());
    runtime.enqueue_github_event(request);
    let gate = Arc::new(tokio::sync::Notify::new());
    let running = Arc::clone(&runtime);
    let operation_gate = Arc::clone(&gate);
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let attempts = Arc::new(AtomicUsize::new(0));
    let worker = tokio::spawn(async move {
        running
            .process_github_events("github.com", &sender, incoming, |key| {
                let started = started.clone();
                let gate = Arc::clone(&operation_gate);
                let attempts = Arc::clone(&attempts);
                async move {
                    let released = gate.notified();
                    tokio::pin!(released);
                    released.as_mut().enable();
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    started.send(attempt).expect("receiver");
                    if attempt == 0 {
                        released.await;
                        (key, Err(PollError::Unavailable))
                    } else {
                        (key, Ok(Admission::Accepted))
                    }
                }
            })
            .await;
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), starts.recv())
            .await
            .is_err()
    );
    availability.send_replace(true);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .expect("reconnect starts request")
            .expect("worker"),
        0
    );
    availability.send_replace(false);
    availability.send_replace(true);
    tokio::task::yield_now().await;
    gate.notify_waiters();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .expect("new connection is used")
            .expect("worker"),
        1
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), starts.recv())
            .await
            .is_err()
    );
    worker.abort();
    assert!(worker.await.expect_err("cancelled").is_cancelled());
}

#[tokio::test]
async fn a_late_deferred_result_observes_progress_that_happened_while_it_was_running() {
    let directory = tempfile::tempdir().expect("directory");
    let runtime = Arc::new(
        InboundRuntime::open(
            &directory.path().join("ezra.db"),
            Arc::new(tokio::sync::Mutex::new(
                crate::manager::settings::Settings::default(),
            )),
        )
        .await
        .expect("runtime"),
    );
    let incoming = runtime
        .github_routing_receiver
        .lock()
        .await
        .take()
        .expect("receiver");
    let first_gate = Arc::new(tokio::sync::Notify::new());
    let deferred_gate = Arc::new(tokio::sync::Notify::new());
    let running = Arc::clone(&runtime);
    let operation_first_gate = Arc::clone(&first_gate);
    let operation_deferred_gate = Arc::clone(&deferred_gate);
    let attempts = Arc::new(AtomicUsize::new(0));
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let (completed, mut completions) = tokio::sync::mpsc::unbounded_channel();
    let worker = tokio::spawn(async move {
        let sender = Sender::default();
        running
            .process_github_events("github.com", &sender, incoming, |key| {
                let first_gate = Arc::clone(&operation_first_gate);
                let deferred_gate = Arc::clone(&operation_deferred_gate);
                let attempts = Arc::clone(&attempts);
                let started = started.clone();
                let completed = completed.clone();
                async move {
                    let gate = if key.id == "first" {
                        first_gate
                    } else {
                        deferred_gate
                    };
                    let released = gate.notified();
                    tokio::pin!(released);
                    released.as_mut().enable();
                    started.send(key.id.clone()).expect("receiver");
                    if key.id == "first" {
                        released.await;
                        completed.send(()).expect("completion receiver");
                        (key, Ok(Admission::Accepted))
                    } else if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        released.await;
                        (key, Ok(Admission::Deferred))
                    } else {
                        (key, Ok(Admission::Accepted))
                    }
                }
            })
            .await;
    });
    for identifier in ["first", "second"] {
        runtime.enqueue_github_event(EventKey {
            conversation: ConversationKey {
                source: "github:github.com".into(),
                subject: "1/2".into(),
            },
            id: identifier.into(),
        });
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), starts.recv())
                .await
                .expect("task starts")
                .expect("worker"),
            identifier
        );
    }
    first_gate.notify_waiters();
    tokio::time::timeout(Duration::from_secs(2), completions.recv())
        .await
        .expect("first operation finished")
        .expect("worker");
    deferred_gate.notify_waiters();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .expect("deferred operation uses completed progress")
            .expect("worker"),
        "second"
    );
    worker.abort();
    assert!(worker.await.expect_err("cancelled").is_cancelled());
}
