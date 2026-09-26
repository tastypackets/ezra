use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::agents::{Agent, DownloadProgress};
use super::login::{LoginPrompt, SignInStatus};
use super::state::AppState;
use crate::path_ext::PathExt;

/// Everything shown about one agent. Details read from the CLI or its files are `None`
/// when they cannot be read, for example after a CLI changes how it stores sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentStatus {
    pub agent: Agent,
    pub configured: bool,
    pub installed_version: Option<String>,
    pub logged_in: bool,
    pub account: Option<String>,
    pub login_prompt: Option<LoginPrompt>,
    pub session_count: Option<u64>,
    pub config_disk_bytes: Option<u64>,
    /// Present while an install or update is downloading.
    pub install_progress: Option<DownloadProgress>,
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
                .is_some_and(|login| login.has_finished())
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
        let sign_in = SignInStatus::query(agent, &state.install_paths).await;
        let config_directory = state.install_paths.config_directory(agent);
        Self {
            agent,
            configured: state.settings.lock().await.agent(agent).configured,
            installed_version: state.install_paths.installed_version(agent),
            logged_in: sign_in.logged_in,
            account: sign_in.account,
            login_prompt,
            session_count: config_directory
                .and_then(|directory| agent.session_count(directory).ok()),
            config_disk_bytes: config_directory.and_then(|directory| directory.total_bytes().ok()),
            install_progress,
        }
    }
}

impl Agent {
    /// Claude keeps `projects/<project>/<session>.jsonl`. Codex keeps `sessions/<year>/<month>/<day>/rollout-*.jsonl`.
    fn session_count(self, config_directory: &Path) -> io::Result<u64> {
        match self {
            Self::Claude => config_directory
                .join("projects")
                .entries_or_empty()?
                .iter()
                .filter(|project| project.is_dir())
                .try_fold(0_u64, |count, project| {
                    Ok(count.saturating_add(project.count_files_with_extension("jsonl", false)?))
                }),
            Self::Codex => config_directory
                .join("sessions")
                .count_files_with_extension("jsonl", true),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write(path: &Path) {
        fs::create_dir_all(path.parent().expect("test paths have a parent"))
            .expect("parent directory is created");
        fs::write(path, "{}").expect("test file is written");
    }

    fn session_count(agent: Agent, config: &Path) -> u64 {
        agent
            .session_count(config)
            .expect("sessions can be counted")
    }

    #[test]
    fn claude_sessions_are_jsonl_files_per_project() {
        let config = tempfile::tempdir().expect("temporary directory");
        for session in [
            "projects/-projects-app/one.jsonl",
            "projects/-projects-app/two.jsonl",
            "projects/-projects-other/three.jsonl",
            "projects/-projects-app/notes.txt",
        ] {
            write(&config.path().join(session));
        }
        assert_eq!(session_count(Agent::Claude, config.path()), 3);
    }

    #[test]
    fn codex_sessions_are_nested_by_date() {
        let config = tempfile::tempdir().expect("temporary directory");
        write(&config.path().join("sessions/2026/09/25/rollout-a.jsonl"));
        write(&config.path().join("sessions/2026/09/26/rollout-b.jsonl"));
        assert_eq!(session_count(Agent::Codex, config.path()), 2);
    }

    #[test]
    fn no_session_store_means_no_sessions() {
        let config = tempfile::tempdir().expect("temporary directory");
        assert_eq!(session_count(Agent::Claude, config.path()), 0);
        assert_eq!(session_count(Agent::Codex, config.path()), 0);
    }
}
