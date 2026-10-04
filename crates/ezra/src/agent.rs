use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A command-line coding agent the manager can install and run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown agent {0:?}")]
pub struct UnknownAgent(pub String);

impl fmt::Display for Agent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.command_name())
    }
}

impl FromStr for Agent {
    type Err = UnknownAgent;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|agent| agent.command_name() == name)
            .ok_or_else(|| UnknownAgent(name.to_owned()))
    }
}

impl Agent {
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    pub fn command_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// Where a release unpacked at `version_path` keeps the command.
    pub fn command_in(self, version_path: &Path) -> PathBuf {
        match self {
            Self::Claude => version_path.to_path_buf(),
            Self::Codex => version_path.join("bin/codex"),
        }
    }

    pub fn config_directory_variable(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE_CONFIG_DIR",
            Self::Codex => "CODEX_HOME",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_parse_back_and_match_the_serialized_form() {
        for agent in Agent::ALL {
            assert_eq!(agent.to_string().parse(), Ok(agent));
            assert_eq!(
                serde_json::to_value(agent).expect("agent serializes"),
                serde_json::json!(agent.command_name())
            );
        }
        assert_eq!(
            "gemini".parse::<Agent>(),
            Err(UnknownAgent("gemini".to_owned()))
        );
        assert!("Codex".parse::<Agent>().is_err());
    }
}
