use axum::Json;
use axum::extract::{Path, State};

use super::{ApiError, AppState, Session};
use std::sync::Arc;

use crate::manager::agents::{Agent, InstallProgress};
use crate::manager::status::AgentStatus;

pub async fn list(_: Session, State(state): State<AppState>) -> Json<Vec<AgentStatus>> {
    Json(AgentStatus::gather_all(&state).await)
}

/// Installs the newest release, or updates to it, and marks the agent configured.
pub async fn install(
    _: Session,
    State(state): State<AppState>,
    Path(agent): Path<Agent>,
) -> Result<Json<AgentStatus>, ApiError> {
    state.install_and_record(agent).await?;
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
            if self.install_paths.installed_version(agent).is_none() {
                tracing::info!("reinstalling {agent}, which is configured but not installed");
                if let Err(ApiError::AgentFailed(message) | ApiError::Internal(message)) =
                    self.install_and_record(agent).await
                {
                    tracing::warn!("{message}");
                }
            }
        }
    }

    async fn install_and_record(&self, agent: Agent) -> Result<(), ApiError> {
        let _one_install_at_a_time = self.install_lock.lock().await;
        let progress = Arc::new(InstallProgress::default());
        self.installs_in_progress
            .lock()
            .await
            .insert(agent, Arc::clone(&progress));
        let outcome = self
            .install_paths
            .install_latest(agent, self.download_tls_verification, &progress)
            .await;
        self.installs_in_progress.lock().await.remove(&agent);
        let version = outcome.map_err(|error| {
            ApiError::AgentFailed(format!("could not install {agent}: {error}"))
        })?;
        tracing::info!("{agent} {version} is installed");

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
                login_prompt: None,
                session_count: Some(0),
                config_disk_bytes: Some(0),
                install_progress: None,
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
