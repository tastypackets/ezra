use std::cmp::Reverse;
use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex as SyncMutex, PoisonError};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout, timeout_at};
use utoipa::ToSchema;

use super::agents::Agent;
use super::events::{Events, Topic};
use super::login::{SignInStatus, StrExt};
use super::state::AppState;
use crate::path_ext::PathExt;

pub const RECHECK_INTERVAL: Duration = Duration::from_secs(60);
pub const USAGE_INTERVAL: Duration = Duration::from_secs(15);
pub const FIRST_RETRY_DELAY: Duration = Duration::from_secs(5);
const LONGEST_RETRY_DELAY: Duration = Duration::from_secs(300);
const HEALTHY_RUN: Duration = Duration::from_secs(600);
const UPDATE_RESTART_DEADLINE: Duration = Duration::from_secs(6 * 60 * 60);
pub const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const OUTPUT_LINES_KEPT: usize = 20;
const OUTPUT_LINES_REPORTED: usize = 5;
const LOG_FILE: &str = "server.log";
pub const LOG_TIME: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");
const ROTATED_LOG_FILE: &str = "server.log.1";
const LATEST_LOG_LINK: &str = "latest";
const LOG_TAIL_LINES: usize = 200;
const LOG_TAIL_BYTES: u64 = 256 * 1024;
const SESSION_LOGS_KEPT_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const SESSION_LOGS_LARGEST: u64 = 64 * 1024 * 1024;

/// The signals an agent's supervisor and servers wait on, and whether the supervisor has stopped.
#[derive(Debug, Default)]
pub struct Supervision {
    changes: watch::Sender<u64>,
    restarts: watch::Sender<u64>,
    shutdown: watch::Sender<bool>,
    stopped: watch::Sender<bool>,
}

impl Supervision {
    /// Settings, folders or the install changed, so servers may need to start, stop or restart.
    pub fn reconsider(&self) {
        self.changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// The sign-in changed, maybe to another account, so servers start again with it.
    pub fn restart(&self) {
        self.restarts
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    pub fn begin_shut_down(&self) {
        self.shutdown.send_replace(true);
    }

    /// Waits until the supervisor has stopped. False when `longest` passed first.
    pub async fn wait_until_stopped(&self, longest: Duration) -> bool {
        let mut stopped = self.stopped.subscribe();
        timeout(longest, stopped.wait_for(|stopped| *stopped))
            .await
            .is_ok()
    }

    pub fn signals(&self, sign_in: watch::Receiver<Option<SignInStatus>>) -> Signals {
        Signals {
            sign_in,
            changes: self.changes.subscribe(),
            restarts: self.restarts.subscribe(),
            shutdown: self.shutdown.subscribe(),
        }
    }

    pub fn mark_stopped(&self) {
        self.stopped.send_replace(true);
    }
}

/// One task's view of the sign-in, change, restart and shutdown signals.
pub struct Signals {
    pub sign_in: watch::Receiver<Option<SignInStatus>>,
    pub changes: watch::Receiver<u64>,
    pub restarts: watch::Receiver<u64>,
    pub shutdown: watch::Receiver<bool>,
}

/// What woke a waiting task.
#[derive(Debug, PartialEq, Eq)]
pub enum Wake {
    Changed,
    ShutDown,
}

impl Signals {
    pub fn is_shutting_down(&self) -> bool {
        *self.shutdown.borrow()
    }

    pub async fn wait_for_change(&mut self, longest: Duration) -> Wake {
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

pub trait FlagExt {
    async fn until_set(&mut self);
}

impl FlagExt for watch::Receiver<bool> {
    async fn until_set(&mut self) {
        let _closed_counts_as_set = self.wait_for(|set| *set).await.map(|_| ());
    }
}

/// What should be running right now. `Unknown` until the agent first answers a sign-in check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted<L, W> {
    Off,
    Waiting(W),
    Unknown,
    Server(L),
}

/// Why a server run ended.
pub enum RunEnd<P> {
    /// What should run changed, so it was stopped on purpose.
    Reconsidered,
    /// The manager is stopping.
    ShutDown,
    /// It could not start, did not connect, or stopped by itself.
    Failed(Failure<P>),
}

/// What went wrong with a server run.
#[derive(Debug, PartialEq, Eq)]
pub struct Failure<P> {
    pub message: String,
    pub problem: Option<P>,
}

impl<P> From<String> for Failure<P> {
    fn from(message: String) -> Self {
        Self {
            message,
            problem: None,
        }
    }
}

/// What to do with a running server after something changed.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    /// The sign-in could not be checked, so nothing changes.
    Unsure,
    Stop,
    /// Restart it on this version once it is not busy.
    Update(String),
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

/// A running server waiting to restart on a newer version.
pub struct UpdateWait {
    pub version: String,
    restart_by: OffsetDateTime,
    deadline: Instant,
}

impl UpdateWait {
    pub fn starting_now(version: String) -> Self {
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

    pub fn pending(&self) -> PendingUpdate {
        PendingUpdate {
            version: self.version.clone(),
            restart_by: self.restart_by,
        }
    }

    /// Due once the server is not busy, or at the deadline.
    pub fn is_due(&self, busy: bool) -> bool {
        !busy || Instant::now() >= self.deadline
    }
}

/// Unexpected stops in a row, which set how long to wait before starting again.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Failures(u32);

impl Failures {
    /// Forgets the earlier failures when the run that began at `started` lasted long enough to be
    /// healthy.
    pub fn forget_after_healthy_run(&mut self, started: Instant) {
        if started.elapsed() >= HEALTHY_RUN {
            *self = Self::default();
        }
    }

    pub fn add_one(&mut self) {
        self.0 = self.0.saturating_add(1);
    }

    pub fn retry_delay(self) -> Duration {
        let doublings = self.0.saturating_sub(1).min(16);
        FIRST_RETRY_DELAY
            .saturating_mul(2_u32.saturating_pow(doublings))
            .min(LONGEST_RETRY_DELAY)
    }
}

/// A status the API reads, published to its topic whenever a change alters it.
#[derive(Debug)]
pub struct Published<T> {
    value: SyncMutex<T>,
    events: Events,
    topic: Topic,
}

impl<T: Clone + PartialEq> Published<T> {
    pub fn new(value: T, events: Events, topic: Topic) -> Self {
        Self {
            value: SyncMutex::new(value),
            events,
            topic,
        }
    }

    pub fn read<R>(&self, read: impl FnOnce(&T) -> R) -> R {
        read(&self.value.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Applies `change`, and publishes when the value differs afterwards.
    pub fn update<R>(&self, change: impl FnOnce(&mut T) -> R) -> R {
        let (changed, result) = {
            let mut value = self.value.lock().unwrap_or_else(PoisonError::into_inner);
            let before = value.clone();
            let result = change(&mut value);
            (*value != before, result)
        };
        if changed {
            self.events.publish(self.topic);
        }
        result
    }
}

/// One server's log directory. The manager adds the server's output lines to `server.log`. Claude
/// Code also writes its debug log there, rotates it to `server.log.1` at 10 MiB and adds a debug
/// log and a transcript per session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerLog(pub PathBuf);

impl ServerLog {
    pub fn debug_file(&self) -> PathBuf {
        self.0.join(LOG_FILE)
    }

    /// Adds a line the server printed, stamped like Claude Code's own lines.
    async fn append_output(&self, line: &str) -> io::Result<()> {
        let stamp = OffsetDateTime::now_utc()
            .format(LOG_TIME)
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
    pub async fn prepare(&self) -> io::Result<()> {
        let directory = self.0.clone();
        tokio::task::spawn_blocking(move || -> io::Result<()> {
            fs::create_dir_all(&directory)?;
            let mut sessions: Vec<(PathBuf, SystemTime, u64)> = directory
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
        })
        .await
        .map_err(io::Error::other)?
    }
}

/// Looks at each line a server prints, after it is logged.
pub trait LineWatcher: Send + 'static {
    /// False leaves the line out of the lines an exit is described with.
    fn watch(&mut self, line: &str) -> bool;
}

/// A server's output: the tasks that log its stdout and stderr, and the last lines they kept.
#[derive(Debug, Default)]
pub struct ServerOutput {
    kept: Arc<SyncMutex<VecDeque<String>>>,
    readers: Vec<JoinHandle<()>>,
}

impl ServerOutput {
    /// Reads the child's stdout and stderr into `log`, each through a watcher of its own.
    pub fn read<W: LineWatcher>(
        child: &mut Child,
        log: &ServerLog,
        mut watcher: impl FnMut() -> W,
    ) -> Self {
        let kept: Arc<SyncMutex<VecDeque<String>>> = Arc::default();
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            let reader = LineReader {
                log: log.clone(),
                watcher: watcher(),
                kept: Arc::clone(&kept),
            };
            readers.push(tokio::spawn(reader.read(stdout)));
        }
        if let Some(stderr) = child.stderr.take() {
            let reader = LineReader {
                log: log.clone(),
                watcher: watcher(),
                kept: Arc::clone(&kept),
            };
            readers.push(tokio::spawn(reader.read(stderr)));
        }
        Self { kept, readers }
    }

    /// Gives the readers a moment to log the last lines, then stops them.
    pub async fn stop_reading(&mut self) {
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

    /// The exit followed by the last lines kept, and what `problem` finds in the kept lines.
    pub fn describe_exit<P>(
        &self,
        exit: io::Result<ExitStatus>,
        problem: impl FnOnce(&[&str]) -> Option<P>,
    ) -> Failure<P> {
        let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
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
            problem: problem(&lines),
        }
    }
}

/// Reads one of a server's streams line by line into its log and the kept lines.
struct LineReader<W> {
    log: ServerLog,
    watcher: W,
    kept: Arc<SyncMutex<VecDeque<String>>>,
}

impl<W: LineWatcher> LineReader<W> {
    async fn read(mut self, stream: impl AsyncRead + Unpin) {
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
            if line.trim().is_empty() {
                continue;
            }
            if let Err(error) = self.log.append_output(&line).await {
                tracing::debug!("could not log to {}: {error}", self.log.0.display());
            }
            if !self.watcher.watch(&line) {
                continue;
            }
            let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
            kept.push_back(line);
            while kept.len() > OUTPUT_LINES_KEPT {
                kept.pop_front();
            }
        }
    }
}

impl AppState {
    /// Checks the agent's sign-in when the last answer is older than the recheck interval. False
    /// when shutdown interrupted it.
    pub async fn check_sign_in(&self, agent: Agent, signals: &mut Signals) -> bool {
        tokio::select! {
            () = self.agent_checks.check_sign_in(agent, RECHECK_INTERVAL) => {}
            () = signals.shutdown.until_set() => return false,
        }
        signals.sign_in.mark_unchanged();
        true
    }

    /// The agent's install or sign-in changed, so its remote control may need to start or stop.
    pub fn reconsider_remote(&self, agent: Agent) {
        match agent {
            Agent::Claude => self.remote_control.supervision.reconsider(),
            Agent::Codex => {}
        }
    }

    /// Skipped while an install runs, which removes them itself.
    pub async fn remove_unused_versions(&self, agent: Agent) {
        let Ok(_one_install_at_a_time) = self.install_lock.try_lock() else {
            return;
        };
        let paths = Arc::clone(&self.install_paths);
        let removed =
            tokio::task::spawn_blocking(move || paths.remove_unused_versions(agent)).await;
        if let Ok(Err(error)) = removed {
            tracing::warn!("could not remove old {agent} versions: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::mem;
    use std::ops::Range;
    use std::os::unix::fs::symlink;
    use std::process::Stdio;

    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    use tokio::process::Command;

    use super::*;
    use crate::manager::api::test_support::TestManager;

    struct SkipLines;

    impl LineWatcher for SkipLines {
        fn watch(&mut self, line: &str) -> bool {
            line != "skip"
        }
    }

    #[tokio::test]
    async fn waiting_for_the_stop_ends_once_it_is_marked() {
        let supervision = Supervision::default();
        assert!(!supervision.wait_until_stopped(Duration::ZERO).await);
        supervision.mark_stopped();
        assert!(supervision.wait_until_stopped(Duration::ZERO).await);
    }

    #[tokio::test]
    async fn each_agent_reconsiders_only_its_own_remote_control() {
        let manager = TestManager::new();
        let claude = manager.state.remote_control.supervision.changes.subscribe();
        manager.state.reconsider_remote(Agent::Codex);
        assert!(!claude.has_changed().expect("the sender is alive"));
        manager.state.reconsider_remote(Agent::Claude);
        assert!(claude.has_changed().expect("the sender is alive"));
    }

    #[tokio::test]
    async fn reading_stops_when_a_process_the_server_left_keeps_its_output_open() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let log = ServerLog(directory.path().to_path_buf());
        let mut child = Command::new("sh")
            .args(["-c", "echo one; echo skip; sleep 30 & exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("the server starts");
        let group = Pid::from_raw(
            child
                .id()
                .and_then(|id| i32::try_from(id).ok())
                .expect("the server has a pid"),
        );
        let mut output = ServerOutput::read(&mut child, &log, || SkipLines);
        let exit = child.wait().await;

        let started = Instant::now();
        output.stop_reading().await;
        let waited = started.elapsed();
        killpg(group, Signal::SIGKILL).expect("the sleep is killed");
        assert!(
            (OUTPUT_DRAIN_TIMEOUT..OUTPUT_DRAIN_TIMEOUT.saturating_mul(2)).contains(&waited),
            "{waited:?}"
        );
        assert_eq!(
            output.describe_exit(exit, |_| None::<()>).message,
            "exit status: 0: one"
        );
        let logged = log.tail().expect("the log is readable");
        assert!(
            logged.iter().any(|line| line.ends_with("[OUTPUT] skip")),
            "{logged:?}"
        );
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
    fn only_a_healthy_run_forgets_the_failures() {
        let mut failures = Failures::default();
        failures.add_one();
        failures.add_one();
        failures.forget_after_healthy_run(Instant::now());
        assert_eq!(failures, Failures(2));
        let long_ago = Instant::now()
            .checked_sub(HEALTHY_RUN)
            .expect("the manager has run long enough");
        failures.forget_after_healthy_run(long_ago);
        assert_eq!(failures, Failures(0));
    }

    #[test]
    fn exit_description_ends_with_the_last_lines() {
        let output = ServerOutput::default();
        output
            .kept
            .lock()
            .expect("output lock")
            .extend(["one", "Workspace not trusted"].map(str::to_owned));
        let failure = output.describe_exit(Err(io::Error::other("gone")), |lines| {
            lines.contains(&"one").then_some("found")
        });
        assert_eq!(
            failure,
            Failure {
                message: "gone: one\nWorkspace not trusted".to_owned(),
                problem: Some("found"),
            }
        );
    }

    #[test]
    fn an_update_waits_while_busy_until_the_deadline() {
        let waiting = UpdateWait::starting_now("2.1.290".to_owned());
        assert!(!waiting.is_due(true));
        assert!(waiting.is_due(false));
        let overdue = UpdateWait {
            deadline: Instant::now(),
            ..waiting
        };
        assert!(overdue.is_due(true));
        assert!(overdue.is_due(false));
        assert_eq!(overdue.pending().version, "2.1.290");
        assert!(overdue.pending().restart_by > OffsetDateTime::now_utc());
    }

    #[test]
    fn only_a_change_is_published() {
        let events = Events::default();
        let status = Published::new(1, events.clone(), Topic::RemoteControl);
        let revision = events.revision();

        status.update(|value| *value = 1);
        assert_eq!(events.revision(), revision);
        assert_eq!(status.update(|value| mem::replace(value, 2)), 1);
        assert_eq!(status.read(|value| *value), 2);
        assert_eq!(events.revision(), revision.wrapping_add(1));
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

    #[tokio::test]
    async fn old_and_excess_session_logs_are_removed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let log = ServerLog(directory.path().join("projects"));
        log.prepare().await.expect("the directory is created");
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

        log.prepare().await.expect("session logs are pruned");
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
}
