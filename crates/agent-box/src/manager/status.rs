use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::agents::{self, Agent};
use super::login::{self, LoginPrompt};
use super::state::AppState;

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
}

pub async fn all_agent_statuses(state: &AppState) -> Vec<AgentStatus> {
    let mut statuses = Vec::new();
    for agent in Agent::ALL {
        statuses.push(agent_status(agent, state).await);
    }
    statuses
}

pub async fn agent_status(agent: Agent, state: &AppState) -> AgentStatus {
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
    let sign_in = login::sign_in_status(agent, &state.install_paths).await;
    let config_directory = state.install_paths.config_directory(agent);
    AgentStatus {
        agent,
        configured: state.settings.lock().await.agent(agent).configured,
        installed_version: agents::installed_version(agent, &state.install_paths),
        logged_in: sign_in.logged_in,
        account: sign_in.account,
        login_prompt,
        session_count: config_directory.and_then(|directory| session_count(agent, directory).ok()),
        config_disk_bytes: config_directory.and_then(|directory| disk_bytes(directory).ok()),
    }
}

/// Claude keeps `projects/<project>/<session>.jsonl`; Codex keeps `sessions/<year>/<month>/<day>/rollout-*.jsonl`.
fn session_count(agent: Agent, config_directory: &Path) -> io::Result<u64> {
    match agent {
        Agent::Claude => {
            let projects = config_directory.join("projects");
            let mut count = 0;
            for project in read_dir_or_empty(&projects)? {
                if project.is_dir() {
                    count += count_jsonl_files(&project, false)?;
                }
            }
            Ok(count)
        }
        Agent::Codex => count_jsonl_files(&config_directory.join("sessions"), true),
    }
}

fn count_jsonl_files(directory: &Path, recursive: bool) -> io::Result<u64> {
    let mut count = 0;
    for path in read_dir_or_empty(directory)? {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() && recursive {
            count += count_jsonl_files(&path, true)?;
        } else if metadata.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
        {
            count += 1;
        }
    }
    Ok(count)
}

fn disk_bytes(path: &Path) -> io::Result<u64> {
    let metadata = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        metadata => metadata?,
    };
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut total = 0;
    for entry in read_dir_or_empty(path)? {
        total += disk_bytes(&entry)?;
    }
    Ok(total)
}

fn read_dir_or_empty(directory: &Path) -> io::Result<Vec<std::path::PathBuf>> {
    match fs::read_dir(directory) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn claude_sessions_are_jsonl_files_per_project() {
        let config = tempfile::tempdir().unwrap();
        write(
            &config.path().join("projects/-projects-app/one.jsonl"),
            "{}",
        );
        write(
            &config.path().join("projects/-projects-app/two.jsonl"),
            "{}",
        );
        write(
            &config.path().join("projects/-projects-other/three.jsonl"),
            "{}",
        );
        write(&config.path().join("projects/-projects-app/notes.txt"), "");
        assert_eq!(session_count(Agent::Claude, config.path()).unwrap(), 3);
    }

    #[test]
    fn codex_sessions_are_nested_by_date() {
        let config = tempfile::tempdir().unwrap();
        write(
            &config.path().join("sessions/2026/09/25/rollout-a.jsonl"),
            "{}",
        );
        write(
            &config.path().join("sessions/2026/09/26/rollout-b.jsonl"),
            "{}",
        );
        assert_eq!(session_count(Agent::Codex, config.path()).unwrap(), 2);
    }

    #[test]
    fn no_session_store_means_no_sessions() {
        let config = tempfile::tempdir().unwrap();
        assert_eq!(session_count(Agent::Claude, config.path()).unwrap(), 0);
        assert_eq!(session_count(Agent::Codex, config.path()).unwrap(), 0);
    }

    #[test]
    fn disk_use_adds_up_file_sizes() {
        let config = tempfile::tempdir().unwrap();
        write(&config.path().join("a"), "12345");
        write(&config.path().join("nested/b"), "123");
        assert_eq!(disk_bytes(config.path()).unwrap(), 8);
        assert_eq!(disk_bytes(&config.path().join("missing")).unwrap(), 0);
    }
}
