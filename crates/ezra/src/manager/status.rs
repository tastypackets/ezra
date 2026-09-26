use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use super::agents::{Agent, DownloadProgress, VersionExt};
use super::login::{ClaudeCredentials, LoginPrompt};
use super::state::AppState;

/// One agent's install and sign-in state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct AgentStatus {
    pub agent: Agent,
    /// Installed through the manager at least once.
    pub configured: bool,
    /// Absent when the agent is not installed.
    pub installed_version: Option<String>,
    pub logged_in: bool,
    /// The signed-in account as the CLI reports it, absent when unknown.
    pub account: Option<String>,
    /// When the sign-in stops working unless the agent signs in again, absent when unknown.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub sign_in_ends_at: Option<OffsetDateTime>,
    /// Present while a sign-in is waiting for the person signing in.
    pub login_prompt: Option<LoginPrompt>,
    /// Bytes the agent keeps on /config, absent when they cannot be measured.
    pub config_disk_bytes: Option<u64>,
    /// Present while an install or update is downloading.
    pub install_progress: Option<DownloadProgress>,
    /// A newer release than the installed version, absent when none is known.
    pub available_update: Option<String>,
}

impl AgentStatus {
    pub async fn gather_all(state: &AppState) -> Vec<Self> {
        let mut statuses = Vec::new();
        for agent in Agent::ALL {
            statuses.push(Self::gather(agent, state).await);
        }
        statuses
    }

    /// Reads what is known and runs nothing.
    pub async fn gather(agent: Agent, state: &AppState) -> Self {
        let login_prompt = state
            .logins
            .lock()
            .await
            .get(&agent)
            .and_then(|login| login.prompt().cloned());
        let install_progress = state
            .installs_in_progress
            .lock()
            .await
            .get(&agent)
            .map(|progress| progress.snapshot());
        let (configured, channel) = {
            let settings = state.settings.lock().await;
            (
                settings.agent(agent).configured,
                settings.release_channel(agent),
            )
        };
        let installed_version = state.install_paths.installed_version(agent);
        let available_update = match (
            &installed_version,
            state.latest_releases.lock().await.get(&agent),
        ) {
            (Some(installed), Some(latest))
                if latest.channel == channel && latest.version.is_newer_than(installed) =>
            {
                Some(latest.version.clone())
            }
            _ => None,
        };
        let sign_in = state.agent_checks.sign_in(agent).unwrap_or_default();
        let config_directory = state.install_paths.config_directory(agent);
        let sign_in_ends_at = match (agent, config_directory) {
            (Agent::Claude, Some(directory)) if sign_in.logged_in => {
                ClaudeCredentials::read(directory).sign_in_ends_at()
            }
            _ => None,
        };
        Self {
            agent,
            configured,
            installed_version,
            logged_in: sign_in.logged_in,
            account: sign_in.account,
            sign_in_ends_at,
            login_prompt,
            config_disk_bytes: state.agent_checks.config_bytes(agent),
            install_progress,
            available_update,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::manager::agents::ReleaseChannel;
    use crate::manager::api::test_support::{ResponseExt, TestManager};
    use crate::manager::updates::LatestRelease;
    use crate::path_ext::PathExt;

    #[tokio::test]
    async fn listing_agents_runs_no_cli() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        manager.install_fake_cli(
            Agent::Claude,
            r#"echo '{"loggedIn":true,"email":"a@example.com"}'"#,
        );
        manager.install_fake_cli(Agent::Codex, "echo 'Logged in using ChatGPT'");
        let sign_ins = async || {
            let listing: Vec<AgentStatus> = manager
                .get("/api/v1/agents", Some(&cookie))
                .await
                .json()
                .await;
            listing
                .into_iter()
                .map(|status| (status.logged_in, status.account))
                .collect::<Vec<_>>()
        };
        for _ in 0..3 {
            assert_eq!(sign_ins().await, [(false, None), (false, None)]);
        }
        for agent in Agent::ALL {
            assert_eq!(manager.fake_cli_runs(agent), Vec::<String>::new());
        }

        for agent in Agent::ALL {
            manager.state.agent_checks.refresh(agent).await;
        }
        for _ in 0..3 {
            assert_eq!(
                sign_ins().await,
                [
                    (true, Some("a@example.com".to_owned())),
                    (true, Some("ChatGPT".to_owned()))
                ]
            );
            AgentStatus::gather_all(&manager.state).await;
        }
        assert_eq!(manager.fake_cli_runs(Agent::Claude), ["auth status"]);
        assert_eq!(manager.fake_cli_runs(Agent::Codex), ["login status"]);
    }

    fn write(path: &Path) {
        fs::create_dir_all(path.parent().expect("test paths have a parent"))
            .expect("parent directory is created");
        fs::write(path, "{}").expect("test file is written");
    }

    #[tokio::test]
    async fn a_newer_release_on_the_chosen_channel_is_an_available_update() {
        let manager = TestManager::new();
        let binaries = tempfile::tempdir().expect("temporary directory");
        let installed = binaries.path().join("2.1.0");
        write(&installed);
        manager
            .state
            .install_paths
            .command(Agent::Claude)
            .replace_symlink(&installed)
            .expect("command link is created");
        let latest = |channel, version: &str| LatestRelease {
            channel,
            version: version.to_owned(),
        };
        manager
            .state
            .latest_releases
            .lock()
            .await
            .insert(Agent::Codex, latest(ReleaseChannel::Latest, "0.157.1"));
        let codex = AgentStatus::gather(Agent::Codex, &manager.state).await;
        assert_eq!(codex.available_update, None);
        for (release, expected) in [
            (latest(ReleaseChannel::Latest, "2.1.1"), Some("2.1.1")),
            (latest(ReleaseChannel::Latest, "2.1.0"), None),
            (latest(ReleaseChannel::Latest, "2.0.9"), None),
            (latest(ReleaseChannel::Stable, "2.1.1"), None),
        ] {
            manager
                .state
                .latest_releases
                .lock()
                .await
                .insert(Agent::Claude, release.clone());
            let status = AgentStatus::gather(Agent::Claude, &manager.state).await;
            assert_eq!(status.available_update.as_deref(), expected, "{release:?}");
        }
    }
}
