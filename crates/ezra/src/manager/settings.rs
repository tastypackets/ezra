use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::agents::{Agent, ReleaseChannel};
use super::auth::HashedPassword;
use super::remote_control::RemoteControlSettings;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub manager: ManagerSettings,
    #[serde(default)]
    pub agents: AgentsSettings,
}

impl Settings {
    pub fn agent(&self, agent: Agent) -> &AgentSettings {
        match agent {
            Agent::Claude => &self.agents.claude.agent,
            Agent::Codex => &self.agents.codex,
        }
    }

    pub fn agent_mut(&mut self, agent: Agent) -> &mut AgentSettings {
        match agent {
            Agent::Claude => &mut self.agents.claude.agent,
            Agent::Codex => &mut self.agents.codex,
        }
    }

    pub fn release_channel(&self, agent: Agent) -> ReleaseChannel {
        match agent {
            Agent::Claude => self.agents.claude.release_channel,
            Agent::Codex => ReleaseChannel::Latest,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagerSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_hash: Option<HashedPassword>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsSettings {
    #[serde(default)]
    pub claude: ClaudeSettings,
    #[serde(default)]
    pub codex: AgentSettings,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeSettings {
    #[serde(flatten)]
    pub agent: AgentSettings,
    #[serde(default)]
    pub release_channel: ReleaseChannel,
    #[serde(default)]
    pub remote_control: RemoteControlSettings,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSettings {
    /// Installed through the manager at least once, so it is reinstalled when missing.
    #[serde(default)]
    pub configured: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("could not read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("{path} is not valid TOML: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("could not write {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("could not serialize the settings: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl Settings {
    /// A missing file means default settings.
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|source| SettingsError::Parse {
                path: path.to_owned(),
                source,
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(SettingsError::Read {
                path: path.to_owned(),
                source,
            }),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        let text = toml::to_string_pretty(self)?;
        let write = || -> io::Result<()> {
            if let Some(directory) = path.parent() {
                fs::create_dir_all(directory)?;
            }
            fs::write(path, text)
        };
        write().map_err(|source| SettingsError::Write {
            path: path.to_owned(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_means_defaults() {
        let directory = tempfile::tempdir().expect("temporary directory");
        assert_eq!(
            Settings::load(&directory.path().join("settings.toml")).expect("defaults load"),
            Settings::default()
        );
    }

    #[test]
    fn saved_settings_load_back() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ezra/settings.toml");
        let mut settings = Settings {
            manager: ManagerSettings {
                password_hash: Some(
                    HashedPassword::from_password("correct horse").expect("password hashes"),
                ),
            },
            ..Settings::default()
        };
        settings.agent_mut(Agent::Codex).configured = true;
        settings.agents.claude.release_channel = ReleaseChannel::Stable;
        settings.save(&path).expect("settings save");
        assert_eq!(Settings::load(&path).expect("settings load"), settings);
    }

    #[test]
    fn claude_settings_sit_in_one_table() {
        let settings: Settings =
            toml::from_str("[agents.claude]\nconfigured = true\nrelease_channel = \"stable\"\n")
                .expect("settings parse");
        assert!(settings.agent(Agent::Claude).configured);
        assert_eq!(
            settings.release_channel(Agent::Claude),
            ReleaseChannel::Stable
        );
        assert_eq!(
            settings.release_channel(Agent::Codex),
            ReleaseChannel::Latest
        );
    }

    #[test]
    fn invalid_toml_is_an_error() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("settings.toml");
        fs::write(&path, "[manager\n").expect("settings file is written");
        assert!(matches!(
            Settings::load(&path),
            Err(SettingsError::Parse { .. })
        ));
    }
}
