use std::collections::VecDeque;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as SyncMutex, PoisonError};
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep, timeout};
use utoipa::ToSchema;

use super::agents::Agent;
use super::login::{AgentCli, StrExt};
use super::state::AppState;

pub const SERVED_DIRECTORY: &str = "/projects";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(5);
const SIGN_IN_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const GROUP_POLL_INTERVAL: Duration = Duration::from_millis(100);
const RECHECK_INTERVAL: Duration = Duration::from_secs(60);
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(5);
const LONGEST_RETRY_DELAY: Duration = Duration::from_secs(300);
const HEALTHY_RUN: Duration = Duration::from_secs(600);
const OUTPUT_LINES_KEPT: usize = 20;
const OUTPUT_LINES_REPORTED: usize = 5;
const VARIABLES_THAT_DISABLE_REMOTE_CONTROL: [&str; 7] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "DISABLE_GROWTHBOOK",
    "DISABLE_TELEMETRY",
];

/// How Claude Code's Remote Control server runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct RemoteControlSettings {
    /// Serve /projects to the Claude app while Claude Code is signed in.
    #[schema(required = true)]
    pub enabled: bool,
    /// The permission mode for sessions started from the Claude app, such as `auto`.
    #[schema(required = true)]
    pub permission_mode: String,
    /// The most sessions that run at once, from 1 to 32.
    #[schema(required = true, minimum = 1, maximum = 32)]
    pub capacity: u32,
}

impl RemoteControlSettings {
    pub const MOST_SESSIONS: u32 = 32;

    /// `None` when the settings can be passed to Claude, or why not.
    pub fn problem(&self) -> Option<&'static str> {
        let mode = self.permission_mode.trim();
        if mode.is_empty() || mode.contains(char::is_whitespace) {
            Some("the permission mode must be one word")
        } else if !(1..=Self::MOST_SESSIONS).contains(&self.capacity) {
            Some("capacity must be from 1 to 32")
        } else {
            None
        }
    }

    pub fn trimmed(mut self) -> Self {
        self.permission_mode = self.permission_mode.trim().to_owned();
        self
    }
}

impl Default for RemoteControlSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            permission_mode: "auto".to_owned(),
            capacity: 4,
        }
    }
}

/// Where the Remote Control server is in its life.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    /// Turned off in the settings.
    Off,
    /// Waiting for Claude Code to be installed and signed in.
    #[default]
    Waiting,
    /// Started, not connected yet.
    Starting,
    /// Connected and taking sessions.
    Running,
    /// Stopped unexpectedly and starting again soon.
    Retrying,
}

/// The manager's view of the Remote Control server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RemoteControlStatus {
    pub state: ServerState,
    /// Where to continue in a browser, once connected.
    pub url: Option<String>,
    /// The end of the server's output when it last stopped unexpectedly.
    pub last_error: Option<String>,
    /// Unexpected stops since the manager started.
    pub restarts: u32,
}

/// The shared handle the API reads status from and nudges when something changed.
#[derive(Debug, Default)]
pub struct RemoteControl {
    status: SyncMutex<RemoteControlStatus>,
    changed: Notify,
    restart_requested: AtomicBool,
    shutting_down: AtomicBool,
    shut_down_requested: Notify,
    stopped: AtomicBool,
    stopped_signal: Notify,
}

impl RemoteControl {
    pub fn status(&self) -> RemoteControlStatus {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Settings or the install changed, so the server may need to start, stop or restart.
    pub fn reconsider(&self) {
        self.changed.notify_one();
    }

    /// The sign-in changed, which a running server only picks up by starting again.
    pub fn restart(&self) {
        self.restart_requested.store(true, Ordering::SeqCst);
        self.changed.notify_one();
    }

    pub fn begin_shut_down(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.shut_down_requested.notify_one();
    }

    pub async fn wait_until_stopped(&self) {
        let stopped = self.stopped_signal.notified();
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        let longest = STOP_GRACE_PERIOD.saturating_add(OUTPUT_DRAIN_TIMEOUT);
        if timeout(longest, stopped).await.is_err() {
            tracing::warn!("Claude Remote Control did not stop in time");
        }
    }

    fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    fn take_restart_request(&self) -> bool {
        self.restart_requested.swap(false, Ordering::SeqCst)
    }

    fn mark_stopped(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.stopped_signal.notify_one();
    }

    fn update(&self, change: impl FnOnce(&mut RemoteControlStatus)) {
        change(&mut self.status.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

/// What should be running right now. `Unknown` when the sign-in could not be checked in time.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Wanted {
    Off,
    Waiting,
    Unknown,
    Server(Launch),
}

/// One `claude remote-control` process to start. A new Claude version is a different launch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Launch {
    claude: PathBuf,
    claude_version: Option<String>,
    config_directory: Option<PathBuf>,
    directory: PathBuf,
    settings: RemoteControlSettings,
}

impl Launch {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.claude);
        command
            .current_dir(&self.directory)
            .args(["remote-control", "--spawn", "same-dir", "--capacity"])
            .arg(self.settings.capacity.to_string())
            .arg("--permission-mode")
            .arg(&self.settings.permission_mode)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        for variable in VARIABLES_THAT_DISABLE_REMOTE_CONTROL {
            command.env_remove(variable);
        }
        command
    }

    /// Answers Claude's onboarding, Remote Control consent and folder trust prompts in advance.
    fn accept_prompts(&self) -> Result<(), String> {
        let Some(config_directory) = &self.config_directory else {
            return Ok(());
        };
        ClaudeGlobalConfig(config_directory.join(".claude.json"))
            .accept_remote_control_in(&self.directory)
            .map_err(|error| format!("could not prepare Claude's settings: {error}"))
    }
}

/// `$CLAUDE_CONFIG_DIR/.claude.json`, which Claude also writes. Unknown keys are kept.
struct ClaudeGlobalConfig(PathBuf);

impl ClaudeGlobalConfig {
    /// Writes only when something changes.
    fn accept_remote_control_in(&self, directory: &Path) -> io::Result<()> {
        let original = match fs::read(&self.0) {
            Ok(bytes) => Some(serde_json::from_slice::<Map<String, Value>>(&bytes)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let mut config = original.clone().unwrap_or_default();
        config.insert("hasCompletedOnboarding".to_owned(), Value::Bool(true));
        config.insert("remoteDialogSeen".to_owned(), Value::Bool(true));
        let projects = config
            .entry("projects")
            .or_insert_with(|| Value::Object(Map::new()));
        if !projects.is_object() {
            *projects = Value::Object(Map::new());
        }
        if let Some(projects) = projects.as_object_mut() {
            let project = projects
                .entry(directory.to_string_lossy())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(project) = project.as_object_mut() {
                project.insert("hasTrustDialogAccepted".to_owned(), Value::Bool(true));
            }
        }
        if original.as_ref() == Some(&config) {
            return Ok(());
        }
        if let Some(directory) = self.0.parent() {
            fs::create_dir_all(directory)?;
        }
        let staging = self.0.with_extension("json.ezra");
        fs::write(&staging, serde_json::to_vec_pretty(&config)?)?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;
        fs::rename(&staging, &self.0)
    }
}

/// Why a server run ended.
enum RunEnd {
    /// What should run changed, so it was stopped on purpose.
    Reconsidered,
    /// The manager is stopping.
    ShutDown,
    /// It could not start, did not connect, or stopped by itself.
    Failed(String),
}

/// Unexpected stops in a row, which set how long to wait before starting again.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Failures(u32);

impl Failures {
    fn retry_delay(self) -> Duration {
        let doublings = self.0.saturating_sub(1).min(16);
        FIRST_RETRY_DELAY
            .saturating_mul(2_u32.saturating_pow(doublings))
            .min(LONGEST_RETRY_DELAY)
    }
}

impl AppState {
    pub async fn supervise_remote_control(self) {
        let mut failures = Failures::default();
        loop {
            let run = match self.wanted_remote_control().await {
                Wanted::Off => self.idle_remote_control(ServerState::Off).await,
                Wanted::Waiting | Wanted::Unknown => {
                    self.idle_remote_control(ServerState::Waiting).await
                }
                Wanted::Server(launch) => {
                    let started = Instant::now();
                    let end = self.run_remote_control(&launch).await;
                    if started.elapsed() >= HEALTHY_RUN {
                        failures = Failures::default();
                    }
                    end
                }
            };
            match run {
                RunEnd::Reconsidered => {}
                RunEnd::ShutDown => break,
                RunEnd::Failed(message) => {
                    failures = Failures(failures.0.saturating_add(1));
                    tracing::warn!("Claude Remote Control stopped: {message}");
                    self.remote_control.update(|status| {
                        status.state = ServerState::Retrying;
                        status.url = None;
                        status.last_error = Some(message);
                        status.restarts = status.restarts.saturating_add(1);
                    });
                    if let RunEnd::ShutDown = self.wait_for_change(failures.retry_delay()).await {
                        break;
                    }
                }
            }
        }
        self.remote_control.mark_stopped();
    }

    async fn idle_remote_control(&self, state: ServerState) -> RunEnd {
        self.remote_control.update(|status| {
            status.state = state;
            status.url = None;
        });
        self.wait_for_change(RECHECK_INTERVAL).await
    }

    async fn wait_for_change(&self, longest: Duration) -> RunEnd {
        if self.remote_control.is_shutting_down() {
            return RunEnd::ShutDown;
        }
        tokio::select! {
            () = self.remote_control.changed.notified() => RunEnd::Reconsidered,
            () = self.remote_control.shut_down_requested.notified() => RunEnd::ShutDown,
            () = sleep(longest) => RunEnd::Reconsidered,
        }
    }

    async fn wanted_remote_control(&self) -> Wanted {
        let settings = self
            .settings
            .lock()
            .await
            .agents
            .claude
            .remote_control
            .clone();
        if !settings.enabled {
            return Wanted::Off;
        }
        let Ok(claude) = AgentCli::installed(Agent::Claude, &self.install_paths) else {
            return Wanted::Waiting;
        };
        match timeout(SIGN_IN_CHECK_TIMEOUT, claude.sign_in_status()).await {
            Ok(sign_in) if sign_in.logged_in => {}
            Ok(_) => return Wanted::Waiting,
            Err(_) => return Wanted::Unknown,
        }
        Wanted::Server(Launch {
            claude: self.install_paths.command(Agent::Claude),
            claude_version: self.install_paths.installed_version(Agent::Claude),
            config_directory: self
                .install_paths
                .config_directory(Agent::Claude)
                .map(Path::to_path_buf),
            directory: self.remote_control_directory.clone(),
            settings,
        })
    }

    async fn still_wanted(&self, launch: &Launch) -> bool {
        if self.remote_control.take_restart_request() {
            return false;
        }
        match self.wanted_remote_control().await {
            Wanted::Server(wanted) => wanted == *launch,
            Wanted::Unknown => true,
            Wanted::Off | Wanted::Waiting => false,
        }
    }

    async fn run_remote_control(&self, launch: &Launch) -> RunEnd {
        self.remote_control.take_restart_request();
        if self.remote_control.is_shutting_down() {
            return RunEnd::ShutDown;
        }
        if let Err(message) = launch.accept_prompts() {
            return RunEnd::Failed(message);
        }
        let mut server = match ServerRun::start(launch, &self.remote_control) {
            Ok(server) => server,
            Err(error) => return RunEnd::Failed(format!("could not start: {error}")),
        };
        tracing::info!(
            "Claude Remote Control is starting in {}",
            launch.directory.display()
        );
        let connect_deadline = sleep(CONNECT_TIMEOUT);
        tokio::pin!(connect_deadline);
        let mut recheck = interval(RECHECK_INTERVAL);
        recheck.set_missed_tick_behavior(MissedTickBehavior::Delay);
        recheck.reset();
        loop {
            let connected = self.remote_control.status().state == ServerState::Running;
            tokio::select! {
                exit = server.child.wait() => {
                    return RunEnd::Failed(server.finish(exit).await);
                }
                () = &mut connect_deadline, if !connected => {
                    server.stop().await;
                    return RunEnd::Failed("did not connect within 2 minutes".to_owned());
                }
                () = self.remote_control.changed.notified() => {
                    if !self.still_wanted(launch).await {
                        server.stop().await;
                        return RunEnd::Reconsidered;
                    }
                }
                _ = recheck.tick() => {
                    if !self.still_wanted(launch).await {
                        server.stop().await;
                        return RunEnd::Reconsidered;
                    }
                }
                () = self.remote_control.shut_down_requested.notified() => {
                    server.stop().await;
                    return RunEnd::ShutDown;
                }
            }
        }
    }
}

/// A running server, the readers of its output, and the sessions in its process group.
struct ServerRun {
    child: Child,
    group: Option<ProcessGroup>,
    readers: Vec<JoinHandle<()>>,
    output: ServerOutput,
}

impl ServerRun {
    fn start(launch: &Launch, remote_control: &Arc<RemoteControl>) -> io::Result<Self> {
        let mut child = launch.command().spawn()?;
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .map(|id| ProcessGroup(Pid::from_raw(id)));
        let output = ServerOutput::default();
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(tokio::spawn(
                output.clone().collect(stdout, Arc::clone(remote_control)),
            ));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(tokio::spawn(
                output.clone().collect(stderr, Arc::clone(remote_control)),
            ));
        }
        remote_control.update(|status| {
            status.state = ServerState::Starting;
            status.url = None;
        });
        Ok(Self {
            child,
            group,
            readers,
            output,
        })
    }

    async fn stop(mut self) {
        if let Some(group) = &self.group {
            group.terminate(&mut self.child).await;
        }
        let _already_gone = self.child.kill().await;
        self.stop_reading().await;
    }

    /// After the server exited by itself: stops the sessions it left and describes the exit.
    async fn finish(mut self, exit: io::Result<ExitStatus>) -> String {
        if let Some(group) = &self.group {
            group.terminate(&mut self.child).await;
        }
        self.stop_reading().await;
        self.output.describe_exit(exit)
    }

    async fn stop_reading(&mut self) {
        for reader in &mut self.readers {
            if timeout(OUTPUT_DRAIN_TIMEOUT, &mut *reader).await.is_err() {
                reader.abort();
            }
        }
    }
}

/// The server and the sessions it started, which share its process group.
struct ProcessGroup(Pid);

impl ProcessGroup {
    /// SIGTERM, then SIGKILL for whatever is left after the grace period.
    async fn terminate(&self, leader: &mut Child) {
        let _already_gone = killpg(self.0, Signal::SIGTERM);
        let deadline = Instant::now().checked_add(STOP_GRACE_PERIOD);
        while deadline.is_some_and(|deadline| Instant::now() < deadline) {
            let _reaped = leader.try_wait();
            if self.is_gone() {
                return;
            }
            sleep(GROUP_POLL_INTERVAL).await;
        }
        let _already_gone = killpg(self.0, Signal::SIGKILL);
        let _reaped = leader.wait().await;
    }

    fn is_gone(&self) -> bool {
        killpg(self.0, None) == Err(Errno::ESRCH)
    }
}

/// The last lines the server printed, shared by its stdout and stderr readers.
#[derive(Debug, Clone, Default)]
struct ServerOutput(Arc<SyncMutex<VecDeque<String>>>);

impl ServerOutput {
    async fn collect(self, stream: impl AsyncRead + Unpin, remote_control: Arc<RemoteControl>) {
        let mut reader = BufReader::new(stream);
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&bytes)
                .trim_end()
                .without_terminal_codes();
            if let Some(url) = line.connect_url() {
                tracing::info!("Claude Remote Control is connected: {url}");
                remote_control.update(|status| {
                    status.state = ServerState::Running;
                    status.url = Some(url);
                });
            }
            let mut kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            kept.push_back(line);
            while kept.len() > OUTPUT_LINES_KEPT {
                kept.pop_front();
            }
        }
    }

    fn describe_exit(&self, exit: io::Result<ExitStatus>) -> String {
        let kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let last_lines: Vec<&str> = kept
            .iter()
            .map(String::as_str)
            .filter(|line| !line.trim().is_empty())
            .collect();
        let skipped = last_lines.len().saturating_sub(OUTPUT_LINES_REPORTED);
        let tail = last_lines.get(skipped..).unwrap_or_default().join("\n");
        let exit = match exit {
            Ok(status) => status.to_string(),
            Err(error) => error.to_string(),
        };
        if tail.is_empty() {
            exit
        } else {
            format!("{exit}: {tail}")
        }
    }
}

trait RemoteControlLineExt {
    /// The claude.ai link printed once the server is connected.
    fn connect_url(&self) -> Option<String>;
}

impl RemoteControlLineExt for str {
    fn connect_url(&self) -> Option<String> {
        self.split_whitespace()
            .find(|word| word.starts_with("https://") && word.contains("environment="))
            .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::api::test_support::TestManager;
    use crate::path_ext::PathExt;

    const SIGNED_IN: &str = r#"{"loggedIn":true}"#;

    /// A `claude` that is signed in and whose Remote Control runs `server`.
    fn fake_claude(manager: &TestManager, directory: &Path, server: &str) -> AppState {
        let script = directory.join("claude");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  auth) echo '{SIGNED_IN}' ;;\n  remote-control) {server} ;;\nesac\n"
            ),
        )
        .expect("script is written");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
            .expect("script is executable");
        manager
            .state
            .install_paths
            .command(Agent::Claude)
            .replace_symlink(&script)
            .expect("command link is created");
        let served = directory.join("projects");
        fs::create_dir_all(&served).expect("served directory is created");
        let mut state = manager.state.clone();
        state.remote_control_directory = served;
        state
    }

    async fn wait_for(state: &AppState, wanted: ServerState) -> RemoteControlStatus {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("the deadline fits");
        loop {
            let status = state.remote_control.status();
            if status.state == wanted {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "still {status:?}, wanted {wanted:?}"
            );
            sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn server_runs_while_enabled_and_stops_when_turned_off() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = fake_claude(
            &manager,
            directory.path(),
            "echo 'Continue coding in the Claude mobile app or https://claude.ai/code?environment=env_test'; exec sleep 60",
        );
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());

        let running = wait_for(&state, ServerState::Running).await;
        assert_eq!(
            running.url.as_deref(),
            Some("https://claude.ai/code?environment=env_test")
        );

        state
            .update_settings(|settings| {
                settings.agents.claude.remote_control.enabled = false;
                Ok::<(), crate::manager::settings::SettingsError>(())
            })
            .await
            .expect("settings save");
        state.remote_control.reconsider();
        wait_for(&state, ServerState::Off).await;

        state.remote_control.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn a_server_that_exits_is_retried() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = fake_claude(&manager, directory.path(), "echo 'Workspace not trusted'");
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());

        let retrying = wait_for(&state, ServerState::Retrying).await;
        assert_eq!(retrying.restarts, 1);
        assert!(
            retrying
                .last_error
                .as_deref()
                .is_some_and(|error| error.ends_with("Workspace not trusted")),
            "{retrying:?}"
        );

        state.remote_control.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    fn launch(directory: &Path) -> Launch {
        Launch {
            claude: PathBuf::from("/home/dev/.local/bin/claude"),
            claude_version: Some("2.1.283".to_owned()),
            config_directory: Some(directory.join("claude")),
            directory: directory.join("projects"),
            settings: RemoteControlSettings::default(),
        }
    }

    #[test]
    fn server_runs_in_the_served_directory_without_disabling_variables() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let launch = launch(directory.path());
        let command = launch.command();
        let command = command.as_std();
        let arguments: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            arguments,
            [
                "remote-control",
                "--spawn",
                "same-dir",
                "--capacity",
                "4",
                "--permission-mode",
                "auto"
            ]
        );
        assert_eq!(command.get_current_dir(), Some(launch.directory.as_path()));
        for variable in VARIABLES_THAT_DISABLE_REMOTE_CONTROL {
            assert!(
                command
                    .get_envs()
                    .any(|(name, value)| name == variable && value.is_none()),
                "{variable} is not removed"
            );
        }
    }

    #[test]
    fn prompts_are_accepted_and_other_settings_kept() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join(".claude.json");
        fs::write(
            &config,
            r#"{"userID":"abc","projects":{"/other":{"hasTrustDialogAccepted":false}}}"#,
        )
        .expect("config is written");
        ClaudeGlobalConfig(config.clone())
            .accept_remote_control_in(Path::new("/projects"))
            .expect("prompts are accepted");
        let written: Value =
            serde_json::from_slice(&fs::read(&config).expect("config is read")).expect("JSON");
        assert_eq!(written["userID"], "abc");
        assert_eq!(written["hasCompletedOnboarding"], true);
        assert_eq!(written["remoteDialogSeen"], true);
        assert_eq!(
            written["projects"]["/projects"]["hasTrustDialogAccepted"],
            true
        );
        assert_eq!(
            written["projects"]["/other"]["hasTrustDialogAccepted"],
            false
        );
        assert_eq!(
            fs::metadata(&config)
                .expect("config exists")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn accepted_config_is_not_rewritten() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join(".claude.json");
        let accepted = r#"{"hasCompletedOnboarding":true,"remoteDialogSeen":true,"projects":{"/projects":{"hasTrustDialogAccepted":true}}}"#;
        fs::write(&config, accepted).expect("config is written");
        ClaudeGlobalConfig(config.clone())
            .accept_remote_control_in(Path::new("/projects"))
            .expect("prompts are accepted");
        assert_eq!(
            fs::read_to_string(&config).expect("config is read"),
            accepted
        );
    }

    #[test]
    fn missing_config_is_created_and_broken_config_is_left_alone() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join(".claude.json");
        ClaudeGlobalConfig(config.clone())
            .accept_remote_control_in(Path::new("/projects"))
            .expect("prompts are accepted");
        assert!(config.exists());

        fs::write(&config, "{not json").expect("config is written");
        assert!(
            ClaudeGlobalConfig(config.clone())
                .accept_remote_control_in(Path::new("/projects"))
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(&config).expect("config is read"),
            "{not json"
        );
    }

    #[test]
    fn connect_url_comes_from_the_continue_line() {
        assert_eq!(
            "Continue coding in the Claude mobile app or https://claude.ai/code?environment=env_01AB"
                .connect_url()
                .as_deref(),
            Some("https://claude.ai/code?environment=env_01AB")
        );
        assert_eq!("Capacity: 1/4 · New sessions".connect_url(), None);
    }

    #[test]
    fn retries_back_off_up_to_five_minutes() {
        assert_eq!(Failures(1).retry_delay(), Duration::from_secs(5));
        assert_eq!(Failures(2).retry_delay(), Duration::from_secs(10));
        assert_eq!(Failures(4).retry_delay(), Duration::from_secs(40));
        assert_eq!(Failures(10).retry_delay(), LONGEST_RETRY_DELAY);
        assert_eq!(Failures(u32::MAX).retry_delay(), LONGEST_RETRY_DELAY);
    }

    #[test]
    fn exit_description_ends_with_the_last_lines() {
        let output = ServerOutput::default();
        output
            .0
            .lock()
            .expect("output lock")
            .extend(["one", "", "Workspace not trusted"].map(str::to_owned));
        let description = output.describe_exit(Err(io::Error::other("gone")));
        assert_eq!(description, "gone: one\nWorkspace not trusted");
    }

    #[tokio::test]
    async fn sessions_left_by_a_stopped_server_are_stopped_too() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_file = directory.path().join("session.pid");
        let state = fake_claude(
            &manager,
            directory.path(),
            &format!(
                "sleep 60 & echo $! > {}; echo 'Workspace not trusted'",
                pid_file.display()
            ),
        );
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Retrying).await;

        let session: i32 = fs::read_to_string(&pid_file)
            .expect("the session wrote its pid")
            .trim()
            .parse()
            .expect("pid is a number");
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("the deadline fits");
        while nix::sys::signal::kill(Pid::from_raw(session), None).is_ok() {
            assert!(
                Instant::now() < deadline,
                "session {session} is still running"
            );
            sleep(Duration::from_millis(50)).await;
        }

        state.remote_control.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn a_restart_starts_a_new_server() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let starts = directory.path().join("starts");
        let state = fake_claude(
            &manager,
            directory.path(),
            &format!(
                "echo started >> {}; echo 'https://claude.ai/code?environment=env_test'; exec sleep 60",
                starts.display()
            ),
        );
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Running).await;

        state.remote_control.restart();
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("the deadline fits");
        while fs::read_to_string(&starts)
            .unwrap_or_default()
            .lines()
            .count()
            < 2
        {
            assert!(Instant::now() < deadline, "the server did not start again");
            sleep(Duration::from_millis(50)).await;
        }
        wait_for(&state, ServerState::Running).await;

        state.remote_control.begin_shut_down();
        supervisor.await.expect("supervisor stops");
        assert!(state.remote_control.stopped.load(Ordering::SeqCst));
    }
}
