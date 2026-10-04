use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, gethostname};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use time::Time;
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep, timeout};
use utoipa::ToSchema;

use super::agents::Agent;
use super::codex_remote::CodexRemoteStatus;
use super::events::{Events, Topic};
use super::folders::Folder;
use super::login::AgentCli;
use super::processes::ProcessFamily;
use super::state::AppState;
use super::supervision::{
    Failure, Failures, FlagExt, LineWatcher, OUTPUT_DRAIN_TIMEOUT, PendingUpdate, Published,
    RECHECK_INTERVAL, RunEnd, ServerLog, ServerOutput, Signals, Supervision,
    UPDATE_RESTART_DEADLINE, USAGE_INTERVAL, UpdateWait, Verdict, Wake, Wanted,
};
use crate::path_ext::PathExt;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
const CONFIG_LOCK_STALE: Duration = Duration::from_secs(10);
const CONFIG_LOCK_WAIT: Duration = Duration::from_secs(15);
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(35);
const GROUP_POLL_INTERVAL: Duration = Duration::from_millis(100);
const STARTING_USAGE_INTERVAL: Duration = Duration::from_secs(1);
const CLAUDE_LOG_STAMP: &[BorrowedFormatItem<'_>] = format_description!("[hour]:[minute]:[second]");
/// The modes `claude remote-control --permission-mode` takes, `manual` being `default`.
const PERMISSION_MODES: [&str; 7] = [
    "acceptEdits",
    "auto",
    "bypassPermissions",
    "default",
    "dontAsk",
    "manual",
    "plan",
];
const SESSION_ARGUMENT: &str = "--sdk-url";
const UNKNOWN_PERMISSION_MODE: &str = "Claude Code has no permission mode by that name";
/// Variables that make `claude remote-control` refuse to start. Ones set in a settings.json
/// `env` block still can.
const VARIABLES_THAT_DISABLE_REMOTE_CONTROL: [&str; 15] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_UNIX_SOCKET",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_REMOTE",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "DISABLE_GROWTHBOOK",
];

/// How Claude Code's Remote Control servers run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct RemoteControlSettings {
    /// Serve /home/dev/projects, and the folders chosen, to the Claude app while Claude Code is
    /// signed in.
    #[schema(required = true)]
    pub enabled: bool,
    /// The permission mode for sessions started from the Claude app, such as `auto`.
    #[schema(required = true)]
    pub permission_mode: String,
    /// The most sessions each server runs at once, at least 1. Absent uses Claude Code's
    /// default.
    #[schema(required = true, minimum = 1)]
    pub capacity: Option<u32>,
    /// Whether repositories that appear in /home/dev/projects start with their switch on.
    #[schema(required = true)]
    pub serve_repositories: bool,
    /// Where new sessions in repositories work, unless a folder chooses.
    #[schema(required = true)]
    pub spawn: SpawnMode,
}

impl RemoteControlSettings {
    /// `None` when the settings can be passed to Claude, or why not.
    pub fn problem(&self) -> Option<&'static str> {
        if !PERMISSION_MODES.contains(&self.permission_mode.trim()) {
            Some(UNKNOWN_PERMISSION_MODE)
        } else if self.capacity.is_some_and(|capacity| capacity < 1) {
            Some("capacity must be at least 1")
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
            capacity: None,
            serve_repositories: true,
            spawn: SpawnMode::Worktree,
        }
    }
}

/// Where sessions started from the Claude app work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SpawnMode {
    /// In the served folder.
    #[default]
    SameDir,
    /// Each in its own git worktree in `.claude/worktrees`, only in a repository.
    Worktree,
}

impl SpawnMode {
    fn argument(self) -> &'static str {
        match self {
            Self::SameDir => "same-dir",
            Self::Worktree => "worktree",
        }
    }
}

/// A folder's own Claude Code options. Absent ones follow the Settings page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClaudeOptions {
    /// Where new sessions work. Outside a repository they always work in the folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn: Option<SpawnMode>,
    /// The permission mode for the folder's sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// The most sessions the folder's server runs at once, at least 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 1)]
    pub capacity: Option<u32>,
}

impl ClaudeOptions {
    /// `None` when Claude takes the options, or why not.
    pub fn problem(&self) -> Option<&'static str> {
        if self
            .permission_mode
            .as_deref()
            .map(str::trim)
            .is_some_and(|mode| !mode.is_empty() && !PERMISSION_MODES.contains(&mode))
        {
            Some(UNKNOWN_PERMISSION_MODE)
        } else if self.capacity.is_some_and(|capacity| capacity < 1) {
            Some("capacity must be at least 1")
        } else {
            None
        }
    }

    /// An empty permission mode follows the Settings page.
    pub fn trimmed(mut self) -> Self {
        self.permission_mode = self
            .permission_mode
            .map(|mode| mode.trim().to_owned())
            .filter(|mode| !mode.is_empty());
        self
    }
}

/// Where a Remote Control server is in its life.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    /// Turned off in the settings.
    Off,
    /// Waiting for the agent to be installed and signed in.
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
    /// The end of the server's output when it last stopped unexpectedly, until it connects again.
    pub last_error: Option<String>,
    /// What that output says went wrong, absent when it matches no known problem.
    pub problem: Option<ServerProblem>,
    /// Unexpected stops since the manager started.
    pub restarts: u32,
    /// What the server and its sessions use, absent while no server process runs.
    pub usage: Option<ServerUsage>,
    /// A newer Claude Code the running server restarts on, absent when it runs the installed one.
    pub update: Option<PendingUpdate>,
}

impl RemoteControlStatus {
    fn enter(&mut self, state: ServerState) {
        self.state = state;
        self.url = None;
        self.usage = None;
        self.update = None;
    }

    fn connect(&mut self, url: String) {
        self.state = ServerState::Running;
        self.url = Some(url);
        self.last_error = None;
        self.problem = None;
    }
}

/// A known reason a server stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServerProblem {
    /// Claude rejected Claude Code's sign-in, or it is missing or not a claude.ai sign-in.
    SignIn,
    /// A Claude Code setting or environment variable stops Remote Control.
    BlockedBySetting,
    /// Remote Control is off for the account, which a new sign-in rechecks.
    NotEnabled,
    /// The organization does not allow Remote Control.
    NotAllowed,
    /// Claude could not be reached or kept failing.
    Offline,
}

impl ServerProblem {
    /// Parts of the messages Claude Code 2.1 prints, checked in this order.
    const MESSAGES: [(&str, Self); 27] = [
        (
            "so this session is using API-key auth",
            Self::BlockedBySetting,
        ),
        ("using CLAUDE_CODE_OAUTH_TOKEN auth", Self::BlockedBySetting),
        ("requires a full-scope login token", Self::BlockedBySetting),
        ("ANTHROPIC_UNIX_SOCKET is set", Self::BlockedBySetting),
        ("requires feature-flag evaluation", Self::BlockedBySetting),
        (
            "only available when using Claude via api.anthropic.com",
            Self::BlockedBySetting,
        ),
        (
            "not available inside a cloud session",
            Self::BlockedBySetting,
        ),
        ("You must be logged in to use Remote Control", Self::SignIn),
        ("Unable to determine your organization", Self::SignIn),
        ("Authentication failed (401)", Self::SignIn),
        ("requires a claude.ai subscription", Self::SignIn),
        ("requires claude.ai subscription auth", Self::SignIn),
        ("this device is not enrolled", Self::SignIn),
        ("isn't enabled for this account", Self::NotEnabled),
        ("disabled by your organization's policy", Self::NotAllowed),
        (
            "environments are not available for your account",
            Self::NotAllowed,
        ),
        ("Access denied (403)", Self::NotAllowed),
        (
            "may not be available for this organization",
            Self::NotAllowed,
        ),
        ("Server unreachable for", Self::Offline),
        ("Persistent errors for", Self::Offline),
        ("service was unreachable", Self::Offline),
        (
            "Couldn't verify your organization's Remote Control policy",
            Self::Offline,
        ),
        (
            "Couldn't verify your organization's policy for",
            Self::Offline,
        ),
        ("EAI_AGAIN", Self::Offline),
        ("ENOTFOUND", Self::Offline),
        ("ECONNREFUSED", Self::Offline),
        ("ETIMEDOUT", Self::Offline),
    ];

    fn in_output(lines: &[&str]) -> Option<Self> {
        Self::MESSAGES.into_iter().find_map(|(message, problem)| {
            lines
                .iter()
                .any(|line| line.contains(message))
                .then_some(problem)
        })
    }
}

/// What a running server and its sessions use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServerUsage {
    /// Sessions running now.
    pub sessions: u32,
    /// The most sessions the server runs at once, absent when Claude Code's default applies.
    pub capacity: Option<u32>,
    /// Memory the server, its sessions and their commands use, with shared memory counted once.
    pub memory_bytes: u64,
}

/// Every Remote Control server, and where the Claude app lists them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RemoteControlOverview {
    /// The device the Claude app lists the servers under, the container's hostname.
    pub device: Option<String>,
    /// The server for /home/dev/projects, waiting when it has not started yet.
    pub projects: RemoteControlStatus,
    /// The servers for folders in /home/dev/projects, by folder name.
    pub folders: BTreeMap<String, RemoteControlStatus>,
    /// The server that serves this box to the ChatGPT app.
    pub codex: CodexRemoteStatus,
}

/// The shared handle the API reads statuses from and signals changes through.
#[derive(Debug)]
pub struct RemoteControl {
    device: Option<String>,
    servers: Published<BTreeMap<PathBuf, RemoteControlStatus>>,
    logs: PathBuf,
    pub supervision: Supervision,
}

impl RemoteControl {
    /// How long the supervisor may take to stop its servers.
    pub const LONGEST_STOP: Duration = STOP_GRACE_PERIOD.saturating_add(OUTPUT_DRAIN_TIMEOUT);

    /// Publishes every status change to `events` and keeps each server's logs under `logs`.
    pub fn new(events: Events, logs: PathBuf) -> Self {
        Self {
            device: gethostname()
                .ok()
                .and_then(|hostname| hostname.into_string().ok()),
            servers: Published::new(BTreeMap::new(), events, Topic::RemoteControl),
            logs,
            supervision: Supervision::default(),
        }
    }

    /// The server for `directory`, absent when it has none.
    pub fn status_of(&self, directory: &Path) -> Option<RemoteControlStatus> {
        self.servers.read(|servers| servers.get(directory).cloned())
    }

    /// Waits until `directory` has no server. False when `longest` passed first.
    pub async fn wait_until_gone(&self, directory: &Path, longest: Duration) -> bool {
        timeout(longest, async {
            while self.status_of(directory).is_some() {
                sleep(GROUP_POLL_INTERVAL).await;
            }
        })
        .await
        .is_ok()
    }

    pub fn overview(&self, projects: &Path, codex: CodexRemoteStatus) -> RemoteControlOverview {
        self.servers.read(|servers| RemoteControlOverview {
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
            codex,
        })
    }

    pub fn log(&self, served: &Served) -> ServerLog {
        ServerLog(match served {
            Served::Projects => self.logs.join("projects"),
            Served::Folder(name) => self.logs.join("folders").join(name),
        })
    }

    fn update(&self, directory: &Path, change: impl FnOnce(&mut RemoteControlStatus)) {
        self.servers
            .update(|servers| change(servers.entry(directory.to_path_buf()).or_default()));
    }

    /// Removes the logs of folders that are not in the projects directory any more.
    fn remove_logs_of_gone_folders(&self, present: &[Folder]) -> io::Result<()> {
        for log in self.logs.join("folders").entries_or_empty()? {
            let gone = log
                .file_name()
                .is_none_or(|name| !present.iter().any(|folder| name == folder.name.as_str()));
            if gone {
                log.remove_if_present()?;
            }
        }
        Ok(())
    }

    fn forget(&self, directory: &Path) {
        self.servers.update(|servers| servers.remove(directory));
    }
}

/// Which directory a server serves: the whole projects directory, or one folder in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Served {
    Projects,
    Folder(String),
}

/// One `claude remote-control` process to start. A new Claude version is a different launch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Launch {
    claude: PathBuf,
    claude_version: Option<String>,
    config_directory: Option<PathBuf>,
    directory: PathBuf,
    spawn: SpawnMode,
    log: ServerLog,
    permission_mode: String,
    capacity: Option<u32>,
}

impl Launch {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.claude);
        command
            .current_dir(&self.directory)
            .args(["remote-control", "--spawn", self.spawn.argument()])
            .args(
                self.capacity
                    .map(|capacity| ["--capacity".to_owned(), capacity.to_string()])
                    .into_iter()
                    .flatten(),
            )
            .arg("--permission-mode")
            .arg(&self.permission_mode)
            .arg("--debug-file")
            .arg(self.log.debug_file())
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
    async fn accept_prompts(&self) -> Result<(), String> {
        let Some(config_directory) = &self.config_directory else {
            return Ok(());
        };
        ClaudeGlobalConfig(config_directory.join(".claude.json"))
            .accept_remote_control_in(&self.directory)
            .await
            .map_err(|error| format!("could not prepare Claude's settings: {error}"))
    }

    /// The same server on a different Claude Code version.
    fn is_update_of(&self, running: &Self) -> bool {
        self.claude_version != running.claude_version
            && *self
                == Self {
                    claude_version: self.claude_version.clone(),
                    ..running.clone()
                }
    }
}

/// `$CLAUDE_CONFIG_DIR/.claude.json`, which Claude also writes. Unknown keys are kept.
struct ClaudeGlobalConfig(PathBuf);

impl ClaudeGlobalConfig {
    /// Writes only when something changes, holding Claude's lock on the file.
    async fn accept_remote_control_in(&self, directory: &Path) -> io::Result<()> {
        let _lock = ConfigLock::take(&self.0).await?;
        let original = match tokio::fs::read(&self.0).await {
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
        let staging = self.0.with_extension("json.ezra");
        tokio::fs::write(&staging, serde_json::to_vec_pretty(&config)?).await?;
        tokio::fs::set_permissions(&staging, fs::Permissions::from_mode(0o600)).await?;
        tokio::fs::rename(&staging, &self.0).await
    }
}

/// The lock Claude takes on a config file: a directory next to it, stale once unchanged for 10 s.
struct ConfigLock(PathBuf);

impl ConfigLock {
    async fn take(file: &Path) -> io::Result<Self> {
        if let Some(directory) = file.parent() {
            tokio::fs::create_dir_all(directory).await?;
        }
        let mut lock = file.as_os_str().to_owned();
        lock.push(".lock");
        let lock = PathBuf::from(lock);
        let deadline = Instant::now().checked_add(CONFIG_LOCK_WAIT);
        loop {
            match tokio::fs::create_dir(&lock).await {
                Ok(()) => return Ok(Self(lock)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            let stale = tokio::fs::metadata(&lock)
                .await
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > CONFIG_LOCK_STALE);
            if stale {
                let _already_gone = tokio::fs::remove_dir(&lock).await;
                continue;
            }
            if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{} stayed locked", file.display()),
                ));
            }
            sleep(GROUP_POLL_INTERVAL).await;
        }
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _already_gone = fs::remove_dir(&self.0);
    }
}

impl AppState {
    /// Runs the projects directory's server, and one server for each folder chosen, until shutdown.
    pub async fn supervise_remote_control(self) {
        let mut signals = self
            .remote_control
            .supervision
            .signals(self.agent_checks.watch_sign_in(Agent::Claude));
        let projects = tokio::spawn(self.clone().supervise_server(Served::Projects));
        let mut folders: HashMap<String, JoinHandle<()>> = HashMap::new();
        loop {
            if !self.check_sign_in(Agent::Claude, &mut signals).await {
                break;
            }
            folders.retain(|_, supervisor| !supervisor.is_finished());
            if let Ok(present) = self.folders().await {
                if let Err(error) = self.remote_control.remove_logs_of_gone_folders(&present) {
                    tracing::warn!("could not remove the logs of folders that are gone: {error}");
                }
                for name in self.served_folder_names(present).await {
                    folders.entry(name.clone()).or_insert_with(|| {
                        tokio::spawn(self.clone().supervise_server(Served::Folder(name)))
                    });
                }
            }
            if signals.wait_for_change(RECHECK_INTERVAL).await == Wake::ShutDown {
                break;
            }
        }
        let _stopped = projects.await;
        for supervisor in folders.into_values() {
            let _stopped = supervisor.await;
        }
        self.remote_control.supervision.mark_stopped();
    }

    async fn served_folder_names(&self, folders: Vec<Folder>) -> Vec<String> {
        let settings = self.settings.lock().await;
        let claude = &settings.agents.claude;
        if !claude.remote_control.enabled {
            return Vec::new();
        }
        folders
            .into_iter()
            .filter(|folder| claude.folder_choice(folder).serve)
            .map(|folder| folder.name)
            .collect()
    }

    async fn supervise_server(self, served: Served) {
        let directory = match &served {
            Served::Projects => self.projects.0.clone(),
            Served::Folder(name) => self.projects.folder(name),
        };
        let mut signals = self
            .remote_control
            .supervision
            .signals(self.agent_checks.watch_sign_in(Agent::Claude));
        let mut failures = Failures::default();
        loop {
            if signals.is_shutting_down() {
                return;
            }
            let run = match self.wanted_server(&served, &directory).await {
                Wanted::Off if served != Served::Projects => {
                    self.remote_control.forget(&directory);
                    if !directory.is_dir()
                        && let Err(error) = self.remote_control.log(&served).0.remove_if_present()
                    {
                        tracing::warn!(
                            "could not remove the log of {}: {error}",
                            directory.display()
                        );
                    }
                    return;
                }
                Wanted::Off => {
                    self.idle_server(&directory, ServerState::Off, &mut signals)
                        .await
                }
                Wanted::Waiting(()) | Wanted::Unknown => {
                    self.idle_server(&directory, ServerState::Waiting, &mut signals)
                        .await
                }
                Wanted::Server(launch) => {
                    let started = Instant::now();
                    let end = self.run_server(&served, &launch, &mut signals).await;
                    failures.forget_after_healthy_run(started);
                    end
                }
            };
            match run {
                RunEnd::Reconsidered => {}
                RunEnd::ShutDown => return,
                RunEnd::Failed(Failure { message, problem }) => {
                    failures.add_one();
                    tracing::warn!(
                        "Claude Remote Control in {} stopped: {message}",
                        directory.display()
                    );
                    self.remote_control.update(&directory, |status| {
                        status.enter(ServerState::Retrying);
                        status.last_error = Some(message);
                        status.problem = problem;
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
    ) -> RunEnd<ServerProblem> {
        self.remote_control
            .update(directory, |status| status.enter(state));
        match signals.wait_for_change(RECHECK_INTERVAL).await {
            Wake::Changed => RunEnd::Reconsidered,
            Wake::ShutDown => RunEnd::ShutDown,
        }
    }

    async fn wanted_server(&self, served: &Served, directory: &Path) -> Wanted<Launch, ()> {
        let folder = match served {
            Served::Projects => None,
            Served::Folder(name) => match self.projects.find(name) {
                Some(folder) => Some(folder),
                None => return Wanted::Off,
            },
        };
        let (settings, choice) = {
            let settings = self.settings.lock().await;
            let claude = &settings.agents.claude;
            (
                claude.remote_control.clone(),
                folder.map(|folder| (claude.folder_choice(&folder), folder.git.is_some())),
            )
        };
        let (choice, in_repository) = choice.unzip();
        let in_repository = in_repository.unwrap_or(false);
        if !settings.enabled || choice.as_ref().is_some_and(|choice| !choice.serve) {
            return Wanted::Off;
        }
        if AgentCli::installed(Agent::Claude, &self.install_paths).is_err() {
            return Wanted::Waiting(());
        }
        match self.agent_checks.sign_in(Agent::Claude) {
            Some(sign_in) if sign_in.logged_in => {}
            Some(_) => return Wanted::Waiting(()),
            None => return Wanted::Unknown,
        }
        let options = choice.map(|choice| choice.options).unwrap_or_default();
        Wanted::Server(Launch {
            claude: self.install_paths.command(Agent::Claude),
            claude_version: self.install_paths.installed_version(Agent::Claude),
            config_directory: self
                .install_paths
                .config_directory(Agent::Claude)
                .map(Path::to_path_buf),
            directory: directory.to_path_buf(),
            spawn: if in_repository {
                options.spawn.unwrap_or(settings.spawn)
            } else {
                SpawnMode::SameDir
            },
            log: self.remote_control.log(served),
            permission_mode: options.permission_mode.unwrap_or(settings.permission_mode),
            capacity: options.capacity.or(settings.capacity),
        })
    }

    async fn verdict(&self, served: &Served, launch: &Launch) -> Verdict {
        match self.wanted_server(served, &launch.directory).await {
            Wanted::Server(wanted) if wanted == *launch => Verdict::Keep,
            Wanted::Server(wanted) if wanted.is_update_of(launch) => {
                Verdict::Update(wanted.claude_version.unwrap_or_default())
            }
            Wanted::Unknown => Verdict::Unsure,
            Wanted::Server(_) | Wanted::Off | Wanted::Waiting(()) => Verdict::Stop,
        }
    }

    async fn run_server(
        &self,
        served: &Served,
        launch: &Launch,
        signals: &mut Signals,
    ) -> RunEnd<ServerProblem> {
        signals.restarts.mark_unchanged();
        if signals.is_shutting_down() {
            return RunEnd::ShutDown;
        }
        if let Err(message) = launch.accept_prompts().await {
            return RunEnd::Failed(message.into());
        }
        if let Err(error) = launch.log.prepare().await {
            tracing::warn!("could not prepare {}: {error}", launch.log.0.display());
        }
        let mut server = match ServerRun::start(launch, &self.remote_control) {
            Ok(server) => server,
            Err(error) => return RunEnd::Failed(format!("could not start: {error}").into()),
        };
        if *served == Served::Projects {
            tokio::spawn(self.clone().read_claude_models());
        }
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
        let mut starting_usage = interval(STARTING_USAGE_INTERVAL);
        starting_usage.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut update: Option<UpdateWait> = None;
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
                    return RunEnd::Failed("did not connect within 2 minutes".to_owned().into());
                }
                _ = signals.restarts.changed() => {
                    return self.stop_server(server, launch).await;
                }
                _ = signals.changes.changed() => {}
                _ = signals.sign_in.changed() => {}
                _ = recheck.tick() => {
                    if let Err(error) = launch.log.prepare().await {
                        tracing::warn!("could not prune {}: {error}", launch.log.0.display());
                    }
                }
                _ = usage.tick() => {
                    if update.is_none() {
                        server.note_usage(&self.remote_control, launch).await;
                        continue;
                    }
                }
                _ = starting_usage.tick(), if !connected => {
                    server.note_usage(&self.remote_control, launch).await;
                    continue;
                }
                () = signals.shutdown.until_set() => {
                    server.stop().await;
                    return RunEnd::ShutDown;
                }
            }
            match self.verdict(served, launch).await {
                Verdict::Keep => {
                    if update.take().is_some() {
                        self.remote_control
                            .update(&launch.directory, |status| status.update = None);
                    }
                }
                Verdict::Unsure => {}
                Verdict::Stop => return self.stop_server(server, launch).await,
                Verdict::Update(version) => {
                    let usage = server.note_usage(&self.remote_control, launch).await;
                    let waiting = update.get_or_insert_with(|| {
                        UpdateWait::starting_now(version.clone(), UPDATE_RESTART_DEADLINE)
                    });
                    waiting.version = version;
                    if waiting.is_due(usage.is_some_and(|usage| usage.sessions > 0)) {
                        tracing::info!(
                            "restarting Claude Remote Control in {} on Claude Code {}",
                            launch.directory.display(),
                            waiting.version
                        );
                        let end = self.stop_server(server, launch).await;
                        self.remove_unused_versions(Agent::Claude).await;
                        return end;
                    }
                    let pending = waiting.pending();
                    self.remote_control
                        .update(&launch.directory, |status| status.update = Some(pending));
                }
            }
        }
    }

    async fn stop_server(&self, server: ServerRun, launch: &Launch) -> RunEnd<ServerProblem> {
        self.remote_control.update(&launch.directory, |status| {
            status.enter(ServerState::Stopping);
        });
        server.stop().await;
        RunEnd::Reconsidered
    }
}

/// A running server, its output, and the sessions in its process group.
struct ServerRun {
    child: Child,
    group: Option<ProcessGroup>,
    output: ServerOutput,
}

impl ServerRun {
    fn start(launch: &Launch, remote_control: &Arc<RemoteControl>) -> io::Result<Self> {
        let mut child = launch.command().spawn()?;
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .map(|id| ProcessGroup(Pid::from_raw(id)));
        let output = ServerOutput::read(&mut child, &launch.log, || ClaudeLines {
            remote_control: Arc::clone(remote_control),
            directory: launch.directory.clone(),
            in_debug_dump: false,
            redraws: RedrawFilter::default(),
        });
        remote_control.update(&launch.directory, |status| {
            status.enter(ServerState::Starting);
        });
        Ok(Self {
            child,
            group,
            output,
        })
    }

    async fn stop(mut self) {
        if let Some(group) = &self.group {
            group.terminate(&mut self.child).await;
        }
        let _already_gone = self.child.kill().await;
        self.output.stop_reading().await;
    }

    /// After the server exited by itself: stops the sessions it left and describes the exit.
    async fn finish(mut self, exit: io::Result<ExitStatus>) -> Failure<ServerProblem> {
        if let Some(group) = &self.group {
            group.terminate(&mut self.child).await;
        }
        self.output.stop_reading().await;
        self.output.describe_exit(exit, ServerProblem::in_output)
    }

    async fn note_usage(
        &self,
        remote_control: &RemoteControl,
        launch: &Launch,
    ) -> Option<ServerUsage> {
        let ProcessGroup(group) = self.group.as_ref()?;
        let (group, capacity) = (*group, launch.capacity);
        let usage = tokio::task::spawn_blocking(move || ProcessGroup(group).usage(capacity))
            .await
            .ok();
        remote_control.update(&launch.directory, |status| status.usage = usage);
        usage
    }
}

/// A server and its sessions, which share its process group. The commands sessions run have
/// groups of their own.
pub struct ProcessGroup(pub Pid);

impl ProcessGroup {
    /// SIGTERM, then SIGKILL for whatever is left after the grace period.
    pub async fn terminate(&self, leader: &mut Child) {
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

    /// Sessions are the leader's live `--sdk-url` children. Memory counts every process it
    /// started.
    fn usage(&self, capacity: Option<u32>) -> ServerUsage {
        let family = ProcessFamily::of(self.0);
        let sessions = family
            .children()
            .filter(|process| {
                process
                    .arguments()
                    .iter()
                    .any(|argument| argument == SESSION_ARGUMENT)
            })
            .count();
        ServerUsage {
            sessions: u32::try_from(sessions).unwrap_or(u32::MAX),
            capacity,
            memory_bytes: family.memory_bytes(),
        }
    }
}

/// Finds the connect URL in Claude Code's output. The account dump Claude Code prints after an
/// error in debug mode goes only to the log.
struct ClaudeLines {
    remote_control: Arc<RemoteControl>,
    directory: PathBuf,
    in_debug_dump: bool,
    redraws: RedrawFilter,
}

impl LineWatcher for ClaudeLines {
    fn lines_to_log(&mut self, raw: &str, line: String) -> Vec<String> {
        self.redraws.lines_to_log(line, raw.starts_redraw())
    }

    fn last_lines(&mut self) -> Vec<String> {
        self.redraws.shorter_redraw()
    }

    fn watch(&mut self, line: &str) -> bool {
        self.in_debug_dump = line.starts_with("[debug] ")
            || (self.in_debug_dump && line.starts_with(char::is_whitespace));
        if self.in_debug_dump {
            return false;
        }
        if let Some(url) = line.connect_url() {
            let mut newly_connected = false;
            self.remote_control.update(&self.directory, |status| {
                if !matches!(status.state, ServerState::Starting | ServerState::Running) {
                    return;
                }
                newly_connected = status.state != ServerState::Running
                    || status.url.as_deref() != Some(url.as_str());
                status.connect(url.clone());
            });
            if newly_connected {
                tracing::info!(
                    "Claude Remote Control in {} is connected: {url}",
                    self.directory.display()
                );
            }
        }
        true
    }
}

/// Logs a status block redraw only when it differs from the one before.
#[derive(Debug, Default)]
struct RedrawFilter {
    previous: Vec<String>,
    current: Vec<String>,
    in_redraw: bool,
    differs: bool,
}

impl RedrawFilter {
    fn lines_to_log(&mut self, line: String, starts_redraw: bool) -> Vec<String> {
        let mut logged = Vec::new();
        if starts_redraw {
            logged.extend(self.shorter_redraw());
            self.previous = std::mem::take(&mut self.current);
            self.in_redraw = true;
            self.differs = false;
        }
        if line.trim().is_empty() {
            return logged;
        }
        if !self.in_redraw || line.has_log_stamp() {
            logged.push(line);
            return logged;
        }
        let index = self.current.len();
        self.current.push(line);
        if self.differs {
            logged.extend(self.current.last().cloned());
        } else if self.previous.get(index) != self.current.last() {
            self.differs = true;
            logged.extend(self.current.iter().cloned());
        }
        logged
    }

    /// The redraw so far, when it only dropped lines from the end of the one before.
    fn shorter_redraw(&self) -> Vec<String> {
        if self.in_redraw && !self.differs && self.current.len() != self.previous.len() {
            self.current.clone()
        } else {
            Vec::new()
        }
    }
}

trait RemoteControlLineExt {
    /// `ESC[<n>A` then `ESC[J`: Claude is redrawing its status block.
    fn starts_redraw(&self) -> bool;

    /// Starts with the `[HH:MM:SS]` stamp of Claude's log lines.
    fn has_log_stamp(&self) -> bool;

    /// The claude.ai link printed once the server is connected: the server's, or its one
    /// session's at capacity 1.
    fn connect_url(&self) -> Option<String>;
}

impl RemoteControlLineExt for str {
    fn starts_redraw(&self) -> bool {
        self.split_once("\u{1b}[J").is_some_and(|(before, _)| {
            before.split("\u{1b}[").skip(1).any(|sequence| {
                let after_count =
                    sequence.trim_start_matches(|character: char| character.is_ascii_digit());
                after_count.len() < sequence.len() && after_count.starts_with('A')
            })
        })
    }

    fn has_log_stamp(&self) -> bool {
        self.strip_prefix('[')
            .and_then(|rest| rest.split_once(']'))
            .is_some_and(|(stamp, _)| Time::parse(stamp, CLAUDE_LOG_STAMP).is_ok())
    }

    fn connect_url(&self) -> Option<String> {
        self.split_whitespace()
            .find(|word| {
                word.starts_with("https://")
                    && (word.contains("environment=") || word.contains("/code/session_"))
            })
            .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use futures_util::StreamExt;

    use super::*;
    use crate::manager::api::test_support::{EventStreamExt, TestManager, wait_until};
    use crate::manager::events::ManagerEvent;
    use crate::manager::folders::ProjectsDirectory;
    use crate::manager::supervision::{FIRST_RETRY_DELAY, LOG_TIME};

    const SIGNED_IN: &str = r#"{"loggedIn":true}"#;
    const WAIT: Duration = Duration::from_secs(10);

    /// A `claude` that is signed in and whose Remote Control runs `server`.
    fn fake_claude(manager: &TestManager, directory: &Path, server: &str) -> AppState {
        manager.install_fake_cli(
            Agent::Claude,
            &format!(
                "case \"$1\" in\n  auth) echo '{SIGNED_IN}' ;;\n  remote-control) {server} ;;\nesac"
            ),
        );
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
        wait_until(
            WAIT,
            || {
                state
                    .remote_control
                    .status_of(directory)
                    .unwrap_or_default()
            },
            |status| status.state == wanted,
        )
        .await
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
        state.remote_control.supervision.reconsider();
        wait_for(&state, ServerState::Off).await;

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn starting_the_projects_server_reads_claude_codes_models() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = fake_claude(
            &manager,
            directory.path(),
            "echo 'https://claude.ai/code?environment=env_test'; exec sleep 60",
        );
        let answer = r#"{"type":"control_response","response":{"subtype":"success","request_id":"ezra-models","response":{"models":[{"value":"opus","supportedEffortLevels":["high"]}]}}}"#;
        manager.install_fake_cli(
            Agent::Claude,
            &format!(
                "case \"$1\" in\n  auth) echo '{SIGNED_IN}' ;;\n  remote-control) echo 'https://claude.ai/code?environment=env_test'; exec sleep 60 ;;\n  -p) cat > /dev/null; echo '{answer}' ;;\nesac"
            ),
        );
        assert!(state.agent_checks.models(Agent::Claude).is_empty());
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Running).await;

        let models = wait_until(
            WAIT,
            || state.agent_checks.models(Agent::Claude),
            |models| !models.is_empty(),
        )
        .await;
        assert_eq!(
            models
                .iter()
                .map(|model| (model.model.as_str(), model.efforts.len()))
                .collect::<Vec<_>>(),
            [("opus", 1)]
        );

        state.remote_control.supervision.begin_shut_down();
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

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn a_rejected_sign_in_is_named_and_the_output_is_logged() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = fake_claude(
            &manager,
            directory.path(),
            "echo 'Error: You must be logged in to use Remote Control.' >&2; echo '[debug] Remote Control auth state:' >&2; echo '  hasOAuthAccessToken=false' >&2; exit 1",
        );
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());

        let retrying = wait_for(&state, ServerState::Retrying).await;
        assert_eq!(retrying.problem, Some(ServerProblem::SignIn));
        assert!(
            retrying.last_error.as_deref().is_some_and(
                |error| error.ends_with("Error: You must be logged in to use Remote Control.")
            ),
            "{retrying:?}"
        );
        let log = state
            .remote_control
            .log(&Served::Projects)
            .tail()
            .expect("log is readable");
        assert!(
            log.iter().any(|line| line
                .ends_with("[OUTPUT] Error: You must be logged in to use Remote Control.")),
            "{log:?}"
        );
        assert!(
            log.iter()
                .any(|line| line.ends_with("[OUTPUT]   hasOAuthAccessToken=false")),
            "{log:?}"
        );
        assert!(
            log.iter().all(|line| line
                .split_once(" [OUTPUT] ")
                .is_some_and(|(stamp, _)| time::PrimitiveDateTime::parse(stamp, LOG_TIME).is_ok())),
            "{log:?}"
        );

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn only_status_block_redraws_that_change_are_logged() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = fake_claude(
            &manager,
            directory.path(),
            r"printf 'Connecting\n'; \
            printf '\033[1A\033[JReady\n    Capacity: 0/4\n'; \
            printf '\033[2A\033[JReady\n    Capacity: 0/4\n'; \
            printf '\033[2A\033[J[12:00:01] Warning: slow\nReady\n    Capacity: 0/4\n'; \
            printf '\033[2A\033[J[12:00:01] Warning: slow\nReady\n    Capacity: 0/4\n'; \
            printf '\033[2A\033[JReady\n    Capacity: 0/4\n'; \
            printf '\033[2A\033[JConnected\n    Capacity: 1/4\n'; \
            printf '\033[2A\033[JConnected\n'; exit 1",
        );
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());

        wait_for(&state, ServerState::Retrying).await;
        let log = state
            .remote_control
            .log(&Served::Projects)
            .tail()
            .expect("log is readable");
        let output: Vec<&str> = log
            .iter()
            .filter_map(|line| line.split_once(" [OUTPUT] ").map(|(_, output)| output))
            .collect();
        assert_eq!(
            output,
            [
                "Connecting",
                "Ready",
                "    Capacity: 0/4",
                "[12:00:01] Warning: slow",
                "[12:00:01] Warning: slow",
                "Connected",
                "    Capacity: 1/4",
                "Connected",
            ]
        );

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn a_server_that_gave_up_offline_starts_again_and_connects() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let marker = directory.path().join("was-offline");
        let state = fake_claude(
            &manager,
            directory.path(),
            &format!(
                "if [ -f {marker} ]; then echo 'https://claude.ai/code?environment=env_test'; exec sleep 60; fi; touch {marker}; echo 'Server unreachable for 10 minutes, giving up.' >&2; exit 1",
                marker = marker.display()
            ),
        );
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());

        let retrying = wait_for(&state, ServerState::Retrying).await;
        assert_eq!(retrying.problem, Some(ServerProblem::Offline));
        let running = wait_until(
            FIRST_RETRY_DELAY.saturating_add(WAIT),
            || {
                state
                    .remote_control
                    .status_of(&state.projects.0)
                    .unwrap_or_default()
            },
            |status| status.state == ServerState::Running,
        )
        .await;
        assert_eq!(
            (running.problem, running.last_error, running.restarts),
            (None, None, 1)
        );

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    /// A signed-in `claude` installed as `version`, which records its version in `starts` when
    /// its Remote Control runs `server`.
    fn install_claude_version(
        manager: &TestManager,
        directory: &Path,
        version: &str,
        server: &str,
    ) {
        manager.install_fake_version(
            Agent::Claude,
            version,
            &format!(
                "case \"$1\" in\n  auth) echo '{SIGNED_IN}' ;;\n  remote-control) echo {version} >> {starts}; {server} ;;\nesac",
                starts = directory.join("starts").display()
            ),
        );
    }

    fn starts(directory: &Path) -> Vec<String> {
        fs::read_to_string(directory.join("starts"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[tokio::test]
    async fn an_idle_server_restarts_on_a_new_claude_version_at_once() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let server = "echo 'https://claude.ai/code?environment=env_test'; exec sleep 60";
        let state = fake_claude(&manager, directory.path(), server);
        install_claude_version(&manager, directory.path(), "2.1.1", server);
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Running).await;

        install_claude_version(&manager, directory.path(), "2.1.2", server);
        state.remote_control.supervision.reconsider();
        wait_until(
            WAIT,
            || starts(directory.path()),
            |starts| starts == &["2.1.1", "2.1.2"],
        )
        .await;
        let running = wait_for(&state, ServerState::Running).await;
        assert_eq!(running.update, None);
        let versions = state.install_paths.versions_directory(Agent::Claude);
        assert!(!versions.join("2.1.1").exists());

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn a_busy_server_restarts_on_a_new_claude_version_once_its_sessions_end() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let session = directory.path().join("session.pid");
        let server = format!(
            "sh -c 'sleep 60; :' session --sdk-url & echo $! > {session}; echo 'https://claude.ai/code?environment=env_test'; exec sleep 60",
            session = session.display()
        );
        let state = fake_claude(&manager, directory.path(), &server);
        install_claude_version(&manager, directory.path(), "2.1.1", &server);
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Running).await;

        install_claude_version(&manager, directory.path(), "2.1.2", &server);
        state.remote_control.supervision.reconsider();
        let waiting = wait_until(
            WAIT,
            || {
                state
                    .remote_control
                    .status_of(&state.projects.0)
                    .unwrap_or_default()
            },
            |status| status.update.is_some(),
        )
        .await;
        assert_eq!(waiting.state, ServerState::Running);
        assert_eq!(
            waiting.update.map(|update| update.version).as_deref(),
            Some("2.1.2")
        );
        assert_eq!(waiting.usage.map(|usage| usage.sessions), Some(1));
        assert_eq!(starts(directory.path()), ["2.1.1"]);
        assert!(
            state
                .install_paths
                .versions_directory(Agent::Claude)
                .join("2.1.1")
                .exists()
        );

        let session: i32 = fs::read_to_string(&session)
            .expect("the session wrote its pid")
            .trim()
            .parse()
            .expect("pid is a number");
        nix::sys::signal::kill(Pid::from_raw(session), Signal::SIGKILL).expect("session ends");
        state.remote_control.supervision.reconsider();
        wait_until(
            WAIT,
            || starts(directory.path()),
            |starts| starts == &["2.1.1", "2.1.2"],
        )
        .await;

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    fn launch(directory: &Path) -> Launch {
        Launch {
            claude: PathBuf::from("/home/dev/.local/bin/claude"),
            claude_version: Some("2.1.283".to_owned()),
            config_directory: Some(directory.join("claude")),
            directory: directory.join("projects"),
            spawn: SpawnMode::SameDir,
            log: ServerLog(directory.join("logs/projects")),
            permission_mode: "auto".to_owned(),
            capacity: Some(4),
        }
    }

    #[test]
    fn worktree_sessions_are_asked_for_by_name() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let launch = Launch {
            spawn: SpawnMode::Worktree,
            ..launch(directory.path())
        };
        let command = launch.command();
        let arguments: Vec<_> = command.as_std().get_args().take(3).collect();
        assert_eq!(arguments, ["remote-control", "--spawn", "worktree"]);
    }

    #[test]
    fn claude_code_picks_the_capacity_when_none_is_set() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let launch = Launch {
            capacity: None,
            ..launch(directory.path())
        };
        let command = launch.command();
        assert!(
            !command
                .as_std()
                .get_args()
                .any(|argument| argument == "--capacity")
        );
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
        let debug_file = directory.path().join("logs/projects/server.log");
        assert_eq!(
            arguments,
            [
                "remote-control",
                "--spawn",
                "same-dir",
                "--capacity",
                "4",
                "--permission-mode",
                "auto",
                "--debug-file",
                &debug_file.to_string_lossy(),
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

    #[tokio::test]
    async fn prompts_are_accepted_and_other_settings_kept() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join(".claude.json");
        fs::write(
            &config,
            r#"{"userID":"abc","projects":{"/other":{"hasTrustDialogAccepted":false}}}"#,
        )
        .expect("config is written");
        ClaudeGlobalConfig(config.clone())
            .accept_remote_control_in(Path::new("/home/dev/projects"))
            .await
            .expect("prompts are accepted");
        let written: Value =
            serde_json::from_slice(&fs::read(&config).expect("config is read")).expect("JSON");
        assert_eq!(written["userID"], "abc");
        assert_eq!(written["hasCompletedOnboarding"], true);
        assert_eq!(written["remoteDialogSeen"], true);
        assert_eq!(
            written["projects"]["/home/dev/projects"]["hasTrustDialogAccepted"],
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

    #[tokio::test]
    async fn accepted_config_is_not_rewritten() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join(".claude.json");
        let accepted = r#"{"hasCompletedOnboarding":true,"remoteDialogSeen":true,"projects":{"/home/dev/projects":{"hasTrustDialogAccepted":true}}}"#;
        fs::write(&config, accepted).expect("config is written");
        ClaudeGlobalConfig(config.clone())
            .accept_remote_control_in(Path::new("/home/dev/projects"))
            .await
            .expect("prompts are accepted");
        assert_eq!(
            fs::read_to_string(&config).expect("config is read"),
            accepted
        );
    }

    #[tokio::test]
    async fn the_config_is_written_once_claudes_lock_is_released_or_stale() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join(".claude.json");
        let lock = directory.path().join(".claude.json.lock");
        fs::create_dir(&lock).expect("Claude holds the lock");
        let released = tokio::spawn({
            let lock = lock.clone();
            async move {
                sleep(Duration::from_millis(300)).await;
                fs::remove_dir(&lock).expect("Claude releases the lock");
            }
        });
        ClaudeGlobalConfig(config.clone())
            .accept_remote_control_in(Path::new("/home/dev/projects"))
            .await
            .expect("prompts are accepted");
        assert!(
            released.is_finished(),
            "the config was written under Claude's lock"
        );
        assert!(config.exists());
        assert!(!lock.exists());

        fs::create_dir(&lock).expect("a crashed Claude left its lock");
        fs::File::open(&lock)
            .and_then(|lock| {
                lock.set_modified(
                    std::time::SystemTime::now()
                        .checked_sub(Duration::from_secs(60))
                        .expect("the time fits"),
                )
            })
            .expect("the lock is old");
        ClaudeGlobalConfig(config.clone())
            .accept_remote_control_in(Path::new("/other"))
            .await
            .expect("prompts are accepted");
        assert!(!lock.exists());
    }

    #[tokio::test]
    async fn missing_config_is_created_and_broken_config_is_left_alone() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join(".claude.json");
        ClaudeGlobalConfig(config.clone())
            .accept_remote_control_in(Path::new("/home/dev/projects"))
            .await
            .expect("prompts are accepted");
        assert!(config.exists());

        fs::write(&config, "{not json").expect("config is written");
        assert!(
            ClaudeGlobalConfig(config.clone())
                .accept_remote_control_in(Path::new("/home/dev/projects"))
                .await
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(&config).expect("config is read"),
            "{not json"
        );
    }

    #[test]
    fn a_redraw_starts_after_the_cursor_moves_up_and_the_screen_clears() {
        assert!("\u{1b}[3A\u{1b}[J·✔︎· Ready".starts_redraw());
        assert!("\u{1b}[12A\u{1b}[J".starts_redraw());
        assert!(!"\u{1b}[J cleared only".starts_redraw());
        assert!(!"\u{1b}[94mhttps://x\u{1b}[0m".starts_redraw());
        assert!(!"Capacity: 1/4".starts_redraw());
    }

    #[test]
    fn only_redraws_that_change_reach_the_log() {
        let mut filter = RedrawFilter::default();
        let mut log =
            |line: &str, starts_redraw: bool| filter.lines_to_log(line.to_owned(), starts_redraw);
        assert_eq!(log("Connected", false), ["Connected"]);
        assert_eq!(log("Ready", true), ["Ready"]);
        assert_eq!(log("Capacity: 1/4", false), ["Capacity: 1/4"]);
        assert!(log("Ready", true).is_empty());
        assert!(log("Capacity: 1/4", false).is_empty());
        assert!(log("Ready", true).is_empty());
        assert_eq!(log("Capacity: 2/4", false), ["Ready", "Capacity: 2/4"]);
        assert!(log("Ready", true).is_empty());
        assert_eq!(log("Capacity: 1/4", false), ["Ready", "Capacity: 1/4"]);
        assert!(log("Ready", true).is_empty());
        assert_eq!(log("", true), ["Ready"]);
        assert!(log("Ready", false).is_empty());
        assert_eq!(log("Session ended", false), ["Ready", "Session ended"]);
        assert!(log("Ready", true).is_empty());
        assert_eq!(filter.shorter_redraw(), ["Ready"]);
    }

    #[test]
    fn claude_log_lines_reach_the_log_apart_from_the_status_block() {
        let mut filter = RedrawFilter::default();
        let mut log =
            |line: &str, starts_redraw: bool| filter.lines_to_log(line.to_owned(), starts_redraw);
        assert_eq!(log("Ready", true), ["Ready"]);
        assert_eq!(
            log("[12:00:01] Warning: slow", true),
            ["[12:00:01] Warning: slow"]
        );
        assert!(log("Ready", false).is_empty());
        assert_eq!(
            log("[12:00:01] Warning: slow", true),
            ["[12:00:01] Warning: slow"]
        );
        assert!(log("Ready", false).is_empty());
        assert!(log("Ready", true).is_empty());
    }

    #[test]
    fn claude_log_lines_start_with_a_time() {
        assert!("[12:00:01] Session started".has_log_stamp());
        assert!(!"[debug] Remote Control auth state:".has_log_stamp());
        assert!(!"[12:00] Ready".has_log_stamp());
        assert!(!"Ready [12:00:01]".has_log_stamp());
    }

    #[test]
    fn connect_url_comes_from_the_continue_line() {
        assert_eq!(
            "Continue coding in the Claude mobile app or https://claude.ai/code?environment=env_01AB"
                .connect_url()
                .as_deref(),
            Some("https://claude.ai/code?environment=env_01AB")
        );
        assert_eq!(
            "Code anywhere with the Claude mobile app or https://claude.ai/code/session_01AB"
                .connect_url()
                .as_deref(),
            Some("https://claude.ai/code/session_01AB")
        );
        assert_eq!("Capacity: 1/4 · New sessions".connect_url(), None);
    }

    #[test]
    fn claude_messages_name_the_problem() {
        for (line, problem) in [
            (
                "Error: You must be logged in to use Remote Control.",
                Some(ServerProblem::SignIn),
            ),
            (
                "Error: Unable to determine your organization for Remote Control eligibility. Run `claude auth login` to refresh your account information.",
                Some(ServerProblem::SignIn),
            ),
            (
                "Error: RegisterEnvironment: Authentication failed (401): invalid token. Remote Control is only available with claude.ai subscriptions. Please use `/login` to sign in with your claude.ai account.",
                Some(ServerProblem::SignIn),
            ),
            (
                "Error: Remote Control requires a full-scope login token. Long-lived tokens (from `claude setup-token` or CLAUDE_CODE_OAUTH_TOKEN) are limited to inference-only for security reasons. Run `claude auth login` to use Remote Control.",
                Some(ServerProblem::BlockedBySetting),
            ),
            (
                "Error: Your organization requires Trusted Devices for Remote Control, but this device is not enrolled. Please run `/login` in Claude Code to enroll this device.",
                Some(ServerProblem::SignIn),
            ),
            (
                "Error: Remote Control isn't enabled for this account. If you recently changed plans, run `claude auth logout` then `claude auth login` to refresh your entitlements, or `claude doctor` for details.",
                Some(ServerProblem::NotEnabled),
            ),
            (
                "Error: Remote Control is disabled by your organization's policy.",
                Some(ServerProblem::NotAllowed),
            ),
            (
                "Remote Control environments are not available for your account.",
                Some(ServerProblem::NotAllowed),
            ),
            (
                "Error: RegisterEnvironment: Access denied (403). Check your organization permissions.",
                Some(ServerProblem::NotAllowed),
            ),
            (
                "Server unreachable for 10 minutes, giving up.",
                Some(ServerProblem::Offline),
            ),
            (
                "Error: Couldn't verify Remote Control eligibility — the feature-flag service was unreachable (offline or blocked). Retry, or run with `--debug` / `claude doctor` for details.",
                Some(ServerProblem::Offline),
            ),
            (
                "Error: getaddrinfo EAI_AGAIN api.anthropic.com",
                Some(ServerProblem::Offline),
            ),
            (
                "Persistent errors for 10 minutes, giving up.",
                Some(ServerProblem::Offline),
            ),
            (
                "Error: Couldn't verify your organization's Remote Control policy. Retry, or run `claude doctor` for details.",
                Some(ServerProblem::Offline),
            ),
            (
                "Error: Couldn't verify your organization's policy for remote control. Check your network connection and try again.",
                Some(ServerProblem::Offline),
            ),
            (
                "Error: Remote Control requires a claude.ai subscription. Run `claude auth login` to sign in with your claude.ai account.",
                Some(ServerProblem::SignIn),
            ),
            (
                "Error: Remote Control requires claude.ai subscription auth. Unset ANTHROPIC_API_KEY / apiKeyHelper / ANTHROPIC_AUTH_TOKEN to use Remote Control.",
                Some(ServerProblem::SignIn),
            ),
            (
                "Error: Remote Control requires claude.ai subscription auth. apiKeyHelper is configured, so this session is using API-key auth — unset it to use Remote Control.",
                Some(ServerProblem::BlockedBySetting),
            ),
            (
                "Error: Remote Control requires claude.ai subscription auth. This session is using CLAUDE_CODE_OAUTH_TOKEN auth — Unset the CLAUDE_CODE_OAUTH_TOKEN environment variable.",
                Some(ServerProblem::BlockedBySetting),
            ),
            (
                "Error: Remote Control requires claude.ai subscription auth. ANTHROPIC_UNIX_SOCKET is set without CLAUDE_CODE_OAUTH_TOKEN, so requests on the socket carry no claude.ai login (on a claude ssh remote: the local machine is API-key-authed).",
                Some(ServerProblem::BlockedBySetting),
            ),
            (
                "Error: Remote Control is not available inside a cloud session.",
                Some(ServerProblem::BlockedBySetting),
            ),
            (
                "Error: Remote Control requires feature-flag evaluation, which is disabled because DISABLE_GROWTHBOOK is set. Unset it (or run in a shell without it) to use Remote Control.",
                Some(ServerProblem::BlockedBySetting),
            ),
            (
                "Error: Remote Control is only available when using Claude via api.anthropic.com.",
                Some(ServerProblem::BlockedBySetting),
            ),
            (
                "Opus with 1M context is not available for your account. Learn more: https://code.claude.com/docs/en/model-config#extended-context-with-1m",
                None,
            ),
            ("Error: Workspace not trusted: /home/dev/projects.", None),
        ] {
            assert_eq!(
                ServerProblem::in_output(&["Starting", line]),
                problem,
                "{line}"
            );
        }
    }

    #[test]
    fn a_new_claude_version_is_an_update_of_the_same_server() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let running = launch(directory.path());
        let updated = Launch {
            claude_version: Some("2.1.290".to_owned()),
            ..running.clone()
        };
        assert!(updated.is_update_of(&running));
        assert!(!running.is_update_of(&running));
        let resized = Launch {
            capacity: Some(8),
            ..updated.clone()
        };
        assert!(!resized.is_update_of(&running));
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
        wait_until(
            WAIT,
            || nix::sys::signal::kill(Pid::from_raw(session), None).is_ok(),
            |running| !running,
        )
        .await;

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn a_group_counts_the_leaders_sessions_and_everyones_memory() {
        let mut leader = Command::new("sh")
            .args([
                "-c",
                "sh -c 'sleep 30; :' session --sdk-url & sh -c 'sleep 30; :' session --sdk-url & sleep 30 & wait",
            ])
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
        let usage = wait_until(WAIT, || group.usage(Some(4)), |usage| usage.sessions == 2).await;
        assert_eq!(usage.capacity, Some(4));
        assert!(usage.memory_bytes > 0);

        group.terminate(&mut leader).await;
        assert_eq!(group.usage(Some(4)).memory_bytes, 0);
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

        state.remote_control.supervision.restart();
        wait_until(
            WAIT,
            || fs::read_to_string(&starts).unwrap_or_default(),
            |started| started.lines().count() == 2,
        )
        .await;
        wait_for(&state, ServerState::Running).await;

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
        assert!(
            state
                .remote_control
                .supervision
                .wait_until_stopped(Duration::ZERO)
                .await
        );
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
        manager.install_fake_cli(
            Agent::Claude,
            &format!(
                "case \"$1\" in\n  auth) if [ -f {marker} ]; then echo '{SIGNED_IN}'; else echo '{{\"loggedIn\":false}}'; fi ;;\n  remote-control) echo 'https://claude.ai/code?environment=env_test'; exec sleep 60 ;;\nesac",
                marker = marker.display()
            ),
        );
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Waiting).await;

        fs::write(&marker, "").expect("marker is written");
        state.agent_checks.refresh(Agent::Claude).await;
        wait_for(&state, ServerState::Running).await;

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    /// A `claude` signed in once `marker` exists, which `auth login` creates.
    fn install_claude_that_signs_in(manager: &TestManager, marker: &Path) -> AppState {
        manager.install_fake_cli(
            Agent::Claude,
            &format!(
                r#"case "$1 $2" in
  "auth login") echo 'Visit: https://claude.com/cai/oauth/authorize?code=true'; read code; touch {marker} ;;
  "auth status") if [ -f {marker} ]; then echo '{SIGNED_IN}'; else echo '{{"loggedIn":false}}'; fi ;;
  "remote-control "*) echo 'https://claude.ai/code?environment=env_test'; exec sleep 60 ;;
esac"#,
                marker = marker.display()
            ),
        );
        let state = manager.state.clone();
        fs::create_dir_all(&state.projects.0).expect("projects directory is created");
        state
    }

    async fn sign_in_through_the_api(manager: &TestManager, cookie: &str) {
        let started = manager
            .post("/api/v1/agents/claude/login", "", Some(cookie))
            .await;
        assert_eq!(started.status(), StatusCode::OK);
        let submitted = manager
            .post(
                "/api/v1/agents/claude/login/code",
                r#"{"code": "good"}"#,
                Some(cookie),
            )
            .await;
        assert_eq!(submitted.status(), StatusCode::NO_CONTENT);
    }

    fn servers_started(manager: &TestManager) -> usize {
        manager
            .fake_cli_runs(Agent::Claude)
            .iter()
            .filter(|run| run.starts_with("remote-control"))
            .count()
    }

    #[tokio::test]
    async fn signing_in_through_the_api_starts_a_waiting_server_once() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = install_claude_that_signs_in(&manager, &directory.path().join("signed-in"));
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Waiting).await;
        let mut events = Box::pin(state.events.stream());
        let remote_control = Arc::clone(&state.remote_control);
        let projects = state.projects.0.clone();
        let states_seen = tokio::spawn(async move {
            let mut seen = Vec::new();
            while let Some(event) = events.next().await {
                if let ManagerEvent::Changed {
                    topic: Topic::RemoteControl,
                    ..
                } = event
                    && let Some(status) = remote_control.status_of(&projects)
                {
                    seen.push(status.state);
                }
            }
            seen
        });

        sign_in_through_the_api(&manager, &cookie).await;
        wait_for(&state, ServerState::Running).await;
        sleep(Duration::from_secs(2)).await;
        state.events.close();
        let seen = states_seen.await.expect("states are collected");
        assert!(!seen.contains(&ServerState::Stopping), "{seen:?}");
        assert_eq!(seen.last(), Some(&ServerState::Running), "{seen:?}");
        assert_eq!(servers_started(&manager), 1);

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn signing_in_through_the_api_restarts_a_running_server() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let directory = tempfile::tempdir().expect("temporary directory");
        let marker = directory.path().join("signed-in");
        fs::write(&marker, "").expect("marker is written");
        let state = install_claude_that_signs_in(&manager, &marker);
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Running).await;
        assert_eq!(servers_started(&manager), 1);

        sign_in_through_the_api(&manager, &cookie).await;
        wait_until(WAIT, || servers_started(&manager), |started| *started == 2).await;
        wait_for(&state, ServerState::Running).await;

        state.remote_control.supervision.begin_shut_down();
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
            .change_folder_choice("app", |choice| choice.serve = true)
            .await
            .expect("the choice is saved");
        let served = wait_in(&state, &app, ServerState::Running).await;
        assert_eq!(
            served.url.as_deref(),
            Some("https://claude.ai/code?environment=env_app")
        );

        state
            .change_folder_choice("app", |choice| choice.serve = false)
            .await
            .expect("the choice is saved");
        assert!(state.remote_control.wait_until_gone(&app, WAIT).await);

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn a_served_folder_stops_when_it_becomes_a_linked_worktree() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = fake_claude(
            &manager,
            directory.path(),
            "echo 'https://claude.ai/code?environment=env_test'; exec sleep 60",
        );
        let app = state.projects.folder("app");
        let feature = state.projects.folder("feature");
        fs::create_dir_all(&app).expect("repository folder is created");
        fs::create_dir_all(&feature).expect("feature folder is created");
        for arguments in [
            vec!["init", "--quiet", "--initial-branch=main"],
            vec!["commit", "--quiet", "--allow-empty", "--message=first"],
        ] {
            let output = Command::new("git")
                .current_dir(&app)
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(arguments)
                .output()
                .await
                .expect("git runs");
            assert!(output.status.success(), "{output:?}");
        }
        state
            .change_folder_choice("feature", |choice| choice.serve = true)
            .await
            .expect("the folder choice is saved");
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_in(&state, &app, ServerState::Running).await;
        wait_in(&state, &feature, ServerState::Running).await;

        let output = Command::new("git")
            .current_dir(&app)
            .args(["worktree", "add", "--quiet", "--detach", "../feature"])
            .output()
            .await
            .expect("git creates the linked worktree");
        assert!(output.status.success(), "{output:?}");
        state.remote_control.supervision.reconsider();
        assert!(state.remote_control.wait_until_gone(&feature, WAIT).await);
        assert!(state.settings.lock().await.agents.claude.folders["feature"].serve);
        assert!(feature.join(".git").is_file());
        assert_eq!(
            state
                .remote_control
                .status_of(&app)
                .expect("app is served")
                .state,
            ServerState::Running
        );

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn a_folders_own_options_restart_only_that_folders_server() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let starts = directory.path().join("starts");
        let state = fake_claude(
            &manager,
            directory.path(),
            &format!(
                "echo \"$(basename \"$PWD\") $*\" >> {}; echo \"https://claude.ai/code?environment=env_$(basename \"$PWD\")\"; exec sleep 60",
                starts.display()
            ),
        );
        let app = state.projects.folder("app");
        fs::create_dir_all(app.join(".git")).expect("repository is created");
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Running).await;
        wait_in(&state, &app, ServerState::Running).await;

        let notes = state.projects.folder("notes");
        fs::create_dir_all(&notes).expect("folder is created");
        state
            .change_folder_choice("notes", |choice| choice.serve = true)
            .await
            .expect("the choice is saved");
        wait_in(&state, &notes, ServerState::Running).await;
        state
            .change_folder_choice("app", |choice| {
                choice.options = ClaudeOptions {
                    spawn: Some(SpawnMode::SameDir),
                    permission_mode: Some("plan".to_owned()),
                    capacity: Some(2),
                };
            })
            .await
            .expect("the choice is saved");
        let arguments = |spawn: &str, rest: &str| {
            format!("remote-control --spawn {spawn} {rest}--permission-mode")
        };
        let started_in = |started: &str, folder: &str| -> Vec<String> {
            started
                .lines()
                .filter(|line| line.starts_with(&format!("{folder} ")))
                .map(str::to_owned)
                .collect()
        };
        let started = wait_until(
            WAIT,
            || fs::read_to_string(&starts).unwrap_or_default(),
            |started| started_in(started, "app").len() == 2,
        )
        .await;
        let [first, second] = &started_in(&started, "app")[..] else {
            unreachable!("the wait saw two starts");
        };
        assert!(first.contains(&arguments("worktree", "")), "{first}");
        assert!(first.contains("--permission-mode auto"), "{first}");
        assert!(
            second.contains(&arguments("same-dir", "--capacity 2 ")),
            "{second}"
        );
        assert!(second.contains("--permission-mode plan"), "{second}");
        for plain in ["projects", "notes"] {
            let starts = started_in(&started, plain);
            assert_eq!(starts.len(), 1, "{started}");
            assert!(starts[0].contains(&arguments("same-dir", "")), "{started}");
        }
        wait_in(&state, &app, ServerState::Running).await;

        state.remote_control.supervision.begin_shut_down();
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
            .change_folder_choice("app", |choice| choice.serve = true)
            .await
            .expect("the choice is saved");
        wait_in(&state, &app, ServerState::Waiting).await;
        let published = events.published().await;
        assert!(published.contains(&Topic::RemoteControl), "{published:?}");

        state.remote_control.supervision.begin_shut_down();
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
            .change_folder_choice("app", |choice| choice.serve = true)
            .await
            .expect("the choice is saved");
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Running).await;
        wait_in(&state, &app, ServerState::Running).await;
        let overview = state.remote_control_overview();
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
            .change_folder_choice("app", |choice| choice.serve = false)
            .await
            .expect("the choice is saved");
        wait_in(&state, &app, ServerState::Stopping).await;
        assert!(state.remote_control.wait_until_gone(&app, WAIT).await);

        let published = events.published().await;
        for expected in [Topic::Folders, Topic::RemoteControl] {
            assert!(
                published.contains(&expected),
                "{expected:?} missing from {published:?}"
            );
        }

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn a_folder_is_deleted_after_its_server_stops() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = fake_claude(
            &manager,
            directory.path(),
            "trap 'test -d \"$PWD\" && touch \"../stopped-$(basename \"$PWD\")\"; exit' TERM; echo \"https://claude.ai/code?environment=env_$(basename \"$PWD\")\"; sleep 60 & wait",
        );
        let app = state.projects.folder("app");
        fs::create_dir_all(&app).expect("folder is created");
        state
            .change_folder_choice("app", |choice| choice.serve = true)
            .await
            .expect("the choice is saved");
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_in(&state, &app, ServerState::Running).await;

        state
            .delete_folder("app")
            .await
            .expect("the folder is deleted");
        assert!(!app.exists());
        assert!(state.projects.0.join("stopped-app").exists());
        assert_eq!(state.remote_control.status_of(&app), None);
        assert!(state.settings.lock().await.agents.claude.folders.is_empty());

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn the_logs_of_folders_gone_from_projects_are_removed() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = fake_claude(
            &manager,
            directory.path(),
            "echo \"https://claude.ai/code?environment=env_$(basename \"$PWD\")\"; exec sleep 60",
        );
        let log = |name: &str| state.remote_control.log(&Served::Folder(name.to_owned())).0;
        fs::create_dir_all(log("renamed")).expect("an old log is created");
        let (served, unserved) = (
            state.projects.folder("served"),
            state.projects.folder("unserved"),
        );
        for (folder, name) in [(&served, "served"), (&unserved, "unserved")] {
            fs::create_dir_all(folder).expect("folder is created");
            state
                .change_folder_choice(name, |choice| choice.serve = true)
                .await
                .expect("the choice is saved");
        }
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_in(&state, &served, ServerState::Running).await;
        wait_in(&state, &unserved, ServerState::Running).await;
        assert!(!log("renamed").exists());
        assert!(log("served").is_dir() && log("unserved").is_dir());

        state
            .change_folder_choice("unserved", |choice| choice.serve = false)
            .await
            .expect("the choice is saved");
        assert!(state.remote_control.wait_until_gone(&unserved, WAIT).await);
        assert!(log("unserved").is_dir());

        for folder in [&served, &unserved] {
            fs::remove_dir_all(folder).expect("folder is removed");
        }
        state.remote_control.supervision.reconsider();
        wait_until(
            WAIT,
            || (log("served").exists(), log("unserved").exists()),
            |left| *left == (false, false),
        )
        .await;
        assert!(state.remote_control.log(&Served::Projects).0.is_dir());

        state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }
}
