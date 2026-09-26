use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, Session};
use crate::manager::git::{CommitIdentity, GitError};
use crate::manager::login::{LoginProcess, LoginPrompt};

impl From<GitError> for ApiError {
    fn from(error: GitError) -> Self {
        match error {
            GitError::Io(_) => Self::Internal(error.to_string()),
            GitError::Git { .. } | GitError::GitHub { .. } => Self::AgentFailed(error.to_string()),
        }
    }
}

/// The GitHub sign-in and commit identity git uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GitStatus {
    pub github: GitHubStatus,
    pub identity: CommitIdentity,
}

/// The GitHub sign-in git pushes with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GitHubStatus {
    pub signed_in: bool,
    /// The GitHub account, absent when unknown.
    pub account: Option<String>,
    /// An account is set up, but GitHub did not confirm it.
    pub failing: bool,
    /// Comes from `GH_TOKEN` or `GITHUB_TOKEN`, so sign-in and sign-out return 409.
    pub from_environment: bool,
    /// Present while a sign-in waits for the person signing in.
    pub login_prompt: Option<LoginPrompt>,
}

#[utoipa::path(
    get,
    path = "/api/v1/git",
    operation_id = "getGitStatus",
    tag = "git",
    summary = "Get the GitHub sign-in and commit identity",
    responses(
        (status = 200, description = "Git status", body = GitStatus),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn status(_: Session, State(state): State<AppState>) -> Json<GitStatus> {
    Json(state.git_status().await)
}

#[utoipa::path(
    post,
    path = "/api/v1/git/github/login",
    operation_id = "startGitHubLogin",
    tag = "git",
    summary = "Start GitHub sign-in",
    description = "Starts a device sign-in and returns the page to open and the code to enter there.",
    responses(
        (status = 200, description = "Sign-in started", body = LoginPrompt),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 409, description = "The sign-in comes from an environment variable", body = ErrorBody),
        (status = 502, description = "gh did not start a sign-in", body = ErrorBody)
    )
)]
pub async fn start_github_login(
    _: Session,
    State(state): State<AppState>,
) -> Result<Json<LoginPrompt>, ApiError> {
    state.refuse_environment_sign_in()?;
    state.github_login.lock().await.take();
    let (login, prompt) = state.git_tools.start_github_login().await?;
    *state.github_login.lock().await = Some(login);
    tracing::info!("GitHub sign-in started");
    Ok(Json(prompt))
}

#[utoipa::path(
    post,
    path = "/api/v1/git/github/logout",
    operation_id = "logOutOfGitHub",
    tag = "git",
    summary = "Sign git out of GitHub",
    responses(
        (status = 204, description = "Signed out"),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 409, description = "The sign-in comes from an environment variable", body = ErrorBody),
        (status = 502, description = "gh could not sign out", body = ErrorBody)
    )
)]
pub async fn log_out_of_github(
    _: Session,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    state.refuse_environment_sign_in()?;
    state.github_login.lock().await.take();
    state.git_tools.log_out_of_github().await?;
    tracing::info!("GitHub is signed out");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    put,
    path = "/api/v1/git/identity",
    operation_id = "updateCommitIdentity",
    tag = "git",
    summary = "Set the commit name and email",
    description = "An empty or missing value removes it.",
    request_body = CommitIdentity,
    responses(
        (status = 200, description = "Saved", body = CommitIdentity),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 502, description = "git could not save it", body = ErrorBody)
    )
)]
pub async fn update_identity(
    _: Session,
    State(state): State<AppState>,
    Json(identity): Json<CommitIdentity>,
) -> Result<Json<CommitIdentity>, ApiError> {
    state.git_tools.save_commit_identity(&identity).await?;
    Ok(Json(state.git_tools.commit_identity().await?))
}

impl AppState {
    /// While a sign-in runs, GitHub's part is only its prompt.
    async fn git_status(&self) -> GitStatus {
        let identity = match self.git_tools.commit_identity().await {
            Ok(identity) => identity,
            Err(error) => {
                tracing::warn!("could not read the commit identity: {error}");
                CommitIdentity::default()
            }
        };
        let from_environment = self.git_tools.token_from_environment();
        let login_prompt = self.finish_github_login().await;
        if login_prompt.is_some() {
            return GitStatus {
                github: GitHubStatus {
                    signed_in: false,
                    account: None,
                    failing: false,
                    from_environment,
                    login_prompt,
                },
                identity,
            };
        }
        let sign_in = self.git_tools.github_sign_in().await;
        GitStatus {
            github: GitHubStatus {
                signed_in: sign_in.signed_in,
                account: sign_in.account,
                failing: sign_in.failing,
                from_environment,
                login_prompt: None,
            },
            identity,
        }
    }

    /// Returns the prompt of a sign-in still running. One that succeeded is lent to git.
    async fn finish_github_login(&self) -> Option<LoginPrompt> {
        let mut login = self.github_login.lock().await;
        match login.as_mut().map(LoginProcess::outcome) {
            Some(None) => login.as_ref().and_then(|login| login.prompt().cloned()),
            Some(Some(succeeded)) => {
                login.take();
                if succeeded {
                    tracing::info!("GitHub is signed in");
                    self.lend_github_sign_in_to_git().await;
                }
                None
            }
            None => None,
        }
    }

    /// Covers sign-ins made outside the manager, such as `gh auth login` in a terminal.
    pub async fn lend_github_sign_in_at_start(self) {
        if self.git_tools.github_sign_in().await.signed_in {
            self.lend_github_sign_in_to_git().await;
        }
    }

    async fn lend_github_sign_in_to_git(&self) {
        if let Err(error) = self.git_tools.lend_github_sign_in_to_git().await {
            tracing::warn!("could not let git use the GitHub sign-in: {error}");
        }
    }

    fn refuse_environment_sign_in(&self) -> Result<(), ApiError> {
        if self.git_tools.token_from_environment() {
            Err(ApiError::Conflict(
                "the GitHub sign-in comes from the GH_TOKEN or GITHUB_TOKEN variable".to_owned(),
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::super::test_support::TestManager;

    #[tokio::test]
    async fn git_needs_a_login() {
        let manager = TestManager::new();
        manager.logged_in().await;
        assert_eq!(
            manager.get("/api/v1/git", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        for path in ["/api/v1/git/github/login", "/api/v1/git/github/logout"] {
            assert_eq!(
                manager.post(path, "", None).await.status(),
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }
        assert_eq!(
            manager
                .put("/api/v1/git/identity", r#"{"name":"x"}"#, None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
