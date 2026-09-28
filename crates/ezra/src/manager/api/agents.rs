use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{Instant, sleep};
use utoipa::IntoParams;

use super::{ApiError, AppState, ErrorBody, Session, internal};

use crate::manager::agents::{Agent, InstallProgress};
use crate::manager::events::Topic;
use crate::manager::processes::Process;
use crate::manager::status::AgentStatus;
use crate::manager::updates::LatestRelease;
use crate::path_ext::PathExt;

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

/// How long an uninstall waits for the agent's servers and sessions to stop.
const UNINSTALL_STOP_TIMEOUT: Duration = Duration::from_secs(45);
const UNINSTALL_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UninstallQuery {
    /// Also delete the agent's sign-in, settings and chats.
    #[serde(default)]
    saved_data: bool,
}

#[utoipa::path(
    delete,
    path = "/api/v1/agents/{agent}",
    operation_id = "uninstallAgent",
    tag = "agents",
    summary = "Uninstall an agent",
    description = "Stops the agent's servers, removes its program and, when asked, its saved data.",
    params(("agent" = Agent, Path, description = "The agent"), UninstallQuery),
    responses(
        (status = 204, description = "Uninstalled"),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 500, description = "The files could not be removed", body = ErrorBody)
    )
)]
pub async fn uninstall(
    _: Session,
    State(state): State<AppState>,
    Path(agent): Path<Agent>,
    Query(query): Query<UninstallQuery>,
) -> Result<StatusCode, ApiError> {
    let uninstalling = state.clone();
    tokio::spawn(async move {
        uninstalling
            .uninstall_and_record(agent, query.saved_data)
            .await
    })
    .await
    .map_err(internal)??;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/agents/{agent}/restart-servers",
    operation_id = "restartAgentServers",
    tag = "agents",
    summary = "Restart an agent's servers",
    description = "Stops the agent's remote control servers with their running sessions, then starts them again.",
    params(("agent" = Agent, Path, description = "The agent")),
    responses(
        (status = 204, description = "Restarting"),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn restart_servers(
    _: Session,
    State(state): State<AppState>,
    Path(agent): Path<Agent>,
) -> StatusCode {
    tracing::info!("restarting the {agent} servers");
    state.restart_remote(agent);
    StatusCode::NO_CONTENT
}

impl AppState {
    /// Runs at manager start: a configured agent is missing after the container was recreated.
    pub async fn reinstall_configured_agents(self) {
        futures_util::future::join_all(
            Agent::ALL.map(|agent| self.reinstall_configured_agent(agent)),
        )
        .await;
    }

    async fn reinstall_configured_agent(&self, agent: Agent) {
        let _install = self.install_locks.get(agent).lock().await;
        if !self.settings.lock().await.agent(agent).configured
            || self.install_paths.installed_version(agent).is_some()
        {
            return;
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
                    self.install_locked(agent).await
                {
                    tracing::warn!("{message}");
                }
            }
            Err(error) => tracing::warn!("could not link the kept {agent}: {error}"),
        }
    }

    async fn uninstall_and_record(&self, agent: Agent, saved_data: bool) -> Result<(), ApiError> {
        let _install = self.install_locks.get(agent).lock().await;
        self.update_settings(|settings| {
            settings.agent_mut(agent).configured = false;
            Ok::<(), ApiError>(())
        })
        .await?;
        let command = self.install_paths.command(agent);
        tokio::task::spawn_blocking(move || command.remove_if_present())
            .await
            .map_err(internal)?
            .map_err(|error| ApiError::Internal(format!("could not uninstall {agent}: {error}")))?;
        self.latest_releases.lock().await.remove(&agent);
        self.agent_checks.refresh(agent).await;
        self.reconsider_remote(agent);
        self.events.publish(Topic::Agents);
        if !self.wait_until_nothing_runs(agent).await {
            tracing::warn!("{agent} was still running when its files were removed");
        }
        let paths = Arc::clone(&self.install_paths);
        tokio::task::spawn_blocking(move || {
            paths.uninstall(agent)?;
            if saved_data {
                paths.remove_saved_data(agent)?;
            }
            Ok::<(), std::io::Error>(())
        })
        .await
        .map_err(internal)?
        .map_err(|error| ApiError::Internal(format!("could not uninstall {agent}: {error}")))?;
        tracing::info!("{agent} is uninstalled");
        self.agent_checks.refresh(agent).await;
        self.events.publish(Topic::Agents);
        Ok(())
    }

    /// Waits until no process runs from the agent's kept versions. False when it timed out.
    async fn wait_until_nothing_runs(&self, agent: Agent) -> bool {
        let versions = self.install_paths.versions_directory(agent);
        let deadline = Instant::now().checked_add(UNINSTALL_STOP_TIMEOUT);
        loop {
            let directory = versions.clone();
            let running = tokio::task::spawn_blocking(move || Process::running_from(&directory))
                .await
                .unwrap_or_default();
            if running.is_empty() {
                return true;
            }
            if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
                return false;
            }
            sleep(UNINSTALL_POLL_INTERVAL).await;
        }
    }

    async fn install_and_record(&self, agent: Agent) -> Result<(), ApiError> {
        let _install = self.install_locks.get(agent).lock().await;
        self.install_locked(agent).await
    }

    /// The caller holds this agent's install lock.
    async fn install_locked(&self, agent: Agent) -> Result<(), ApiError> {
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
    use std::future::Future;
    use std::task::{Context, Waker};

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
    async fn uninstalling_removes_the_program_and_only_on_request_the_saved_data() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        assert_eq!(
            manager.delete("/api/v1/agents/claude", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        for agent in Agent::ALL {
            manager.install_fake_version(agent, "1.0.0", "true");
            let saved = manager
                .state
                .install_paths
                .config_directory(agent)
                .expect("the test manager has config directories")
                .join("saved.json");
            std::fs::create_dir_all(saved.parent().expect("a directory")).expect("is created");
            std::fs::write(&saved, "{}").expect("is written");
        }
        manager
            .state
            .update_settings(|settings| {
                for agent in Agent::ALL {
                    settings.agent_mut(agent).configured = true;
                }
                Ok::<(), ApiError>(())
            })
            .await
            .expect("settings are saved");

        for (agent, path) in [
            (Agent::Claude, "/api/v1/agents/claude"),
            (Agent::Codex, "/api/v1/agents/codex?saved_data=true"),
        ] {
            let response = manager.delete(path, Some(&cookie)).await;
            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{agent}");
            let paths = &manager.state.install_paths;
            assert!(!paths.command(agent).exists(), "{agent}");
            assert!(!paths.versions_directory(agent).exists(), "{agent}");
            assert!(!manager.state.settings.lock().await.agent(agent).configured);
        }
        let config = |agent| {
            manager
                .state
                .install_paths
                .config_directory(agent)
                .expect("the test manager has config directories")
                .to_path_buf()
        };
        assert!(config(Agent::Claude).join("saved.json").exists());
        assert!(config(Agent::Codex).is_dir());
        assert!(!config(Agent::Codex).join("saved.json").exists());
    }

    #[tokio::test]
    async fn agent_operations_wait_only_for_their_own_install_lock() {
        for (blocked, other) in [(Agent::Claude, Agent::Codex), (Agent::Codex, Agent::Claude)] {
            let manager = TestManager::new();
            for agent in Agent::ALL {
                manager.install_fake_version(agent, "1.0.0", "true");
                manager
                    .state
                    .settings
                    .lock()
                    .await
                    .agent_mut(agent)
                    .configured = true;
            }
            let guard = manager.state.install_locks.get(blocked).lock().await;
            {
                let install = manager.state.install_and_record(blocked);
                tokio::pin!(install);
                assert!(
                    install
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                        .is_pending()
                );
                assert!(manager.state.installs_in_progress.lock().await.is_empty());
            }
            let uninstall = manager.state.uninstall_and_record(blocked, false);
            tokio::pin!(uninstall);
            assert!(
                uninstall
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
            assert!(
                manager
                    .state
                    .settings
                    .lock()
                    .await
                    .agent(blocked)
                    .configured
            );
            assert!(manager.state.install_paths.command(blocked).exists());

            tokio::time::timeout(
                Duration::from_secs(5),
                manager.state.uninstall_and_record(other, false),
            )
            .await
            .expect("the other agent is not blocked")
            .expect("the other agent is uninstalled");
            assert!(!manager.state.install_paths.command(other).exists());
            assert!(manager.state.install_paths.command(blocked).exists());

            drop(guard);
            tokio::time::timeout(Duration::from_secs(5), uninstall)
                .await
                .expect("uninstall resumes after its lock is released")
                .expect("the blocked agent is uninstalled");
            assert!(!manager.state.install_paths.command(blocked).exists());
        }
    }

    #[tokio::test]
    async fn startup_restores_each_cached_agent_under_its_own_lock() {
        for (blocked, other) in [(Agent::Claude, Agent::Codex), (Agent::Codex, Agent::Claude)] {
            let manager = TestManager::new();
            for agent in Agent::ALL {
                manager.install_fake_version(agent, "1.0.0", "true");
                manager
                    .state
                    .install_paths
                    .command(agent)
                    .remove_if_present()
                    .expect("link is removed");
                manager
                    .state
                    .settings
                    .lock()
                    .await
                    .agent_mut(agent)
                    .configured = true;
            }
            let guard = manager.state.install_locks.get(blocked).lock().await;
            let restore = manager.state.clone().reinstall_configured_agents();
            tokio::pin!(restore);
            assert!(
                restore
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
            assert!(!manager.state.install_paths.command(blocked).exists());
            assert_eq!(
                manager
                    .state
                    .install_paths
                    .installed_version(other)
                    .as_deref(),
                Some("1.0.0")
            );

            drop(guard);
            tokio::time::timeout(Duration::from_secs(5), restore)
                .await
                .expect("startup resumes after the agent lock is released");
            assert_eq!(
                manager
                    .state
                    .install_paths
                    .installed_version(blocked)
                    .as_deref(),
                Some("1.0.0")
            );
        }
    }

    #[tokio::test]
    async fn startup_rechecks_configured_after_waiting_for_the_agent_lock() {
        for agent in Agent::ALL {
            let manager = TestManager::new();
            manager.install_fake_version(agent, "1.0.0", "true");
            manager
                .state
                .install_paths
                .command(agent)
                .remove_if_present()
                .expect("link is removed");
            manager
                .state
                .settings
                .lock()
                .await
                .agent_mut(agent)
                .configured = true;
            let guard = manager.state.install_locks.get(agent).lock().await;
            let restore = manager.state.clone().reinstall_configured_agents();
            tokio::pin!(restore);
            assert!(
                restore
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );

            manager
                .state
                .settings
                .lock()
                .await
                .agent_mut(agent)
                .configured = false;
            drop(guard);
            tokio::time::timeout(Duration::from_secs(5), restore)
                .await
                .expect("startup skips the unconfigured agent");
            assert!(!manager.state.install_paths.command(agent).exists());
            assert!(!manager.state.settings.lock().await.agent(agent).configured);
        }
    }

    #[tokio::test]
    async fn startup_preserves_a_version_linked_while_waiting_for_the_agent_lock() {
        for agent in Agent::ALL {
            let manager = TestManager::new();
            manager.install_fake_version(agent, "2.0.0", "true");
            manager
                .state
                .install_paths
                .command(agent)
                .remove_if_present()
                .expect("link is removed");
            manager
                .state
                .settings
                .lock()
                .await
                .agent_mut(agent)
                .configured = true;
            let guard = manager.state.install_locks.get(agent).lock().await;
            let restore = manager.state.clone().reinstall_configured_agents();
            tokio::pin!(restore);
            assert!(
                restore
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );

            manager.install_fake_version(agent, "1.0.0", "true");
            drop(guard);
            tokio::time::timeout(Duration::from_secs(5), restore)
                .await
                .expect("startup preserves the installed agent");
            assert_eq!(
                manager
                    .state
                    .install_paths
                    .installed_version(agent)
                    .as_deref(),
                Some("1.0.0")
            );
        }
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
