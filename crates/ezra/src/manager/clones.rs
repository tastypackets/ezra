use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex as SyncMutex, PoisonError};
use std::time::Duration;

use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::Notify;
use tokio::time::{Instant, timeout};
use utoipa::ToSchema;

use super::events::{Events, Topic};
use super::folders::{FolderNameExt, ProjectsDirectory};
use super::remote_control::ProcessGroup;
use super::settings::{FolderChoice, SettingsError};
use super::state::AppState;
use crate::path_ext::PathExt;

const STAGING_PREFIX: &str = ".ezra-clone-";
const GITHUB: &str = "https://github.com";
const REPORT_INTERVAL: Duration = Duration::from_millis(250);
const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const LINES_KEPT: usize = 5;

/// Why a clone cannot start.
#[derive(Debug)]
pub enum CloneError {
    NoRepository,
    InvalidName,
    Exists(String),
    Running(String),
}

/// A repository to clone into a new folder in /projects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CloneRequest {
    /// A git URL or path, or `owner/repo` for GitHub.
    pub repository: String,
    /// The new folder's name.
    pub name: String,
    /// Whether the Claude app lists the folder, the "Serve new repositories" setting when absent.
    pub serve: Option<bool>,
}

/// A clone that is running or failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CloneStatus {
    /// The folder the repository goes into.
    pub name: String,
    /// The repository as given.
    pub repository: String,
    /// How far the clone has got, from 0 to 100.
    #[schema(maximum = 100)]
    pub percent: u8,
    /// Why the clone failed, absent while it runs.
    pub error: Option<String>,
}

/// Every clone that is running or failed, by folder name.
#[derive(Debug)]
pub struct Clones {
    entries: SyncMutex<BTreeMap<String, CloneEntry>>,
    events: Events,
}

#[derive(Debug)]
struct CloneEntry {
    status: CloneStatus,
    stop: Arc<Notify>,
}

impl Clones {
    /// Publishes every change to `events`.
    pub fn new(events: Events) -> Self {
        Self {
            entries: SyncMutex::default(),
            events,
        }
    }

    pub fn statuses(&self) -> Vec<CloneStatus> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(|entry| entry.status.clone())
            .collect()
    }

    /// Stops a running clone, or forgets a failed one. False when there is none.
    pub fn stop(&self, name: &str) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = entries.get(name) else {
            return false;
        };
        if entry.status.error.is_none() {
            entry.stop.notify_one();
            return true;
        }
        entries.remove(name);
        drop(entries);
        self.events.publish(Topic::Clones);
        true
    }

    /// Registers a clone, replacing a failed one into the same folder.
    fn begin(
        &self,
        name: &str,
        repository: &str,
    ) -> Result<(CloneStatus, Arc<Notify>), CloneError> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries
            .get(name)
            .is_some_and(|entry| entry.status.error.is_none())
        {
            return Err(CloneError::Running(name.to_owned()));
        }
        let status = CloneStatus {
            name: name.to_owned(),
            repository: repository.to_owned(),
            percent: 0,
            error: None,
        };
        let stop = Arc::new(Notify::new());
        entries.insert(
            name.to_owned(),
            CloneEntry {
                status: status.clone(),
                stop: Arc::clone(&stop),
            },
        );
        drop(entries);
        self.events.publish(Topic::Clones);
        Ok((status, stop))
    }

    fn update(&self, name: &str, change: impl FnOnce(&mut CloneStatus)) {
        let changed = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(name)
            .is_some_and(|entry| {
                let before = entry.status.clone();
                change(&mut entry.status);
                entry.status != before
            });
        if changed {
            self.events.publish(Topic::Clones);
        }
    }

    fn forget(&self, name: &str) {
        let forgotten = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        if forgotten.is_some() {
            self.events.publish(Topic::Clones);
        }
    }
}

/// Why a clone did not finish.
enum CloneEnd {
    Stopped,
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
enum PlaceError {
    #[error("could not move the clone into /projects: {0}")]
    Rename(io::Error),
    #[error(transparent)]
    Settings(#[from] SettingsError),
}

impl AppState {
    /// Starts cloning into a new folder in /projects and returns at once.
    pub fn start_clone(&self, request: CloneRequest) -> Result<CloneStatus, CloneError> {
        let repository = request.repository.trim();
        if repository.is_empty() {
            return Err(CloneError::NoRepository);
        }
        if !request.name.is_folder_name() {
            return Err(CloneError::InvalidName);
        }
        if fs::symlink_metadata(self.projects.folder(&request.name)).is_ok() {
            return Err(CloneError::Exists(request.name));
        }
        let (status, stop) = self.clones.begin(&request.name, repository)?;
        tokio::spawn(self.clone().run_clone(
            repository.clone_url(),
            request.name,
            request.serve,
            stop,
        ));
        Ok(status)
    }

    async fn run_clone(self, url: String, name: String, serve: Option<bool>, stop: Arc<Notify>) {
        let staging = self.projects.staging_folder(&name);
        let end = match self.clone_into(&url, &staging, &name, &stop).await {
            Ok(()) => self
                .place_clone(&staging, &name, serve)
                .await
                .map_err(|error| CloneEnd::Failed(error.to_string())),
            Err(end) => Err(end),
        };
        if end.is_err() {
            let leftover = staging.clone();
            if let Ok(Err(error)) =
                tokio::task::spawn_blocking(move || leftover.remove_if_present()).await
            {
                tracing::warn!("could not remove {}: {error}", staging.display());
            }
        }
        match end {
            Ok(()) => {
                tracing::info!("cloned {url} into /projects/{name}");
                self.remote_control.supervision.reconsider();
                self.events.publish(Topic::Folders);
                self.clones.forget(&name);
            }
            Err(CloneEnd::Stopped) => self.clones.forget(&name),
            Err(CloneEnd::Failed(message)) => {
                tracing::warn!("could not clone {url}: {message}");
                self.clones
                    .update(&name, |status| status.error = Some(message));
            }
        }
    }

    async fn clone_into(
        &self,
        url: &str,
        staging: &Path,
        name: &str,
        stop: &Notify,
    ) -> Result<(), CloneEnd> {
        let leftover = staging.to_path_buf();
        tokio::task::spawn_blocking(move || leftover.remove_if_present())
            .await
            .map_err(io::Error::other)
            .and_then(|removed| removed)
            .map_err(|error| CloneEnd::Failed(format!("could not prepare the clone: {error}")))?;
        let mut child = self
            .git_tools
            .clone_command(url, staging)
            .spawn()
            .map_err(|error| CloneEnd::Failed(format!("could not start git: {error}")))?;
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .map(|id| ProcessGroup(Pid::from_raw(id)));
        let mut reader = child.stderr.take().map(|stderr| {
            tokio::spawn(CloneOutput::default().read(
                stderr,
                Arc::clone(&self.clones),
                name.to_owned(),
            ))
        });
        let exit = tokio::select! {
            exit = child.wait() => exit,
            () = stop.notified() => {
                if let Some(group) = &group {
                    group.terminate(&mut child).await;
                }
                let _already_gone = child.kill().await;
                if let Some(reader) = reader {
                    reader.abort();
                }
                return Err(CloneEnd::Stopped);
            }
        };
        let output = match &mut reader {
            Some(reader) => match timeout(OUTPUT_DRAIN_TIMEOUT, &mut *reader).await {
                Ok(Ok(output)) => output,
                _ => {
                    reader.abort();
                    CloneOutput::default()
                }
            },
            None => CloneOutput::default(),
        };
        match exit {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(CloneEnd::Failed(output.describe_failure(status))),
            Err(error) => Err(CloneEnd::Failed(format!("git did not finish: {error}"))),
        }
    }

    /// Moves the finished clone into place and records its choice under the settings lock.
    async fn place_clone(
        &self,
        staging: &Path,
        name: &str,
        serve: Option<bool>,
    ) -> Result<(), PlaceError> {
        let target = self.projects.folder(name);
        self.update_settings(|settings| {
            fs::rename(staging, &target).map_err(PlaceError::Rename)?;
            let claude = &mut settings.agents.claude;
            let serve = serve.unwrap_or(claude.remote_control.serve_repositories);
            claude.folders.insert(
                name.to_owned(),
                FolderChoice {
                    serve,
                    ..FolderChoice::default()
                },
            );
            Ok(())
        })
        .await
    }
}

impl ProjectsDirectory {
    /// Where a clone runs until it is complete, hidden from the folder list.
    fn staging_folder(&self, name: &str) -> PathBuf {
        self.0.join(format!("{STAGING_PREFIX}{name}"))
    }

    /// Removes what clones cut short by a restart left behind.
    pub fn remove_unfinished_clones(&self) -> io::Result<()> {
        for entry in self.0.entries_or_empty()? {
            if entry
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(STAGING_PREFIX))
            {
                entry.remove_if_present()?;
            }
        }
        Ok(())
    }
}

/// What git printed while cloning.
#[derive(Debug, Default)]
struct CloneOutput {
    /// The whole clone's percentage so far.
    percent: u8,
    /// The last lines that were not progress.
    last_lines: VecDeque<String>,
}

impl CloneOutput {
    /// Reads git's output until it ends, reporting the rising percentage to `clones` at most every
    /// 250 ms.
    async fn read(
        mut self,
        stream: impl AsyncRead + Unpin,
        clones: Arc<Clones>,
        name: String,
    ) -> Self {
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        let mut reported_at: Option<Instant> = None;
        loop {
            let buffer = match reader.fill_buf().await {
                Ok([]) | Err(_) => break,
                Ok(buffer) => buffer,
            };
            let length = buffer.len();
            for &byte in buffer {
                if !matches!(byte, b'\r' | b'\n') {
                    line.push(byte);
                    continue;
                }
                let rose = self.take(&String::from_utf8_lossy(&line));
                line.clear();
                if rose && reported_at.is_none_or(|at| at.elapsed() >= REPORT_INTERVAL) {
                    let percent = self.percent;
                    clones.update(&name, |status| status.percent = percent);
                    reported_at = Some(Instant::now());
                }
            }
            reader.consume(length);
        }
        self.take(&String::from_utf8_lossy(&line));
        self
    }

    /// Takes one line or progress update. True when the percentage rose.
    fn take(&mut self, line: &str) -> bool {
        let line = line.trim();
        if let Some(percent) = line.clone_percent() {
            let rose = percent > self.percent;
            self.percent = self.percent.max(percent);
            return rose;
        }
        if !line.is_empty() && !line.starts_with("Cloning into ") {
            self.last_lines.push_back(line.to_owned());
            if self.last_lines.len() > LINES_KEPT {
                self.last_lines.pop_front();
            }
        }
        false
    }

    fn describe_failure(&self, status: ExitStatus) -> String {
        if self.last_lines.is_empty() {
            format!("git {status}")
        } else {
            Vec::from(self.last_lines.clone()).join("\n")
        }
    }
}

trait CloneProgressExt {
    /// The whole clone's percentage a git progress line stands for, when it is one.
    fn clone_percent(&self) -> Option<u8>;
}

impl CloneProgressExt for str {
    fn clone_percent(&self) -> Option<u8> {
        let line = self.strip_prefix("remote:").map_or(self, str::trim_start);
        let (stage, rest) = line.split_once(':')?;
        let (start, share): (u16, u16) = match stage.trim() {
            "Counting objects" => (0, 5),
            "Compressing objects" => (5, 5),
            "Receiving objects" => (10, 70),
            "Resolving deltas" => (80, 10),
            "Updating files" | "Checking out files" => (90, 10),
            _ => return None,
        };
        let (percent, _) = rest.trim_start().split_once('%')?;
        let percent: u16 = percent.parse().ok()?;
        let within = share.saturating_mul(percent.min(100)).checked_div(100)?;
        u8::try_from(start.saturating_add(within)).ok()
    }
}

trait RepositoryExt {
    /// `owner/repo` as its GitHub URL, anything else as given.
    fn clone_url(&self) -> String;
}

impl RepositoryExt for str {
    fn clone_url(&self) -> String {
        let is_github_name = |part: &str| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
        };
        match self.split_once('/') {
            Some((owner, repository)) if is_github_name(owner) && is_github_name(repository) => {
                let repository = repository.strip_suffix(".git").unwrap_or(repository);
                format!("{GITHUB}/{owner}/{repository}.git")
            }
            _ => self.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_and_repository_mean_github() {
        for (given, url) in [
            ("zeke/app", "https://github.com/zeke/app.git"),
            ("zeke/app.git", "https://github.com/zeke/app.git"),
            (
                "my-org/some_lib.rs",
                "https://github.com/my-org/some_lib.rs.git",
            ),
            (
                "https://github.com/zeke/app.git",
                "https://github.com/zeke/app.git",
            ),
            ("git@github.com:zeke/app.git", "git@github.com:zeke/app.git"),
            ("/srv/git/app", "/srv/git/app"),
            ("file:///srv/git/app", "file:///srv/git/app"),
            ("../app", "../app"),
            ("a/b/c", "a/b/c"),
        ] {
            assert_eq!(given.clone_url(), url, "{given}");
        }
    }

    #[test]
    fn progress_lines_become_one_percentage() {
        for (line, percent) in [
            ("remote: Counting objects: 100% (12/12), done.", Some(5)),
            ("remote: Compressing objects:  50% (4/8)", Some(7)),
            (
                "Receiving objects:  50% (6/12), 1.20 MiB | 2.00 MiB/s",
                Some(45),
            ),
            ("Receiving objects: 100% (12/12), done.", Some(80)),
            ("Resolving deltas: 100% (3/3), done.", Some(90)),
            ("Updating files:  50% (10/20)", Some(95)),
            ("Updating files: 100% (20/20), done.", Some(100)),
            ("remote: Enumerating objects: 12, done.", None),
            ("fatal: repository 'x' not found", None),
            ("Receiving objects: lots", None),
        ] {
            assert_eq!(line.clone_percent(), percent, "{line}");
        }
    }

    #[test]
    fn output_keeps_the_percentage_rising_and_the_last_other_lines() {
        let mut output = CloneOutput::default();
        assert!(!output.take("Cloning into '/projects/.ezra-clone-app'..."));
        assert!(output.take("Receiving objects:  50% (6/12)"));
        assert!(!output.take("remote: Compressing objects: 100% (8/8), done."));
        assert_eq!(output.percent, 45);
        for number in 0..7 {
            output.take(&format!("line {number}"));
        }
        output.take("fatal: early EOF");
        assert_eq!(
            Vec::from(output.last_lines),
            ["line 3", "line 4", "line 5", "line 6", "fatal: early EOF"]
        );
    }
}
