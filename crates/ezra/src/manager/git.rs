use std::collections::HashMap;
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use utoipa::ToSchema;

use super::login::{LoginError, LoginProcess, LoginPrompt, PromptShape};
use crate::process_ext::OutputExt;

const GITHUB_HOST: &str = "github.com";
const GIT_CONFIG_VARIABLE: &str = "GIT_CONFIG_GLOBAL";
const GH_CONFIG_VARIABLE: &str = "GH_CONFIG_DIR";
const TOKEN_VARIABLES: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

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
    gh_config_directory: PathBuf,
    token_from_environment: bool,
}

impl GitTools {
    /// `GIT_CONFIG_GLOBAL` and `GH_CONFIG_DIR`, or the tools' own defaults under `home`.
    pub fn from_environment(home: &Path) -> Self {
        let path_from =
            |variable: &str, default: PathBuf| env::var_os(variable).map_or(default, PathBuf::from);
        Self {
            git_config: path_from(GIT_CONFIG_VARIABLE, home.join(".gitconfig")),
            gh_config_directory: path_from(GH_CONFIG_VARIABLE, home.join(".config/gh")),
            token_from_environment: TOKEN_VARIABLES
                .iter()
                .any(|variable| env::var_os(variable).is_some_and(|token| !token.is_empty())),
        }
    }

    /// gh uses `GH_TOKEN` or `GITHUB_TOKEN` over any sign-in, and refuses to sign in or out.
    pub fn token_from_environment(&self) -> bool {
        self.token_from_environment
    }

    #[cfg(test)]
    pub fn under(directory: &Path) -> Self {
        Self {
            git_config: directory.join("git/config"),
            gh_config_directory: directory.join("gh"),
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
            "--hostname",
            GITHUB_HOST,
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
                "--hostname",
                GITHUB_HOST,
            ])
            .stdin(Stdio::null())
            .output()
            .await
        else {
            return GitHubSignIn::default();
        };
        GitHubSignIn::from_status_json(&output.stdout)
    }

    /// Signs out every github.com account, so no other one takes over.
    pub async fn log_out_of_github(&self) -> Result<(), GitError> {
        for account in self.github_sign_in().await.accounts {
            let output = self
                .gh()
                .args([
                    "auth",
                    "logout",
                    "--hostname",
                    GITHUB_HOST,
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
            .config_values("credential.https://github.com.helper")
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
            .args(["auth", "setup-git", "--hostname", GITHUB_HOST])
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

    async fn config_values(&self, key: &str) -> Result<Vec<String>, GitError> {
        let output = self
            .git()
            .args(["config", "--global", "--get-all", key])
            .stdin(Stdio::null())
            .output()
            .await?;
        match output.status.code() {
            Some(0) => Ok(output.stdout_text().lines().map(str::to_owned).collect()),
            Some(1) => Ok(Vec::new()),
            _ => Err(GitError::Git {
                action: "config",
                output: output.stderr_text(),
            }),
        }
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

    async fn create_config_directory(&self) -> io::Result<()> {
        match self.git_config.parent() {
            Some(directory) => tokio::fs::create_dir_all(directory).await,
            None => Ok(()),
        }
    }

    fn git(&self) -> Command {
        let mut command = Command::new("git");
        command.env(GIT_CONFIG_VARIABLE, &self.git_config);
        command
    }

    fn gh(&self) -> Command {
        let mut command = Command::new("gh");
        command
            .env(GIT_CONFIG_VARIABLE, &self.git_config)
            .env(GH_CONFIG_VARIABLE, &self.gh_config_directory);
        command
    }
}

/// What gh reports about the github.com sign-in.
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
    fn from_status_json(status_json: &[u8]) -> Self {
        #[derive(Deserialize)]
        struct Status {
            hosts: HashMap<String, Vec<Account>>,
        }
        #[derive(Deserialize)]
        struct Account {
            state: String,
            active: bool,
            login: String,
            error: Option<String>,
        }
        let Ok(mut status) = serde_json::from_slice::<Status>(status_json) else {
            return Self::default();
        };
        let accounts = status.hosts.remove(GITHUB_HOST).unwrap_or_default();
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

/// The name and email git writes into commits.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CommitIdentity {
    pub name: Option<String>,
    pub email: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_in_account_comes_from_the_active_github_entry() {
        let status = br#"{"hosts":{"github.com":[
            {"state":"success","active":false,"host":"github.com","login":"old","tokenSource":"/config/gh/hosts.yml","gitProtocol":"https"},
            {"state":"success","active":true,"host":"github.com","login":"zeke","tokenSource":"/config/gh/hosts.yml","gitProtocol":"https"}]}}"#;
        assert_eq!(
            GitHubSignIn::from_status_json(status),
            GitHubSignIn {
                signed_in: true,
                account: Some("zeke".to_owned()),
                failing: false,
                accounts: vec!["old".to_owned(), "zeke".to_owned()],
            }
        );
    }

    #[test]
    fn rejected_token_is_failing() {
        let status = br#"{"hosts":{"github.com":[{"state":"error","error":"401 Bad credentials","active":true,"host":"github.com","login":"","tokenSource":"GH_TOKEN","gitProtocol":"https"}]}}"#;
        assert_eq!(
            GitHubSignIn::from_status_json(status),
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
            GitHubSignIn::from_status_json(br#"{"hosts":{}}"#),
            GitHubSignIn::default()
        );
        assert_eq!(
            GitHubSignIn::from_status_json(b"not json"),
            GitHubSignIn::default()
        );
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
