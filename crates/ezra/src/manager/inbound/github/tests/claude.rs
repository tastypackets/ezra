use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use ezra::agent::Agent;
use ezra::inbound::Shortcut;
use ezra::inbound::store::SessionTarget;

use super::*;
use crate::logging::CapturedLogs;
use crate::manager::api::test_support::TestManager;
use crate::manager::claude_remote::ClaudeRemote;
use crate::manager::claude_remote::fake::{FakeSessionsApi, Reply};
use crate::manager::folders::ProjectsDirectory;
use crate::manager::inbound::AgentSenders;
use crate::manager::remote_control::{RemoteControlStatus, ServerState};

/// Takes every Codex request without a native Codex.
struct CodexTaker;

impl MessageSender for CodexTaker {
    async fn create_chat(
        &self,
        _workspace: &str,
        _chat_name: Option<&str>,
        _options: &Shortcut,
    ) -> Result<String, MessageSendError> {
        Ok("codex-chat".to_owned())
    }

    async fn queue_message(
        &self,
        _chat_id: &str,
        event: &InboundEvent,
    ) -> Result<MessageReceipt, MessageSendError> {
        Ok(MessageReceipt {
            chat_name: None,
            native_message_id: event.key.id.clone(),
            delivery_id: event.key.delivery_id(),
        })
    }
}

/// The routing loop with a real Claude sender, Remote Control statuses, store and log, against
/// the fake sessions API and a fake `claude` whose follow-ups are queued.
struct ClaudeRouting {
    manager: TestManager,
    fake: FakeSessionsApi,
    database: tempfile::TempDir,
    runtime: Arc<InboundRuntime>,
    statuses: tokio::sync::mpsc::Receiver<GitHubFeedback>,
    seen: HashMap<EventKey, CommentStatus>,
    worker: tokio::task::JoinHandle<()>,
}

impl ClaudeRouting {
    async fn new() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let manager = TestManager::new();
        manager.install_fake_cli(
            Agent::Claude,
            "case \"$1\" in\n  -p) cat > /dev/null; echo '{\"ok\":true}' ;;\nesac",
        );
        let config = manager
            .state
            .install_paths
            .config_directory(Agent::Claude)
            .expect("Claude has a config directory")
            .to_path_buf();
        fs::create_dir_all(&config).expect("config directory is created");
        let fake = FakeSessionsApi::start().await;
        let database = tempfile::tempdir().expect("temporary directory");
        let runtime = Arc::new(
            InboundRuntime::open(
                &database.path().join("ezra.db"),
                Arc::new(tokio::sync::Mutex::new(
                    crate::manager::settings::Settings::default(),
                )),
            )
            .await
            .expect("runtime"),
        );
        let (status_sender, statuses) = tokio::sync::mpsc::channel(32);
        *runtime.github_feedback_sender.lock().await = Some(status_sender);
        let senders = AgentSenders {
            claude: Arc::new(
                ClaudeRemote::new(
                    Arc::clone(&manager.state.remote_control),
                    Arc::clone(&manager.state.install_paths),
                    manager.state.projects.clone(),
                )
                .with_api(fake.base.clone(), Duration::from_secs(20)),
            ),
            codex: Arc::new(CodexTaker),
        };
        let tools = crate::manager::git::GitTools::under(database.path());
        let projects = ProjectsDirectory(manager.state.projects.0.clone());
        let running = Arc::clone(&runtime);
        let worker = tokio::spawn(async move {
            running.route_github(&tools, &projects, &senders).await;
        });
        let routing = Self {
            manager,
            fake,
            database,
            runtime,
            statuses,
            seen: HashMap::new(),
            worker,
        };
        routing.sign_in();
        for name in ["a", "b"] {
            routing.check_out(name);
        }
        routing.connect(&routing.projects(), "env_projects");
        routing
    }

    fn sign_in(&self) {
        fs::write(
            self.credentials(),
            r#"{"claudeAiOauth":{"accessToken":"test-access-token","scopes":["user:sessions:claude_code"]}}"#,
        )
        .expect("credentials are written");
    }

    /// Another connection to the routing loop's database.
    async fn database(&self) -> sqlx::SqlitePool {
        sqlx::SqlitePool::connect(&format!(
            "sqlite:{}",
            self.database.path().join("ezra.db").display()
        ))
        .await
        .expect("the database connects")
    }

    fn credentials(&self) -> PathBuf {
        self.manager
            .state
            .install_paths
            .config_directory(Agent::Claude)
            .expect("Claude has a config directory")
            .join(".credentials.json")
    }

    /// A served folder holding the checkout of `owner/<name>`.
    fn check_out(&self, name: &str) {
        let git = self.folder(name).join(".git");
        fs::create_dir_all(&git).expect("checkout is created");
        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD is written");
        fs::write(
            git.join("config"),
            format!("[remote \"origin\"]\n\turl = https://github.com/owner/{name}.git\n"),
        )
        .expect("config is written");
    }

    fn projects(&self) -> PathBuf {
        self.manager.state.projects.0.clone()
    }

    fn folder(&self, name: &str) -> PathBuf {
        self.manager.state.projects.folder(name)
    }

    fn show(&self, directory: &Path, status: RemoteControlStatus) {
        self.manager
            .state
            .remote_control
            .show(directory, Some(status));
    }

    /// Shows `status` paused, as the supervisor does when it stops or holds a server on purpose.
    fn pause(&self, directory: &Path, status: RemoteControlStatus) {
        self.manager
            .state
            .remote_control
            .pauses
            .pause(directory.to_path_buf());
        self.show(directory, status);
    }

    /// Shows `status` and resumes, as the supervisor does when a server fails or connects.
    fn resume(&self, directory: &Path, status: RemoteControlStatus) {
        self.show(directory, status);
        self.manager
            .state
            .remote_control
            .pauses
            .resume(&directory.to_path_buf());
    }

    fn connect(&self, directory: &Path, environment: &str) {
        self.resume(
            directory,
            RemoteControlStatus {
                state: ServerState::Running,
                url: Some(format!("https://claude.ai/code?environment={environment}")),
                ..RemoteControlStatus::default()
            },
        );
    }

    /// A request for `agent` in a new chat on issue `number` of `owner/<repository>`, from
    /// comment `10<number>`.
    fn request(repository: &str, number: u64, agent: Agent) -> InboundEvent {
        InboundEvent {
            key: EventKey {
                conversation: ConversationKey {
                    source: "github:github.com".into(),
                    subject: format!("7/{number}"),
                },
                id: format!("10{number}"),
            },
            new_chat: true,
            options: Shortcut {
                agent,
                ..Shortcut::default()
            },
            chat_name: Some(format!("owner/{repository}#{number}")),
            source_url: Some(format!(
                "https://github.com/owner/{repository}/issues/{number}#issuecomment-10{number}"
            )),
            actor: "1".into(),
            created_at: OffsetDateTime::now_utc(),
            message: "Fix it".into(),
            initial_context: None,
        }
    }

    async fn send(&self, event: &InboundEvent) {
        self.runtime
            .store
            .insert(event, InboundSettings::default().queue_limits())
            .await
            .expect("request inserts");
        self.runtime
            .enqueue_github_event(event.key.clone(), event.options.agent);
    }

    /// The status the routing loop reported for `event`.
    async fn status(&mut self, event: &InboundEvent) -> CommentStatus {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = self.seen.remove(&event.key) {
                    return status;
                }
                let feedback = self.statuses.recv().await.expect("feedback sender");
                self.seen.insert(feedback.key, feedback.status);
            }
        })
        .await
        .expect("the request is decided")
    }

    async fn state(&self, event: &InboundEvent) -> Option<DeliveryState> {
        self.runtime
            .store
            .delivery_state(&event.key)
            .await
            .expect("state")
    }

    fn created_environments(&self) -> Vec<String> {
        self.fake
            .seen()
            .into_iter()
            .filter(|request| request.route() == "POST /v1/code/sessions")
            .map(|request| {
                request.body["environment_id"]
                    .as_str()
                    .expect("an environment")
                    .to_owned()
            })
            .collect()
    }
}

impl Drop for ClaudeRouting {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

trait LoggedExt {
    /// The first line about `event` that contains `text`, once it is logged.
    async fn logged(&self, event: &InboundEvent, text: &str) -> String;
}

impl LoggedExt for CapturedLogs {
    async fn logged(&self, event: &InboundEvent, text: &str) -> String {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(line) = self
                    .lines_with(&event.key.delivery_id())
                    .into_iter()
                    .find(|line| line.contains(text))
                {
                    return line;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "no line about {:?} says {text}:\n{}",
                event.key,
                self.text()
            )
        })
    }
}

#[tokio::test]
async fn a_claude_request_waits_while_its_folder_server_starts_and_goes_out_when_it_connects() {
    let (logs, _capture) = CapturedLogs::start("warn,ezra::manager::inbound=debug");
    let mut routing = ClaudeRouting::new().await;
    routing.connect(&routing.folder("b"), "env_b");
    routing.pause(
        &routing.folder("a"),
        RemoteControlStatus {
            state: ServerState::Starting,
            ..RemoteControlStatus::default()
        },
    );
    routing.fake.reply(
        "POST /v1/code/sessions",
        [
            Reply::new(200, json!({"id": "cse_01B"})),
            Reply::new(200, json!({"id": "cse_01A"})),
        ],
    );
    for (session, environment) in [("cse_01A", "env_a"), ("cse_01B", "env_b")] {
        routing.fake.reply(
            &format!("GET /v1/code/sessions/{session}"),
            [Reply::new(
                200,
                json!({"id": session, "environment_id": environment}),
            )],
        );
    }
    let waiting = ClaudeRouting::request("a", 1, Agent::Claude);
    let other_folder = ClaudeRouting::request("b", 2, Agent::Claude);
    let codex = ClaudeRouting::request("a", 3, Agent::Codex);

    routing.send(&waiting).await;
    let parked = logs
        .logged(
            &waiting,
            "GitHub request waits while its destination is paused",
        )
        .await;
    assert!(
        parked.contains(&format!(
            "Claude Remote Control in {} is starting",
            routing.folder("a").display()
        )),
        "{parked}"
    );
    routing.send(&other_folder).await;
    routing.send(&codex).await;
    assert_eq!(
        routing.status(&other_folder).await,
        CommentStatus::Delivered
    );
    assert_eq!(routing.status(&codex).await, CommentStatus::Delivered);
    assert_eq!(routing.state(&waiting).await, Some(DeliveryState::Pending));
    assert!(routing.seen.is_empty() && routing.statuses.try_recv().is_err());
    assert_eq!(routing.created_environments(), ["env_b"]);

    routing.connect(&routing.folder("a"), "env_a");
    assert_eq!(routing.status(&waiting).await, CommentStatus::Delivered);
    assert_eq!(routing.created_environments(), ["env_b", "env_a"]);
    assert_eq!(
        routing.manager.fake_cli_runs(Agent::Claude),
        [
            "-p --cloud cse_01B --output-format json",
            "-p --cloud cse_01A --output-format json"
        ]
    );
    for request in [&waiting, &other_folder, &codex] {
        let lines = logs.lines_with(&request.key.delivery_id());
        assert!(lines.iter().all(|line| !line.contains("WARN")), "{lines:?}");
    }
}

#[tokio::test]
async fn a_claude_request_waiting_for_a_restart_fails_when_the_server_stops_unexpectedly() {
    let (logs, _capture) = CapturedLogs::start("warn,ezra::manager::inbound=debug");
    let mut routing = ClaudeRouting::new().await;
    let folder = routing.folder("a");
    routing.pause(
        &folder,
        RemoteControlStatus {
            state: ServerState::Starting,
            ..RemoteControlStatus::default()
        },
    );
    let request = ClaudeRouting::request("a", 1, Agent::Claude);
    routing.send(&request).await;
    logs.logged(
        &request,
        "GitHub request waits while its destination is paused",
    )
    .await;

    routing.resume(
        &folder,
        RemoteControlStatus {
            state: ServerState::Retrying,
            last_error: Some("did not connect within 2 minutes".to_owned()),
            ..RemoteControlStatus::default()
        },
    );
    assert_eq!(routing.status(&request).await, CommentStatus::Failed);
    assert_eq!(routing.state(&request).await, Some(DeliveryState::Failed));
    let warnings: Vec<_> = logs
        .lines_with(&request.key.delivery_id())
        .into_iter()
        .filter(|line| line.contains("WARN"))
        .collect();
    let [logged] = <[String; 1]>::try_from(warnings).expect("the failure is logged once");
    for part in [
        "GitHub request failed before submission",
        "agent=claude",
        &format!(
            "Claude Remote Control in {} stopped unexpectedly",
            folder.display()
        ),
    ] {
        assert!(logged.contains(part), "{logged}");
    }
    assert!(routing.fake.seen().is_empty());
}

#[tokio::test]
async fn with_no_server_connected_a_claude_request_waits_only_while_its_server_is_down_on_purpose()
{
    let (logs, _capture) = CapturedLogs::start("warn,ezra::manager::inbound=debug");
    let mut routing = ClaudeRouting::new().await;
    let projects = routing.projects();
    let in_projects =
        |state: &str| format!("Claude Remote Control in {} {state}", projects.display());
    routing.manager.state.remote_control.show(&projects, None);
    routing
        .manager
        .state
        .remote_control
        .pauses
        .pause(projects.clone());
    let starting = ClaudeRouting::request("a", 1, Agent::Claude);
    routing.send(&starting).await;
    let parked = logs
        .logged(
            &starting,
            "GitHub request waits while its destination is paused",
        )
        .await;
    assert!(parked.contains(&in_projects("has not started")), "{parked}");

    routing.pause(
        &projects,
        RemoteControlStatus {
            state: ServerState::Off,
            ..RemoteControlStatus::default()
        },
    );
    let turned_off = ClaudeRouting::request("a", 2, Agent::Claude);
    routing.send(&turned_off).await;
    let parked = logs
        .logged(
            &turned_off,
            "GitHub request waits while its destination is paused",
        )
        .await;
    assert!(parked.contains(&in_projects("is turned off")), "{parked}");
    assert_eq!(routing.state(&starting).await, Some(DeliveryState::Pending));
    assert_eq!(
        routing.state(&turned_off).await,
        Some(DeliveryState::Pending)
    );

    routing.fake.reply(
        "POST /v1/code/sessions",
        [
            Reply::new(200, json!({"id": "cse_01A"})),
            Reply::new(200, json!({"id": "cse_01B"})),
        ],
    );
    for session in ["cse_01A", "cse_01B"] {
        routing.fake.reply(
            &format!("GET /v1/code/sessions/{session}"),
            [Reply::new(
                200,
                json!({"id": session, "environment_id": "env_projects"}),
            )],
        );
    }
    routing.connect(&projects, "env_projects");
    assert_eq!(routing.status(&starting).await, CommentStatus::Delivered);
    assert_eq!(routing.status(&turned_off).await, CommentStatus::Delivered);
    assert_eq!(
        routing.created_environments(),
        ["env_projects", "env_projects"]
    );
    for request in [&starting, &turned_off] {
        let lines = logs.lines_with(&request.key.delivery_id());
        assert!(lines.iter().all(|line| !line.contains("WARN")), "{lines:?}");
    }
}

#[tokio::test]
async fn with_no_server_connected_a_claude_request_fails_right_away_when_its_server_crashed() {
    let (logs, _capture) = CapturedLogs::start("warn,ezra::manager::inbound=debug");
    let mut routing = ClaudeRouting::new().await;
    let projects = routing.projects();
    let in_projects =
        |state: &str| format!("Claude Remote Control in {} {state}", projects.display());
    let crashed = |state| RemoteControlStatus {
        state,
        last_error: Some("exit status: 1".to_owned()),
        ..RemoteControlStatus::default()
    };
    for (number, (status, reason)) in (1..).zip([
        (
            crashed(ServerState::Retrying),
            in_projects("stopped unexpectedly"),
        ),
        (
            crashed(ServerState::Starting),
            in_projects("has not connected since it stopped unexpectedly"),
        ),
    ]) {
        routing.show(&projects, status);
        let request = ClaudeRouting::request("a", number, Agent::Claude);
        routing.send(&request).await;
        assert_eq!(routing.status(&request).await, CommentStatus::Failed);
        assert_eq!(routing.state(&request).await, Some(DeliveryState::Failed));
        let logged = logs
            .logged(&request, "GitHub request failed before submission")
            .await;
        for part in ["WARN", "agent=claude", &reason] {
            assert!(logged.contains(part), "{logged}");
        }
    }

    routing.pause(
        &projects,
        RemoteControlStatus {
            state: ServerState::Stopping,
            ..RemoteControlStatus::default()
        },
    );
    let restarting = ClaudeRouting::request("a", 3, Agent::Claude);
    routing.send(&restarting).await;
    logs.logged(
        &restarting,
        "GitHub request waits while its destination is paused",
    )
    .await;
    routing.resume(&projects, crashed(ServerState::Retrying));
    assert_eq!(routing.status(&restarting).await, CommentStatus::Failed);
    let logged = logs
        .logged(&restarting, "GitHub request failed before submission")
        .await;
    assert!(
        logged.contains(&in_projects("stopped unexpectedly")),
        "{logged}"
    );
    assert!(routing.fake.seen().is_empty());
}

#[tokio::test]
async fn a_claude_request_fails_right_away_and_says_why_when_its_server_cannot_take_it() {
    let (logs, _capture) = CapturedLogs::start("warn");
    let mut routing = ClaudeRouting::new().await;
    let folder = routing.folder("a");
    let in_folder = |state: &str| format!("Claude Remote Control in {} {state}", folder.display());
    let crashed = |state| RemoteControlStatus {
        state,
        last_error: Some("exit status: 1".to_owned()),
        ..RemoteControlStatus::default()
    };
    let mut requests = Vec::new();
    for (number, (status, reason)) in (1..).zip([
        (
            crashed(ServerState::Retrying),
            in_folder("stopped unexpectedly"),
        ),
        (
            crashed(ServerState::Starting),
            in_folder("has not connected since it stopped unexpectedly"),
        ),
        (
            RemoteControlStatus {
                state: ServerState::Waiting,
                ..RemoteControlStatus::default()
            },
            in_folder("waits for Claude Code to be installed and signed in"),
        ),
        (
            RemoteControlStatus {
                state: ServerState::Running,
                url: Some("https://claude.ai/code/session_01CD".to_owned()),
                ..RemoteControlStatus::default()
            },
            in_folder(
                "shows a session link instead of an environment, as it does with Sessions per folder 1",
            ),
        ),
    ]) {
        routing.show(&folder, status);
        let request = ClaudeRouting::request("a", number, Agent::Claude);
        routing.send(&request).await;
        assert_eq!(routing.status(&request).await, CommentStatus::Failed);
        assert_eq!(routing.state(&request).await, Some(DeliveryState::Failed));
        let logged = logs
            .logged(&request, "GitHub request failed before submission")
            .await;
        for part in ["WARN", "agent=claude", &reason] {
            assert!(logged.contains(part), "{logged}");
        }
        requests.push(request);
    }

    routing.connect(&folder, "env_a");
    fs::remove_file(routing.credentials()).expect("credentials are removed");
    let signed_out = ClaudeRouting::request("a", 5, Agent::Claude);
    routing.send(&signed_out).await;
    assert_eq!(routing.status(&signed_out).await, CommentStatus::Failed);
    let logged = logs
        .logged(&signed_out, "GitHub request failed before submission")
        .await;
    for part in [
        "agent=claude",
        "could not read Claude Code's credentials",
        "sign Claude Code in again with claude auth login",
    ] {
        assert!(logged.contains(part), "{logged}");
    }
    requests.push(signed_out);

    routing.sign_in();
    routing.fake.reply(
        "POST /v1/code/sessions",
        [Reply::new(503, json!({})), Reply::new(503, json!({}))],
    );
    let refused = ClaudeRouting::request("a", 6, Agent::Claude);
    routing.send(&refused).await;
    assert_eq!(routing.status(&refused).await, CommentStatus::Failed);
    let logged = logs
        .logged(&refused, "GitHub chat creation failed before submission")
        .await;
    for part in ["agent=claude", "Claude answered 503: Service Unavailable"] {
        assert!(logged.contains(part), "{logged}");
    }
    requests.push(refused);

    let mut replaced = ClaudeRouting::request("a", 7, Agent::Claude);
    replaced.new_chat = false;
    routing
        .runtime
        .store
        .bind_conversation(
            &replaced.key.conversation,
            &SessionTarget {
                host_id: routing.runtime.host_id.clone(),
                agent: "claude".into(),
                chat_id: "cse_01OLD".into(),
                workspace: folder.to_str().expect("a UTF-8 path").into(),
            },
        )
        .await
        .expect("the discussion is bound");
    routing.fake.reply(
        "GET /v1/code/sessions/cse_01OLD",
        [Reply::new(
            200,
            json!({"id": "cse_01OLD", "status": "archived", "environment_id": "env_a"}),
        )],
    );
    routing.send(&replaced).await;
    assert_eq!(routing.status(&replaced).await, CommentStatus::Failed);
    let logged = logs
        .logged(&replaced, "inbound request failed before submission")
        .await;
    for part in [
        "agent=claude",
        "delivery_id=",
        "Claude answered 503: Service Unavailable",
    ] {
        assert!(logged.contains(part), "{logged}");
    }
    requests.push(replaced);

    let asked = routing.fake.seen().len();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(routing.fake.seen().len(), asked, "nothing is tried again");
    assert!(routing.manager.fake_cli_runs(Agent::Claude).is_empty());
    for request in &requests {
        let warnings: Vec<_> = logs
            .lines_with(&request.key.delivery_id())
            .into_iter()
            .filter(|line| line.contains("WARN"))
            .collect();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }
}

#[tokio::test]
async fn a_store_error_fails_the_request_and_is_logged_with_it_on_the_routing_and_dispatch_paths() {
    const REFUSAL: &str = "conversation agent differs from its session";
    let (logs, _capture) = CapturedLogs::start("warn");
    let mut routing = ClaudeRouting::new().await;
    let folder = routing.folder("a");
    routing.connect(&folder, "env_a");
    let database = routing.database().await;
    routing.fake.reply(
        "POST /v1/code/sessions",
        [
            Reply::new(200, json!({"id": "cse_01NEW"})),
            Reply::new(200, json!({"id": "cse_01NEXT"})),
        ],
    );

    sqlx::query(
        "CREATE TRIGGER refuse_conversation BEFORE INSERT ON inbound_conversations
         BEGIN SELECT RAISE(ABORT, 'conversation agent differs from its session'); END",
    )
    .execute(&database)
    .await
    .expect("the trigger is created");
    let routed = ClaudeRouting::request("a", 1, Agent::Claude);
    routing.send(&routed).await;
    assert_eq!(routing.status(&routed).await, CommentStatus::Failed);
    assert_eq!(routing.state(&routed).await, Some(DeliveryState::Failed));
    let logged = logs
        .logged(&routed, "GitHub request failed before submission")
        .await;
    for part in ["WARN", "agent=claude", REFUSAL] {
        assert!(logged.contains(part), "{logged}");
    }
    sqlx::query("DROP TRIGGER refuse_conversation")
        .execute(&database)
        .await
        .expect("the trigger is dropped");

    let mut replaced = ClaudeRouting::request("a", 2, Agent::Claude);
    replaced.new_chat = false;
    routing
        .runtime
        .store
        .bind_conversation(
            &replaced.key.conversation,
            &SessionTarget {
                host_id: routing.runtime.host_id.clone(),
                agent: "claude".into(),
                chat_id: "cse_01OLD".into(),
                workspace: folder.to_str().expect("a UTF-8 path").into(),
            },
        )
        .await
        .expect("the discussion is bound");
    routing.fake.reply(
        "GET /v1/code/sessions/cse_01OLD",
        [Reply::new(
            200,
            json!({"id": "cse_01OLD", "status": "archived", "environment_id": "env_a"}),
        )],
    );
    sqlx::query(
        "CREATE TRIGGER refuse_replacement BEFORE UPDATE OF chat_id ON inbound_sessions
         BEGIN SELECT RAISE(ABORT, 'conversation agent differs from its session'); END",
    )
    .execute(&database)
    .await
    .expect("the trigger is created");
    routing.send(&replaced).await;
    assert_eq!(routing.status(&replaced).await, CommentStatus::Failed);
    assert_eq!(routing.state(&replaced).await, Some(DeliveryState::Failed));
    let logged = logs
        .logged(&replaced, "GitHub request failed before submission")
        .await;
    for part in ["WARN", "agent=claude", REFUSAL] {
        assert!(logged.contains(part), "{logged}");
    }

    assert_eq!(routing.created_environments(), ["env_a", "env_a"]);
    assert!(routing.manager.fake_cli_runs(Agent::Claude).is_empty());
    for request in [&routed, &replaced] {
        let warnings: Vec<_> = logs
            .lines_with(&request.key.delivery_id())
            .into_iter()
            .filter(|line| line.contains("WARN"))
            .collect();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }
}
