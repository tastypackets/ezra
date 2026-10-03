use std::collections::BTreeMap;
use std::path::PathBuf;

use ezra::agent::Agent;
use ezra::inbound::github::{AccountComment, AccountCommentSource, AccountCommentsPage};

use super::*;
use crate::manager::inbound::AgentSenders;
use crate::manager::supervision::Pauses;

#[tokio::test]
async fn expired_waiting_requests_report_failure_and_keep_dedupe() {
    let (logs, _capture) = crate::logging::CapturedLogs::start("warn");
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
    let [logged] = <[String; 1]>::try_from(logs.lines_with(&event.key.delivery_id()))
        .expect("the expiry is logged once");
    for part in [
        "WARN",
        "GitHub request expired before it was sent",
        "agent=codex",
        "waiting_expiry_hours=24",
    ] {
        assert!(logged.contains(part), "{logged}");
    }
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
async fn a_claude_shortcut_saves_and_queues_a_claude_request() {
    let directory = tempfile::tempdir().expect("directory");
    let runtime = InboundRuntime::open(
        &directory.path().join("ezra.db"),
        Arc::new(tokio::sync::Mutex::new(
            crate::manager::settings::Settings::default(),
        )),
    )
    .await
    .expect("runtime");
    let mut incoming = runtime
        .github_routing_receiver
        .lock()
        .await
        .take()
        .expect("receiver");
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
    let mut comment = Source::comment(10);
    comment["body"] = json!("/ezra-claude fix the crash");
    comment["updated_at"] = json!(
        (source_time - time::Duration::seconds(1))
            .format(&time::format_description::well_known::Rfc3339)
            .expect("time")
    );
    let source = Source {
        comments: Mutex::new(vec![comment]),
        observed_at: Some(source_time),
        ..Source::default()
    };
    let mut settings = InboundSettings::default();
    let shortcut = ezra::inbound::Shortcut {
        agent: Agent::Claude,
        model: Some("opus".into()),
        effort: None,
    };
    settings
        .shortcuts
        .insert("/ezra-claude".into(), shortcut.clone());
    runtime
        .scan_account(
            &source,
            "github.com",
            &settings,
            &BTreeMap::from([(
                "owner/repo".to_owned(),
                PathBuf::from("/home/dev/projects/repo"),
            )]),
        )
        .await
        .expect("scan");
    let pending = runtime
        .store
        .pending_routing("github:github.com", "")
        .await
        .expect("persisted");
    let [event] = pending.as_slice() else {
        panic!("one request is saved: {pending:?}");
    };
    assert_eq!(event.options, shortcut);
    assert!(event.message.contains("Request:\nfix the crash\n"));
    let Ok(RoutingWake::Event(key, agent)) = incoming.try_recv() else {
        panic!("the request is queued for routing");
    };
    assert_eq!((key, agent), (event.key.clone(), Agent::Claude));
}

#[tokio::test]
async fn a_shortcut_with_invalid_options_fails_only_its_comment() {
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
    let comments = [(11, "/ezra fix it"), (10, "/ezra-bad fix it")]
        .into_iter()
        .map(|(identifier, body)| {
            let mut comment = Source::comment(identifier);
            comment["body"] = json!(body);
            comment["updated_at"] = json!(
                (source_time - time::Duration::seconds(1))
                    .format(&time::format_description::well_known::Rfc3339)
                    .expect("time")
            );
            comment
        })
        .collect();
    let source = Source {
        comments: Mutex::new(comments),
        observed_at: Some(source_time),
        ..Source::default()
    };
    let mut settings = InboundSettings::default();
    settings.shortcuts.insert(
        "/ezra-bad".into(),
        ezra::inbound::Shortcut {
            model: Some("--settings=x".into()),
            ..Default::default()
        },
    );
    runtime
        .scan_account(
            &source,
            "github.com",
            &settings,
            &BTreeMap::from([(
                "owner/repo".to_owned(),
                PathBuf::from("/home/dev/projects/repo"),
            )]),
        )
        .await
        .expect("scan finishes");
    let pending = runtime
        .store
        .pending_routing("github:github.com", "")
        .await
        .expect("persisted");
    assert_eq!(
        pending
            .iter()
            .map(|event| event.key.id.as_str())
            .collect::<Vec<_>>(),
        ["11"]
    );
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
            .find_binding(&pending[0].key.conversation, Agent::Codex)
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
        let senders = AgentSenders {
            claude: Arc::new(Sender::default()),
            codex: Arc::new(Sender::default()),
        };
        running
            .process_github_events("github.com", &senders, incoming, |key| {
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
        runtime.enqueue_github_event(request(identifier), Agent::Codex);
    }
    for _ in 0..8 {
        tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .expect("initial task starts")
            .expect("worker");
    }
    runtime.enqueue_github_event(request(0), Agent::Codex);
    runtime.enqueue_github_event(request(8), Agent::Codex);
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

#[derive(Default)]
struct ResumingSender(tokio::sync::watch::Sender<()>);

impl MessageSender for ResumingSender {
    fn resumes(&self) -> Option<tokio::sync::watch::Receiver<()>> {
        Some(self.0.subscribe())
    }
    async fn create_chat(
        &self,
        _workspace: &str,
        _name: Option<&str>,
        _options: &ezra::inbound::Shortcut,
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
async fn a_paused_request_starts_once_on_resume_and_a_resume_during_work_is_not_lost() {
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
    let resumes = tokio::sync::watch::Sender::new(());
    let senders = AgentSenders {
        claude: Arc::new(ResumingSender::default()),
        codex: Arc::new(ResumingSender(resumes.clone())),
    };
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
    runtime.enqueue_github_event(request.clone(), Agent::Codex);
    runtime.enqueue_github_event(request, Agent::Codex);
    let gate = Arc::new(tokio::sync::Notify::new());
    let running = Arc::clone(&runtime);
    let operation_gate = Arc::clone(&gate);
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let attempts = Arc::new(AtomicUsize::new(0));
    let worker = tokio::spawn(async move {
        running
            .process_github_events("github.com", &senders, incoming, |key| {
                let started = started.clone();
                let gate = Arc::clone(&operation_gate);
                let attempts = Arc::clone(&attempts);
                async move {
                    let released = gate.notified();
                    tokio::pin!(released);
                    released.as_mut().enable();
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    started.send(attempt).expect("receiver");
                    match attempt {
                        0 => (key, Err(PollError::Paused("restarting".into()))),
                        1 => {
                            released.await;
                            (key, Err(PollError::Paused("restarting".into())))
                        }
                        _ => (key, Ok(Admission::Accepted)),
                    }
                }
            })
            .await;
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .expect("the request starts")
            .expect("worker"),
        0
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), starts.recv())
            .await
            .is_err()
    );
    resumes.send_replace(());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .expect("a resume starts the request")
            .expect("worker"),
        1
    );
    resumes.send_replace(());
    tokio::task::yield_now().await;
    gate.notify_waiters();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .expect("the resume during work is used")
            .expect("worker"),
        2
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
        let senders = AgentSenders {
            claude: Arc::new(Sender::default()),
            codex: Arc::new(Sender::default()),
        };
        running
            .process_github_events("github.com", &senders, incoming, |key| {
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
        runtime.enqueue_github_event(
            EventKey {
                conversation: ConversationKey {
                    source: "github:github.com".into(),
                    subject: "1/2".into(),
                },
                id: identifier.into(),
            },
            Agent::Codex,
        );
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

#[tokio::test]
async fn a_paused_agent_parks_only_its_own_requests() {
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
    let claude = tokio::sync::watch::Sender::new(());
    let senders = AgentSenders {
        claude: Arc::new(ResumingSender(claude.clone())),
        codex: Arc::new(ResumingSender::default()),
    };
    let incoming = runtime
        .github_routing_receiver
        .lock()
        .await
        .take()
        .expect("receiver");
    let request = |identifier: &str| EventKey {
        conversation: ConversationKey {
            source: "github:github.com".into(),
            subject: format!("1/{identifier}"),
        },
        id: identifier.into(),
    };
    let claude_attempts = Arc::new(AtomicUsize::new(0));
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let running = Arc::clone(&runtime);
    let worker = tokio::spawn(async move {
        running
            .process_github_events("github.com", &senders, incoming, |key| {
                let started = started.clone();
                let claude_attempts = Arc::clone(&claude_attempts);
                async move {
                    started.send(key.id.clone()).expect("receiver");
                    if key.id == "claude" && claude_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (key, Err(PollError::Paused("restarting".into())));
                    }
                    (key, Ok(Admission::Accepted))
                }
            })
            .await;
    });
    runtime.enqueue_github_event(request("claude"), Agent::Claude);
    assert_eq!(starts.next_start().await, "claude");
    runtime.enqueue_github_event(request("codex"), Agent::Codex);
    assert_eq!(
        starts.next_start().await,
        "codex",
        "Codex routes while the Claude request waits"
    );
    runtime.enqueue_github_event(request("later-codex"), Agent::Codex);
    assert_eq!(starts.next_start().await, "later-codex");
    assert!(
        tokio::time::timeout(Duration::from_millis(30), starts.recv())
            .await
            .is_err()
    );
    claude.send_replace(());
    assert_eq!(
        starts.next_start().await,
        "claude",
        "the parked Claude request starts again after a resume"
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
async fn a_request_that_cannot_reach_a_destination_that_is_not_paused_fails() {
    let (logs, _capture) = crate::logging::CapturedLogs::start("warn");
    let fixture = DeliveryFixture::new().await;
    let runtime = Arc::clone(&fixture.runtime);
    let request = DeliveryFixture::request(42, 10, Agent::Codex);
    fixture.insert(&[&request]).await;
    let (status_sender, mut statuses) = tokio::sync::mpsc::channel(8);
    *runtime.github_feedback_sender.lock().await = Some(status_sender);
    let codex = tokio::sync::watch::Sender::new(());
    let senders = AgentSenders {
        claude: Arc::new(ResumingSender::default()),
        codex: Arc::new(ResumingSender(codex.clone())),
    };
    let incoming = runtime
        .github_routing_receiver
        .lock()
        .await
        .take()
        .expect("receiver");
    let attempts = Arc::new(AtomicUsize::new(0));
    let running = Arc::clone(&runtime);
    let operation_attempts = Arc::clone(&attempts);
    let worker = tokio::spawn(async move {
        running
            .process_github_events("github.com", &senders, incoming, |key| {
                let attempts = Arc::clone(&operation_attempts);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    (key, Err(PollError::Unavailable("offline".into())))
                }
            })
            .await;
    });
    attempts.reach(1).await;
    let feedback = tokio::time::timeout(Duration::from_secs(2), statuses.recv())
        .await
        .expect("the failure is reported")
        .expect("feedback sender");
    assert_eq!(
        (feedback.key, feedback.status),
        (request.key.clone(), CommentStatus::Failed)
    );
    assert_eq!(
        runtime
            .store
            .delivery_state(&request.key)
            .await
            .expect("state"),
        Some(DeliveryState::Failed)
    );
    codex.send_replace(());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(statuses.try_recv().is_err());
    let [logged] = <[String; 1]>::try_from(logs.lines_with(&request.key.delivery_id()))
        .expect("the failure is logged once");
    for part in [
        "WARN",
        "GitHub request failed before submission",
        "agent=codex",
        "error=agent is unavailable: offline",
    ] {
        assert!(logged.contains(part), "{logged}");
    }
    worker.abort();
    assert!(worker.await.expect_err("cancelled").is_cancelled());
}

#[tokio::test]
async fn a_route_that_fails_after_its_request_was_settled_is_no_second_failure() {
    let (logs, _capture) = crate::logging::CapturedLogs::start("warn");
    let fixture = DeliveryFixture::new().await;
    let runtime = Arc::clone(&fixture.runtime);
    let request = DeliveryFixture::request(42, 10, Agent::Codex);
    fixture.insert(&[&request]).await;
    assert!(
        runtime
            .store
            .fail_waiting_event(&request.key)
            .await
            .expect("the request fails elsewhere")
    );
    let senders = AgentSenders {
        claude: Arc::new(ResumingSender::default()),
        codex: Arc::new(ResumingSender::default()),
    };
    let incoming = runtime
        .github_routing_receiver
        .lock()
        .await
        .take()
        .expect("receiver");
    let attempts = Arc::new(AtomicUsize::new(0));
    let running = Arc::clone(&runtime);
    let operation_attempts = Arc::clone(&attempts);
    let worker = tokio::spawn(async move {
        running
            .process_github_events("github.com", &senders, incoming, |key| {
                let attempts = Arc::clone(&operation_attempts);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    (key, Err(PollError::Unavailable("offline".into())))
                }
            })
            .await;
    });
    runtime.enqueue_github_event(request.key.clone(), Agent::Codex);
    attempts.reach(1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        runtime
            .store
            .delivery_state(&request.key)
            .await
            .expect("state"),
        Some(DeliveryState::Failed)
    );
    assert!(
        logs.lines_with(&request.key.delivery_id()).is_empty(),
        "{}",
        logs.text()
    );
    worker.abort();
    assert!(worker.await.expect_err("cancelled").is_cancelled());
}

#[tokio::test]
async fn a_paused_destination_waits_for_its_agents_next_resume() {
    let (logs, _capture) = crate::logging::CapturedLogs::start("warn");
    let fixture = DeliveryFixture::new().await;
    let runtime = Arc::clone(&fixture.runtime);
    let claude = tokio::sync::watch::Sender::new(());
    let senders = AgentSenders {
        claude: Arc::new(ResumingSender(claude.clone())),
        codex: Arc::new(ResumingSender::default()),
    };
    let incoming = runtime
        .github_routing_receiver
        .lock()
        .await
        .take()
        .expect("receiver");
    let restarts = Arc::new(AtomicUsize::new(0));
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let running = Arc::clone(&runtime);
    let operation_restarts = Arc::clone(&restarts);
    let worker = tokio::spawn(async move {
        running
            .process_github_events("github.com", &senders, incoming, |key| {
                let started = started.clone();
                let restarts = Arc::clone(&operation_restarts);
                async move {
                    started.send(key.id.clone()).expect("receiver");
                    tokio::task::yield_now().await;
                    if key.id == "restarting" && restarts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (
                            key,
                            Err(PollError::Paused(
                                "Claude Remote Control in /home/dev/projects/a is starting".into(),
                            )),
                        );
                    }
                    (key, Ok(Admission::Accepted))
                }
            })
            .await;
    });
    let request = |identifier: &str| EventKey {
        conversation: ConversationKey {
            source: "github:github.com".into(),
            subject: format!("1/{identifier}"),
        },
        id: identifier.into(),
    };
    runtime.enqueue_github_event(request("restarting"), Agent::Claude);
    assert_eq!(starts.next_start().await, "restarting");
    restarts.reach(1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    runtime.enqueue_github_event(request("restarting"), Agent::Claude);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), starts.recv())
            .await
            .is_err(),
        "a parked request is not tried again without a change"
    );
    runtime.enqueue_github_event(request("other-folder"), Agent::Claude);
    assert_eq!(starts.next_start().await, "other-folder");
    runtime.enqueue_github_event(request("codex"), Agent::Codex);
    assert_eq!(starts.next_start().await, "codex");

    claude.send_replace(());
    assert_eq!(starts.next_start().await, "restarting");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(restarts.load(Ordering::SeqCst), 2);
    assert!(starts.try_recv().is_err());
    assert!(
        logs.lines_with(&request("restarting").delivery_id())
            .is_empty(),
        "a request that waits is no failure"
    );
    worker.abort();
    assert!(worker.await.expect_err("cancelled").is_cancelled());
}

#[tokio::test]
async fn a_pause_reported_after_a_resume_is_tried_again_at_once() {
    let fixture = DeliveryFixture::new().await;
    let runtime = Arc::clone(&fixture.runtime);
    let claude = tokio::sync::watch::Sender::new(());
    let senders = AgentSenders {
        claude: Arc::new(ResumingSender(claude.clone())),
        codex: Arc::new(ResumingSender::default()),
    };
    let incoming = runtime
        .github_routing_receiver
        .lock()
        .await
        .take()
        .expect("receiver");
    let gate = Arc::new(tokio::sync::Notify::new());
    let attempts = Arc::new(AtomicUsize::new(0));
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let running = Arc::clone(&runtime);
    let operation_gate = Arc::clone(&gate);
    let worker = tokio::spawn(async move {
        running
            .process_github_events("github.com", &senders, incoming, |key| {
                let started = started.clone();
                let gate = Arc::clone(&operation_gate);
                let attempts = Arc::clone(&attempts);
                async move {
                    let released = gate.notified();
                    tokio::pin!(released);
                    released.as_mut().enable();
                    started.send(key.id.clone()).expect("receiver");
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        released.await;
                        return (
                            key,
                            Err(PollError::Paused(
                                "Claude Remote Control in /home/dev/projects/a is starting".into(),
                            )),
                        );
                    }
                    (key, Ok(Admission::Accepted))
                }
            })
            .await;
    });
    runtime.enqueue_github_event(
        EventKey {
            conversation: ConversationKey {
                source: "github:github.com".into(),
                subject: "1/2".into(),
            },
            id: "raced".into(),
        },
        Agent::Claude,
    );
    assert_eq!(starts.next_start().await, "raced");
    claude.send_replace(());
    tokio::task::yield_now().await;
    gate.notify_waiters();
    assert_eq!(
        starts.next_start().await,
        "raced",
        "the destination resumed while the request was routed"
    );
    worker.abort();
    assert!(worker.await.expect_err("cancelled").is_cancelled());
}

trait StartsExt {
    async fn next_start(&mut self) -> String;
}

impl StartsExt for tokio::sync::mpsc::UnboundedReceiver<String> {
    async fn next_start(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(2), self.recv())
            .await
            .expect("a request starts")
            .expect("worker")
    }
}

type Deliveries = tokio::sync::mpsc::UnboundedReceiver<(Agent, String, EventKey)>;

trait AttemptsExt {
    async fn reach(&self, count: usize);
}

impl AttemptsExt for AtomicUsize {
    async fn reach(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while self.load(Ordering::SeqCst) < count {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("attempts reach the expected count");
    }
}

#[derive(Default)]
struct Refusal {
    refusing: AtomicBool,
    refused: AtomicUsize,
}

impl Refusal {
    fn refuses(&self) -> bool {
        let refusing = self.refusing.load(Ordering::SeqCst);
        if refusing {
            self.refused.fetch_add(1, Ordering::SeqCst);
        }
        refusing
    }
}

struct DeliverySender {
    agent: Agent,
    pauses: Arc<Pauses<()>>,
    refusal: Arc<Refusal>,
    created: AtomicUsize,
    delivered: tokio::sync::mpsc::UnboundedSender<(Agent, String, EventKey)>,
}

impl DeliverySender {
    fn reach(&self) -> Result<(), MessageSendError> {
        if self.pauses.is_paused(&()) {
            return Err(MessageSendError::Paused("paused".into()));
        }
        if self.refusal.refuses() {
            return Err(MessageSendError::Unavailable("offline".into()));
        }
        Ok(())
    }
}

impl MessageSender for DeliverySender {
    fn resumes(&self) -> Option<tokio::sync::watch::Receiver<()>> {
        Some(self.pauses.resumes())
    }
    async fn create_chat(
        &self,
        _workspace: &str,
        _name: Option<&str>,
        options: &ezra::inbound::Shortcut,
    ) -> Result<String, MessageSendError> {
        assert_eq!(options.agent, self.agent);
        self.reach()?;
        let created = self
            .created
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        Ok(format!("{}-chat-{created}", self.agent))
    }
    async fn queue_message(
        &self,
        chat_id: &str,
        event: &InboundEvent,
    ) -> Result<MessageReceipt, MessageSendError> {
        assert_eq!(event.options.agent, self.agent);
        self.reach()?;
        self.delivered
            .send((self.agent, chat_id.to_owned(), event.key.clone()))
            .expect("receiver");
        Ok(MessageReceipt {
            chat_name: None,
            native_message_id: event.key.id.clone(),
            delivery_id: event.key.delivery_id(),
        })
    }
}

struct DeliveryFixture {
    directory: tempfile::TempDir,
    runtime: Arc<InboundRuntime>,
}

impl DeliveryFixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().expect("directory");
        std::fs::create_dir(directory.path().join("projects")).expect("projects directory");
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
        Self { directory, runtime }
    }

    fn request(number: u64, identifier: u64, agent: Agent) -> InboundEvent {
        InboundEvent {
            key: EventKey {
                conversation: ConversationKey {
                    source: "github:github.com".into(),
                    subject: format!("7/{number}"),
                },
                id: identifier.to_string(),
            },
            new_chat: false,
            options: ezra::inbound::Shortcut {
                agent,
                ..Default::default()
            },
            chat_name: None,
            source_url: Some(format!(
                "https://github.com/owner/repo/issues/{number}#issuecomment-{identifier}"
            )),
            actor: "1".into(),
            created_at: OffsetDateTime::now_utc(),
            message: "Issue or pull request: follow up".into(),
            initial_context: None,
        }
    }

    async fn bind_to_codex(&self, event: &InboundEvent, chat_id: &str) {
        self.runtime
            .store
            .bind_conversation(
                &event.key.conversation,
                &ezra::inbound::store::SessionTarget {
                    host_id: self.runtime.host_id.clone(),
                    agent: "codex".into(),
                    chat_id: chat_id.into(),
                    workspace: "/home/dev/projects/repo".into(),
                },
            )
            .await
            .expect("discussion binds to Codex");
    }

    async fn insert(&self, events: &[&InboundEvent]) {
        for event in events {
            self.runtime
                .store
                .insert(event, InboundSettings::default().queue_limits())
                .await
                .expect("request inserts");
        }
    }

    fn route(
        &self,
        claude: &Arc<Pauses<()>>,
        codex: &Arc<Pauses<()>>,
        claude_refusal: &Arc<Refusal>,
    ) -> (tokio::task::JoinHandle<()>, Deliveries) {
        let (delivered, deliveries) = tokio::sync::mpsc::unbounded_channel();
        let senders = AgentSenders {
            claude: Arc::new(DeliverySender {
                agent: Agent::Claude,
                pauses: Arc::clone(claude),
                refusal: Arc::clone(claude_refusal),
                created: AtomicUsize::new(0),
                delivered: delivered.clone(),
            }),
            codex: Arc::new(DeliverySender {
                agent: Agent::Codex,
                pauses: Arc::clone(codex),
                refusal: Arc::default(),
                created: AtomicUsize::new(0),
                delivered,
            }),
        };
        let tools = crate::manager::git::GitTools::under(self.directory.path());
        let projects = ProjectsDirectory(self.directory.path().join("projects"));
        let running = Arc::clone(&self.runtime);
        let worker = tokio::spawn(async move {
            running.route_github(&tools, &projects, &senders).await;
        });
        (worker, deliveries)
    }

    async fn next_delivery(deliveries: &mut Deliveries) -> (Agent, String, EventKey) {
        tokio::time::timeout(Duration::from_secs(2), deliveries.recv())
            .await
            .expect("a request is delivered")
            .expect("senders")
    }

    async fn delivered_to(&self, event: &InboundEvent) -> (String, String) {
        let mut state = None;
        for _ in 0..200 {
            state = self
                .runtime
                .store
                .delivery_state(&event.key)
                .await
                .expect("state");
            if state == Some(DeliveryState::Delivered) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(state, Some(DeliveryState::Delivered));
        let binding = self
            .runtime
            .store
            .find_binding(&event.key.conversation, event.options.agent)
            .await
            .expect("binding reads")
            .expect("bound");
        (binding.agent, binding.chat_id)
    }
}

#[tokio::test]
async fn each_request_is_delivered_by_its_agent_and_waits_only_for_it() {
    let fixture = DeliveryFixture::new().await;
    let codex_request = DeliveryFixture::request(42, 10, Agent::Codex);
    let mut claude_request = DeliveryFixture::request(43, 11, Agent::Claude);
    claude_request.new_chat = true;
    fixture.bind_to_codex(&codex_request, "codex-chat").await;
    fixture.insert(&[&claude_request, &codex_request]).await;
    let claude = Arc::new(Pauses::new([()]));
    let (worker, mut deliveries) =
        fixture.route(&claude, &Arc::new(Pauses::new([])), &Arc::default());
    assert_eq!(
        DeliveryFixture::next_delivery(&mut deliveries).await,
        (Agent::Codex, "codex-chat".into(), codex_request.key.clone())
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), deliveries.recv())
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .runtime
            .store
            .delivery_state(&claude_request.key)
            .await
            .expect("state"),
        Some(DeliveryState::Pending)
    );
    claude.resume(&());
    assert_eq!(
        DeliveryFixture::next_delivery(&mut deliveries).await,
        (
            Agent::Claude,
            "claude-chat-1".into(),
            claude_request.key.clone()
        )
    );
    assert_eq!(
        fixture.delivered_to(&codex_request).await,
        ("codex".into(), "codex-chat".into())
    );
    assert_eq!(
        fixture.delivered_to(&claude_request).await,
        ("claude".into(), "claude-chat-1".into())
    );
    worker.abort();
    assert!(worker.await.expect_err("cancelled").is_cancelled());
}

#[tokio::test]
async fn a_waiting_claude_request_does_not_hold_back_codex_in_its_discussion() {
    let fixture = DeliveryFixture::new().await;
    let mut claude_request = DeliveryFixture::request(42, 10, Agent::Claude);
    claude_request.new_chat = true;
    let codex_request = DeliveryFixture::request(42, 11, Agent::Codex);
    fixture.bind_to_codex(&codex_request, "codex-chat").await;
    fixture.insert(&[&claude_request, &codex_request]).await;
    let claude = Arc::new(Pauses::new([()]));
    let (worker, mut deliveries) =
        fixture.route(&claude, &Arc::new(Pauses::new([])), &Arc::default());
    assert_eq!(
        DeliveryFixture::next_delivery(&mut deliveries).await,
        (Agent::Codex, "codex-chat".into(), codex_request.key.clone())
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), deliveries.recv())
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .runtime
            .store
            .delivery_state(&claude_request.key)
            .await
            .expect("state"),
        Some(DeliveryState::Pending)
    );
    claude.resume(&());
    assert_eq!(
        DeliveryFixture::next_delivery(&mut deliveries).await,
        (
            Agent::Claude,
            "claude-chat-1".into(),
            claude_request.key.clone()
        )
    );
    assert_eq!(
        fixture.delivered_to(&codex_request).await,
        ("codex".into(), "codex-chat".into())
    );
    assert_eq!(
        fixture.delivered_to(&claude_request).await,
        ("claude".into(), "claude-chat-1".into())
    );
    worker.abort();
    assert!(worker.await.expect_err("cancelled").is_cancelled());
}

#[tokio::test]
async fn alternating_agents_on_a_discussion_reuse_each_agents_chat() {
    let fixture = DeliveryFixture::new().await;
    let mut first_claude = DeliveryFixture::request(42, 10, Agent::Claude);
    first_claude.new_chat = true;
    let first_codex = DeliveryFixture::request(42, 11, Agent::Codex);
    let second_claude = DeliveryFixture::request(42, 12, Agent::Claude);
    let second_codex = DeliveryFixture::request(42, 13, Agent::Codex);
    fixture.bind_to_codex(&first_claude, "codex-chat").await;
    fixture.insert(&[&first_claude, &first_codex]).await;
    let running = Arc::new(Pauses::new([]));
    let (worker, mut deliveries) = fixture.route(&running, &running, &Arc::default());
    let mut delivered = Vec::new();
    for _ in 0..2 {
        delivered.push(DeliveryFixture::next_delivery(&mut deliveries).await);
    }
    fixture.insert(&[&second_claude, &second_codex]).await;
    for request in [&second_claude, &second_codex] {
        fixture
            .runtime
            .enqueue_github_event(request.key.clone(), request.options.agent);
    }
    for _ in 0..2 {
        delivered.push(DeliveryFixture::next_delivery(&mut deliveries).await);
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), deliveries.recv())
            .await
            .is_err()
    );
    let delivered_by = |agent: Agent| -> Vec<(String, EventKey)> {
        delivered
            .iter()
            .filter(|(delivering_agent, _, _)| *delivering_agent == agent)
            .map(|(_, chat_id, key)| (chat_id.clone(), key.clone()))
            .collect()
    };
    assert_eq!(
        delivered_by(Agent::Claude),
        [
            ("claude-chat-1".to_owned(), first_claude.key.clone()),
            ("claude-chat-1".to_owned(), second_claude.key.clone())
        ]
    );
    assert_eq!(
        delivered_by(Agent::Codex),
        [
            ("codex-chat".to_owned(), first_codex.key.clone()),
            ("codex-chat".to_owned(), second_codex.key.clone())
        ]
    );
    for (request, chat) in [
        (&first_claude, ("claude", "claude-chat-1")),
        (&first_codex, ("codex", "codex-chat")),
        (&second_claude, ("claude", "claude-chat-1")),
        (&second_codex, ("codex", "codex-chat")),
    ] {
        assert_eq!(
            fixture.delivered_to(request).await,
            (chat.0.to_owned(), chat.1.to_owned())
        );
    }
    worker.abort();
    assert!(worker.await.expect_err("cancelled").is_cancelled());
}

#[tokio::test]
async fn store_work_in_the_routing_loop_keeps_routes_moving() {
    use super::super::account::RoutesExt;
    use futures_util::stream::FuturesUnordered;

    let (route_ran, work_waits) = tokio::sync::oneshot::channel();
    let routes = FuturesUnordered::new();
    routes.push(async move {
        route_ran.send(()).expect("work is waiting");
        "route"
    });
    let mut routes = routes;
    let mut finished = std::collections::VecDeque::new();
    let work = async {
        work_waits.await.expect("route ran");
        "work"
    };
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), routes.draining(work, &mut finished))
            .await
            .expect("work finishes while routes run"),
        "work"
    );
    assert_eq!(finished, ["route"]);
}

#[tokio::test]
async fn a_full_claude_budget_does_not_hold_back_codex() {
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
    let senders = AgentSenders {
        claude: Arc::new(ResumingSender::default()),
        codex: Arc::new(ResumingSender::default()),
    };
    let incoming = runtime
        .github_routing_receiver
        .lock()
        .await
        .take()
        .expect("receiver");
    let gate = Arc::new(tokio::sync::Notify::new());
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let running = Arc::clone(&runtime);
    let operation_gate = Arc::clone(&gate);
    let worker = tokio::spawn(async move {
        running
            .process_github_events("github.com", &senders, incoming, |key| {
                let started = started.clone();
                let gate = Arc::clone(&operation_gate);
                async move {
                    let released = gate.notified();
                    tokio::pin!(released);
                    released.as_mut().enable();
                    started.send(key.id.clone()).expect("receiver");
                    if key.id.starts_with("claude") {
                        released.await;
                    }
                    (key, Ok(Admission::Accepted))
                }
            })
            .await;
    });
    let request = |identifier: String| EventKey {
        conversation: ConversationKey {
            source: "github:github.com".into(),
            subject: format!("1/{identifier}"),
        },
        id: identifier,
    };
    for number in 0..9 {
        runtime.enqueue_github_event(request(format!("claude-{number}")), Agent::Claude);
    }
    runtime.enqueue_github_event(request("codex".into()), Agent::Codex);
    let mut first = Vec::new();
    for _ in 0..9 {
        first.push(
            tokio::time::timeout(Duration::from_secs(2), starts.recv())
                .await
                .expect("request starts")
                .expect("worker"),
        );
    }
    first.sort();
    let mut expected: Vec<String> = (0..8).map(|number| format!("claude-{number}")).collect();
    expected.push("codex".into());
    expected.sort();
    assert_eq!(first, expected);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), starts.recv())
            .await
            .is_err()
    );
    gate.notify_waiters();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .expect("a freed Claude slot starts the next Claude request")
            .expect("worker"),
        "claude-8"
    );
    worker.abort();
    assert!(worker.await.expect_err("cancelled").is_cancelled());
}
