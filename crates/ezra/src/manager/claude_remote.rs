mod credentials;
#[cfg(test)]
mod fake;
#[path = "claude_remote/follow-up.rs"]
mod follow_up;
mod models;
#[path = "claude_remote/sessions-api.rs"]
mod sessions_api;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex as SyncMutex, PoisonError, Weak};
use std::time::Duration;

use ezra::inbound::{
    InboundEvent, MessageAttempt, MessageReceipt, MessageSendError, MessageSender, Shortcut,
    UntrackedAttempt,
};
use reqwest::Url;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, watch};

use super::agents::{Agent, InstallPaths};
use super::folders::ProjectsDirectory;
use super::login::AgentCli;
use super::remote_control::{RemoteControl, RemoteControlStatus};
use credentials::{ClaudeLogin, LoginProblem};
use follow_up::{FollowUp, Refusal};
use sessions_api::{API_BASE, ApiError, NewSession, SessionId, SessionRecord, SessionsApi};

const FOLLOW_UP_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_TITLE: &str = "ezra";

/// Sends inbound requests to Claude Code sessions in the environments of ezra's Remote Control
/// servers.
pub struct ClaudeRemote {
    remote_control: Arc<RemoteControl>,
    install_paths: Arc<InstallPaths>,
    projects: ProjectsDirectory,
    api: SessionsApi,
    chats: ChatLocks,
    follow_up_within: Duration,
}

/// A call to the sessions API, made again with a token read anew when Claude rejects the first.
#[derive(Clone, Copy)]
enum SessionRequest<'a> {
    Create(&'a NewSession<'a>),
    Get(&'a SessionId),
}

#[derive(Debug, thiserror::Error)]
enum SessionCallError {
    #[error(transparent)]
    SignIn(#[from] LoginProblem),
    #[error(transparent)]
    Api(#[from] ApiError),
}

/// One lock per session, so that the messages for a session go out in order.
#[derive(Debug, Default)]
struct ChatLocks(SyncMutex<HashMap<String, Weak<AsyncMutex<()>>>>);

impl ClaudeRemote {
    pub fn new(
        remote_control: Arc<RemoteControl>,
        install_paths: Arc<InstallPaths>,
        projects: ProjectsDirectory,
    ) -> Self {
        Self {
            remote_control,
            install_paths,
            projects,
            api: SessionsApi::new(Url::parse(API_BASE).expect("the API base is a URL")),
            chats: ChatLocks::default(),
            follow_up_within: FOLLOW_UP_TIMEOUT,
        }
    }

    /// Talks to the sessions API at `base` and gives the follow-up command `follow_up_within`.
    #[cfg(test)]
    pub fn with_api(self, base: Url, follow_up_within: Duration) -> Self {
        Self {
            api: SessionsApi::new(base),
            follow_up_within,
            ..self
        }
    }

    /// The environment of the server for `workspace` when it has one, otherwise of the projects
    /// server. Unavailable while that server is not connected.
    fn environment_for(&self, workspace: &str) -> Result<String, MessageSendError> {
        self.remote_control
            .status_of(Path::new(workspace))
            .or_else(|| self.remote_control.status_of(&self.projects.0))
            .as_ref()
            .and_then(RemoteControlStatus::connected_environment)
            .map(str::to_owned)
            .ok_or(MessageSendError::Unavailable)
    }

    async fn ask(&self, request: SessionRequest<'_>) -> Result<SessionRecord, SessionCallError> {
        let login = ClaudeLogin::locate(
            self.install_paths.config_directory(Agent::Claude),
            self.install_paths.home(),
        );
        let mut rejected = false;
        loop {
            let token = login.access_token().await?;
            let answer = match request {
                SessionRequest::Create(session) => self.api.create(&token, session).await,
                SessionRequest::Get(id) => self.api.get(&token, id).await,
            };
            match answer {
                Err(ApiError::Unauthorized) if !rejected => rejected = true,
                answer => return Ok(answer?),
            }
        }
    }

    /// The session's record, or `None` when it cannot be read and the follow-up command decides.
    /// The command uses the same sign-in, so one the API rejects makes the sender unavailable.
    async fn check(&self, session: &SessionId) -> Result<Option<SessionRecord>, MessageSendError> {
        let record = match self.ask(SessionRequest::Get(session)).await {
            Ok(record) => record,
            Err(
                error @ (SessionCallError::SignIn(_)
                | SessionCallError::Api(ApiError::Unauthorized)),
            ) => return Err(error.into_send_error()),
            Err(error) => {
                tracing::warn!(%error, "could not read the Claude session, sending without checking it");
                return Ok(None);
            }
        };
        if record.is_archived() {
            return Err(MessageSendError::NeedsReplacement(
                "the Claude session is archived".to_owned(),
            ));
        }
        if let Some(environment) = record.environment_id.as_deref()
            && self.remote_control.serving(environment).is_none()
        {
            return Err(MessageSendError::NeedsReplacement(
                "the Claude session is in an environment no server here is connected in".to_owned(),
            ));
        }
        Ok(Some(record))
    }
}

impl SessionCallError {
    fn into_send_error(self) -> MessageSendError {
        match self {
            Self::SignIn(_) | Self::Api(ApiError::Unauthorized) => {
                tracing::warn!(error = %self, "Claude Code's sign-in cannot reach sessions, sign it in again with claude auth login");
                MessageSendError::Unavailable
            }
            Self::Api(error) => MessageSendError::Uncertain(error.to_string()),
        }
    }
}

impl ChatLocks {
    async fn hold(&self, session: &SessionId) -> OwnedMutexGuard<()> {
        let chat_lock = {
            let mut locks = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            locks.retain(|_, chat_lock| chat_lock.strong_count() > 0);
            locks
                .get(session.suffix())
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| {
                    let chat_lock = Arc::new(AsyncMutex::new(()));
                    locks.insert(session.suffix().to_owned(), Arc::downgrade(&chat_lock));
                    chat_lock
                })
        };
        chat_lock.lock_owned().await
    }
}

impl MessageSender for ClaudeRemote {
    fn control_available(&self) -> bool {
        self.remote_control.has_environment()
    }

    fn control_changes(&self) -> Option<watch::Receiver<bool>> {
        Some(self.remote_control.environment_changes())
    }

    async fn create_chat(
        &self,
        workspace: &str,
        chat_name: Option<&str>,
        options: &Shortcut,
    ) -> Result<String, MessageSendError> {
        let environment = self.environment_for(workspace)?;
        let session = NewSession::new(
            chat_name.unwrap_or(DEFAULT_TITLE),
            &environment,
            options.model.as_deref(),
            options.effort.as_deref(),
        );
        let record = self
            .ask(SessionRequest::Create(&session))
            .await
            .map_err(SessionCallError::into_send_error)?;
        let id = record
            .id
            .as_deref()
            .and_then(SessionId::parse)
            .ok_or_else(|| {
                MessageSendError::Uncertain(
                    "Claude created a session without a usable id".to_owned(),
                )
            })?;
        tracing::info!(
            session = id.as_str(),
            "Claude session created for an inbound request"
        );
        Ok(id.as_str().to_owned())
    }

    async fn queue_message(
        &self,
        chat_id: &str,
        event: &InboundEvent,
    ) -> Result<MessageReceipt, MessageSendError> {
        self.queue_message_tracked(chat_id, event, &UntrackedAttempt)
            .await
    }

    async fn queue_message_tracked(
        &self,
        chat_id: &str,
        event: &InboundEvent,
        attempt: &(impl MessageAttempt + Sync),
    ) -> Result<MessageReceipt, MessageSendError> {
        if !self.control_available() {
            return Err(MessageSendError::Unavailable);
        }
        let session = SessionId::parse(chat_id).ok_or_else(|| {
            MessageSendError::NeedsReplacement("the chat is not a Claude session id".to_owned())
        })?;
        let cli = AgentCli::installed(Agent::Claude, &self.install_paths)
            .map_err(|_| MessageSendError::Unavailable)?;
        let _chat_hold = self.chats.hold(&session).await;
        let record = self.check(&session).await?;
        let directory = record
            .as_ref()
            .and_then(|record| record.environment_id.as_deref())
            .and_then(|environment| self.remote_control.serving(environment))
            .unwrap_or_else(|| self.projects.0.clone());
        attempt.mark_attempted(&event.key).await?;
        let follow_up = FollowUp {
            session: &session,
            message: &event.message,
            directory: &directory,
        };
        let result = match follow_up.run(&cli, self.follow_up_within).await {
            Ok(result) => result,
            Err(error) => {
                if error.sent_nothing() {
                    attempt.mark_rejected(&event.key).await?;
                }
                return Err(MessageSendError::Uncertain(error.to_string()));
            }
        };
        match result.refusal() {
            None if result
                .session_id
                .as_deref()
                .is_none_or(|id| session.is_same_session(id)) =>
            {
                // The command echoes no correlation id, so the receipt carries ezra's own.
                Ok(MessageReceipt {
                    chat_name: record.and_then(|record| record.title),
                    native_message_id: session.as_str().to_owned(),
                    delivery_id: event.key.delivery_id(),
                })
            }
            None => Err(MessageSendError::Uncertain(
                "Claude Code queued the message in another session".to_owned(),
            )),
            Some(Refusal::Gone) => {
                attempt.mark_rejected(&event.key).await?;
                Err(MessageSendError::NeedsReplacement(result.reason()))
            }
            Some(Refusal::SignIn) => {
                attempt.mark_rejected(&event.key).await?;
                tracing::warn!(reason = %result.reason(), "Claude Code cannot send to sessions, sign it in again with claude auth login");
                Err(MessageSendError::Unavailable)
            }
            Some(Refusal::Other) => Err(MessageSendError::Uncertain(result.reason())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ezra::inbound::store::{
        DeliveryScope, DeliveryState, DispatchOutcome, EventStore, SessionTarget,
    };
    use ezra::inbound::{ConversationKey, EventKey, InboundSettings};
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tokio::time::timeout;

    use super::*;
    use crate::manager::api::test_support::TestManager;
    use crate::manager::remote_control::ServerState;
    use fake::{FakeSessionsApi, Reply};

    const TOKEN: &str = "test-access-token";
    const SESSION: &str = "cse_01AB";
    const DELIVERED: &str = r#"{"ok":true,"session_id":"session_01AB","url":"https://claude.ai/code/session_01AB?from=cli&m=0"}"#;

    struct Fixture {
        manager: TestManager,
        fake: FakeSessionsApi,
        remote: ClaudeRemote,
        out: TempDir,
    }

    /// Records what had happened by the time the dispatcher was told about the attempt.
    struct Probe<'fixture> {
        fixture: &'fixture Fixture,
        marked: SyncMutex<Vec<(usize, usize)>>,
        rejected: AtomicUsize,
    }

    impl MessageAttempt for Probe<'_> {
        async fn mark_attempted(
            &self,
            key: &ezra::inbound::EventKey,
        ) -> Result<(), MessageSendError> {
            assert_eq!(key, &Fixture::event().key);
            let seen = (self.fixture.runs().len(), self.fixture.fake.seen().len());
            self.marked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(seen);
            Ok(())
        }

        async fn mark_rejected(
            &self,
            _key: &ezra::inbound::EventKey,
        ) -> Result<(), MessageSendError> {
            self.rejected.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    impl Probe<'_> {
        fn marked(&self) -> Vec<(usize, usize)> {
            self.marked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        fn rejected(&self) -> usize {
            self.rejected.load(Ordering::SeqCst)
        }
    }

    impl Fixture {
        /// Claude Code signed in, with a `claude` whose follow-ups save stdin and the working
        /// directory, log `start` and `end` to `$OUT/order` around waiting `$OUT/delay` seconds,
        /// and print `$OUT/reply`.
        async fn new() -> Self {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let manager = TestManager::new();
            let out = tempfile::tempdir().expect("temporary directory");
            manager.install_fake_cli(
                Agent::Claude,
                &format!(
                    "OUT='{}'\ncase \"$1\" in\n  -p) cat > \"$OUT/stdin\"; pwd > \"$OUT/pwd\"; echo start >> \"$OUT/order\"; sleep \"$(cat \"$OUT/delay\" 2>/dev/null || echo 0)\"; echo end >> \"$OUT/order\"; cat \"$OUT/reply\" ;;\nesac",
                    out.path().display()
                ),
            );
            let fake = FakeSessionsApi::start().await;
            let remote = ClaudeRemote::new(
                Arc::clone(&manager.state.remote_control),
                Arc::clone(&manager.state.install_paths),
                manager.state.projects.clone(),
            )
            .with_api(fake.base.clone(), Duration::from_secs(20));
            let fixture = Self {
                manager,
                fake,
                remote,
                out,
            };
            fs::create_dir_all(fixture.config()).expect("config directory is created");
            fixture.sign_in(TOKEN);
            fixture.reply(DELIVERED);
            fixture
        }

        fn config(&self) -> PathBuf {
            self.manager
                .state
                .install_paths
                .config_directory(Agent::Claude)
                .expect("Claude has a config directory")
                .to_path_buf()
        }

        fn sign_in(&self, token: &str) {
            fs::write(
                self.config().join(".credentials.json"),
                format!(
                    r#"{{"claudeAiOauth":{{"accessToken":"{token}","scopes":["user:inference","user:sessions:claude_code"]}}}}"#
                ),
            )
            .expect("credentials are written");
        }

        fn projects(&self) -> PathBuf {
            self.manager.state.projects.0.clone()
        }

        fn folder(&self) -> PathBuf {
            self.manager.state.projects.folder("repository")
        }

        fn show(&self, directory: &Path, state: ServerState, url: Option<&str>) {
            self.manager.state.remote_control.show(
                directory,
                Some(RemoteControlStatus {
                    state,
                    url: url.map(str::to_owned),
                    ..RemoteControlStatus::default()
                }),
            );
        }

        fn connect(&self, directory: &Path, environment: &str) {
            self.show(
                directory,
                ServerState::Running,
                Some(&format!("https://claude.ai/code?environment={environment}")),
            );
        }

        fn reply(&self, stdout: &str) {
            fs::write(self.out.path().join("reply"), stdout).expect("reply is written");
        }

        fn written(&self, name: &str) -> String {
            fs::read_to_string(self.out.path().join(name)).expect("the fake wrote the file")
        }

        fn runs(&self) -> Vec<String> {
            self.manager.fake_cli_runs(Agent::Claude)
        }

        fn probe(&self) -> Probe<'_> {
            Probe {
                fixture: self,
                marked: SyncMutex::default(),
                rejected: AtomicUsize::new(0),
            }
        }

        fn record(&self, record: Value) {
            self.fake.reply(
                &format!("GET /v1/code/sessions/{SESSION}"),
                [Reply::new(200, record)],
            );
        }

        fn created(&self, replies: impl IntoIterator<Item = Reply>) {
            self.fake.reply("POST /v1/code/sessions", replies);
        }

        fn created_bodies(&self) -> Vec<Value> {
            self.fake
                .seen()
                .into_iter()
                .filter(|request| request.route() == "POST /v1/code/sessions")
                .map(|request| request.body)
                .collect()
        }

        async fn create(&self, workspace: &Path) -> Result<String, MessageSendError> {
            self.remote
                .create_chat(
                    workspace.to_str().expect("a UTF-8 path"),
                    None,
                    &Shortcut::default(),
                )
                .await
        }

        async fn send(&self, probe: &Probe<'_>) -> Result<MessageReceipt, MessageSendError> {
            self.remote
                .queue_message_tracked(SESSION, &Self::event(), probe)
                .await
        }

        fn event() -> InboundEvent {
            InboundEvent {
                key: EventKey {
                    conversation: ConversationKey {
                        source: "github:github.com".to_owned(),
                        subject: "1/2".to_owned(),
                    },
                    id: "3".to_owned(),
                },
                new_chat: false,
                options: Shortcut::default(),
                chat_name: None,
                source_url: None,
                actor: "4".to_owned(),
                created_at: time::OffsetDateTime::UNIX_EPOCH,
                message: "--help\n# Fix it\n\n```sh\necho $(id) `pwd`\n```\n> é ✓\n".to_owned(),
                initial_context: None,
            }
        }
    }

    fn uncertain(result: Result<impl std::fmt::Debug, MessageSendError>) -> String {
        match result {
            Err(MessageSendError::Uncertain(reason)) => reason,
            other => panic!("expected an uncertain result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn claude_is_available_while_a_server_is_connected_in_an_environment() {
        let fixture = Fixture::new().await;
        let mut changes = fixture
            .remote
            .control_changes()
            .expect("changes are watched");
        assert!(!fixture.remote.control_available());

        fixture.show(&fixture.projects(), ServerState::Starting, None);
        assert!(!fixture.remote.control_available());
        fixture.connect(&fixture.projects(), "env_projects");
        assert!(fixture.remote.control_available());
        assert!(changes.has_changed().expect("the sender is alive"));
        changes.mark_unchanged();

        fixture.connect(&fixture.folder(), "env_folder");
        assert!(changes.has_changed().expect("the sender is alive"));
        assert!(*changes.borrow_and_update());
    }

    #[tokio::test]
    async fn new_sessions_go_to_the_environment_of_the_workspace_or_the_projects_server() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        fixture.connect(&fixture.folder(), "env_folder");
        fixture.created((1..=4).map(|_| Reply::new(201, json!({"id": "session_01AB"}))));
        let other = fixture.manager.state.projects.folder("unserved");
        for workspace in [
            fixture.folder(),
            other,
            PathBuf::from("/home/dev"),
            fixture.projects(),
        ] {
            assert_eq!(
                fixture
                    .create(&workspace)
                    .await
                    .expect("a session is created"),
                "session_01AB"
            );
        }
        let environments: Vec<_> = fixture
            .created_bodies()
            .iter()
            .map(|body| body["environment_id"].clone())
            .collect();
        assert_eq!(
            environments,
            ["env_folder", "env_projects", "env_projects", "env_projects"]
        );
    }

    #[tokio::test]
    async fn a_workspace_server_that_is_not_connected_holds_new_sessions_back() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        for state in [
            ServerState::Waiting,
            ServerState::Starting,
            ServerState::Retrying,
            ServerState::Stopping,
        ] {
            fixture.show(&fixture.folder(), state, None);
            assert!(matches!(
                fixture.create(&fixture.folder()).await,
                Err(MessageSendError::Unavailable)
            ));
        }
        fixture.show(
            &fixture.folder(),
            ServerState::Running,
            Some("https://claude.ai/code/session_01CD"),
        );
        assert!(matches!(
            fixture.create(&fixture.folder()).await,
            Err(MessageSendError::Unavailable)
        ));

        fixture
            .manager
            .state
            .remote_control
            .show(&fixture.folder(), None);
        fixture.show(&fixture.projects(), ServerState::Retrying, None);
        assert!(matches!(
            fixture.create(Path::new("/home/dev")).await,
            Err(MessageSendError::Unavailable)
        ));
        assert!(fixture.fake.seen().is_empty());
    }

    #[tokio::test]
    async fn a_new_session_has_the_title_and_settings() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        fixture.created([
            Reply::new(200, json!({"id": "cse_01AB", "title": "owner/repo#1"})),
            Reply::new(200, json!({"id": "session_01CD"})),
        ]);
        let options = Shortcut {
            model: Some("opus".to_owned()),
            effort: Some("high".to_owned()),
            ..Shortcut::default()
        };
        assert_eq!(
            fixture
                .remote
                .create_chat("/home/dev", Some("owner/repo#1"), &options)
                .await
                .expect("a session is created"),
            "cse_01AB"
        );
        assert_eq!(
            fixture
                .create(Path::new("/home/dev"))
                .await
                .expect("a session is created"),
            "session_01CD"
        );

        let [named, plain] = fixture.fake.seen().try_into().expect("two sessions");
        assert_eq!(
            named.body,
            json!({
                "title": "owner/repo#1",
                "environment_id": "env_projects",
                "events": [],
                "config": {"sources": [], "outcomes": [], "model": "opus", "effort_level": "high"},
            })
        );
        assert_eq!(plain.body["title"], "ezra");
        assert_eq!(plain.body["config"], json!({"sources": [], "outcomes": []}));
        for request in [&named, &plain] {
            assert_eq!(
                request.header("authorization"),
                Some(format!("Bearer {TOKEN}"))
            );
        }
    }

    #[tokio::test]
    async fn a_rejected_token_is_read_again_once() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        let credentials = fixture.config().join(".credentials.json");
        let refreshed = fs::read_to_string(&credentials)
            .expect("credentials are read")
            .replace(TOKEN, "refreshed-token");
        fixture.created([
            Reply::new(401, json!({"error": {"message": "expired"}})).before(move || {
                fs::write(credentials, refreshed).expect("credentials are refreshed");
            }),
            Reply::new(200, json!({"id": "cse_01AB"})),
            Reply::new(401, json!({})),
            Reply::new(401, json!({})),
        ]);
        assert_eq!(
            fixture
                .create(Path::new("/home/dev"))
                .await
                .expect("the retry creates a session"),
            "cse_01AB"
        );
        assert!(matches!(
            fixture.create(Path::new("/home/dev")).await,
            Err(MessageSendError::Unavailable)
        ));
        let tokens: Vec<_> = fixture
            .fake
            .seen()
            .iter()
            .filter_map(|request| request.header("authorization"))
            .collect();
        assert_eq!(
            tokens,
            [
                format!("Bearer {TOKEN}"),
                "Bearer refreshed-token".to_owned(),
                "Bearer refreshed-token".to_owned(),
                "Bearer refreshed-token".to_owned(),
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_creation_is_uncertain_and_says_why_without_the_token() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        fixture.created([
            Reply::new(400, json!({"type": "error", "error": {"type": "invalid_request_error", "message": "environment is at capacity"}})),
            Reply::new(503, json!({})),
            Reply::new(200, json!({"title": "no id"})),
            Reply::new(200, json!({"id": "--help"})),
        ]);
        let mut reasons = Vec::new();
        for _ in 0..4 {
            reasons.push(uncertain(fixture.create(Path::new("/home/dev")).await));
        }
        assert_eq!(
            reasons[0],
            "Claude answered 400: environment is at capacity"
        );
        assert_eq!(reasons[1], "Claude answered 503: Service Unavailable");
        assert!(
            reasons[2..]
                .iter()
                .all(|reason| reason == "Claude created a session without a usable id")
        );
        assert!(reasons.iter().all(|reason| !reason.contains(TOKEN)));
    }

    #[tokio::test]
    async fn without_a_sign_in_no_session_is_created() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        for credentials in [
            None,
            Some(r#"{"claudeAiOauth":{"accessToken":"t","scopes":["user:inference"]}}"#),
        ] {
            let path = fixture.config().join(".credentials.json");
            match credentials {
                None => fs::remove_file(&path).expect("credentials are removed"),
                Some(credentials) => {
                    fs::write(&path, credentials).expect("credentials are written")
                }
            }
            assert!(matches!(
                fixture.create(Path::new("/home/dev")).await,
                Err(MessageSendError::Unavailable)
            ));
        }
        assert!(fixture.fake.seen().is_empty());
    }

    #[tokio::test]
    async fn a_follow_up_reaches_the_session_on_stdin_from_its_server_directory() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        fixture.connect(&fixture.folder(), "env_folder");
        fs::create_dir_all(fixture.folder()).expect("folder is created");
        fixture.record(json!({"id": "session_01AB", "title": "owner/repo#1", "status": "idle", "environment_id": "env_folder"}));
        let probe = fixture.probe();
        let receipt = fixture.send(&probe).await.expect("the message is sent");
        assert_eq!(
            receipt,
            MessageReceipt {
                chat_name: Some("owner/repo#1".to_owned()),
                native_message_id: SESSION.to_owned(),
                delivery_id: Fixture::event().key.delivery_id(),
            }
        );
        assert_eq!(fixture.written("stdin"), Fixture::event().message);
        assert_eq!(
            PathBuf::from(fixture.written("pwd").trim()),
            fs::canonicalize(fixture.folder()).expect("the folder exists")
        );
        assert_eq!(
            fixture.runs(),
            [format!("-p --cloud {SESSION} --output-format json")]
        );
        assert_eq!(
            probe.marked(),
            [(0, 1)],
            "marked after the check, before Claude Code ran"
        );
        assert_eq!(probe.rejected(), 0);
    }

    #[tokio::test]
    async fn an_archived_or_unserved_session_is_replaced_without_running_claude_code() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        for record in [
            json!({"id": SESSION, "status": "archived", "environment_id": "env_projects"}),
            json!({"id": SESSION, "session_status": "SESSION_STATUS_ARCHIVED"}),
            json!({"id": SESSION, "status": "idle", "environment_id": "env_before_restart"}),
        ] {
            fixture.record(record);
            let probe = fixture.probe();
            assert!(matches!(
                fixture.send(&probe).await,
                Err(MessageSendError::NeedsReplacement(_))
            ));
            assert!(probe.marked().is_empty());
        }
        assert!(fixture.runs().is_empty());
    }

    #[tokio::test]
    async fn claude_code_decides_when_the_session_cannot_be_read() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        fs::create_dir_all(fixture.projects()).expect("projects directory is created");
        let probe = fixture.probe();
        let receipt = fixture.send(&probe).await.expect("the message is sent");
        assert_eq!(receipt.chat_name, None);
        assert_eq!(
            PathBuf::from(fixture.written("pwd").trim()),
            fs::canonicalize(fixture.projects()).expect("the directory exists")
        );
        assert_eq!(probe.marked(), [(0, 1)]);

        fixture
            .record(json!({"session": {"id": SESSION, "title": "owner/repo#1", "status": "idle"}}));
        let receipt = fixture.send(&probe).await.expect("the message is sent");
        assert_eq!(receipt.chat_name.as_deref(), Some("owner/repo#1"));
        assert_eq!(
            PathBuf::from(fixture.written("pwd").trim()),
            fs::canonicalize(fixture.projects()).expect("the directory exists")
        );
    }

    #[tokio::test]
    async fn a_follow_up_that_runs_too_long_stays_uncertain() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        fs::create_dir_all(fixture.projects()).expect("projects directory is created");
        fs::write(fixture.out.path().join("delay"), "30").expect("delay is written");
        let remote = ClaudeRemote::new(
            Arc::clone(&fixture.manager.state.remote_control),
            Arc::clone(&fixture.manager.state.install_paths),
            fixture.manager.state.projects.clone(),
        )
        .with_api(fixture.fake.base.clone(), Duration::from_secs(1));
        let probe = fixture.probe();
        let reason = uncertain(
            remote
                .queue_message_tracked(SESSION, &Fixture::event(), &probe)
                .await,
        );
        assert!(reason.starts_with("Claude Code did not finish"), "{reason}");
        assert_eq!((probe.marked().len(), probe.rejected()), (1, 0));
    }

    #[tokio::test]
    async fn a_follow_up_that_cannot_start_sends_nothing() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        let claude = fs::canonicalize(fixture.manager.state.install_paths.command(Agent::Claude))
            .expect("claude is installed");
        fs::set_permissions(&claude, fs::Permissions::from_mode(0o644))
            .expect("claude is made unrunnable");
        let probe = fixture.probe();
        let reason = uncertain(fixture.send(&probe).await);
        assert!(
            reason.starts_with("could not start Claude Code"),
            "{reason}"
        );
        assert_eq!((probe.marked().len(), probe.rejected()), (1, 1));
    }

    #[tokio::test]
    async fn follow_ups_to_one_session_run_one_at_a_time() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        fs::create_dir_all(fixture.projects()).expect("projects directory is created");
        fs::write(fixture.out.path().join("delay"), "0.3").expect("delay is written");
        let event = Fixture::event();
        let (first, second) = tokio::join!(
            fixture.remote.queue_message(SESSION, &event),
            fixture.remote.queue_message("session_01AB", &event),
        );
        first.expect("the first message is sent");
        second.expect("the second message is sent");
        assert_eq!(
            fixture.written("order").lines().collect::<Vec<_>>(),
            ["start", "end", "start", "end"]
        );
    }

    #[tokio::test]
    async fn claude_code_answers_decide_what_happens_to_the_request() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        fs::create_dir_all(fixture.projects()).expect("projects directory is created");
        let refused =
            |error: &str| json!({"ok": false, "session_id": SESSION, "error": error}).to_string();
        for (stdout, outcome, rejected) in [
            (
                refused(&format!(
                    "cloud session {SESSION} is archived and cannot accept new messages"
                )),
                "replace",
                1,
            ),
            (
                refused("invalid session ID: must be a cse_… or session_… tagged ID"),
                "replace",
                1,
            ),
            (
                refused("Attaching to an existing cloud session is not enabled for your account."),
                "unavailable",
                1,
            ),
            (
                refused("Session expired. Please run /login to sign in again."),
                "unavailable",
                1,
            ),
            (refused("rate limited"), "uncertain", 0),
            ("Error: something went wrong".to_owned(), "uncertain", 0),
            (
                r#"{"ok":true,"session_id":"session_01ZZ"}"#.to_owned(),
                "uncertain",
                0,
            ),
            (r#"{"ok":true}"#.to_owned(), "delivered", 0),
            (DELIVERED.to_owned(), "delivered", 0),
        ] {
            fixture.reply(&stdout);
            let probe = fixture.probe();
            let result = fixture.send(&probe).await;
            let actual = match &result {
                Ok(_) => "delivered",
                Err(MessageSendError::NeedsReplacement(_)) => "replace",
                Err(MessageSendError::Unavailable) => "unavailable",
                Err(MessageSendError::Uncertain(_)) => "uncertain",
            };
            assert_eq!(
                (actual, probe.rejected()),
                (outcome, rejected),
                "{stdout}: {result:?}"
            );
            assert_eq!(probe.marked().len(), 1);
        }
    }

    #[tokio::test]
    async fn a_chat_that_is_not_a_session_id_is_replaced_without_sending() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        for chat in ["--help", "session_01AB/../x", "thread-1"] {
            let probe = fixture.probe();
            assert!(matches!(
                fixture
                    .remote
                    .queue_message_tracked(chat, &Fixture::event(), &probe)
                    .await,
                Err(MessageSendError::NeedsReplacement(_))
            ));
            assert!(probe.marked().is_empty());
        }
        assert!(fixture.runs().is_empty());
        assert!(fixture.fake.seen().is_empty());
    }

    #[tokio::test]
    async fn an_unavailable_sender_never_marks_an_attempt() {
        let fixture = Fixture::new().await;
        let probe = fixture.probe();
        assert!(matches!(
            fixture.send(&probe).await,
            Err(MessageSendError::Unavailable)
        ));
        assert!(matches!(
            fixture
                .remote
                .queue_message(SESSION, &Fixture::event())
                .await,
            Err(MessageSendError::Unavailable)
        ));

        fixture.connect(&fixture.projects(), "env_projects");
        let credentials = fixture.config().join(".credentials.json");
        fs::remove_file(&credentials).expect("credentials are removed");
        assert!(matches!(
            fixture.send(&probe).await,
            Err(MessageSendError::Unavailable)
        ));
        assert!(fixture.fake.seen().is_empty());

        fixture.sign_in(TOKEN);
        fixture.fake.reply(
            &format!("GET /v1/code/sessions/{SESSION}"),
            [Reply::new(401, json!({})), Reply::new(401, json!({}))],
        );
        assert!(matches!(
            fixture.send(&probe).await,
            Err(MessageSendError::Unavailable)
        ));
        assert_eq!(fixture.fake.seen().len(), 2);

        fs::remove_file(fixture.manager.state.install_paths.command(Agent::Claude))
            .expect("claude is removed");
        assert!(matches!(
            fixture.send(&probe).await,
            Err(MessageSendError::Unavailable)
        ));
        assert_eq!(fixture.fake.seen().len(), 2);
        assert!(probe.marked().is_empty());
        assert!(fixture.runs().is_empty());
    }

    #[tokio::test]
    async fn the_dispatcher_replaces_an_archived_session_and_sends_the_description_again() {
        let fixture = Fixture::new().await;
        fixture.connect(&fixture.projects(), "env_projects");
        fixture.connect(&fixture.folder(), "env_folder");
        fs::create_dir_all(fixture.folder()).expect("folder is created");
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = EventStore::open(&directory.path().join("ezra.db"))
            .await
            .expect("store opens");
        let mut event = Fixture::event();
        event.chat_name = Some("owner/repo#1: Fix it".to_owned());
        event.initial_context = Some("\n\nOriginal description".to_owned());
        event.options.agent = Agent::Claude;
        event.options.model = Some("opus".to_owned());
        let workspace = fixture.folder().to_str().expect("a UTF-8 path").to_owned();
        store
            .bind_conversation(
                &event.key.conversation,
                &SessionTarget {
                    host_id: "host".to_owned(),
                    agent: "claude".to_owned(),
                    chat_id: SESSION.to_owned(),
                    workspace: workspace.clone(),
                },
            )
            .await
            .expect("the conversation is bound");
        store
            .insert(&event, InboundSettings::default().queue_limits())
            .await
            .expect("the event is saved");
        fixture
            .record(json!({"session": {"id": SESSION, "status": "archived", "environment_id": "env_folder"}}));
        fixture.created([Reply::new(200, json!({"session": {"id": "session_01NEW"}}))]);
        fixture.fake.reply(
            "GET /v1/code/sessions/session_01NEW",
            [Reply::new(200, json!({"response_shape": {"id": "session_01NEW", "title": "owner/repo#1: Fix it", "environment_id": "env_folder"}}))],
        );
        fixture.reply(r#"{"ok":true,"session_id":"session_01NEW"}"#);

        let outcome = store
            .dispatch_next(
                DeliveryScope {
                    host_id: "host",
                    agent: "claude",
                },
                &fixture.remote,
            )
            .await
            .expect("the event is dispatched");
        assert!(
            matches!(outcome, DispatchOutcome::Delivered { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            store
                .delivery_state(&event.key)
                .await
                .expect("state is read"),
            Some(DeliveryState::Delivered)
        );
        let binding = store
            .find_binding(&event.key.conversation, Agent::Claude)
            .await
            .expect("binding is read")
            .expect("still bound");
        assert_eq!(
            (binding.chat_id.as_str(), binding.workspace.as_str()),
            ("session_01NEW", workspace.as_str())
        );
        let [created] = fixture
            .created_bodies()
            .try_into()
            .expect("one new session");
        assert_eq!(
            (
                &created["environment_id"],
                &created["title"],
                &created["config"]["model"]
            ),
            (
                &json!("env_folder"),
                &json!("owner/repo#1: Fix it"),
                &json!("opus")
            )
        );
        assert_eq!(
            fixture.runs(),
            ["-p --cloud session_01NEW --output-format json"]
        );
        assert_eq!(
            fixture.written("stdin"),
            event.with_initial_context().message
        );
    }

    #[tokio::test]
    async fn messages_to_one_session_go_out_one_at_a_time() {
        let locks = ChatLocks::default();
        let session = |id: &str| SessionId::parse(id).expect("the id is valid");
        let first = locks.hold(&session("cse_01AB")).await;
        assert!(
            timeout(
                Duration::from_millis(20),
                locks.hold(&session("session_01AB"))
            )
            .await
            .is_err(),
            "both forms of an id are one session"
        );
        let other = timeout(Duration::from_secs(5), locks.hold(&session("cse_01CD")))
            .await
            .expect("another session is not held up");
        drop(first);
        let next = timeout(Duration::from_secs(5), locks.hold(&session("session_01AB")))
            .await
            .expect("the session is free again");
        drop((next, other));
        let _current = locks.hold(&session("cse_01EF")).await;
        assert_eq!(
            locks.0.lock().unwrap_or_else(PoisonError::into_inner).len(),
            1
        );
    }
}
