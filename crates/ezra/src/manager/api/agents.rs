use axum::Json;
use axum::extract::{Path, State};

use super::{ApiError, AppState, ErrorBody, Session, internal};
use std::sync::Arc;

use crate::manager::agents::{Agent, InstallProgress};
use crate::manager::events::Topic;
use crate::manager::status::AgentStatus;
use crate::manager::updates::LatestRelease;

#[utoipa::path(
    get,
    path = "/api/v1/agents",
    operation_id = "listAgents",
    tag = "agents",
    summary = "List agents",
    description = "Returns every agent with its install and sign-in state.",
    responses(
        (status = 200, description = "Agents", body = Vec<AgentStatus>),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn list(_: Session, State(state): State<AppState>) -> Json<Vec<AgentStatus>> {
    Json(AgentStatus::gather_all(&state).await)
}

#[utoipa::path(
    post,
    path = "/api/v1/agents/{agent}/install",
    operation_id = "installAgent",
    tag = "agents",
    summary = "Install or update an agent",
    description = "Installs the newest release on the agent's release channel when it is newer than the installed one, and marks the agent configured.",
    params(("agent" = Agent, Path, description = "The agent")),
    responses(
        (status = 200, description = "Installed", body = AgentStatus),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 502, description = "The download or checksum failed", body = ErrorBody)
    )
)]
pub async fn install(
    _: Session,
    State(state): State<AppState>,
    Path(agent): Path<Agent>,
) -> Result<Json<AgentStatus>, ApiError> {
    let installing = state.clone();
    tokio::spawn(async move { installing.install_and_record(agent).await })
        .await
        .map_err(internal)??;
    Ok(Json(AgentStatus::gather(agent, &state).await))
}

impl AppState {
    /// Runs at manager start: a configured agent is missing after the container was recreated.
    pub async fn reinstall_configured_agents(self) {
        let configured_agents: Vec<Agent> = {
            let settings = self.settings.lock().await;
            Agent::ALL
                .into_iter()
                .filter(|agent| settings.agent(*agent).configured)
                .collect()
        };
        for agent in configured_agents {
            if self.install_paths.installed_version(agent).is_some() {
                continue;
            }
            match self.install_paths.link_kept_version(agent) {
                Ok(Some(version)) => {
                    tracing::info!("{agent} {version} is linked from /cache");
                    self.reconsider_remote(agent);
                    self.events.publish(Topic::Agents);
                }
                Ok(None) => {
                    tracing::info!("reinstalling {agent}, which is configured but not installed");
                    if let Err(ApiError::AgentFailed(message) | ApiError::Internal(message)) =
                        self.install_and_record(agent).await
                    {
                        tracing::warn!("{message}");
                    }
                }
                Err(error) => tracing::warn!("could not link the kept {agent}: {error}"),
            }
        }
    }

    async fn install_and_record(&self, agent: Agent) -> Result<(), ApiError> {
        let _one_install_at_a_time = self.install_lock.lock().await;
        let channel = self.settings.lock().await.release_channel(agent);
        let progress = Arc::new(InstallProgress::default());
        self.installs_in_progress
            .lock()
            .await
            .insert(agent, Arc::clone(&progress));
        self.events.publish(Topic::Agents);
        let outcome = self
            .install_paths
            .install_latest(agent, channel, self.download_tls_verification, &progress)
            .await;
        self.installs_in_progress.lock().await.remove(&agent);
        self.events.publish(Topic::Agents);
        let version = outcome.map_err(|error| {
            ApiError::AgentFailed(format!("could not install {agent}: {error}"))
        })?;
        tracing::info!("{agent} {version} is installed");
        self.latest_releases
            .lock()
            .await
            .insert(agent, LatestRelease { channel, version });
        self.agent_checks.refresh(agent).await;
        self.reconsider_remote(agent);

        self.update_settings(|settings| {
            settings.agent_mut(agent).configured = true;
            Ok::<(), ApiError>(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::super::test_support::{ResponseExt, TestManager};
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
        for agent in Agent::ALL {
            manager.state.agent_checks.refresh(agent).await;
        }
        let listing: Vec<AgentStatus> = manager
            .get("/api/v1/agents", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(
            listing,
            Agent::ALL.map(|agent| AgentStatus {
                agent,
                configured: false,
                installed_version: None,
                logged_in: false,
                account: None,
                sign_in_ends_at: None,
                login_prompt: None,
                config_disk_bytes: Some(0),
                install_progress: None,
                available_update: None,
            })
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
