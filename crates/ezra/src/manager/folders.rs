use std::collections::BTreeSet;
use std::fs;
use std::future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio::time::{Instant, MissedTickBehavior, interval, timeout};
use utoipa::ToSchema;

use super::events::Topic;
use super::remote_control::SpawnMode;
use super::settings::{FolderChoice, SettingsError};
use super::state::AppState;
use crate::path_ext::PathExt;

#[derive(Debug, thiserror::Error)]
pub enum FolderChoiceError {
    #[error("no such folder")]
    NoSuchFolder,
    #[error("worktrees need a git repository")]
    NotARepository,
    #[error("could not list the folders in /projects: {0}")]
    Scan(io::Error),
    #[error(transparent)]
    Settings(#[from] SettingsError),
}

pub const PROJECTS_DIRECTORY: &str = "/projects";
const RESCAN_INTERVAL: Duration = Duration::from_secs(30);
const RESCAN_INTERVAL_WHILE_WATCHING: Duration = Duration::from_secs(300);
const QUIET_PERIOD: Duration = Duration::from_secs(1);
const LONGEST_SETTLE: Duration = Duration::from_secs(10);
const BLOCK_START: &str = "<!-- ezra:folders:start -->";
const BLOCK_END: &str = "<!-- ezra:folders:end -->";
const UNBORN_REFTABLE_BRANCH: &str = ".invalid";

/// One top-level folder in /projects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Folder {
    /// The folder's name in /projects.
    pub name: String,
    /// Present when the folder is a git repository.
    pub git: Option<GitDetails>,
}

impl Folder {
    /// The name, then the branch and origin when it is a repository.
    fn describe(&self) -> String {
        let Some(git) = &self.git else {
            return self.name.clone();
        };
        let details: Vec<String> = [
            git.branch.as_ref().map(|branch| format!("branch {branch}")),
            git.repository
                .as_ref()
                .map(|repository| format!("origin {repository}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        if details.is_empty() {
            format!("{}: git repository", self.name)
        } else {
            format!("{}: {}", self.name, details.join(", "))
        }
    }
}

/// Where a repository comes from and what it has checked out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GitDetails {
    /// The checked-out branch, absent when not on a branch.
    pub branch: Option<String>,
    /// Where `origin` points, without credentials.
    pub repository: Option<String>,
    /// Linked worktrees that still exist, counted on the main checkout only.
    pub worktrees: u32,
}

/// A folder with what the Claude app sees of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FolderStatus {
    #[serde(flatten)]
    pub folder: Folder,
    /// Chosen to be served to the Claude app, by its own choice or by default.
    pub serve: bool,
    /// Where its sessions work, always `same-dir` outside a repository.
    pub spawn: SpawnMode,
}

/// The directory whose folders agents work in, usually /projects.
#[derive(Debug, Clone)]
pub struct ProjectsDirectory(pub PathBuf);

impl ProjectsDirectory {
    pub fn folder(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// The top-level folder called `name`, when there is one.
    pub fn find(&self, name: &str) -> Option<Folder> {
        let path = self.folder(name);
        fs::symlink_metadata(&path)
            .is_ok_and(|metadata| metadata.is_dir())
            .then(|| Folder {
                name: name.to_owned(),
                git: GitCheckout::in_folder(&path).details(),
            })
    }

    /// Top-level folders sorted by name. Hidden folders, and ones that vanish or cannot be read
    /// while listing, are left out.
    pub fn folders(&self) -> io::Result<Vec<Folder>> {
        let mut folders: Vec<Folder> = fs::read_dir(&self.0)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                (!name.starts_with('.')).then(|| Folder {
                    git: GitCheckout::in_folder(&entry.path()).details(),
                    name,
                })
            })
            .collect();
        folders.sort_by(|first, second| first.name.cmp(&second.name));
        Ok(folders)
    }

    /// Rewrites the managed block in `AGENTS.md` when the folders changed, keeping everything else.
    pub fn describe_folders_for_agents(
        &self,
        folders: &[Folder],
        github_account: Option<&str>,
    ) -> io::Result<()> {
        let path = self.0.join("AGENTS.md");
        let existing = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        let block = FoldersBlock {
            folders,
            github_account,
        };
        let updated = existing.with_folders_block(&block.to_markdown());
        if updated == existing {
            return Ok(());
        }
        fs::write(path, updated)
    }
}

/// The part of `/projects/AGENTS.md` the manager owns.
struct FoldersBlock<'block> {
    folders: &'block [Folder],
    github_account: Option<&'block str>,
}

impl FoldersBlock<'_> {
    fn to_markdown(&self) -> String {
        let mut markdown = format!(
            "{BLOCK_START}\n\
             This file applies only when the working directory is /projects. Inside a project \
             folder, ignore it.\n\
             /projects holds the projects in this box, one per folder.\n"
        );
        match self.github_account {
            Some("") => markdown.push_str("gh is signed in to GitHub.\n"),
            Some(account) => {
                markdown.push_str(&format!("gh is signed in to GitHub as {account}.\n"))
            }
            None => {}
        }
        if self.folders.is_empty() {
            markdown.push_str("No projects yet.\n");
        } else {
            markdown.push_str("Folders:\n");
            for folder in self.folders {
                markdown.push_str(&format!("- {}\n", folder.describe().single_line()));
            }
        }
        markdown.push_str(BLOCK_END);
        markdown
    }
}

trait MarkdownExt {
    /// The text with control characters, such as line breaks, as spaces.
    fn single_line(&self) -> String;
    /// Replaces the managed block, or adds it at the end when there is none. Markers only count
    /// as whole lines, and the last start line pairs with the next end line.
    fn with_folders_block(&self, block: &str) -> String;
}

impl MarkdownExt for str {
    fn single_line(&self) -> String {
        self.chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect()
    }

    fn with_folders_block(&self, block: &str) -> String {
        let lines: Vec<&str> = self.lines().collect();
        let block_lines = lines
            .iter()
            .rposition(|line| line.trim() == BLOCK_START)
            .and_then(|start| {
                let (before, from_start) = lines.split_at(start);
                let (end, _) = from_start
                    .iter()
                    .enumerate()
                    .skip(1)
                    .find(|(_, line)| line.trim() == BLOCK_END)?;
                let (_, from_end) = from_start.split_at(end);
                Some((before, from_end.get(1..).unwrap_or_default()))
            });
        match block_lines {
            Some((before, after)) => {
                let mut updated = before.to_vec();
                updated.push(block);
                updated.extend_from_slice(after);
                let mut text = updated.join("\n");
                if self.ends_with('\n') {
                    text.push('\n');
                }
                text
            }
            None if self.trim().is_empty() => format!("{block}\n"),
            None => format!("{}\n\n{block}\n", self.trim_end()),
        }
    }
}

/// A folder's git metadata.
struct GitCheckout {
    git_directory: Option<PathBuf>,
}

impl GitCheckout {
    fn in_folder(folder: &Path) -> Self {
        let dot_git = folder.join(".git");
        let git_directory = if dot_git.is_dir() {
            Some(dot_git)
        } else {
            fs::read_to_string(&dot_git).ok().and_then(|pointer| {
                pointer
                    .trim()
                    .strip_prefix("gitdir:")
                    .map(|path| folder.join(path.trim()))
            })
        };
        Self { git_directory }
    }

    fn details(&self) -> Option<GitDetails> {
        let git_directory = self.git_directory.as_ref()?;
        let branch = fs::read_to_string(git_directory.join("HEAD"))
            .ok()
            .and_then(|head| {
                head.trim()
                    .strip_prefix("ref:")
                    .map(str::trim_start)
                    .and_then(|reference| reference.strip_prefix("refs/heads/"))
                    .filter(|branch| *branch != UNBORN_REFTABLE_BRANCH)
                    .map(str::to_owned)
            });
        let common_directory = fs::read_to_string(git_directory.join("commondir")).map_or_else(
            |_| git_directory.clone(),
            |common| git_directory.join(common.trim()),
        );
        let repository = fs::read_to_string(common_directory.join("config"))
            .ok()
            .and_then(|config| config.origin_url())
            .map(|url| url.without_credentials());
        Some(GitDetails {
            branch,
            repository,
            worktrees: Self::linked_worktrees(git_directory),
        })
    }

    /// Linked worktrees whose folder still exists, from the `gitdir` file of each entry in
    /// `worktrees`.
    fn linked_worktrees(git_directory: &Path) -> u32 {
        let entries = git_directory
            .join("worktrees")
            .entries_or_empty()
            .unwrap_or_default();
        let existing = entries
            .iter()
            .filter(|entry| {
                fs::read_to_string(entry.join("gitdir"))
                    .is_ok_and(|gitdir| entry.join(gitdir.trim()).exists())
            })
            .count();
        u32::try_from(existing).unwrap_or(u32::MAX)
    }
}

trait GitConfigExt {
    /// `url` under `[remote "origin"]`, unquoted and without a trailing comment.
    fn origin_url(&self) -> Option<String>;
    /// Drops `user:token@` from http and https URLs.
    fn without_credentials(&self) -> String;
}

impl GitConfigExt for str {
    fn origin_url(&self) -> Option<String> {
        let mut in_origin = false;
        for line in self.lines().map(str::trim) {
            let entry = if let Some(section) = line.strip_prefix('[') {
                let (header, rest) = section.split_once(']').unwrap_or((section, ""));
                let (kind, name) = header
                    .split_once(char::is_whitespace)
                    .unwrap_or((header, ""));
                in_origin = kind.eq_ignore_ascii_case("remote") && name.trim() == "\"origin\"";
                rest.trim()
            } else {
                line
            };
            if in_origin
                && let Some((key, value)) = entry.split_once('=')
                && key.trim().eq_ignore_ascii_case("url")
            {
                return Some(value.trim().config_value());
            }
        }
        None
    }

    fn without_credentials(&self) -> String {
        let Some((scheme, rest)) = self.split_once("://") else {
            return self.to_owned();
        };
        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return self.to_owned();
        }
        let (authority, path) = rest
            .split_once('/')
            .map_or((rest, None), |(authority, path)| (authority, Some(path)));
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        match path {
            Some(path) => format!("{scheme}://{host}/{path}"),
            None => format!("{scheme}://{host}"),
        }
    }
}

trait ConfigValueExt {
    /// A git config value without quotes, escapes or a trailing `#` or `;` comment.
    fn config_value(&self) -> String;
}

impl ConfigValueExt for str {
    fn config_value(&self) -> String {
        let mut value = String::new();
        let mut quoted = false;
        let mut characters = self.chars();
        while let Some(character) = characters.next() {
            match character {
                '"' => quoted = !quoted,
                '\\' => value.extend(characters.next()),
                '#' | ';' if !quoted => break,
                other => value.push(other),
            }
        }
        value.trim().to_owned()
    }
}

impl AppState {
    /// The folders in /projects, none when it does not exist.
    pub async fn folders(&self) -> io::Result<Vec<Folder>> {
        let projects = self.projects.clone();
        match tokio::task::spawn_blocking(move || projects.folders()).await {
            Ok(Ok(folders)) => Ok(folders),
            Ok(Err(error)) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Ok(Err(error)) => Err(error),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    pub async fn folder_statuses(&self) -> io::Result<Vec<FolderStatus>> {
        let folders = self.folders().await?;
        let settings = self.settings.lock().await;
        Ok(folders
            .into_iter()
            .map(|folder| {
                let FolderChoice { serve, spawn } = settings.agents.claude.folder_choice(&folder);
                FolderStatus {
                    folder,
                    serve,
                    spawn,
                }
            })
            .collect())
    }

    /// Records the folder's own choice, and drops the choices of folders that are gone.
    pub async fn change_folder_choice(
        &self,
        name: &str,
        change: impl FnOnce(&mut FolderChoice),
    ) -> Result<(), FolderChoiceError> {
        let folders = self.folders().await.map_err(FolderChoiceError::Scan)?;
        let folder = folders
            .iter()
            .find(|folder| folder.name == name)
            .ok_or(FolderChoiceError::NoSuchFolder)?;
        self.update_settings(|settings| {
            let claude = &mut settings.agents.claude;
            let mut choice = claude.folder_choice(folder);
            change(&mut choice);
            if choice.spawn == SpawnMode::Worktree && folder.git.is_none() {
                return Err(FolderChoiceError::NotARepository);
            }
            claude
                .folders
                .retain(|recorded, _| folders.iter().any(|present| present.name == *recorded));
            claude.folders.insert(name.to_owned(), choice);
            Ok(())
        })
        .await?;
        self.remote_control.reconsider();
        self.events.publish(Topic::Folders);
        Ok(())
    }

    /// Gives repositories seen for the first time the default choice, and drops the choices of
    /// folders that are gone. True when the choices changed.
    pub async fn record_folder_choices(&self, folders: &[Folder]) -> Result<bool, SettingsError> {
        let mut changed = false;
        self.update_settings(|settings| {
            let serve_repositories = settings.agents.claude.remote_control.serve_repositories;
            let choices = &mut settings.agents.claude.folders;
            let before = choices.clone();
            choices.retain(|name, _| folders.iter().any(|folder| folder.name == *name));
            for folder in folders.iter().filter(|folder| folder.git.is_some()) {
                choices.entry(folder.name.clone()).or_insert(FolderChoice {
                    serve: serve_repositories,
                    spawn: SpawnMode::SameDir,
                });
            }
            changed = *choices != before;
            Ok::<(), SettingsError>(())
        })
        .await?;
        Ok(changed)
    }

    /// Rescans /projects on every change and every few minutes, then updates `AGENTS.md`, the folder
    /// switches and the servers.
    pub async fn describe_folders_regularly(self) {
        let mut watcher = FolderWatcher::start(&self.projects.0);
        let mut rescan = interval(watcher.rescan_interval());
        rescan.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut github_account = self.github_account.subscribe();
        let mut known: Vec<Folder> = Vec::new();
        loop {
            tokio::select! {
                _ = rescan.tick() => {}
                _ = github_account.changed() => {}
                () = watcher.settled_change() => {}
            }
            let projects = self.projects.clone();
            let account = github_account.borrow_and_update().clone();
            let described = tokio::task::spawn_blocking(move || {
                let folders = projects.folders()?;
                if let Err(error) =
                    projects.describe_folders_for_agents(&folders, account.as_deref())
                {
                    tracing::warn!("could not write /projects/AGENTS.md: {error}");
                }
                Ok::<_, io::Error>(folders)
            })
            .await;
            match described {
                Ok(Ok(folders)) => {
                    watcher.follow(&self.projects, &folders);
                    let recorded =
                        self.record_folder_choices(&folders)
                            .await
                            .unwrap_or_else(|error| {
                                tracing::warn!("could not record the folder choices: {error}");
                                false
                            });
                    if recorded || folders != known {
                        known = folders;
                        self.remote_control.reconsider();
                        self.events.publish(Topic::Folders);
                    }
                }
                Ok(Err(error)) => {
                    tracing::warn!("could not list the folders in /projects: {error}");
                }
                Err(error) => tracing::warn!("folder scan stopped: {error}"),
            }
        }
    }
}

/// Watches /projects, each folder in it, and each repository's `.git`. Without inotify, rescans
/// run every 30 s instead.
struct FolderWatcher {
    watcher: Option<RecommendedWatcher>,
    watched: BTreeSet<PathBuf>,
    changed: Arc<Notify>,
}

impl FolderWatcher {
    fn start(projects: &Path) -> Self {
        let changed = Arc::new(Notify::new());
        let notifier = Arc::clone(&changed);
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let relevant = event.map_or(true, |event| {
                matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Remove(_)
                        | EventKind::Modify(ModifyKind::Name(_))
                )
            });
            if relevant {
                notifier.notify_one();
            }
        })
        .and_then(|mut watcher| {
            watcher.watch(projects, RecursiveMode::NonRecursive)?;
            Ok(watcher)
        })
        .inspect_err(|error| {
            tracing::warn!(
                "cannot watch {}, rescanning every 30 s instead: {error}",
                projects.display()
            );
        })
        .ok();
        Self {
            watcher,
            watched: BTreeSet::new(),
            changed,
        }
    }

    fn rescan_interval(&self) -> Duration {
        if self.watcher.is_some() {
            RESCAN_INTERVAL_WHILE_WATCHING
        } else {
            RESCAN_INTERVAL
        }
    }

    /// Watches each folder for a new `.git`, each repository's `.git` for a new branch or remote,
    /// and its `.git/worktrees` for a new worktree. Stops watching what is gone. A path that was
    /// removed and made again is watched again.
    fn follow(&mut self, projects: &ProjectsDirectory, folders: &[Folder]) {
        let Some(watcher) = &mut self.watcher else {
            return;
        };
        let wanted: BTreeSet<PathBuf> = folders
            .iter()
            .flat_map(|folder| {
                let path = projects.folder(&folder.name);
                let git = folder.git.is_some().then(|| path.join(".git"));
                let worktrees = git.as_ref().map(|git| git.join("worktrees"));
                [Some(path), git, worktrees]
            })
            .flatten()
            .collect();
        self.watched.retain(|path| {
            let keep = wanted.contains(path);
            if !keep {
                let _already_gone = watcher.unwatch(path);
            }
            keep
        });
        for path in wanted {
            if watcher.watch(&path, RecursiveMode::NonRecursive).is_ok() {
                self.watched.insert(path);
            }
        }
    }

    /// Returns after a change once a second passes without another, or after 10 s of changes.
    async fn settled_change(&self) {
        if self.watcher.is_none() {
            return future::pending().await;
        }
        self.changed.notified().await;
        let Some(deadline) = Instant::now().checked_add(LONGEST_SETTLE) else {
            return;
        };
        while Instant::now() < deadline
            && timeout(QUIET_PERIOD, self.changed.notified()).await.is_ok()
        {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, git: Option<(&str, &str)>) -> Folder {
        Folder {
            name: name.to_owned(),
            git: git.map(|(repository, branch)| GitDetails {
                branch: Some(branch.to_owned()),
                repository: Some(repository.to_owned()),
                worktrees: 0,
            }),
        }
    }

    #[test]
    fn folders_are_listed_with_their_git_details() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path();
        fs::create_dir_all(root.join("app/.git")).expect("repository is created");
        fs::write(root.join("app/.git/HEAD"), "ref:  refs/heads/main\n").expect("HEAD is written");
        fs::write(
            root.join("app/.git/config"),
            "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://example.com/other\n\
             [Remote \"origin\"]\n\tURL = \"https://zeke:ghp_secret@github.com/zeke/app.git\" # mine\n",
        )
        .expect("config is written");
        fs::create_dir_all(root.join("notes")).expect("folder is created");
        fs::create_dir_all(root.join(".cache")).expect("hidden folder is created");
        fs::write(root.join("AGENTS.md"), "").expect("file is written");

        assert_eq!(
            ProjectsDirectory(root.to_path_buf())
                .folders()
                .expect("folders are listed"),
            [
                folder("app", Some(("https://github.com/zeke/app.git", "main"))),
                folder("notes", None),
            ]
        );
    }

    #[test]
    fn linked_worktrees_read_the_shared_config() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path();
        fs::create_dir_all(root.join("main/.git/worktrees/feature"))
            .expect("worktree directory is created");
        fs::write(
            root.join("main/.git/config"),
            "[remote \"origin\"]\n\turl = https://github.com/zeke/app.git\n",
        )
        .expect("config is written");
        fs::write(root.join("main/.git/HEAD"), "ref: refs/heads/main\n").expect("HEAD is written");
        let worktree_git = root.join("main/.git/worktrees/feature");
        fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").expect("HEAD is written");
        fs::write(worktree_git.join("commondir"), "../..\n").expect("commondir is written");
        fs::write(
            worktree_git.join("gitdir"),
            format!("{}\n", root.join("feature/.git").display()),
        )
        .expect("gitdir is written");
        fs::create_dir_all(root.join("feature")).expect("worktree is created");
        fs::write(
            root.join("feature/.git"),
            format!("gitdir: {}\n", worktree_git.display()),
        )
        .expect("pointer is written");

        let folders = ProjectsDirectory(root.to_path_buf())
            .folders()
            .expect("folders are listed");
        assert_eq!(
            folders,
            [
                folder(
                    "feature",
                    Some(("https://github.com/zeke/app.git", "feature"))
                ),
                Folder {
                    name: "main".to_owned(),
                    git: Some(GitDetails {
                        branch: Some("main".to_owned()),
                        repository: Some("https://github.com/zeke/app.git".to_owned()),
                        worktrees: 1,
                    }),
                },
            ]
        );
    }

    #[test]
    fn worktrees_whose_folder_is_gone_are_not_counted() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let git = directory.path().join("app/.git");
        let claude_worktrees = directory.path().join("app/.claude/worktrees");
        for (name, gitdir) in [
            ("kept", claude_worktrees.join("kept/.git")),
            (
                "relative",
                PathBuf::from("../../../.claude/worktrees/relative/.git"),
            ),
            ("deleted", claude_worktrees.join("deleted/.git")),
        ] {
            fs::create_dir_all(git.join("worktrees").join(name)).expect("entry is created");
            fs::write(
                git.join("worktrees").join(name).join("gitdir"),
                format!("{}\n", gitdir.display()),
            )
            .expect("gitdir is written");
        }
        for name in ["kept", "relative"] {
            fs::create_dir_all(claude_worktrees.join(name)).expect("worktree is created");
            fs::write(claude_worktrees.join(name).join(".git"), "gitdir: x\n")
                .expect("pointer is written");
        }
        let folders = ProjectsDirectory(directory.path().to_path_buf())
            .folders()
            .expect("folders are listed");
        assert_eq!(
            folders
                .first()
                .and_then(|folder| folder.git.as_ref())
                .map(|git| git.worktrees),
            Some(2)
        );
    }

    #[test]
    fn unborn_reftable_branch_is_no_branch() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::create_dir_all(directory.path().join("app/.git")).expect("repository is created");
        fs::write(
            directory.path().join("app/.git/HEAD"),
            "ref: refs/heads/.invalid\n",
        )
        .expect("HEAD is written");
        let folders = ProjectsDirectory(directory.path().to_path_buf())
            .folders()
            .expect("folders are listed");
        assert_eq!(
            folders.first().and_then(|folder| folder.git.clone()),
            Some(GitDetails::default())
        );
    }

    #[test]
    fn credentials_are_removed_from_web_urls() {
        for (url, expected) in [
            (
                "https://user:token@github.com/a/b.git",
                "https://github.com/a/b.git",
            ),
            (
                "https://ghp_token@github.com/a/b.git",
                "https://github.com/a/b.git",
            ),
            ("https://github.com/a/b.git", "https://github.com/a/b.git"),
            ("HTTP://u:p@example.com:8080", "HTTP://example.com:8080"),
            (
                "ssh://git@github.com/a/b.git",
                "ssh://git@github.com/a/b.git",
            ),
            ("git@github.com:a/b.git", "git@github.com:a/b.git"),
        ] {
            assert_eq!(url.without_credentials(), expected, "{url}");
        }
    }

    #[test]
    fn config_values_lose_quotes_escapes_and_comments() {
        assert_eq!("\"a b\" ; note".config_value(), "a b");
        assert_eq!("\"has # hash\"".config_value(), "has # hash");
        assert_eq!("plain # note".config_value(), "plain");
        assert_eq!(r#"es\"caped"#.config_value(), "es\"caped");
    }

    #[test]
    fn the_managed_block_is_replaced_and_the_rest_kept() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let projects = ProjectsDirectory(directory.path().to_path_buf());
        let agents = directory.path().join("AGENTS.md");
        fs::write(&agents, "# My notes\n\nKeep this.\n").expect("file is written");

        let folders = [folder(
            "app",
            Some(("https://github.com/zeke/app.git", "main")),
        )];
        projects
            .describe_folders_for_agents(&folders, None)
            .expect("block is written");
        let first = fs::read_to_string(&agents).expect("file is read");
        assert!(
            first.starts_with("# My notes\n\nKeep this.\n\n<!-- ezra:folders:start -->"),
            "{first}"
        );
        assert!(
            first
                .contains("Folders:\n- app: branch main, origin https://github.com/zeke/app.git\n"),
            "{first}"
        );

        fs::write(&agents, format!("{first}\nAfter the block.\n")).expect("file is written");
        projects
            .describe_folders_for_agents(&[], Some("zeke"))
            .expect("block is written");
        let second = fs::read_to_string(&agents).expect("file is read");
        assert!(second.contains("No projects yet."), "{second}");
        assert!(
            second.contains("gh is signed in to GitHub as zeke."),
            "{second}"
        );
        assert!(!first.contains("gh is signed in"), "{first}");
        assert!(!second.contains("- app"), "{second}");
        assert!(
            second.ends_with("<!-- ezra:folders:end -->\n\nAfter the block.\n"),
            "{second}"
        );
        assert_eq!(second.matches(BLOCK_START).count(), 1);
    }

    #[test]
    fn an_unfinished_block_does_not_swallow_text() {
        let text = format!("# notes\n{BLOCK_START}\nold rows\nMy important text\n");
        let once = text.with_folders_block(&format!("{BLOCK_START}\nnew\n{BLOCK_END}"));
        assert!(once.contains("My important text"), "{once}");
        let twice = once.with_folders_block(&format!("{BLOCK_START}\nnewer\n{BLOCK_END}"));
        assert!(twice.contains("My important text"), "{twice}");
        assert_eq!(twice.matches("newer").count(), 1, "{twice}");
        assert!(!twice.contains("\nnew\n"), "{twice}");
    }

    #[test]
    fn markers_inside_names_cannot_grow_the_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let projects = ProjectsDirectory(directory.path().to_path_buf());
        let folders = [Folder {
            name: format!("x{BLOCK_END}"),
            git: None,
        }];
        projects
            .describe_folders_for_agents(&folders, None)
            .expect("block is written");
        let first = fs::read_to_string(directory.path().join("AGENTS.md")).expect("file is read");
        projects
            .describe_folders_for_agents(&folders, None)
            .expect("block is written");
        let second = fs::read_to_string(directory.path().join("AGENTS.md")).expect("file is read");
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn new_folders_and_repositories_are_noticed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let projects = ProjectsDirectory(directory.path().to_path_buf());
        let mut watcher = FolderWatcher::start(&projects.0);
        let noticed = Duration::from_secs(5);

        fs::create_dir(projects.folder("app")).expect("folder is created");
        timeout(noticed, watcher.settled_change())
            .await
            .expect("the new folder is noticed");

        watcher.follow(&projects, &projects.folders().expect("folders are listed"));
        fs::create_dir(projects.folder("app").join(".git")).expect("repository is created");
        timeout(noticed, watcher.settled_change())
            .await
            .expect("the new repository is noticed");

        watcher.follow(&projects, &projects.folders().expect("folders are listed"));
        fs::write(
            projects.folder("app").join(".git/HEAD"),
            "ref: refs/heads/main\n",
        )
        .expect("HEAD is written");
        timeout(noticed, watcher.settled_change())
            .await
            .expect("the branch change is noticed");

        let worktrees = projects.folder("app").join(".git/worktrees");
        for round in ["first", "second"] {
            fs::create_dir(&worktrees).expect("worktrees directory is created");
            timeout(noticed, watcher.settled_change())
                .await
                .unwrap_or_else(|_| panic!("the {round} worktrees directory is not noticed"));
            watcher.follow(&projects, &projects.folders().expect("folders are listed"));
            fs::create_dir(worktrees.join("feature")).expect("worktree entry is created");
            timeout(noticed, watcher.settled_change())
                .await
                .unwrap_or_else(|_| panic!("the {round} new worktree is not noticed"));
            fs::remove_dir_all(&worktrees).expect("worktrees directory is removed");
            timeout(noticed, watcher.settled_change())
                .await
                .unwrap_or_else(|_| panic!("the {round} removal is not noticed"));
        }

        watcher.follow(&projects, &projects.folders().expect("folders are listed"));
        projects.folders().expect("folders are listed");
        assert!(
            timeout(Duration::from_millis(1500), watcher.settled_change())
                .await
                .is_err(),
            "scanning counted as a change"
        );
    }

    #[test]
    fn folders_are_described_on_one_line() {
        assert_eq!(folder("notes", None).describe(), "notes");
        assert_eq!(
            Folder {
                name: "fresh".to_owned(),
                git: Some(GitDetails::default()),
            }
            .describe(),
            "fresh: git repository"
        );
        assert_eq!(
            folder("app", Some(("https://github.com/zeke/app.git", "main"))).describe(),
            "app: branch main, origin https://github.com/zeke/app.git"
        );
        assert_eq!("a\nb\tc".single_line(), "a b c");
    }
}
