use std::collections::HashMap;
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use utoipa::ToSchema;

use super::github_host::GitHubHost;
use super::login::{LoginError, LoginProcess, LoginPrompt, PromptShape};
use crate::process_ext::OutputExt;

const GITHUB_REPOSITORIES: &str =
    "/user/repos?affiliation=owner,collaborator,organization_member&sort=pushed&per_page=100";
/// gh lists `GH_HOST` with this source when nothing is signed in there.
const GH_PLACEHOLDER_TOKEN_SOURCE: &str = "default";
const GIT_CONFIG_VARIABLE: &str = "GIT_CONFIG_GLOBAL";
const GH_CONFIG_VARIABLE: &str = "GH_CONFIG_DIR";
const EXCLUDES_FILE_KEY: &str = "core.excludesFile";
const CLAUDE_WORKTREES_PATTERN: &str = ".claude/worktrees/";

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git {action} failed: {output}")]
    Git {
        action: &'static str,
        output: String,
    },
    #[error("gh {action} failed: {output}")]
    GitHub {
        action: &'static str,
        output: String,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// git and the GitHub CLI, pointed at their settings on the config volume.
#[derive(Debug, Clone)]
pub struct GitTools {
    git_config: PathBuf,
    /// The excludes file to set when git has none.
    excludes_file: PathBuf,
    gh_config_directory: PathBuf,
    host: GitHubHost,
    token_from_environment: bool,
}

impl GitTools {
    /// `GIT_CONFIG_GLOBAL`, `GH_CONFIG_DIR` and `GH_HOST`, or the manager's fallbacks under `home`.
    pub fn from_environment(home: &Path) -> Self {
        let path_from =
            |variable: &str, default: PathBuf| env::var_os(variable).map_or(default, PathBuf::from);
        let git_config = env::var_os(GIT_CONFIG_VARIABLE).map(PathBuf::from);
        let host = GitHubHost::from_environment();
        Self {
            excludes_file: git_config.as_deref().map_or_else(
                || home.join(".config/git/ignore"),
                |config| config.with_file_name("ignore"),
            ),
            git_config: git_config.unwrap_or_else(|| home.join(".gitconfig")),
            gh_config_directory: path_from(GH_CONFIG_VARIABLE, home.join(".config/gh")),
            token_from_environment: host
                .token_variables()
                .iter()
                .any(|variable| env::var_os(variable).is_some_and(|token| !token.is_empty())),
            host,
        }
    }

    pub fn host(&self) -> &GitHubHost {
        &self.host
    }

    /// gh uses a token from the host's variables over any sign-in, and refuses to sign in or out.
    pub fn token_from_environment(&self) -> bool {
        self.token_from_environment
    }

    #[cfg(test)]
    pub fn under(directory: &Path) -> Self {
        Self {
            git_config: directory.join("git/config"),
            excludes_file: directory.join("git/ignore"),
            gh_config_directory: directory.join("gh"),
            host: GitHubHost::default(),
            token_from_environment: false,
        }
    }

    /// Starts the device sign-in, asking for the `workflow` scope.
    pub async fn start_github_login(&self) -> Result<(LoginProcess, LoginPrompt), LoginError> {
        let mut command = self.gh();
        command.args([
            "auth",
            "login",
            "--web",
            &self.hostname_argument(),
            "--git-protocol",
            "https",
            "--scopes",
            "workflow",
            "--insecure-storage",
        ]);
        LoginProcess::start("gh", PromptShape::LinkAndCode, command).await
    }

    /// Anything unexpected in gh's answer counts as signed out.
    pub async fn github_sign_in(&self) -> GitHubSignIn {
        let Ok(output) = self
            .gh()
            .args([
                "auth",
                "status",
                "--json",
                "hosts",
                &self.hostname_argument(),
            ])
            .stdin(Stdio::null())
            .output()
            .await
        else {
            return GitHubSignIn::default();
        };
        GitHubSignIn::from_status_json(&output.stdout, &self.host)
    }

    /// Signs out every account on the host, so no other one takes over.
    pub async fn log_out_of_github(&self) -> Result<(), GitError> {
        for account in self.github_sign_in().await.accounts {
            let output = self
                .gh()
                .args([
                    "auth",
                    "logout",
                    &self.hostname_argument(),
                    "--user",
                    &account,
                ])
                .stdin(Stdio::null())
                .output()
                .await?;
            if !output.status.success() {
                return Err(GitError::GitHub {
                    action: "logout",
                    output: output.stderr_text(),
                });
            }
        }
        Ok(())
    }

    /// Makes git ask gh for GitHub credentials, unless it already does.
    pub async fn lend_github_sign_in_to_git(&self) -> Result<(), GitError> {
        let helpers = self
            .config_values(&format!("credential.{}.helper", self.host.https_url()))
            .await?;
        if helpers
            .iter()
            .any(|helper| helper.contains("auth git-credential"))
        {
            return Ok(());
        }
        self.create_config_directory().await?;
        let output = self
            .gh()
            .args(["auth", "setup-git", &self.hostname_argument()])
            .stdin(Stdio::null())
            .output()
            .await?;
        if output.status.success() {
            Ok(())
        } else {
            Err(GitError::GitHub {
                action: "setup-git",
                output: output.stderr_text(),
            })
        }
    }

    pub async fn commit_identity(&self) -> Result<CommitIdentity, GitError> {
        Ok(CommitIdentity {
            name: self.config_values("user.name").await?.pop(),
            email: self.config_values("user.email").await?.pop(),
        })
    }

    /// An empty or missing value removes it.
    pub async fn save_commit_identity(&self, identity: &CommitIdentity) -> Result<(), GitError> {
        self.set_config_value("user.name", identity.name.as_deref())
            .await?;
        self.set_config_value("user.email", identity.email.as_deref())
            .await
    }

    /// Adds `.claude/worktrees/` to the excludes file git already uses, or sets the manager's own.
    pub async fn ignore_claude_worktrees(&self) -> Result<(), GitError> {
        let chosen = self
            .git()
            .args([
                "config",
                "--global",
                "--includes",
                "--type=path",
                "--get",
                EXCLUDES_FILE_KEY,
            ])
            .stdin(Stdio::null())
            .output()
            .await?
            .config_values()?
            .pop();
        let excludes_file = chosen
            .as_ref()
            .map_or_else(|| self.excludes_file.clone(), PathBuf::from);
        ExcludesFile(excludes_file)
            .include(CLAUDE_WORKTREES_PATTERN)
            .await?;
        if chosen.is_none() {
            self.set_config_value(
                EXCLUDES_FILE_KEY,
                Some(&self.excludes_file.to_string_lossy()),
            )
            .await?;
        }
        Ok(())
    }

    async fn config_values(&self, key: &str) -> Result<Vec<String>, GitError> {
        self.git()
            .args(["config", "--global", "--get-all", key])
            .stdin(Stdio::null())
            .output()
            .await?
            .config_values()
    }

    async fn set_config_value(&self, key: &str, value: Option<&str>) -> Result<(), GitError> {
        self.create_config_directory().await?;
        let value = value.map(str::trim).filter(|value| !value.is_empty());
        let mut command = self.git();
        command.args(["config", "--global"]);
        match value {
            Some(value) => command.args([key, value]),
            None => command.args(["--unset-all", key]),
        };
        let output = command.stdin(Stdio::null()).output().await?;
        match output.status.code() {
            Some(0) => Ok(()),
            Some(5) if value.is_none() => Ok(()),
            _ => Err(GitError::Git {
                action: "config",
                output: output.stderr_text(),
            }),
        }
    }

    /// The first 100 repositories the GitHub account owns or works on, last pushed first.
    pub async fn github_repositories(&self) -> Result<Vec<GitHubRepository>, GitError> {
        let output = self
            .gh()
            .args(["api", &self.hostname_argument(), GITHUB_REPOSITORIES])
            .stdin(Stdio::null())
            .output()
            .await?;
        if !output.status.success() {
            return Err(GitError::GitHub {
                action: "api",
                output: output.stderr_text(),
            });
        }
        serde_json::from_slice(&output.stdout).map_err(|error| GitError::GitHub {
            action: "api",
            output: format!("unexpected answer: {error}"),
        })
    }

    /// `git clone` of `url` into `destination`, with progress on standard error and no prompts.
    pub fn clone_command(&self, url: &str, destination: &Path) -> Command {
        let mut command = self.git();
        command
            .args(["clone", "--progress", "--", url])
            .arg(destination)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        command
    }

    /// Changes in the repository at `folder` and the worktrees inside it, and the commits and
    /// stashes no remote has.
    pub async fn unsaved_work(&self, folder: &Path) -> Result<UnsavedWork, GitError> {
        let folder = tokio::fs::canonicalize(folder).await?;
        let worktrees = self
            .output_in(&folder, "worktree", &["worktree", "list", "--porcelain"])
            .await?;
        let mut uncommitted_changes: u32 = 0;
        for worktree in worktrees
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(Path::new)
            .filter(|worktree| worktree.starts_with(&folder) && worktree.is_dir())
        {
            let changes = self
                .output_in(
                    worktree,
                    "status",
                    &[
                        "--no-optional-locks",
                        "status",
                        "--porcelain",
                        "--untracked-files=normal",
                    ],
                )
                .await?;
            uncommitted_changes = uncommitted_changes.saturating_add(changes.line_count());
        }
        let commits = self
            .output_in(
                &folder,
                "rev-list",
                &[
                    "rev-list",
                    "--count",
                    "--exclude=refs/stash",
                    "--exclude=refs/notes/*",
                    "--exclude=refs/original/*",
                    "--exclude=refs/prefetch/*",
                    "--all",
                    "--not",
                    "--remotes",
                ],
            )
            .await?;
        let stashes = self.output_in(&folder, "stash", &["stash", "list"]).await?;
        Ok(UnsavedWork {
            uncommitted_changes,
            unpushed_commits: commits.trim().parse().map_err(|_| GitError::Git {
                action: "rev-list",
                output: format!("unexpected count {commits:?}"),
            })?,
            stashes: stashes.line_count(),
        })
    }

    async fn output_in(
        &self,
        folder: &Path,
        action: &'static str,
        arguments: &[&str],
    ) -> Result<String, GitError> {
        let output = self
            .git()
            .current_dir(folder)
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .await?;
        if output.status.success() {
            Ok(output.stdout_text())
        } else {
            Err(GitError::Git {
                action,
                output: output.stderr_text(),
            })
        }
    }

    async fn create_config_directory(&self) -> io::Result<()> {
        match self.git_config.parent() {
            Some(directory) => tokio::fs::create_dir_all(directory).await,
            None => Ok(()),
        }
    }

    fn git(&self) -> Command {
        let mut command = Command::new("git");
        command
            .env(GIT_CONFIG_VARIABLE, &self.git_config)
            .env(GH_CONFIG_VARIABLE, &self.gh_config_directory);
        command
    }

    fn hostname_argument(&self) -> String {
        format!("--hostname={}", self.host)
    }

    fn gh(&self) -> Command {
        let mut command = Command::new("gh");
        command
            .env(GIT_CONFIG_VARIABLE, &self.git_config)
            .env(GH_CONFIG_VARIABLE, &self.gh_config_directory);
        command
    }
}

trait ConfigOutputExt {
    /// The values `git config` printed, none when the key is not set.
    fn config_values(&self) -> Result<Vec<String>, GitError>;
}

impl ConfigOutputExt for Output {
    fn config_values(&self) -> Result<Vec<String>, GitError> {
        match self.status.code() {
            Some(0) => Ok(self.stdout_text().lines().map(str::to_owned).collect()),
            Some(1) => Ok(Vec::new()),
            _ => Err(GitError::Git {
                action: "config",
                output: self.stderr_text(),
            }),
        }
    }
}

/// A file of ignore patterns for every repository.
struct ExcludesFile(PathBuf);

impl ExcludesFile {
    /// Adds `pattern` as a line of its own unless one already holds it. Other lines are kept.
    async fn include(&self, pattern: &str) -> io::Result<()> {
        let existing = match tokio::fs::read_to_string(&self.0).await {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        if existing.lines().any(|line| line.trim_end() == pattern) {
            return Ok(());
        }
        if let Some(directory) = self.0.parent() {
            tokio::fs::create_dir_all(directory).await?;
        }
        let separator = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        tokio::fs::write(&self.0, format!("{existing}{separator}{pattern}\n")).await
    }
}

/// What gh reports about the sign-in on the GitHub host.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GitHubSignIn {
    /// The active account works.
    pub signed_in: bool,
    /// The active account.
    pub account: Option<String>,
    /// An account is stored or set, but GitHub did not confirm it.
    pub failing: bool,
    /// Every stored account.
    pub accounts: Vec<String>,
}

impl GitHubSignIn {
    /// Parses `gh auth status --json hosts`.
    fn from_status_json(status_json: &[u8], host: &GitHubHost) -> Self {
        #[derive(Deserialize)]
        struct Status {
            hosts: HashMap<String, Vec<Account>>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Account {
            state: String,
            active: bool,
            login: String,
            error: Option<String>,
            #[serde(default)]
            token_source: String,
        }
        let Ok(mut status) = serde_json::from_slice::<Status>(status_json) else {
            return Self::default();
        };
        let accounts: Vec<Account> = status
            .hosts
            .remove(host.as_str())
            .unwrap_or_default()
            .into_iter()
            .filter(|account| account.token_source != GH_PLACEHOLDER_TOKEN_SOURCE)
            .collect();
        let Some(active) = accounts.iter().find(|account| account.active) else {
            return Self::default();
        };
        let signed_in = active.state == "success";
        if let Some(error) = active.error.as_deref().filter(|_| !signed_in) {
            tracing::debug!("GitHub did not confirm the sign-in: {error}");
        }
        Self {
            signed_in,
            account: Some(active.login.clone()).filter(|login| !login.is_empty()),
            failing: !signed_in,
            accounts: accounts
                .iter()
                .map(|account| account.login.clone())
                .filter(|login| !login.is_empty())
                .collect(),
        }
    }
}

/// Work in a repository that no remote has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UnsavedWork {
    /// Changed and untracked paths in the folder and its worktrees, an untracked folder counting
    /// once.
    pub uncommitted_changes: u32,
    /// Commits on no remote, stashes left out.
    pub unpushed_commits: u32,
    pub stashes: u32,
}

/// A GitHub repository the signed-in account can clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GitHubRepository {
    /// `owner/name`.
    pub full_name: String,
    pub description: Option<String>,
    pub private: bool,
}

trait LineCountExt {
    /// Non-empty lines, as many as a `u32` holds.
    fn line_count(&self) -> u32;
}

impl LineCountExt for str {
    fn line_count(&self) -> u32 {
        u32::try_from(self.lines().filter(|line| !line.is_empty()).count()).unwrap_or(u32::MAX)
    }
}

/// The name and email git writes into commits.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CommitIdentity {
    pub name: Option<String>,
    pub email: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn signed_in_account_comes_from_the_active_github_entry() {
        let status = br#"{"hosts":{"github.com":[
            {"state":"success","active":false,"host":"github.com","login":"old","tokenSource":"/config/gh/hosts.yml","gitProtocol":"https"},
            {"state":"success","active":true,"host":"github.com","login":"zeke","tokenSource":"/config/gh/hosts.yml","gitProtocol":"https"}]}}"#;
        assert_eq!(
            GitHubSignIn::from_status_json(status, &GitHubHost::default()),
            GitHubSignIn {
                signed_in: true,
                account: Some("zeke".to_owned()),
                failing: false,
                accounts: vec!["old".to_owned(), "zeke".to_owned()],
            }
        );
    }

    #[test]
    fn only_the_configured_host_counts() {
        let status = br#"{"hosts":{
            "github.com":[{"state":"success","active":true,"host":"github.com","login":"zeke","tokenSource":"/config/gh/hosts.yml","gitProtocol":"https"}],
            "ghe.example.com":[{"state":"success","active":true,"host":"ghe.example.com","login":"z.keator","tokenSource":"GH_ENTERPRISE_TOKEN","gitProtocol":"https"}]}}"#;
        let enterprise =
            GitHubSignIn::from_status_json(status, &GitHubHost::from_value("ghe.example.com"));
        assert_eq!(enterprise.account.as_deref(), Some("z.keator"));
        let elsewhere =
            GitHubSignIn::from_status_json(status, &GitHubHost::from_value("acme.ghe.com"));
        assert_eq!(elsewhere, GitHubSignIn::default());
    }

    #[test]
    fn the_placeholder_gh_lists_for_gh_host_is_signed_out() {
        let status = br#"{"hosts":{"ghe.example.com":[{"state":"error","error":"no token","active":true,"host":"ghe.example.com","login":"","tokenSource":"default","gitProtocol":"https"}]}}"#;
        assert_eq!(
            GitHubSignIn::from_status_json(status, &GitHubHost::from_value("ghe.example.com")),
            GitHubSignIn::default()
        );
    }

    #[test]
    fn an_account_without_a_token_source_still_counts() {
        let status =
            br#"{"hosts":{"github.com":[{"state":"success","active":true,"login":"e2e"}]}}"#;
        assert!(GitHubSignIn::from_status_json(status, &GitHubHost::default()).signed_in);
    }

    #[test]
    fn rejected_token_is_failing() {
        let status = br#"{"hosts":{"github.com":[{"state":"error","error":"401 Bad credentials","active":true,"host":"github.com","login":"","tokenSource":"GH_TOKEN","gitProtocol":"https"}]}}"#;
        assert_eq!(
            GitHubSignIn::from_status_json(status, &GitHubHost::default()),
            GitHubSignIn {
                signed_in: false,
                account: None,
                failing: true,
                accounts: Vec::new(),
            }
        );
    }

    #[test]
    fn no_hosts_or_unexpected_output_is_signed_out() {
        assert_eq!(
            GitHubSignIn::from_status_json(br#"{"hosts":{}}"#, &GitHubHost::default()),
            GitHubSignIn::default()
        );
        assert_eq!(
            GitHubSignIn::from_status_json(b"not json", &GitHubHost::default()),
            GitHubSignIn::default()
        );
    }

    #[tokio::test]
    async fn every_repository_ignores_claude_worktrees() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let tools = GitTools::under(directory.path());
        tools
            .ignore_claude_worktrees()
            .await
            .expect("worktrees are ignored");
        tools
            .ignore_claude_worktrees()
            .await
            .expect("worktrees are ignored again");
        let excludes_file = directory.path().join("git/ignore");
        assert_eq!(
            tools
                .config_values(EXCLUDES_FILE_KEY)
                .await
                .expect("config reads"),
            [excludes_file.to_string_lossy()]
        );
        assert_eq!(
            fs::read_to_string(&excludes_file).expect("excludes file is read"),
            ".claude/worktrees/\n"
        );

        let repository = directory.path().join("app");
        let initialized = tools
            .git()
            .args(["init", "--quiet"])
            .arg(&repository)
            .status()
            .await
            .expect("git runs");
        assert!(initialized.success());
        let checked = tools
            .git()
            .current_dir(&repository)
            .args(["check-ignore", "--quiet", ".claude/worktrees/feature/file"])
            .status()
            .await
            .expect("git runs");
        assert!(checked.success(), "the worktree is not ignored");
    }

    #[tokio::test]
    async fn an_excludes_file_already_set_is_kept_and_extended() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let tools = GitTools::under(directory.path());
        let mine = directory.path().join("mine");
        fs::write(&mine, "*.log").expect("excludes file is written");
        tools
            .set_config_value(EXCLUDES_FILE_KEY, Some(&mine.to_string_lossy()))
            .await
            .expect("config saves");
        tools
            .ignore_claude_worktrees()
            .await
            .expect("worktrees are ignored");
        assert_eq!(
            tools
                .config_values(EXCLUDES_FILE_KEY)
                .await
                .expect("config reads"),
            [mine.to_string_lossy()]
        );
        assert_eq!(
            fs::read_to_string(&mine).expect("excludes file is read"),
            "*.log\n.claude/worktrees/\n"
        );
        assert!(!directory.path().join("git/ignore").exists());
    }

    #[tokio::test]
    async fn commit_identity_is_saved_and_cleared() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let tools = GitTools::under(directory.path());
        assert_eq!(
            tools.commit_identity().await.expect("identity reads"),
            CommitIdentity::default()
        );
        let identity = CommitIdentity {
            name: Some(" Ada Lovelace ".to_owned()),
            email: Some("ada@example.com".to_owned()),
        };
        tools
            .save_commit_identity(&identity)
            .await
            .expect("identity saves");
        assert_eq!(
            tools.commit_identity().await.expect("identity reads"),
            CommitIdentity {
                name: Some("Ada Lovelace".to_owned()),
                email: Some("ada@example.com".to_owned()),
            }
        );
        tools
            .save_commit_identity(&CommitIdentity {
                name: Some(String::new()),
                email: None,
            })
            .await
            .expect("identity clears");
        assert_eq!(
            tools.commit_identity().await.expect("identity reads"),
            CommitIdentity::default()
        );
    }
}
