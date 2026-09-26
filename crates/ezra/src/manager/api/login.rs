use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;

use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, Session};
use crate::manager::agents::Agent;
use crate::manager::events::Topic;
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

#[derive(Deserialize, ToSchema)]
pub struct CodeBody {
    code: String,
}

#[utoipa::path(
    post,
    path = "/api/v1/agents/{agent}/login",
    operation_id = "startAgentLogin",
    tag = "agents",
    summary = "Start signing in",
    description = "Starts the agent's sign-in, replacing any unfinished one, and returns the link to open.",
    params(("agent" = Agent, Path, description = "The agent")),
    responses(
        (status = 200, description = "Sign-in started", body = LoginPrompt),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 409, description = "The agent is not installed", body = ErrorBody),
        (status = 502, description = "The agent did not show a sign-in link", body = ErrorBody)
    )
)]
pub async fn start(
    _: Session,
    State(state): State<AppState>,
    Path(agent): Path<Agent>,
) -> Result<Json<LoginPrompt>, ApiError> {
    state.logins.lock().await.remove(&agent);
    state.events.publish(Topic::Agents);
    let (login, prompt) = AgentCli::installed(agent, &state.install_paths)?
        .start_login()
        .await?;
    state.logins.lock().await.insert(agent, login);
    state.events.publish(Topic::Agents);
    Ok(Json(prompt))
}

#[utoipa::path(
    post,
    path = "/api/v1/agents/{agent}/login/code",
    operation_id = "submitAgentLoginCode",
    tag = "agents",
    summary = "Finish Claude sign-in",
    description = "Passes the code Claude shows after website sign-in to the waiting sign-in.",
    params(("agent" = Agent, Path, description = "The agent, only claude takes a code")),
    request_body = CodeBody,
    responses(
        (status = 204, description = "Signed in"),
        (status = 400, description = "The agent does not take a code", body = ErrorBody),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 409, description = "No sign-in is in progress", body = ErrorBody),
        (status = 502, description = "The sign-in did not finish", body = ErrorBody)
    )
)]
pub async fn submit_code(
    _: Session,
    State(state): State<AppState>,
    Path(agent): Path<Agent>,
    Json(body): Json<CodeBody>,
) -> Result<StatusCode, ApiError> {
    if agent != Agent::Claude {
        return Err(ApiError::BadRequest("only Claude sign-in takes a code"));
    }
    let login = state
        .logins
        .lock()
        .await
        .remove(&agent)
        .ok_or_else(|| ApiError::Conflict(format!("no {agent} sign-in is in progress")))?;
    let submitted = login.submit_code(&body.code).await;
    state.events.publish(Topic::Agents);
    submitted?;
    tracing::info!("{agent} is signed in");
    state.remote_control.restart();
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/agents/{agent}/logout",
    operation_id = "logOutAgent",
    tag = "agents",
    summary = "Sign the agent out",
    description = "Signs the agent's CLI out of its account.",
    params(("agent" = Agent, Path, description = "The agent")),
    responses(
        (status = 204, description = "Signed out"),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 409, description = "The agent is not installed", body = ErrorBody),
        (status = 502, description = "The CLI could not sign out", body = ErrorBody)
    )
)]
pub async fn log_out(
    _: Session,
    State(state): State<AppState>,
    Path(agent): Path<Agent>,
) -> Result<StatusCode, ApiError> {
    state.logins.lock().await.remove(&agent);
    state.events.publish(Topic::Agents);
    AgentCli::installed(agent, &state.install_paths)?
        .log_out()
        .await?;
    if agent == Agent::Claude {
        state.remote_control.reconsider();
    }
    state.events.publish(Topic::Agents);
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
