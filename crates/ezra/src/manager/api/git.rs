use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, Session};
use crate::manager::events::Topic;
use crate::manager::git::{CommitIdentity, GitError, GitHubRepository, GitHubSignIn};
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
    /// The GitHub host, from `GH_HOST`, github.com by default.
    pub host: String,
    pub signed_in: bool,
    /// The GitHub account, absent when unknown.
    pub account: Option<String>,
    /// An account is set up, but GitHub did not confirm it.
    pub failing: bool,
    /// Comes from one of `token_variables`, so sign-in and sign-out return 409.
    pub from_environment: bool,
    /// The variables gh takes a token from for this host, over any sign-in.
    pub token_variables: Vec<String>,
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
    state.events.publish(Topic::Git);
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
    let signed_out = state.git_tools.log_out_of_github().await;
    state.events.publish(Topic::Git);
    signed_out?;
    state.note_github_sign_in(GitHubSignIn::default());
    tracing::info!("GitHub is signed out");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/git/github/repositories",
    operation_id = "listGitHubRepositories",
    tag = "git",
    summary = "List the GitHub account's repositories",
    description = "Returns up to 100 repositories the account owns or works on, none while signed out.",
    responses(
        (status = 200, description = "Repositories, most recently pushed first", body = Vec<GitHubRepository>),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 502, description = "GitHub did not answer", body = ErrorBody)
    )
)]
pub async fn github_repositories(
    _: Session,
    State(state): State<AppState>,
) -> Result<Json<Vec<GitHubRepository>>, ApiError> {
    if !state.github_sign_in.borrow().signed_in {
        return Ok(Json(Vec::new()));
    }
    Ok(Json(state.git_tools.github_repositories().await?))
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
    state.events.publish(Topic::Git);
    Ok(Json(state.git_tools.commit_identity().await?))
}

impl AppState {
    async fn git_status(&self) -> GitStatus {
        if self.finish_github_login().await.is_none() {
            self.note_github_sign_in(self.git_tools.github_sign_in().await);
        }
        self.known_git_status().await
    }

    /// The GitHub sign-in as the last check found it, without asking GitHub again or finishing a
    /// sign-in. While a sign-in runs, GitHub's part is only its prompt.
    pub(crate) async fn known_git_status(&self) -> GitStatus {
        let identity = match self.git_tools.commit_identity().await {
            Ok(identity) => identity,
            Err(error) => {
                tracing::warn!("could not read the commit identity: {error}");
                CommitIdentity::default()
            }
        };
        let login_prompt = self
            .github_login
            .lock()
            .await
            .as_ref()
            .filter(|login| login.outcome().is_none())
            .and_then(|login| login.prompt().cloned());
        let sign_in = if login_prompt.is_some() {
            GitHubSignIn::default()
        } else {
            self.github_sign_in.borrow().clone()
        };
        GitStatus {
            github: GitHubStatus {
                host: self.git_tools.host().to_string(),
                signed_in: sign_in.signed_in,
                account: sign_in.account,
                failing: sign_in.failing,
                from_environment: self.git_tools.token_from_environment(),
                token_variables: self
                    .git_tools
                    .host()
                    .token_variables()
                    .map(str::to_owned)
                    .to_vec(),
                login_prompt,
            },
            identity,
        }
    }

    /// Returns the prompt of a sign-in still running. One that succeeded is lent to git.
    async fn finish_github_login(&self) -> Option<LoginPrompt> {
        let mut login = self.github_login.lock().await;
        match login.as_ref().map(LoginProcess::outcome) {
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

    /// Makes git ignore Claude's worktrees, and lends git a sign-in made outside the manager, such
    /// as `gh auth login` in a terminal.
    pub async fn prepare_git_at_start(self) {
        if let Err(error) = self.git_tools.ignore_claude_worktrees().await {
            tracing::warn!("could not make git ignore .claude/worktrees: {error}");
        }
        let sign_in = self.git_tools.github_sign_in().await;
        let signed_in = sign_in.signed_in;
        self.note_github_sign_in(sign_in);
        if signed_in {
            self.lend_github_sign_in_to_git().await;
        }
    }

    fn note_github_sign_in(&self, sign_in: GitHubSignIn) {
        let changed = self.github_sign_in.send_if_modified(|current| {
            let changed = *current != sign_in;
            *current = sign_in;
            changed
        });
        if changed {
            self.events.publish(Topic::Git);
        }
    }

    async fn lend_github_sign_in_to_git(&self) {
        if let Err(error) = self.git_tools.lend_github_sign_in_to_git().await {
            tracing::warn!("could not let git use the GitHub sign-in: {error}");
        }
    }

    fn refuse_environment_sign_in(&self) -> Result<(), ApiError> {
        if self.git_tools.token_from_environment() {
            let [first, second] = self.git_tools.host().token_variables();
            Err(ApiError::Conflict(format!(
                "the GitHub sign-in comes from the {first} or {second} variable"
            )))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::super::test_support::{ResponseExt, TestManager};
    use crate::manager::git::GitHubRepository;

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
        assert_eq!(
            manager
                .get("/api/v1/git/github/repositories", None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn signed_out_of_github_there_are_no_repositories() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let repositories: Vec<GitHubRepository> = manager
            .get("/api/v1/git/github/repositories", Some(&cookie))
            .await
            .json()
            .await;
        assert!(repositories.is_empty());
    }
}
