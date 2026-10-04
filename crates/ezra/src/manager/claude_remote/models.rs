use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::time::timeout;

use crate::manager::agents::Agent;
use crate::manager::login::AgentCli;
use crate::manager::state::AppState;
use crate::manager::status::{AgentEffort, AgentModel};

const LIST_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_ID: &str = "ezra-models";

#[derive(Debug, thiserror::Error)]
pub enum ModelsError {
    #[error("could not run claude: {0}")]
    Io(#[from] io::Error),
    #[error("claude did not answer within {LIST_TIMEOUT:?}")]
    TimedOut,
    #[error("claude did not list its models: {0}")]
    NoList(String),
}

#[derive(Deserialize)]
struct Reply {
    #[serde(rename = "type")]
    kind: String,
    response: Option<Response>,
}

#[derive(Deserialize)]
struct Response {
    subtype: String,
    request_id: Option<String>,
    error: Option<String>,
    response: Option<Initialized>,
}

#[derive(Deserialize)]
struct Initialized {
    models: Vec<ClaudeModel>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeModel {
    value: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    description: String,
    supports_effort: Option<bool>,
    #[serde(default)]
    supported_effort_levels: Vec<String>,
}

impl From<ClaudeModel> for AgentModel {
    fn from(model: ClaudeModel) -> Self {
        let efforts = if model.supports_effort == Some(false) {
            Vec::new()
        } else {
            model.supported_effort_levels
        };
        Self {
            model: model.value,
            display_name: model.display_name,
            description: model.description,
            efforts: efforts
                .into_iter()
                .map(|effort| AgentEffort {
                    effort,
                    description: String::new(),
                })
                .collect(),
        }
    }
}

impl AgentCli {
    /// Claude Code's models and the efforts each takes, as its stream-json handshake answers
    /// in `directory`. The handshake sends no prompt, so no model runs.
    pub async fn claude_models(&self, directory: &Path) -> Result<Vec<AgentModel>, ModelsError> {
        let mut command = self.command();
        command
            .args([
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--no-session-persistence",
            ])
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let request = serde_json::json!({
            "type": "control_request",
            "request_id": REQUEST_ID,
            "request": {"subtype": "initialize"},
        });
        let mut stdin = child.stdin.take().expect("stdin is piped");
        stdin.write_all(format!("{request}\n").as_bytes()).await?;
        drop(stdin);
        let output = timeout(LIST_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| ModelsError::TimedOut)??;
        let response = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<Reply>(line).ok())
            .filter(|reply| reply.kind == "control_response")
            .filter_map(|reply| reply.response)
            .find(|response| response.request_id.as_deref() == Some(REQUEST_ID))
            .ok_or_else(|| ModelsError::NoList(format!("it exited with {}", output.status)))?;
        match response {
            Response {
                response: Some(initialized),
                ..
            } if response.subtype == "success" => Ok(initialized
                .models
                .into_iter()
                .map(AgentModel::from)
                .collect()),
            Response { error, .. } => Err(ModelsError::NoList(
                error.unwrap_or_else(|| "it answered without models".to_owned()),
            )),
        }
    }
}

impl AppState {
    /// Reads the models Claude Code lists and keeps them with its checks.
    pub async fn read_claude_models(self) {
        let listed = match AgentCli::installed(Agent::Claude, &self.install_paths) {
            Ok(cli) => cli.claude_models(&self.projects.0).await,
            Err(error) => {
                tracing::warn!("could not read Claude Code's models: {error}");
                return;
            }
        };
        match listed {
            Ok(models) => self.agent_checks.store_models(Agent::Claude, models),
            Err(error) => tracing::warn!("could not read Claude Code's models: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::manager::api::test_support::TestManager;

    /// A `claude` that saves its stdin and working directory under `out`, then prints `stdout`.
    fn claude(manager: &TestManager, out: &Path, stdout: &str) -> AgentCli {
        fs::write(out.join("stdout"), stdout).expect("stdout is written");
        manager.install_fake_cli(
            Agent::Claude,
            &format!(
                "OUT='{}'\ncat > \"$OUT/stdin\"\npwd > \"$OUT/pwd\"\ncat \"$OUT/stdout\"",
                out.display()
            ),
        );
        AgentCli::installed(Agent::Claude, &manager.state.install_paths).expect("claude installed")
    }

    #[tokio::test]
    async fn the_handshake_lists_each_model_with_the_efforts_it_takes() {
        let manager = TestManager::new();
        let out = tempfile::tempdir().expect("temporary directory");
        let answer = serde_json::json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": REQUEST_ID, "response": {
                "commands": [], "newField": {},
                "models": [
                    {"value": "opus", "displayName": "Opus", "description": "For complex work",
                     "supportsEffort": true, "supportedEffortLevels": ["low", "xhigh"], "newFlag": true},
                    {"value": "haiku", "displayName": "Haiku", "description": "Fastest"},
                    {"value": "plain", "supportsEffort": false, "supportedEffortLevels": ["low"]},
                ],
            }},
        });
        let cli = claude(
            &manager,
            out.path(),
            &format!("{{\"type\":\"system\"}}\nnot json\n{answer}\n"),
        );
        let directory = tempfile::tempdir().expect("temporary directory");

        let models = cli
            .claude_models(directory.path())
            .await
            .expect("models are listed");

        let effort = |effort: &str| AgentEffort {
            effort: effort.to_owned(),
            description: String::new(),
        };
        assert_eq!(
            models,
            [
                AgentModel {
                    model: "opus".to_owned(),
                    display_name: "Opus".to_owned(),
                    description: "For complex work".to_owned(),
                    efforts: vec![effort("low"), effort("xhigh")],
                },
                AgentModel {
                    model: "haiku".to_owned(),
                    display_name: "Haiku".to_owned(),
                    description: "Fastest".to_owned(),
                    efforts: Vec::new(),
                },
                AgentModel {
                    model: "plain".to_owned(),
                    display_name: String::new(),
                    description: String::new(),
                    efforts: Vec::new(),
                },
            ]
        );
        let sent: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(out.path().join("stdin")).expect("stdin is saved"),
        )
        .expect("one JSON request");
        assert_eq!(
            sent,
            serde_json::json!({"type": "control_request", "request_id": REQUEST_ID, "request": {"subtype": "initialize"}})
        );
        assert_eq!(
            fs::read_to_string(out.path().join("pwd"))
                .expect("pwd is saved")
                .trim(),
            directory.path().to_str().expect("a UTF-8 path")
        );
        assert_eq!(
            manager.fake_cli_runs(Agent::Claude),
            [
                "-p --input-format stream-json --output-format stream-json --verbose --no-session-persistence"
            ]
        );
    }

    #[tokio::test]
    async fn a_handshake_without_models_says_why() {
        let manager = TestManager::new();
        let out = tempfile::tempdir().expect("temporary directory");
        let refused = serde_json::json!({
            "type": "control_response",
            "response": {"subtype": "error", "request_id": REQUEST_ID, "error": "not signed in"},
        });
        for (stdout, reason) in [
            (
                refused.to_string(),
                "claude did not list its models: not signed in",
            ),
            (
                String::new(),
                "claude did not list its models: it exited with exit status: 0",
            ),
        ] {
            let cli = claude(&manager, out.path(), &stdout);
            let error = cli
                .claude_models(out.path())
                .await
                .expect_err("no models are listed");
            assert_eq!(error.to_string(), reason);
        }
    }
}
