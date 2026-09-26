use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex as SyncMutex, PoisonError};
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, gethostname};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep, timeout, timeout_at};
use utoipa::ToSchema;

use super::agents::Agent;
use super::events::{Events, Topic};
use super::folders::Folder;
use super::login::{AgentCli, SignInStatus, StrExt};
use super::state::AppState;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(4);
const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const GROUP_POLL_INTERVAL: Duration = Duration::from_millis(100);
const RECHECK_INTERVAL: Duration = Duration::from_secs(60);
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(5);
const LONGEST_RETRY_DELAY: Duration = Duration::from_secs(300);
const HEALTHY_RUN: Duration = Duration::from_secs(600);
const USAGE_INTERVAL: Duration = Duration::from_secs(15);
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

/// How Claude Code's Remote Control servers run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct RemoteControlSettings {
    /// Serve /projects, and the folders chosen, to the Claude app while Claude Code is signed in.
    #[schema(required = true)]
    pub enabled: bool,
    /// The permission mode for sessions started from the Claude app, such as `auto`.
    #[schema(required = true)]
    pub permission_mode: String,
    /// The most sessions each server runs at once, from 1 to 32.
    #[schema(required = true, minimum = 1, maximum = 32)]
    pub capacity: u32,
    /// Whether repositories that appear in /projects start with their switch on.
    #[schema(required = true)]
    pub serve_repositories: bool,
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
            serve_repositories: true,
        }
    }
}

/// Where a Remote Control server is in its life.
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
    /// Stopping on purpose.
    Stopping,
}

/// The manager's view of one Remote Control server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RemoteControlStatus {
    pub state: ServerState,
    /// Where to continue in a browser, once connected.
    pub url: Option<String>,
    /// The end of the server's output when it last stopped unexpectedly.
    pub last_error: Option<String>,
    /// Unexpected stops since the manager started.
    pub restarts: u32,
    /// What the server and its sessions use, absent while no server process runs.
    pub usage: Option<ServerUsage>,
}

impl RemoteControlStatus {
    fn enter(&mut self, state: ServerState) {
        self.state = state;
        self.url = None;
        self.usage = None;
    }
}

/// What a running server and its sessions use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServerUsage {
    /// Sessions running now.
    pub sessions: u32,
    /// The most sessions the server runs at once.
    pub capacity: u32,
    /// Memory the server and its sessions use, with shared memory counted once.
    pub memory_bytes: u64,
}

/// Every Remote Control server, and where the Claude app lists them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RemoteControlOverview {
    /// The device the Claude app lists the servers under, the container's hostname.
    pub device: Option<String>,
    /// The server for /projects, waiting when it has not started yet.
    pub projects: RemoteControlStatus,
    /// The servers for folders in /projects, by folder name.
    pub folders: BTreeMap<String, RemoteControlStatus>,
}

/// The shared handle the API reads statuses from and signals changes through.
#[derive(Debug)]
pub struct RemoteControl {
    device: Option<String>,
    servers: SyncMutex<BTreeMap<PathBuf, RemoteControlStatus>>,
    events: Events,
    changes: watch::Sender<u64>,
    restarts: watch::Sender<u64>,
    shutdown: watch::Sender<bool>,
    stopped: watch::Sender<bool>,
}

impl RemoteControl {
    /// Publishes every status change to `events`.
    pub fn new(events: Events) -> Self {
        Self {
            device: gethostname()
                .ok()
                .and_then(|hostname| hostname.into_string().ok()),
            servers: SyncMutex::default(),
            events,
            changes: watch::Sender::new(0),
            restarts: watch::Sender::new(0),
            shutdown: watch::Sender::new(false),
            stopped: watch::Sender::new(false),
        }
    }

    /// The server for `directory`, absent when it has none.
    pub fn status_of(&self, directory: &Path) -> Option<RemoteControlStatus> {
        self.servers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(directory)
            .cloned()
    }

    pub fn overview(&self, projects: &Path) -> RemoteControlOverview {
        let servers = self.servers.lock().unwrap_or_else(PoisonError::into_inner);
        RemoteControlOverview {
            device: self.device.clone(),
            projects: servers.get(projects).cloned().unwrap_or_default(),
            folders: servers
                .iter()
                .filter(|(directory, _)| directory.parent() == Some(projects))
                .filter_map(|(directory, status)| {
                    let name = directory.file_name()?.to_str()?;
                    Some((name.to_owned(), status.clone()))
                })
                .collect(),
        }
    }

    /// Settings, folders or the install changed, so servers may need to start, stop or restart.
    pub fn reconsider(&self) {
        self.changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// The sign-in changed, which running servers only pick up by starting again.
    pub fn restart(&self) {
        self.restarts
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    pub fn begin_shut_down(&self) {
        self.shutdown.send_replace(true);
    }

    pub async fn wait_until_stopped(&self) {
        let mut stopped = self.stopped.subscribe();
        let longest = STOP_GRACE_PERIOD.saturating_add(OUTPUT_DRAIN_TIMEOUT);
        if timeout(longest, stopped.wait_for(|stopped| *stopped))
            .await
            .is_err()
        {
            tracing::warn!("Claude Remote Control did not stop in time");
        }
    }

    fn signals(&self, sign_in: watch::Receiver<Option<SignInStatus>>) -> Signals {
        Signals {
            sign_in,
            changes: self.changes.subscribe(),
            restarts: self.restarts.subscribe(),
            shutdown: self.shutdown.subscribe(),
        }
    }

    fn update(&self, directory: &Path, change: impl FnOnce(&mut RemoteControlStatus)) {
        let changed = {
            let mut servers = self.servers.lock().unwrap_or_else(PoisonError::into_inner);
            let before = servers.get(directory).cloned();
            let status = servers.entry(directory.to_path_buf()).or_default();
            change(status);
            before.as_ref() != Some(&*status)
        };
        if changed {
            self.events.publish(Topic::RemoteControl);
        }
    }

    fn forget(&self, directory: &Path) {
        let forgotten = self
            .servers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(directory);
        if forgotten.is_some() {
            self.events.publish(Topic::RemoteControl);
        }
    }
}

/// One task's view of the sign-in, change, restart and shutdown signals.
struct Signals {
    sign_in: watch::Receiver<Option<SignInStatus>>,
    changes: watch::Receiver<u64>,
    restarts: watch::Receiver<u64>,
    shutdown: watch::Receiver<bool>,
}

/// What woke a waiting task.
#[derive(Debug, PartialEq, Eq)]
enum Wake {
    Changed,
    ShutDown,
}

impl Signals {
    fn is_shutting_down(&self) -> bool {
        *self.shutdown.borrow()
    }

    async fn wait_for_change(&mut self, longest: Duration) -> Wake {
        if self.is_shutting_down() {
            return Wake::ShutDown;
        }
        tokio::select! {
            _ = self.changes.changed() => Wake::Changed,
            _ = self.restarts.changed() => Wake::Changed,
            _ = self.sign_in.changed() => Wake::Changed,
            () = self.shutdown.until_set() => Wake::ShutDown,
            () = sleep(longest) => Wake::Changed,
        }
    }
}

trait FlagExt {
    async fn until_set(&mut self);
}

impl FlagExt for watch::Receiver<bool> {
    async fn until_set(&mut self) {
        let _closed_counts_as_set = self.wait_for(|set| *set).await.map(|_| ());
    }
}

/// Which directory a server serves: all of /projects, or one folder in it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Served {
    Projects,
    Folder(String),
}

/// What should be running right now. `Unknown` until Claude first answers a sign-in check.
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
    permission_mode: String,
    capacity: u32,
}

impl Launch {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.claude);
        command
            .current_dir(&self.directory)
            .args(["remote-control", "--spawn", "same-dir", "--capacity"])
            .arg(self.capacity.to_string())
            .arg("--permission-mode")
            .arg(&self.permission_mode)
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
    /// Runs the /projects server, and one server for each folder chosen, until shutdown.
    pub async fn supervise_remote_control(self) {
        let mut signals = self
            .remote_control
            .signals(self.agent_checks.watch_sign_in(Agent::Claude));
        let projects = tokio::spawn(self.clone().supervise_server(Served::Projects));
        let mut folders: HashMap<String, JoinHandle<()>> = HashMap::new();
        loop {
            if !self.check_sign_in(&mut signals).await {
                break;
            }
            folders.retain(|_, supervisor| !supervisor.is_finished());
            for name in self.served_folder_names().await {
                folders.entry(name.clone()).or_insert_with(|| {
                    tokio::spawn(self.clone().supervise_server(Served::Folder(name)))
                });
            }
            if signals.wait_for_change(RECHECK_INTERVAL).await == Wake::ShutDown {
                break;
            }
        }
        let _stopped = projects.await;
        for supervisor in folders.into_values() {
            let _stopped = supervisor.await;
        }
        self.remote_control.stopped.send_replace(true);
    }

    /// Checks Claude Code's sign-in for every server when the last answer is older than the
    /// recheck interval. False when shutdown interrupted it.
    async fn check_sign_in(&self, signals: &mut Signals) -> bool {
        tokio::select! {
            () = self.agent_checks.check_sign_in(Agent::Claude, RECHECK_INTERVAL) => {}
            () = signals.shutdown.until_set() => return false,
        }
        signals.sign_in.mark_unchanged();
        true
    }

    /// Whether the folder's switch is on, the default for a repository not recorded yet.
    pub async fn serves_folder(&self, folder: &Folder) -> bool {
        let settings = self.settings.lock().await;
        let claude = &settings.agents.claude;
        claude
            .folders
            .get(&folder.name)
            .copied()
            .unwrap_or(claude.remote_control.serve_repositories && folder.git.is_some())
    }

    async fn served_folder_names(&self) -> Vec<String> {
        if !self
            .settings
            .lock()
            .await
            .agents
            .claude
            .remote_control
            .enabled
        {
            return Vec::new();
        }
        let Ok(folders) = self.folders().await else {
            return Vec::new();
        };
        let mut served = Vec::new();
        for folder in folders {
            if self.serves_folder(&folder).await {
                served.push(folder.name);
            }
        }
        served
    }

    async fn supervise_server(self, served: Served) {
        let directory = match &served {
            Served::Projects => self.projects.0.clone(),
            Served::Folder(name) => self.projects.folder(name),
        };
        let mut signals = self
            .remote_control
            .signals(self.agent_checks.watch_sign_in(Agent::Claude));
        let mut failures = Failures::default();
        loop {
            if signals.is_shutting_down() {
                return;
            }
            let run = match self.wanted_server(&served, &directory).await {
                Wanted::Off if served != Served::Projects => {
                    self.remote_control.forget(&directory);
                    return;
                }
                Wanted::Off => {
                    self.idle_server(&directory, ServerState::Off, &mut signals)
                        .await
                }
                Wanted::Waiting | Wanted::Unknown => {
                    self.idle_server(&directory, ServerState::Waiting, &mut signals)
                        .await
                }
                Wanted::Server(launch) => {
                    let started = Instant::now();
                    let end = self.run_server(&served, &launch, &mut signals).await;
                    if started.elapsed() >= HEALTHY_RUN {
                        failures = Failures::default();
                    }
                    end
                }
            };
            match run {
                RunEnd::Reconsidered => {}
                RunEnd::ShutDown => return,
                RunEnd::Failed(message) => {
                    failures = Failures(failures.0.saturating_add(1));
                    tracing::warn!(
                        "Claude Remote Control in {} stopped: {message}",
                        directory.display()
                    );
                    self.remote_control.update(&directory, |status| {
                        status.enter(ServerState::Retrying);
                        status.last_error = Some(message);
                        status.restarts = status.restarts.saturating_add(1);
                    });
                    if signals.wait_for_change(failures.retry_delay()).await == Wake::ShutDown {
                        return;
                    }
                }
            }
        }
    }

    async fn idle_server(
        &self,
        directory: &Path,
        state: ServerState,
        signals: &mut Signals,
    ) -> RunEnd {
        self.remote_control
            .update(directory, |status| status.enter(state));
        match signals.wait_for_change(RECHECK_INTERVAL).await {
            Wake::Changed => RunEnd::Reconsidered,
            Wake::ShutDown => RunEnd::ShutDown,
        }
    }

    async fn wanted_server(&self, served: &Served, directory: &Path) -> Wanted {
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
        if let Served::Folder(name) = served {
            let Some(folder) = self.projects.find(name) else {
                return Wanted::Off;
            };
            if !self.serves_folder(&folder).await {
                return Wanted::Off;
            }
        }
        if AgentCli::installed(Agent::Claude, &self.install_paths).is_err() {
            return Wanted::Waiting;
        }
        match self.agent_checks.sign_in(Agent::Claude) {
            Some(sign_in) if sign_in.logged_in => {}
            Some(_) => return Wanted::Waiting,
            None => return Wanted::Unknown,
        }
        Wanted::Server(Launch {
            claude: self.install_paths.command(Agent::Claude),
            claude_version: self.install_paths.installed_version(Agent::Claude),
            config_directory: self
                .install_paths
                .config_directory(Agent::Claude)
                .map(Path::to_path_buf),
            directory: directory.to_path_buf(),
            permission_mode: settings.permission_mode,
            capacity: settings.capacity,
        })
    }

    async fn still_wanted(&self, served: &Served, launch: &Launch) -> bool {
        match self.wanted_server(served, &launch.directory).await {
            Wanted::Server(wanted) => wanted == *launch,
            Wanted::Unknown => true,
            Wanted::Off | Wanted::Waiting => false,
        }
    }

    async fn run_server(&self, served: &Served, launch: &Launch, signals: &mut Signals) -> RunEnd {
        signals.restarts.mark_unchanged();
        if signals.is_shutting_down() {
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
        let mut usage = interval(USAGE_INTERVAL);
        usage.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            let connected = self
                .remote_control
                .status_of(&launch.directory)
                .is_some_and(|status| status.state == ServerState::Running);
            tokio::select! {
                exit = server.child.wait() => {
                    return RunEnd::Failed(server.finish(exit).await);
                }
                () = &mut connect_deadline, if !connected => {
                    server.stop().await;
                    return RunEnd::Failed("did not connect within 2 minutes".to_owned());
                }
                _ = signals.restarts.changed() => {
                    return self.stop_server(server, launch).await;
                }
                _ = signals.changes.changed() => {
                    if !self.still_wanted(served, launch).await {
                        return self.stop_server(server, launch).await;
                    }
                }
                _ = signals.sign_in.changed() => {
                    if !self.still_wanted(served, launch).await {
                        return self.stop_server(server, launch).await;
                    }
                }
                _ = recheck.tick() => {
                    if !self.still_wanted(served, launch).await {
                        return self.stop_server(server, launch).await;
                    }
                }
                _ = usage.tick() => server.note_usage(&self.remote_control, launch).await,
                () = signals.shutdown.until_set() => {
                    server.stop().await;
                    return RunEnd::ShutDown;
                }
            }
        }
    }

    async fn stop_server(&self, server: ServerRun, launch: &Launch) -> RunEnd {
        self.remote_control.update(&launch.directory, |status| {
            status.enter(ServerState::Stopping);
        });
        server.stop().await;
        RunEnd::Reconsidered
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
            readers.push(tokio::spawn(output.clone().collect(
                stdout,
                Arc::clone(remote_control),
                launch.directory.clone(),
            )));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(tokio::spawn(output.clone().collect(
                stderr,
                Arc::clone(remote_control),
                launch.directory.clone(),
            )));
        }
        remote_control.update(&launch.directory, |status| {
            status.enter(ServerState::Starting);
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

    async fn note_usage(&self, remote_control: &RemoteControl, launch: &Launch) {
        let Some(ProcessGroup(group)) = self.group else {
            return;
        };
        let capacity = launch.capacity;
        let usage = tokio::task::spawn_blocking(move || ProcessGroup(group).usage(capacity))
            .await
            .ok();
        remote_control.update(&launch.directory, |status| status.usage = usage);
    }

    async fn stop_reading(&mut self) {
        let deadline = Instant::now().checked_add(OUTPUT_DRAIN_TIMEOUT);
        for reader in &mut self.readers {
            let drained = match deadline {
                Some(deadline) => timeout_at(deadline, &mut *reader).await.is_ok(),
                None => false,
            };
            if !drained {
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

    /// The leader's children are its sessions, and every process in the group counts for memory.
    fn usage(&self, capacity: u32) -> ServerUsage {
        let mut usage = ServerUsage {
            sessions: 0,
            capacity,
            memory_bytes: 0,
        };
        for process in Process::all() {
            let Some(ProcessStat { parent, group }) = process.stat() else {
                continue;
            };
            if group != self.0 {
                continue;
            }
            if parent == self.0 {
                usage.sessions = usage.sessions.saturating_add(1);
            }
            if let Some(memory) = process.proportional_memory() {
                usage.memory_bytes = usage.memory_bytes.saturating_add(memory);
            }
        }
        usage
    }
}

/// A process as /proc describes it.
struct Process(PathBuf);

struct ProcessStat {
    parent: Pid,
    group: Pid,
}

impl Process {
    fn all() -> impl Iterator<Item = Self> {
        fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.parse::<u32>().is_ok())
            })
            .map(|entry| Self(entry.path()))
    }

    fn stat(&self) -> Option<ProcessStat> {
        let stat = fs::read_to_string(self.0.join("stat")).ok()?;
        let (_, after_name) = stat.rsplit_once(')')?;
        let mut fields = after_name.split_whitespace().skip(1);
        let parent = fields.next()?.parse().ok()?;
        let group = fields.next()?.parse().ok()?;
        Some(ProcessStat {
            parent: Pid::from_raw(parent),
            group: Pid::from_raw(group),
        })
    }

    /// Resident memory with each shared page split between the processes sharing it.
    fn proportional_memory(&self) -> Option<u64> {
        let rollup = fs::read_to_string(self.0.join("smaps_rollup")).ok()?;
        let kilobytes: u64 = rollup
            .lines()
            .find_map(|line| line.strip_prefix("Pss:"))?
            .trim()
            .strip_suffix("kB")?
            .trim()
            .parse()
            .ok()?;
        kilobytes.checked_mul(1024)
    }
}

/// The last lines a server printed, shared by its stdout and stderr readers.
#[derive(Debug, Clone, Default)]
struct ServerOutput(Arc<SyncMutex<VecDeque<String>>>);

impl ServerOutput {
    async fn collect(
        self,
        stream: impl AsyncRead + Unpin,
        remote_control: Arc<RemoteControl>,
        directory: PathBuf,
    ) {
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
                let mut newly_connected = false;
                remote_control.update(&directory, |status| {
                    if !matches!(status.state, ServerState::Starting | ServerState::Running) {
                        return;
                    }
                    newly_connected = status.state != ServerState::Running
                        || status.url.as_deref() != Some(url.as_str());
                    status.state = ServerState::Running;
                    status.url = Some(url.clone());
                });
                if newly_connected {
                    tracing::info!(
                        "Claude Remote Control in {} is connected: {url}",
                        directory.display()
                    );
                }
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
    use futures_util::StreamExt;

    use super::*;
    use crate::manager::api::test_support::TestManager;
    use crate::manager::events::ManagerEvent;
    use crate::manager::folders::ProjectsDirectory;
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
        state.projects = ProjectsDirectory(served);
        state
    }

    async fn wait_for(state: &AppState, wanted: ServerState) -> RemoteControlStatus {
        wait_in(state, &state.projects.0, wanted).await
    }

    async fn wait_in(
        state: &AppState,
        directory: &Path,
        wanted: ServerState,
    ) -> RemoteControlStatus {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("the deadline fits");
        loop {
            let status = state
                .remote_control
                .status_of(directory)
                .unwrap_or_default();
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
            permission_mode: "auto".to_owned(),
            capacity: 4,
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
    async fn a_group_counts_the_leaders_children_and_everyones_memory() {
        let mut leader = Command::new("sh")
            .args(["-c", "sleep 30 & sleep 30 & wait"])
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .expect("group starts");
        let group = ProcessGroup(Pid::from_raw(
            leader
                .id()
                .and_then(|id| i32::try_from(id).ok())
                .expect("leader has a pid"),
        ));
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("the deadline fits");
        while group.usage(4).sessions < 2 {
            assert!(Instant::now() < deadline, "the group did not start");
            sleep(Duration::from_millis(20)).await;
        }
        let usage = group.usage(4);
        assert_eq!((usage.sessions, usage.capacity), (2, 4));
        assert!(usage.memory_bytes > 0);

        group.terminate(&mut leader).await;
        assert_eq!(group.usage(4).memory_bytes, 0);
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
        assert!(*state.remote_control.stopped.borrow());
    }

    #[tokio::test]
    async fn signing_in_starts_a_waiting_server_at_once() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let marker = directory.path().join("signed-in");
        let state = fake_claude(
            &manager,
            directory.path(),
            "echo 'https://claude.ai/code?environment=env_test'; exec sleep 60",
        );
        let script = directory.path().join("claude");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  auth) if [ -f {marker} ]; then echo '{SIGNED_IN}'; else echo '{{\"loggedIn\":false}}'; fi ;;\n  remote-control) echo 'https://claude.ai/code?environment=env_test'; exec sleep 60 ;;\nesac\n",
                marker = marker.display()
            ),
        )
        .expect("script is written");
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Waiting).await;

        fs::write(&marker, "").expect("marker is written");
        state.agent_checks.refresh(Agent::Claude).await;
        wait_for(&state, ServerState::Running).await;

        state.remote_control.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn a_chosen_folder_gets_its_own_server() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = fake_claude(
            &manager,
            directory.path(),
            "echo \"https://claude.ai/code?environment=env_$(basename \"$PWD\")\"; exec sleep 60",
        );
        let app = state.projects.0.join("app");
        fs::create_dir_all(&app).expect("folder is created");
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Running).await;
        assert_eq!(state.remote_control.status_of(&app), None);

        state
            .choose_to_serve_folder("app", true)
            .await
            .expect("the choice is saved");
        let served = wait_in(&state, &app, ServerState::Running).await;
        assert_eq!(
            served.url.as_deref(),
            Some("https://claude.ai/code?environment=env_app")
        );

        state
            .choose_to_serve_folder("app", false)
            .await
            .expect("the choice is saved");
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("the deadline fits");
        while state.remote_control.status_of(&app).is_some() {
            assert!(Instant::now() < deadline, "the folder server did not stop");
            sleep(Duration::from_millis(50)).await;
        }

        state.remote_control.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn a_folder_server_waiting_from_the_start_is_published() {
        let manager = TestManager::new();
        let state = manager.state.clone();
        let app = state.projects.folder("app");
        fs::create_dir_all(&app).expect("folder is created");
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Waiting).await;
        let mut events = Box::pin(state.events.stream());
        events.next().await;

        state
            .choose_to_serve_folder("app", true)
            .await
            .expect("the choice is saved");
        wait_in(&state, &app, ServerState::Waiting).await;
        let mut published = Vec::new();
        while let Ok(Some(event)) = timeout(Duration::from_millis(100), events.next()).await {
            if let ManagerEvent::Changed { topic, .. } = event {
                published.push(topic);
            }
        }
        assert!(published.contains(&Topic::RemoteControl), "{published:?}");

        state.remote_control.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn a_server_being_stopped_says_so_and_every_change_is_published() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = fake_claude(
            &manager,
            directory.path(),
            "trap 'sleep 1; exit' TERM; echo \"https://claude.ai/code?environment=env_$(basename \"$PWD\")\"; sleep 60 & wait",
        );
        let mut events = Box::pin(state.events.stream());
        let app = state.projects.0.join("app");
        fs::create_dir_all(&app).expect("folder is created");
        state
            .choose_to_serve_folder("app", true)
            .await
            .expect("the choice is saved");
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Running).await;
        wait_in(&state, &app, ServerState::Running).await;
        let overview = state.remote_control.overview(&state.projects.0);
        assert_eq!(overview.projects.state, ServerState::Running);
        assert_eq!(
            overview
                .folders
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["app"]
        );

        state
            .choose_to_serve_folder("app", false)
            .await
            .expect("the choice is saved");
        wait_in(&state, &app, ServerState::Stopping).await;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("the deadline fits");
        while state.remote_control.status_of(&app).is_some() {
            assert!(Instant::now() < deadline, "the folder server did not stop");
            sleep(Duration::from_millis(50)).await;
        }

        let mut published = Vec::new();
        while let Ok(Some(event)) = timeout(Duration::from_millis(100), events.next()).await {
            if let ManagerEvent::Changed { topic, .. } = event {
                published.push(topic);
            }
        }
        for expected in [Topic::Folders, Topic::RemoteControl] {
            assert!(
                published.contains(&expected),
                "{expected:?} missing from {published:?}"
            );
        }

        state.remote_control.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }
}
