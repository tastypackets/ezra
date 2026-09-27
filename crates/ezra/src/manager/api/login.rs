use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;

use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, Session, internal};
use crate::manager::agents::Agent;
use crate::manager::codex_remote::SignInHold;
use crate::manager::events::Topic;
use crate::manager::login::{AgentCli, LoginEnd, LoginError, LoginProcess, LoginPrompt};

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
    let hold = state.hold_remote_for_sign_in(agent).await;
    state.logins.lock().await.remove(&agent);
    state.events.publish(Topic::Agents);
    let cli = AgentCli::installed(agent, &state.install_paths)?;
    let starting = state.clone();
    let prompt = tokio::spawn(async move {
        let (login, prompt) = match cli.start_login().await {
            Ok(started) => started,
            Err(error) => {
                if hold.is_some() {
                    starting.agent_checks.refresh(agent).await;
                }
                return Err(error);
            }
        };
        let end = login.end();
        starting.logins.lock().await.insert(agent, login);
        starting.events.publish(Topic::Agents);
        tokio::spawn(starting.finish_login(agent, end, hold));
        Ok(prompt)
    })
    .await
    .map_err(internal)??;
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
    let finishing = state.clone();
    let submitted = tokio::spawn(async move {
        let submitted = login.submit_code(&body.code).await;
        if submitted.is_ok() {
            finishing.remote_control.supervision.restart();
        }
        finishing.agent_checks.refresh(agent).await;
        finishing.events.publish(Topic::Agents);
        submitted
    })
    .await
    .map_err(internal)?;
    submitted?;
    tracing::info!("{agent} is signed in");
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
    let hold = state.hold_remote_for_sign_in(agent).await;
    state.logins.lock().await.remove(&agent);
    state.events.publish(Topic::Agents);
    let cli = AgentCli::installed(agent, &state.install_paths)?;
    let signing_out = state.clone();
    tokio::spawn(async move {
        let logged_out = cli.log_out().await;
        signing_out.agent_checks.refresh(agent).await;
        drop(hold);
        logged_out
    })
    .await
    .map_err(internal)??;
    Ok(StatusCode::NO_CONTENT)
}

impl AppState {
    /// Stops Codex's remote control until the hold is dropped. None for Claude.
    async fn hold_remote_for_sign_in(&self, agent: Agent) -> Option<SignInHold> {
        match agent {
            Agent::Claude => None,
            Agent::Codex => Some(self.codex_remote.hold_for_sign_in().await),
        }
    }

    /// Once the sign-in's process ends by itself, refreshes the agent's sign-in, then drops the
    /// prompt and `hold`.
    async fn finish_login(self, agent: Agent, mut end: LoginEnd, hold: Option<SignInHold>) {
        if end.wait().await.is_none() {
            return;
        }
        let still_waiting = |logins: &HashMap<Agent, LoginProcess>| {
            logins
                .get(&agent)
                .is_some_and(|login| end.belongs_to(login))
        };
        if !still_waiting(&*self.logins.lock().await) {
            return;
        }
        self.agent_checks.refresh(agent).await;
        let mut logins = self.logins.lock().await;
        if still_waiting(&logins) {
            logins.remove(&agent);
            drop(logins);
            self.events.publish(Topic::Agents);
        }
        drop(hold);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use futures_util::StreamExt;
    use tokio::time::{Instant, sleep};

    use super::super::test_support::{EventStreamExt, ResponseExt, TestManager};
    use super::*;

    #[tokio::test]
    async fn signing_in_and_out_refreshes_the_sign_in() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let directory = tempfile::tempdir().expect("temporary directory");
        manager.install_fake_cli(
            Agent::Claude,
            &format!(
                r#"case "$1 $2" in
  "auth login") echo 'Visit: https://claude.com/cai/oauth/authorize?code=true'; read code; [ "$code" = good ] && touch {marker} ;;
  "auth status") if [ -f {marker} ]; then echo '{{"loggedIn":true}}'; else echo '{{"loggedIn":false}}'; fi ;;
  "auth logout") rm {marker} ;;
esac"#,
                marker = directory.path().join("signed-in").display()
            ),
        );
        let started = manager
            .post("/api/v1/agents/claude/login", "", Some(&cookie))
            .await;
        assert_eq!(started.status(), StatusCode::OK);
        assert!(
            manager
                .agent_status(Agent::Claude, &cookie)
                .await
                .login_prompt
                .is_some()
        );

        let submitted = manager
            .post(
                "/api/v1/agents/claude/login/code",
                r#"{"code": "good"}"#,
                Some(&cookie),
            )
            .await;
        assert_eq!(submitted.status(), StatusCode::NO_CONTENT);
        let signed_in = manager.agent_status(Agent::Claude, &cookie).await;
        assert_eq!((signed_in.logged_in, signed_in.login_prompt), (true, None));

        let signed_out = manager
            .post("/api/v1/agents/claude/logout", "", Some(&cookie))
            .await;
        assert_eq!(signed_out.status(), StatusCode::NO_CONTENT);
        assert!(!manager.agent_status(Agent::Claude, &cookie).await.logged_in);
        assert_eq!(
            manager.fake_cli_runs(Agent::Claude),
            [
                "auth login --claudeai",
                "auth status",
                "auth logout",
                "auth status"
            ]
        );
    }

    #[tokio::test]
    async fn a_device_sign_in_that_finishes_refreshes_the_sign_in() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let directory = tempfile::tempdir().expect("temporary directory");
        let marker = directory.path().join("signed-in");
        manager.install_fake_cli(
            Agent::Codex,
            &format!(
                r#"case "$*" in
  "login --device-auth")
    echo 'Open https://auth.openai.com/codex/device'
    echo 'Enter this one-time code'
    echo 'ABCD-12345'
    while [ ! -f {marker} ]; do sleep 0.05; done ;;
  "login status") if [ -f {marker} ]; then echo 'Logged in using ChatGPT' >&2; else echo 'Not logged in' >&2; exit 1; fi ;;
esac"#,
                marker = marker.display()
            ),
        );
        let prompt: LoginPrompt = manager
            .post("/api/v1/agents/codex/login", "", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(prompt.code.as_deref(), Some("ABCD-12345"));
        let waiting = manager.agent_status(Agent::Codex, &cookie).await;
        assert_eq!(
            (waiting.logged_in, waiting.login_prompt),
            (false, Some(prompt))
        );

        let mut events = Box::pin(manager.state.events.stream());
        events.next().await;
        fs::write(&marker, "").expect("marker is written");
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("the deadline fits");
        let finished = loop {
            let status = manager.agent_status(Agent::Codex, &cookie).await;
            if status.login_prompt.is_none() {
                break status;
            }
            assert!(Instant::now() < deadline, "the sign-in did not finish");
            sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(
            (finished.logged_in, finished.account.as_deref()),
            (true, Some("ChatGPT"))
        );
        let published = events.published().await;
        assert!(published.contains(&Topic::Agents), "{published:?}");
    }

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
