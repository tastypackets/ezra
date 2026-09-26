use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex as SyncMutex, PoisonError};
use std::time::{Duration, SystemTime};

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, gethostname};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep, timeout, timeout_at};
use utoipa::ToSchema;

use super::agents::Agent;
use super::events::{Events, Topic};
use super::folders::Folder;
use super::login::{AgentCli, SignInStatus, StrExt};
use super::processes::{Process, ProcessStat};
use super::state::AppState;
use crate::path_ext::PathExt;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(4);
const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const GROUP_POLL_INTERVAL: Duration = Duration::from_millis(100);
const RECHECK_INTERVAL: Duration = Duration::from_secs(60);
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(5);
const LONGEST_RETRY_DELAY: Duration = Duration::from_secs(300);
const HEALTHY_RUN: Duration = Duration::from_secs(600);
const USAGE_INTERVAL: Duration = Duration::from_secs(15);
const STARTING_USAGE_INTERVAL: Duration = Duration::from_secs(1);
const OUTPUT_LINES_KEPT: usize = 20;
const OUTPUT_LINES_REPORTED: usize = 5;
const UPDATE_RESTART_DEADLINE: Duration = Duration::from_secs(6 * 60 * 60);
const LOG_FILE: &str = "server.log";
const ROTATED_LOG_FILE: &str = "server.log.1";
const LATEST_LOG_LINK: &str = "latest";
const LOG_TAIL_LINES: usize = 200;
const LOG_TAIL_BYTES: u64 = 256 * 1024;
const SESSION_LOGS_KEPT_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const SESSION_LOGS_LARGEST: u64 = 64 * 1024 * 1024;
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
    /// The most sessions each server runs at once, at least 1. Absent uses Claude Code's
    /// default.
    #[schema(required = true, minimum = 1)]
    pub capacity: Option<u32>,
    /// Whether repositories that appear in /projects start with their switch on.
    #[schema(required = true)]
    pub serve_repositories: bool,
}

impl RemoteControlSettings {
    /// `None` when the settings can be passed to Claude, or why not.
    pub fn problem(&self) -> Option<&'static str> {
        let mode = self.permission_mode.trim();
        if mode.is_empty() || mode.contains(char::is_whitespace) {
            Some("the permission mode must be one word")
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
    /// Where new sessions work, always `same-dir` outside a repository.
    pub spawn: SpawnMode,
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
            .is_some_and(|mode| mode.contains(char::is_whitespace))
        {
            Some("the permission mode must be one word")
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
    /// The account's plan or organization does not allow Remote Control.
    NotAllowed,
    /// Claude could not be reached.
    Offline,
    /// Claude answered with server errors.
    Unavailable,
}

impl ServerProblem {
    /// Parts of the messages Claude Code 2.1 prints, checked in this order.
    const MESSAGES: [(&str, Self); 22] = [
        ("You must be logged in to use Remote Control", Self::SignIn),
        ("Unable to determine your organization", Self::SignIn),
        ("Authentication failed (401)", Self::SignIn),
        ("login expired", Self::SignIn),
        ("OAuth token unavailable", Self::SignIn),
        ("requires a full-scope login token", Self::SignIn),
        ("requires a claude.ai subscription", Self::SignIn),
        ("requires claude.ai subscription auth", Self::SignIn),
        ("this device is not enrolled", Self::SignIn),
        ("isn't enabled for this account", Self::NotAllowed),
        ("disabled by your organization's policy", Self::NotAllowed),
        ("not available for your account", Self::NotAllowed),
        ("Access denied (403)", Self::NotAllowed),
        (
            "may not be available for this organization",
            Self::NotAllowed,
        ),
        ("Server unreachable for", Self::Offline),
        ("service was unreachable", Self::Offline),
        ("EAI_AGAIN", Self::Offline),
        ("ENOTFOUND", Self::Offline),
        ("ECONNREFUSED", Self::Offline),
        ("ETIMEDOUT", Self::Offline),
        ("Persistent errors for", Self::Unavailable),
        ("Failed with status 5", Self::Unavailable),
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

/// A newer Claude Code a running server waits to restart on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PendingUpdate {
    /// The installed version the server restarts on.
    pub version: String,
    /// When it restarts even with sessions running.
    #[serde(with = "time::serde::rfc3339")]
    pub restart_by: OffsetDateTime,
}

/// What a running server and its sessions use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServerUsage {
    /// Sessions running now.
    pub sessions: u32,
    /// The most sessions the server runs at once, absent when Claude Code's default applies.
    pub capacity: Option<u32>,
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
    logs: PathBuf,
    events: Events,
    changes: watch::Sender<u64>,
    restarts: watch::Sender<u64>,
    shutdown: watch::Sender<bool>,
    stopped: watch::Sender<bool>,
}

impl RemoteControl {
    /// Publishes every status change to `events` and keeps each server's logs under `logs`.
    pub fn new(events: Events, logs: PathBuf) -> Self {
        Self {
            device: gethostname()
                .ok()
                .and_then(|hostname| hostname.into_string().ok()),
            servers: SyncMutex::default(),
            logs,
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

    pub fn log(&self, served: &Served) -> ServerLog {
        ServerLog(match served {
            Served::Projects => self.logs.join("projects"),
            Served::Folder(name) => self.logs.join("folders").join(name),
        })
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

    /// Removes the logs of folders that are not in /projects any more.
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
pub enum Served {
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
    fn accept_prompts(&self) -> Result<(), String> {
        let Some(config_directory) = &self.config_directory else {
            return Ok(());
        };
        ClaudeGlobalConfig(config_directory.join(".claude.json"))
            .accept_remote_control_in(&self.directory)
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

/// One server's log directory. Claude writes `server.log`, rotates it to `server.log.1` at 10 MiB
/// and adds a debug log and a transcript per session. The manager adds the server's output lines
/// to `server.log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerLog(PathBuf);

impl ServerLog {
    pub fn debug_file(&self) -> PathBuf {
        self.0.join(LOG_FILE)
    }

    /// Adds a line the server printed, stamped like Claude's own lines.
    async fn append_output(&self, line: &str) -> io::Result<()> {
        let now = OffsetDateTime::now_utc();
        let stamp = now
            .replace_millisecond(now.millisecond())
            .map_err(io::Error::other)?
            .format(&Rfc3339)
            .map_err(io::Error::other)?;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.debug_file())
            .await?;
        file.write_all(format!("{stamp} [OUTPUT] {line}\n").as_bytes())
            .await
    }

    /// The server's last lines, oldest first.
    pub fn tail(&self) -> io::Result<Vec<String>> {
        let mut lines = VecDeque::new();
        let mut budget = LOG_TAIL_BYTES;
        for name in [LOG_FILE, ROTATED_LOG_FILE] {
            if lines.len() >= LOG_TAIL_LINES || budget == 0 {
                break;
            }
            let text = self.0.join(name).read_last_lines(budget)?;
            budget = budget.saturating_sub(u64::try_from(text.len()).unwrap_or(u64::MAX));
            for line in text.lines().rev() {
                lines.push_front(line.to_owned());
            }
        }
        let skipped = lines.len().saturating_sub(LOG_TAIL_LINES);
        Ok(lines.into_iter().skip(skipped).collect())
    }

    /// Creates the directory and removes session logs older than a week, then the oldest ones
    /// past the size limit.
    fn prepare(&self) -> io::Result<()> {
        fs::create_dir_all(&self.0)?;
        let mut sessions: Vec<(PathBuf, SystemTime, u64)> = self
            .0
            .entries_or_empty()?
            .into_iter()
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    ![LOG_FILE, ROTATED_LOG_FILE, LATEST_LOG_LINK]
                        .iter()
                        .any(|kept| name == *kept)
                })
            })
            .filter_map(|path| {
                let metadata = fs::symlink_metadata(&path)
                    .ok()
                    .filter(fs::Metadata::is_file)?;
                Some((path, metadata.modified().ok()?, metadata.len()))
            })
            .collect();
        sessions.sort_by_key(|(_, modified, _)| Reverse(*modified));
        let mut kept_bytes = 0_u64;
        for (path, modified, bytes) in sessions {
            let old = modified
                .elapsed()
                .is_ok_and(|age| age > SESSION_LOGS_KEPT_FOR);
            if old || kept_bytes.saturating_add(bytes) > SESSION_LOGS_LARGEST {
                path.remove_if_present()?;
            } else {
                kept_bytes = kept_bytes.saturating_add(bytes);
            }
        }
        Ok(())
    }
}

/// Why a server run ended.
enum RunEnd {
    /// What should run changed, so it was stopped on purpose.
    Reconsidered,
    /// The manager is stopping.
    ShutDown,
    /// It could not start, did not connect, or stopped by itself.
    Failed(Failure),
}

/// What went wrong with a server run.
#[derive(Debug, PartialEq, Eq)]
struct Failure {
    message: String,
    problem: Option<ServerProblem>,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            problem: None,
        }
    }
}

/// What to do with a running server after something changed.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Keep,
    /// The sign-in could not be checked, so nothing changes.
    Unsure,
    Stop,
    /// Restart it on this Claude Code version once it has no sessions.
    Update(String),
}

/// A running server waiting to restart on a newer Claude Code.
struct UpdateWait {
    version: String,
    restart_by: OffsetDateTime,
    deadline: Instant,
}

impl UpdateWait {
    fn starting_now(version: String) -> Self {
        Self {
            version,
            restart_by: OffsetDateTime::now_utc().saturating_add(
                time::Duration::try_from(UPDATE_RESTART_DEADLINE).unwrap_or(time::Duration::MAX),
            ),
            deadline: Instant::now()
                .checked_add(UPDATE_RESTART_DEADLINE)
                .unwrap_or_else(Instant::now),
        }
    }

    fn pending(&self) -> PendingUpdate {
        PendingUpdate {
            version: self.version.clone(),
            restart_by: self.restart_by,
        }
    }

    fn is_due(&self, usage: Option<ServerUsage>) -> bool {
        usage.is_none_or(|usage| usage.sessions == 0) || Instant::now() >= self.deadline
    }
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
                RunEnd::Failed(Failure { message, problem }) => {
                    failures = Failures(failures.0.saturating_add(1));
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
    ) -> RunEnd {
        self.remote_control
            .update(directory, |status| status.enter(state));
        match signals.wait_for_change(RECHECK_INTERVAL).await {
            Wake::Changed => RunEnd::Reconsidered,
            Wake::ShutDown => RunEnd::ShutDown,
        }
    }

    async fn wanted_server(&self, served: &Served, directory: &Path) -> Wanted {
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
                folder.map(|folder| claude.folder_choice(&folder)),
            )
        };
        if !settings.enabled || choice.as_ref().is_some_and(|choice| !choice.serve) {
            return Wanted::Off;
        }
        if AgentCli::installed(Agent::Claude, &self.install_paths).is_err() {
            return Wanted::Waiting;
        }
        match self.agent_checks.sign_in(Agent::Claude) {
            Some(sign_in) if sign_in.logged_in => {}
            Some(_) => return Wanted::Waiting,
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
            spawn: options.spawn,
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
            Wanted::Server(_) | Wanted::Off | Wanted::Waiting => Verdict::Stop,
        }
    }

    async fn run_server(&self, served: &Served, launch: &Launch, signals: &mut Signals) -> RunEnd {
        signals.restarts.mark_unchanged();
        if signals.is_shutting_down() {
            return RunEnd::ShutDown;
        }
        if let Err(message) = launch.accept_prompts() {
            return RunEnd::Failed(message.into());
        }
        if let Err(error) = launch.log.prepare() {
            tracing::warn!("could not prepare {}: {error}", launch.log.0.display());
        }
        let mut server = match ServerRun::start(launch, &self.remote_control) {
            Ok(server) => server,
            Err(error) => return RunEnd::Failed(format!("could not start: {error}").into()),
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
                    if let Err(error) = launch.log.prepare() {
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
                    let waiting =
                        update.get_or_insert_with(|| UpdateWait::starting_now(version.clone()));
                    waiting.version = version;
                    if waiting.is_due(usage) {
                        tracing::info!(
                            "restarting Claude Remote Control in {} on Claude Code {}",
                            launch.directory.display(),
                            waiting.version
                        );
                        let end = self.stop_server(server, launch).await;
                        self.remove_unused_claude_versions().await;
                        return end;
                    }
                    let pending = waiting.pending();
                    self.remote_control
                        .update(&launch.directory, |status| status.update = Some(pending));
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

    /// Skipped while an install runs, which removes them itself.
    async fn remove_unused_claude_versions(&self) {
        let Ok(_one_install_at_a_time) = self.install_lock.try_lock() else {
            return;
        };
        let paths = Arc::clone(&self.install_paths);
        let removed =
            tokio::task::spawn_blocking(move || paths.remove_unused_versions(Agent::Claude)).await;
        if let Ok(Err(error)) = removed {
            tracing::warn!("could not remove old Claude Code versions: {error}");
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
            readers.push(tokio::spawn(output.clone().collect(
                stdout,
                Arc::clone(remote_control),
                launch.clone(),
            )));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(tokio::spawn(output.clone().collect(
                stderr,
                Arc::clone(remote_control),
                launch.clone(),
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
    async fn finish(mut self, exit: io::Result<ExitStatus>) -> Failure {
        if let Some(group) = &self.group {
            group.terminate(&mut self.child).await;
        }
        self.stop_reading().await;
        self.output.describe_exit(exit)
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

/// A process and the processes it started, which share its process group.
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

    /// The leader's live children are its sessions, and the whole group counts for memory.
    fn usage(&self, capacity: Option<u32>) -> ServerUsage {
        let mut usage = ServerUsage {
            sessions: 0,
            capacity,
            memory_bytes: 0,
        };
        for process in Process::all() {
            let Some(ProcessStat {
                parent,
                group,
                zombie,
            }) = process.stat()
            else {
                continue;
            };
            if group != self.0 {
                continue;
            }
            if parent == self.0 && !zombie {
                usage.sessions = usage.sessions.saturating_add(1);
            }
            if let Some(memory) = process.proportional_memory() {
                usage.memory_bytes = usage.memory_bytes.saturating_add(memory);
            }
        }
        usage
    }
}

/// The last lines a server printed, shared by its stdout and stderr readers. The account dump
/// Claude prints after an error in debug mode goes only to the log.
#[derive(Debug, Clone, Default)]
struct ServerOutput(Arc<SyncMutex<VecDeque<String>>>);

impl ServerOutput {
    async fn collect(
        self,
        stream: impl AsyncRead + Unpin,
        remote_control: Arc<RemoteControl>,
        launch: Launch,
    ) {
        let mut reader = BufReader::new(stream);
        let mut bytes = Vec::new();
        let mut in_debug_dump = false;
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&bytes)
                .trim_end()
                .without_terminal_codes();
            if line.trim().is_empty() {
                continue;
            }
            if let Err(error) = launch.log.append_output(&line).await {
                tracing::debug!("could not log to {}: {error}", launch.log.0.display());
            }
            in_debug_dump = line.starts_with("[debug] ")
                || (in_debug_dump && line.starts_with(char::is_whitespace));
            if in_debug_dump {
                continue;
            }
            if let Some(url) = line.connect_url() {
                let mut newly_connected = false;
                remote_control.update(&launch.directory, |status| {
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
                        launch.directory.display()
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

    fn describe_exit(&self, exit: io::Result<ExitStatus>) -> Failure {
        let kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let lines: Vec<&str> = kept.iter().map(String::as_str).collect();
        let skipped = lines.len().saturating_sub(OUTPUT_LINES_REPORTED);
        let tail = lines.get(skipped..).unwrap_or_default().join("\n");
        let exit = match exit {
            Ok(status) => status.to_string(),
            Err(error) => error.to_string(),
        };
        Failure {
            message: if tail.is_empty() {
                exit
            } else {
                format!("{exit}: {tail}")
            },
            problem: ServerProblem::in_output(&lines),
        }
    }
}

trait RemoteControlLineExt {
    /// The claude.ai link printed once the server is connected: the server's, or its one
    /// session's at capacity 1.
    fn connect_url(&self) -> Option<String>;
}

impl RemoteControlLineExt for str {
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
    use std::io::Write as _;
    use std::ops::Range;
    use std::os::unix::fs::symlink;

    use futures_util::{Stream, StreamExt};

    use super::*;
    use crate::manager::api::test_support::TestManager;
    use crate::manager::events::ManagerEvent;
    use crate::manager::folders::ProjectsDirectory;

    const SIGNED_IN: &str = r#"{"loggedIn":true}"#;

    /// Writes an executable script without this process holding it open for writing.
    fn write_script(path: &Path, script: &str) {
        let mut writer = std::process::Command::new("sh")
            .args(["-c", "cat > \"$0\" && chmod 755 \"$0\""])
            .arg(path)
            .stdin(Stdio::piped())
            .spawn()
            .expect("sh starts");
        writer
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(script.as_bytes())
            .expect("script is sent");
        assert!(writer.wait().expect("sh ends").success());
    }

    /// A `claude` that is signed in and whose Remote Control runs `server`.
    fn fake_claude(manager: &TestManager, directory: &Path, server: &str) -> AppState {
        let script = directory.join("claude");
        write_script(
            &script,
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n  auth) echo '{SIGNED_IN}' ;;\n  remote-control) {server} ;;\nesac\n"
            ),
        );
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

    /// The topics published until nothing more arrives for 100 ms.
    async fn published(events: &mut (impl Stream<Item = ManagerEvent> + Unpin)) -> Vec<Topic> {
        let mut topics = Vec::new();
        while let Ok(Some(event)) = timeout(Duration::from_millis(100), events.next()).await {
            if let ManagerEvent::Changed { topic, .. } = event {
                topics.push(topic);
            }
        }
        topics
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

        state.remote_control.begin_shut_down();
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
        let deadline = Instant::now()
            .checked_add(FIRST_RETRY_DELAY.saturating_add(Duration::from_secs(10)))
            .expect("the deadline fits");
        let running = loop {
            let status = state
                .remote_control
                .status_of(&state.projects.0)
                .unwrap_or_default();
            if status.state == ServerState::Running {
                break status;
            }
            assert!(Instant::now() < deadline, "still {status:?}");
            sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(
            (running.problem, running.last_error, running.restarts),
            (None, None, 1)
        );

        state.remote_control.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    /// Installs a `claude` script as `version`, the way the manager links a real install.
    fn install_fake_claude(state: &AppState, directory: &Path, version: &str, server: &str) {
        let versions = state.install_paths.versions_directory(Agent::Claude);
        fs::create_dir_all(&versions).expect("versions directory is created");
        let script = versions.join(version);
        write_script(
            &script,
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n  auth) echo '{SIGNED_IN}' ;;\n  remote-control) echo {version} >> {starts}; {server} ;;\nesac\n",
                starts = directory.join("starts").display()
            ),
        );
        state
            .install_paths
            .command(Agent::Claude)
            .replace_symlink(&script)
            .expect("command link is created");
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
        install_fake_claude(&state, directory.path(), "2.1.1", server);
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Running).await;

        install_fake_claude(&state, directory.path(), "2.1.2", server);
        state.remote_control.reconsider();
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("the deadline fits");
        while starts(directory.path()) != ["2.1.1", "2.1.2"] {
            assert!(Instant::now() < deadline, "{:?}", starts(directory.path()));
            sleep(Duration::from_millis(50)).await;
        }
        let running = wait_for(&state, ServerState::Running).await;
        assert_eq!(running.update, None);
        let versions = state.install_paths.versions_directory(Agent::Claude);
        assert!(!versions.join("2.1.1").exists());

        state.remote_control.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }

    #[tokio::test]
    async fn a_busy_server_restarts_on_a_new_claude_version_once_its_sessions_end() {
        let manager = TestManager::new();
        let directory = tempfile::tempdir().expect("temporary directory");
        let session = directory.path().join("session.pid");
        let server = format!(
            "sleep 60 & echo $! > {session}; echo 'https://claude.ai/code?environment=env_test'; exec sleep 60",
            session = session.display()
        );
        let state = fake_claude(&manager, directory.path(), &server);
        install_fake_claude(&state, directory.path(), "2.1.1", &server);
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Running).await;

        install_fake_claude(&state, directory.path(), "2.1.2", &server);
        state.remote_control.reconsider();
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("the deadline fits");
        let waiting = loop {
            let status = state
                .remote_control
                .status_of(&state.projects.0)
                .unwrap_or_default();
            if status.update.is_some() {
                break status;
            }
            assert!(Instant::now() < deadline, "still {status:?}");
            sleep(Duration::from_millis(50)).await;
        };
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
        state.remote_control.reconsider();
        while starts(directory.path()) != ["2.1.1", "2.1.2"] {
            assert!(Instant::now() < deadline, "{:?}", starts(directory.path()));
            sleep(Duration::from_millis(50)).await;
        }

        state.remote_control.begin_shut_down();
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
        assert_eq!(
            "Code anywhere with the Claude mobile app or https://claude.ai/code/session_01AB"
                .connect_url()
                .as_deref(),
            Some("https://claude.ai/code/session_01AB")
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
            .extend(["one", "Workspace not trusted"].map(str::to_owned));
        let failure = output.describe_exit(Err(io::Error::other("gone")));
        assert_eq!(
            failure,
            Failure {
                message: "gone: one\nWorkspace not trusted".to_owned(),
                problem: None
            }
        );
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
                Some(ServerProblem::SignIn),
            ),
            (
                "Error: Your organization requires Trusted Devices for Remote Control, but this device is not enrolled. Please run `/login` in Claude Code to enroll this device.",
                Some(ServerProblem::SignIn),
            ),
            (
                "Error: Remote Control isn't enabled for this account. If you recently changed plans, run `claude auth logout` then `claude auth login` to refresh your entitlements, or `claude doctor` for details.",
                Some(ServerProblem::NotAllowed),
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
                Some(ServerProblem::Unavailable),
            ),
            (
                "Error: RegisterEnvironment: Failed with status 503",
                Some(ServerProblem::Unavailable),
            ),
            ("Error: Workspace not trusted: /projects.", None),
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

    #[test]
    fn an_update_waits_for_the_last_session_or_the_deadline() {
        let usage = |sessions| {
            Some(ServerUsage {
                sessions,
                capacity: Some(4),
                memory_bytes: 1,
            })
        };
        let waiting = UpdateWait::starting_now("2.1.290".to_owned());
        assert!(!waiting.is_due(usage(1)));
        assert!(waiting.is_due(usage(0)));
        assert!(waiting.is_due(None));
        let overdue = UpdateWait {
            deadline: Instant::now(),
            ..waiting
        };
        assert!(overdue.is_due(usage(3)));
        assert_eq!(overdue.pending().version, "2.1.290");
        assert!(overdue.pending().restart_by > OffsetDateTime::now_utc());
    }

    #[test]
    fn the_log_tail_continues_into_the_rotated_log() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let log = ServerLog(directory.path().to_path_buf());
        assert!(log.tail().expect("an empty log is readable").is_empty());
        let numbered = |range: Range<usize>| -> String {
            range.map(|number| format!("line {number}\n")).collect()
        };
        fs::write(directory.path().join(ROTATED_LOG_FILE), numbered(0..150))
            .expect("rotated log is written");
        fs::write(directory.path().join(LOG_FILE), numbered(150..250)).expect("log is written");
        let tail = log.tail().expect("log is readable");
        assert_eq!(tail.len(), LOG_TAIL_LINES);
        assert_eq!(tail.first().map(String::as_str), Some("line 50"));
        assert_eq!(tail.last().map(String::as_str), Some("line 249"));
    }

    #[test]
    fn old_and_excess_session_logs_are_removed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let log = ServerLog(directory.path().join("projects"));
        log.prepare().expect("the directory is created");
        let write = |name: &str, bytes: u64, age: Duration| {
            let path = log.0.join(name);
            let file = fs::File::create(&path).expect("file is created");
            file.set_len(bytes).expect("file is sized");
            file.set_modified(SystemTime::now().checked_sub(age).expect("the time fits"))
                .expect("time is set");
        };
        let hour = Duration::from_secs(60 * 60);
        let old = SESSION_LOGS_KEPT_FOR.saturating_add(hour);
        write(LOG_FILE, 1, old);
        write(ROTATED_LOG_FILE, 1, old);
        write("server-old.log", 1, old);
        write("server-recent.log", 1, hour);
        write("bridge-transcript-recent.jsonl", 1, hour);
        write(
            "bridge-transcript-large.jsonl",
            SESSION_LOGS_LARGEST,
            hour.saturating_mul(2),
        );
        symlink(log.debug_file(), log.0.join(LATEST_LOG_LINK)).expect("link is created");

        log.prepare().expect("session logs are pruned");
        let mut remaining: Vec<String> = log
            .0
            .entries_or_empty()
            .expect("directory is readable")
            .into_iter()
            .filter_map(|path| Some(path.file_name()?.to_str()?.to_owned()))
            .collect();
        remaining.sort();
        assert_eq!(
            remaining,
            [
                "bridge-transcript-recent.jsonl",
                LATEST_LOG_LINK,
                "server-recent.log",
                LOG_FILE,
                ROTATED_LOG_FILE,
            ]
        );
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
        while group.usage(Some(4)).sessions < 2 {
            assert!(Instant::now() < deadline, "the group did not start");
            sleep(Duration::from_millis(20)).await;
        }
        let usage = group.usage(Some(4));
        assert_eq!((usage.sessions, usage.capacity), (2, Some(4)));
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
        write_script(
            &directory.path().join("claude"),
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n  auth) if [ -f {marker} ]; then echo '{SIGNED_IN}'; else echo '{{\"loggedIn\":false}}'; fi ;;\n  remote-control) echo 'https://claude.ai/code?environment=env_test'; exec sleep 60 ;;\nesac\n",
                marker = marker.display()
            ),
        );
        let supervisor = tokio::spawn(state.clone().supervise_remote_control());
        wait_for(&state, ServerState::Waiting).await;

        fs::write(&marker, "").expect("marker is written");
        state.agent_checks.refresh(Agent::Claude).await;
        wait_for(&state, ServerState::Running).await;

        state.remote_control.begin_shut_down();
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

        state.remote_control.begin_shut_down();
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
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("the deadline fits");
        while servers_started(&manager) < 2 {
            assert!(Instant::now() < deadline, "the server did not start again");
            sleep(Duration::from_millis(50)).await;
        }
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

        state
            .change_folder_choice("app", |choice| {
                choice.options = ClaudeOptions {
                    spawn: SpawnMode::Worktree,
                    permission_mode: Some("plan".to_owned()),
                    capacity: Some(2),
                };
            })
            .await
            .expect("the choice is saved");
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("the deadline fits");
        let arguments = |spawn: &str, rest: &str| {
            format!("remote-control --spawn {spawn} {rest}--permission-mode")
        };
        loop {
            let started = fs::read_to_string(&starts).unwrap_or_default();
            let app_starts: Vec<&str> = started
                .lines()
                .filter(|line| line.starts_with("app "))
                .collect();
            if let [first, second] = app_starts[..] {
                assert!(first.contains(&arguments("same-dir", "")), "{first}");
                assert!(first.contains("--permission-mode auto"), "{first}");
                assert!(
                    second.contains(&arguments("worktree", "--capacity 2 ")),
                    "{second}"
                );
                assert!(second.contains("--permission-mode plan"), "{second}");
                let projects: Vec<&str> = started
                    .lines()
                    .filter(|line| line.starts_with("projects "))
                    .collect();
                assert_eq!(projects.len(), 1, "{started}");
                assert!(!projects[0].contains("--capacity"), "{started}");
                break;
            }
            assert!(Instant::now() < deadline, "started {started}");
            sleep(Duration::from_millis(50)).await;
        }
        wait_in(&state, &app, ServerState::Running).await;

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
            .change_folder_choice("app", |choice| choice.serve = true)
            .await
            .expect("the choice is saved");
        wait_in(&state, &app, ServerState::Waiting).await;
        let published = published(&mut events).await;
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
            .change_folder_choice("app", |choice| choice.serve = true)
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
            .change_folder_choice("app", |choice| choice.serve = false)
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

        let published = published(&mut events).await;
        for expected in [Topic::Folders, Topic::RemoteControl] {
            assert!(
                published.contains(&expected),
                "{expected:?} missing from {published:?}"
            );
        }

        state.remote_control.begin_shut_down();
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

        state.remote_control.begin_shut_down();
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
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("the deadline fits");
        while state.remote_control.status_of(&unserved).is_some() {
            assert!(Instant::now() < deadline, "the folder server did not stop");
            sleep(Duration::from_millis(50)).await;
        }
        assert!(log("unserved").is_dir());

        for folder in [&served, &unserved] {
            fs::remove_dir_all(folder).expect("folder is removed");
        }
        state.remote_control.reconsider();
        while log("served").exists() || log("unserved").exists() {
            assert!(Instant::now() < deadline, "a log of a gone folder is left");
            sleep(Duration::from_millis(50)).await;
        }
        assert!(state.remote_control.log(&Served::Projects).0.is_dir());

        state.remote_control.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }
}
