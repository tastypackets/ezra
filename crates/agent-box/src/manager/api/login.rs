use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;

use super::{ApiError, AppState, Session};
use crate::manager::agents::Agent;
use crate::manager::login::{AgentCli, LoginError, LoginPrompt};

impl From<LoginError> for ApiError {
    fn from(error: LoginError) -> Self {
        match error {
            LoginError::NotInstalled(_) => Self::Conflict(error.to_string()),
            LoginError::Io(_) => Self::Internal(error.to_string()),
            LoginError::NoPrompt { .. }
            | LoginError::Failed { .. }
            | LoginError::Command { .. } => Self::AgentFailed(error.to_string()),
        }
    }
}

#[derive(Deserialize)]
pub struct CodeBody {
    code: String,
}

/// Starts signing in, replacing any unfinished attempt, and returns the link to open.
pub async fn start(
    _: Session,
    State(state): State<AppState>,
    Path(agent): Path<Agent>,
) -> Result<Json<LoginPrompt>, ApiError> {
    state.logins.lock().await.remove(&agent);
    let (login, prompt) = AgentCli::installed(agent, &state.install_paths)?
        .start_login()
        .await?;
    state.logins.lock().await.insert(agent, login);
    Ok(Json(prompt))
}

/// Claude shows a code after signing in on the website; this hands it to `claude auth login`.
pub async fn submit_code(
    _: Session,
    State(state): State<AppState>,
    Path(agent): Path<Agent>,
    Json(body): Json<CodeBody>,
) -> Result<StatusCode, ApiError> {
    if agent != Agent::Claude {
        return Err(ApiError::BadRequest(
            "only Claude asks for a code; Codex finishes on the website",
        ));
    }
    let login = state
        .logins
        .lock()
        .await
        .remove(&agent)
        .ok_or_else(|| ApiError::Conflict(format!("no {agent} sign-in is in progress")))?;
    login.submit_code(&body.code).await?;
    tracing::info!("{agent} is signed in");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn log_out(
    _: Session,
    State(state): State<AppState>,
    Path(agent): Path<Agent>,
) -> Result<StatusCode, ApiError> {
    state.logins.lock().await.remove(&agent);
    AgentCli::installed(agent, &state.install_paths)?
        .log_out()
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::TestManager;
    use super::*;

    #[tokio::test]
    async fn login_requires_the_agent_to_be_installed() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let response = manager
            .post("/api/v1/agents/claude/login", "", Some(&cookie))
            .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn code_without_a_login_in_progress_is_a_conflict() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let response = manager
            .post(
                "/api/v1/agents/claude/login/code",
                r#"{"code": "abc"}"#,
                Some(&cookie),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn codex_takes_no_code() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let response = manager
            .post(
                "/api/v1/agents/codex/login/code",
                r#"{"code": "abc"}"#,
                Some(&cookie),
            )
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn login_routes_require_a_session() {
        let manager = TestManager::new();
        manager.logged_in().await;
        for path in [
            "/api/v1/agents/claude/login",
            "/api/v1/agents/claude/logout",
        ] {
            let response = manager.post(path, "", None).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
    }
}
