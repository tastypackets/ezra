use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use super::{ApiError, AppState, Session, internal};
use crate::manager::agents::{self, Agent, InstallPaths};
use crate::manager::settings::{self, Settings};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentStatus {
    agent: Agent,
    configured: bool,
    installed_version: Option<String>,
}

fn status_of(agent: Agent, settings: &Settings, install_paths: &InstallPaths) -> AgentStatus {
    AgentStatus {
        agent,
        configured: settings.agent(agent).configured,
        installed_version: agents::installed_version(agent, install_paths),
    }
}

pub async fn list(_: Session, State(state): State<AppState>) -> Json<Vec<AgentStatus>> {
    let settings = state.settings.lock().await;
    Json(
        Agent::ALL
            .into_iter()
            .map(|agent| status_of(agent, &settings, &state.install_paths))
            .collect(),
    )
}

/// Installs the newest release, or updates to it, and marks the agent configured.
pub async fn install(
    _: Session,
    State(state): State<AppState>,
    Path(agent): Path<Agent>,
) -> Result<Json<AgentStatus>, ApiError> {
    install_and_record(&state, agent).await?;
    let settings = state.settings.lock().await;
    Ok(Json(status_of(agent, &settings, &state.install_paths)))
}

/// Runs at manager start: a configured agent is missing after the container was recreated.
pub async fn reinstall_configured_agents(state: AppState) {
    let configured_agents: Vec<Agent> = {
        let settings = state.settings.lock().await;
        Agent::ALL
            .into_iter()
            .filter(|agent| settings.agent(*agent).configured)
            .collect()
    };
    for agent in configured_agents {
        if agents::installed_version(agent, &state.install_paths).is_none() {
            tracing::info!("reinstalling {agent}, which is configured but not installed");
            if let Err(ApiError::InstallFailed(message) | ApiError::Internal(message)) =
                install_and_record(&state, agent).await
            {
                tracing::warn!("{message}");
            }
        }
    }
}

async fn install_and_record(state: &AppState, agent: Agent) -> Result<(), ApiError> {
    let _one_install_at_a_time = state.install_lock.lock().await;
    let version = agents::install_latest(agent, &state.install_paths)
        .await
        .map_err(|error| ApiError::InstallFailed(format!("could not install {agent}: {error}")))?;
    tracing::info!("{agent} {version} is installed");

    let mut settings = state.settings.lock().await;
    if !settings.agent(agent).configured {
        let mut updated_settings = settings.clone();
        updated_settings.agent_mut(agent).configured = true;
        settings::save(&state.settings_path, &updated_settings).map_err(internal)?;
        *settings = updated_settings;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::super::test_support::{TestManager, json_of};
    use super::*;

    #[tokio::test]
    async fn agents_require_a_login() {
        let manager = TestManager::new();
        manager.logged_in().await;
        let listing = manager.get("/api/v1/agents", None).await;
        assert_eq!(listing.status(), StatusCode::UNAUTHORIZED);
        let install = manager
            .post("/api/v1/agents/claude/install", "", None)
            .await;
        assert_eq!(install.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn fresh_manager_lists_both_agents_as_not_installed() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let listing: Vec<AgentStatus> =
            json_of(manager.get("/api/v1/agents", Some(&cookie)).await).await;
        assert_eq!(
            listing,
            [
                AgentStatus {
                    agent: Agent::Claude,
                    configured: false,
                    installed_version: None
                },
                AgentStatus {
                    agent: Agent::Codex,
                    configured: false,
                    installed_version: None
                },
            ]
        );
    }

    #[tokio::test]
    async fn unknown_agent_is_a_bad_request() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let response = manager
            .post("/api/v1/agents/gemini/install", "", Some(&cookie))
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
