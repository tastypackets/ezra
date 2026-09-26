use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::agents::{Agent, DownloadProgress, VersionExt};
use super::login::{LoginPrompt, SignInStatus};
use super::state::AppState;
use crate::path_ext::PathExt;

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

    pub async fn gather(agent: Agent, state: &AppState) -> Self {
        let login_prompt = {
            let mut logins = state.logins.lock().await;
            if logins
                .get_mut(&agent)
                .is_some_and(|login| login.outcome().is_some())
            {
                logins.remove(&agent);
            }
            logins.get(&agent).and_then(|login| login.prompt().cloned())
        };
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
        let sign_in = SignInStatus::query(agent, &state.install_paths).await;
        let config_directory = state.install_paths.config_directory(agent);
        Self {
            agent,
            configured,
            installed_version,
            logged_in: sign_in.logged_in,
            account: sign_in.account,
            login_prompt,
            config_disk_bytes: config_directory.and_then(|directory| directory.total_bytes().ok()),
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
    use crate::manager::api::test_support::TestManager;
    use crate::manager::updates::LatestRelease;

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
