use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ezra::inbound::InboundSettings;
use serde::{Deserialize, Serialize};

use super::agents::{Agent, ReleaseChannel};
use super::auth::HashedPassword;
use super::codex_remote::CodexRemoteSettings;
use super::folders::Folder;
use super::remote_control::{ClaudeOptions, RemoteControlSettings};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub manager: ManagerSettings,
    #[serde(default)]
    pub agents: AgentsSettings,
    #[serde(default)]
    pub inbound: InboundSettings,
}

impl Settings {
    pub fn agent(&self, agent: Agent) -> &AgentSettings {
        match agent {
            Agent::Claude => &self.agents.claude.agent,
            Agent::Codex => &self.agents.codex.agent,
        }
    }

    pub fn agent_mut(&mut self, agent: Agent) -> &mut AgentSettings {
        match agent {
            Agent::Claude => &mut self.agents.claude.agent,
            Agent::Codex => &mut self.agents.codex.agent,
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
    pub codex: CodexSettings,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeSettings {
    #[serde(flatten)]
    pub agent: AgentSettings,
    #[serde(default)]
    pub release_channel: ReleaseChannel,
    #[serde(default)]
    pub remote_control: RemoteControlSettings,
    /// Each folder's choices by name, set to the defaults for a repository when first seen.
    #[serde(default)]
    pub folders: BTreeMap<String, FolderChoice>,
}

impl ClaudeSettings {
    /// The folder's recorded choices, or the defaults.
    pub fn folder_choice(&self, folder: &Folder) -> FolderChoice {
        self.folders
            .get(&folder.name)
            .cloned()
            .unwrap_or(FolderChoice {
                serve: self.remote_control.serve_repositories && folder.git.is_some(),
                ..FolderChoice::default()
            })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexSettings {
    #[serde(flatten)]
    pub agent: AgentSettings,
    #[serde(default)]
    pub remote_control: CodexRemoteSettings,
}

/// One folder's Remote Control choices.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredFolderChoice")]
pub struct FolderChoice {
    pub serve: bool,
    #[serde(flatten)]
    pub options: ClaudeOptions,
}

/// A folder's choices as saved: the switch alone, or every choice.
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredFolderChoice {
    Switch(bool),
    Choices {
        serve: bool,
        #[serde(flatten)]
        options: ClaudeOptions,
    },
}

impl From<StoredFolderChoice> for FolderChoice {
    fn from(stored: StoredFolderChoice) -> Self {
        match stored {
            StoredFolderChoice::Switch(serve) => Self {
                serve,
                options: ClaudeOptions::default(),
            },
            StoredFolderChoice::Choices { serve, options } => Self { serve, options },
        }
    }
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
    use crate::manager::codex_remote::{CodexApprovals, CodexSandbox};
    use crate::manager::folders::GitDetails;
    use crate::manager::remote_control::SpawnMode;

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
    fn inbound_settings_default_for_old_files_and_partial_overrides() {
        let old_settings: Settings =
            toml::from_str("[agents.codex]\nconfigured = true\n").expect("old settings parse");
        assert_eq!(old_settings.inbound, InboundSettings::default());
        assert_eq!(old_settings.inbound.retention_days, 90);
        let partial: Settings =
            toml::from_str("[inbound]\nretention_days = 180\nmax_queued_events = 20\n")
                .expect("partial inbound settings parse");
        assert_eq!(
            partial.inbound,
            InboundSettings {
                retention_days: 180,
                max_queued_events: 20,
                ..InboundSettings::default()
            }
        );
    }

    #[test]
    fn inbound_settings_survive_saving_other_choices() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("settings.toml");
        let mut settings = Settings {
            inbound: InboundSettings {
                retention_days: 120,
                max_queued_events: 50,
                max_queued_message_bytes: 1_000_000,
                max_history_events: 500,
                max_history_message_bytes: 5_000_000,
                max_idle_conversations: 200,
                ..InboundSettings::default()
            },
            ..Settings::default()
        };
        settings.inbound.shortcuts.insert(
            "/ezra-codex".to_owned(),
            ezra::inbound::Shortcut {
                agent: Some("codex".to_owned()),
                model: Some("chosen-model".to_owned()),
                effort: Some("high".to_owned()),
            },
        );
        settings.inbound.github.only_added_repositories = false;
        settings.inbound.github.max_concurrent_requests =
            std::num::NonZeroUsize::new(2).expect("positive concurrency");
        settings.save(&path).expect("inbound settings save");
        let mut loaded = Settings::load(&path).expect("inbound settings load");
        loaded.agent_mut(Agent::Codex).configured = true;
        loaded.save(&path).expect("agent choice saves");
        settings.agent_mut(Agent::Codex).configured = true;
        assert_eq!(Settings::load(&path).expect("settings reload"), settings);
    }

    #[test]
    fn inbound_settings_reject_negative_and_out_of_range_limits() {
        for assignment in [
            "retention_days = -1",
            "retention_days = 4294967296",
            "max_queued_events = -1",
            "max_queued_message_bytes = -1",
            "max_history_events = -1",
            "max_history_message_bytes = -1",
            "max_idle_conversations = -1",
        ] {
            assert!(
                toml::from_str::<Settings>(&format!("[inbound]\n{assignment}\n")).is_err(),
                "{assignment}"
            );
        }
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
    fn codex_saved_before_remote_control_loads_with_its_defaults() {
        let settings: Settings =
            toml::from_str("[agents.codex]\nconfigured = true\n").expect("settings parse");
        assert!(settings.agent(Agent::Codex).configured);
        assert_eq!(
            settings.agents.codex.remote_control,
            CodexRemoteSettings {
                enabled: true,
                sandbox: CodexSandbox::DangerFullAccess,
                approvals: CodexApprovals::OnRequest,
            }
        );
    }

    #[test]
    fn codex_settings_sit_under_its_table() {
        let mut settings = Settings::default();
        settings.agent_mut(Agent::Codex).configured = true;
        settings.agents.codex.remote_control = CodexRemoteSettings {
            enabled: false,
            sandbox: CodexSandbox::WorkspaceWrite,
            approvals: CodexApprovals::Never,
        };
        let saved = toml::to_string_pretty(&settings).expect("settings serialize");
        assert!(
            saved.contains(
                "[agents.codex]\n\
                 configured = true\n\n\
                 [agents.codex.remote_control]\n\
                 enabled = false\n\
                 sandbox = \"workspace-write\"\n\
                 approvals = \"never\"\n"
            ),
            "{saved}"
        );
        assert_eq!(
            toml::from_str::<Settings>(&saved).expect("saved settings parse"),
            settings
        );
    }

    #[test]
    fn folder_switches_saved_alone_load_as_choices() {
        let settings: Settings = toml::from_str(
            "[agents.claude.folders]\n\
             app = true\n\
             notes = false\n\
             site = { serve = true, spawn = \"worktree\" }\n\
             tight = { serve = true, spawn = \"same-dir\", permission_mode = \"plan\", capacity = 1 }\n",
        )
        .expect("settings parse");
        let served = FolderChoice {
            serve: true,
            options: ClaudeOptions::default(),
        };
        let in_worktrees = FolderChoice {
            serve: true,
            options: ClaudeOptions {
                spawn: Some(SpawnMode::Worktree),
                ..ClaudeOptions::default()
            },
        };
        assert_eq!(
            settings.agents.claude.folders,
            BTreeMap::from([
                ("app".to_owned(), served),
                ("notes".to_owned(), FolderChoice::default()),
                ("site".to_owned(), in_worktrees),
                (
                    "tight".to_owned(),
                    FolderChoice {
                        serve: true,
                        options: ClaudeOptions {
                            spawn: Some(SpawnMode::SameDir),
                            permission_mode: Some("plan".to_owned()),
                            capacity: Some(1),
                        },
                    }
                ),
            ])
        );
        let saved = toml::to_string_pretty(&settings).expect("settings serialize");
        assert!(
            saved.contains("[agents.claude.folders.app]\nserve = true\n\n"),
            "{saved}"
        );
        assert_eq!(
            toml::from_str::<Settings>(&saved).expect("saved settings parse"),
            settings
        );
    }

    #[test]
    fn a_folder_without_choices_gets_the_defaults() {
        let mut claude = ClaudeSettings::default();
        let in_worktrees = FolderChoice {
            serve: true,
            options: ClaudeOptions {
                spawn: Some(SpawnMode::Worktree),
                ..ClaudeOptions::default()
            },
        };
        claude
            .folders
            .insert("app".to_owned(), in_worktrees.clone());
        let repository = Folder {
            name: "app".to_owned(),
            git: Some(GitDetails::default()),
        };
        assert_eq!(claude.folder_choice(&repository), in_worktrees);
        let new_repository = Folder {
            name: "new".to_owned(),
            ..repository
        };
        assert_eq!(
            claude.folder_choice(&new_repository),
            FolderChoice {
                serve: true,
                options: ClaudeOptions::default(),
            }
        );
        claude.remote_control.serve_repositories = false;
        assert_eq!(
            claude.folder_choice(&new_repository),
            FolderChoice::default()
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
