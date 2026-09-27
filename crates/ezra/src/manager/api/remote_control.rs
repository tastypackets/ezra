use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderName, StatusCode, header};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, Session, internal};
use crate::manager::codex_remote::{
    CodexPairing, CodexPairingState, ControlError, PairedPhone, PairingError,
};
use crate::manager::remote_control::{RemoteControlOverview, Served};
use crate::manager::supervision::ServerLog;

const CODEX_NOT_RUNNING: &str = "Codex is not running";

impl From<ControlError> for ApiError {
    fn from(error: ControlError) -> Self {
        match error {
            ControlError::Closed => Self::Conflict(CODEX_NOT_RUNNING.to_owned()),
            ControlError::Io(_)
            | ControlError::TimedOut
            | ControlError::ForeignServer(_)
            | ControlError::Codex { .. } => Self::AgentFailed(error.to_string()),
        }
    }
}

impl From<PairingError> for ApiError {
    fn from(error: PairingError) -> Self {
        match error {
            PairingError::NotConnected => Self::Conflict(error.to_string()),
            PairingError::Control(error) => error.into(),
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/remote-control",
    operation_id = "getRemoteControl",
    tag = "remote-control",
    summary = "Get every Remote Control server's state",
    responses(
        (status = 200, description = "Claude Code's servers for /home/dev/projects and its folders, and Codex's server", body = RemoteControlOverview),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn overview(_: Session, State(state): State<AppState>) -> Json<RemoteControlOverview> {
    Json(state.remote_control_overview())
}

impl AppState {
    /// Every server's state, as the API and the first page show it.
    pub fn remote_control_overview(&self) -> RemoteControlOverview {
        self.remote_control
            .overview(&self.projects.0, self.codex_remote.status())
    }
}

/// The end of a Remote Control server's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServerLogTail {
    /// The whole log's path in the container.
    pub path: String,
    /// The last lines, oldest first.
    pub lines: Vec<String>,
}

#[utoipa::path(
    get,
    path = "/api/v1/remote-control/log",
    operation_id = "getRemoteControlLog",
    tag = "remote-control",
    summary = "Get the end of the /home/dev/projects server's log",
    description = "Returns up to the last 200 lines of Claude Code's debug log and the server's output.",
    responses(
        (status = 200, description = "The last lines", body = ServerLogTail),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn projects_log(
    _: Session,
    State(state): State<AppState>,
) -> Result<Json<ServerLogTail>, ApiError> {
    ServerLogTail::read(state.remote_control.log(&Served::Projects))
        .await
        .map(Json)
}

#[utoipa::path(
    get,
    path = "/api/v1/folders/{name}/remote-control/log",
    operation_id = "getFolderRemoteControlLog",
    tag = "folders",
    summary = "Get the end of a folder server's log",
    description = "Returns up to the last 200 lines of Claude Code's debug log and the server's output.",
    params(("name" = String, Path, description = "The folder's name in /home/dev/projects")),
    responses(
        (status = 200, description = "The last lines", body = ServerLogTail),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 404, description = "The folder has no server", body = ErrorBody)
    )
)]
pub async fn folder_log(
    _: Session,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<ServerLogTail>, ApiError> {
    if state
        .remote_control
        .status_of(&state.projects.folder(&name))
        .is_none()
    {
        return Err(ApiError::NotFound("the folder has no server"));
    }
    ServerLogTail::read(state.remote_control.log(&Served::Folder(name)))
        .await
        .map(Json)
}

#[utoipa::path(
    get,
    path = "/api/v1/remote-control/codex/log",
    operation_id = "getCodexRemoteControlLog",
    tag = "remote-control",
    summary = "Get the end of Codex's remote control log",
    description = "Returns up to the last 200 lines Codex printed while serving the ChatGPT app.",
    responses(
        (status = 200, description = "The last lines", body = ServerLogTail),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn codex_log(
    _: Session,
    State(state): State<AppState>,
) -> Result<Json<ServerLogTail>, ApiError> {
    ServerLogTail::read(state.codex_remote.log.clone())
        .await
        .map(Json)
}

#[utoipa::path(
    post,
    path = "/api/v1/remote-control/codex/retry",
    operation_id = "retryCodexRemoteControl",
    tag = "remote-control",
    summary = "Connect Codex to ChatGPT again",
    description = "Clears the problem shown, and turns Codex's connection to ChatGPT back on when the manager turned it off.",
    responses(
        (status = 204, description = "Codex is trying again"),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 409, description = "Codex is not running", body = ErrorBody),
        (status = 502, description = "Codex refused or did not answer", body = ErrorBody)
    )
)]
pub async fn retry_codex(
    _: Session,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let codex_remote = Arc::clone(&state.codex_remote);
    tokio::spawn(async move { codex_remote.retry().await })
        .await
        .map_err(internal)??;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/remote-control/codex/pairing",
    operation_id = "startCodexPairing",
    tag = "remote-control",
    summary = "Get a code to pair a phone with Codex",
    description = "Replaces the earlier code.",
    responses(
        (status = 200, description = "The code", body = CodexPairing),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 409, description = "Codex is not running or not connected to ChatGPT", body = ErrorBody),
        (status = 502, description = "Codex refused or did not answer", body = ErrorBody)
    )
)]
pub async fn start_codex_pairing(
    _: Session,
    State(state): State<AppState>,
) -> Result<Json<CodexPairing>, ApiError> {
    let codex_remote = Arc::clone(&state.codex_remote);
    let pairing = tokio::spawn(async move { codex_remote.start_pairing().await })
        .await
        .map_err(internal)??;
    Ok(Json(pairing))
}

#[utoipa::path(
    get,
    path = "/api/v1/remote-control/codex/pairing",
    operation_id = "getCodexPairing",
    tag = "remote-control",
    summary = "Get the latest pairing code and whether a phone used it",
    responses(
        (status = 200, description = "The latest code", body = CodexPairingState),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn codex_pairing(_: Session, State(state): State<AppState>) -> Json<CodexPairingState> {
    Json(CodexPairingState {
        pairing: state.codex_remote.pairing(),
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/remote-control/codex/pairing/qr.svg",
    operation_id = "getCodexPairingQr",
    tag = "remote-control",
    summary = "Get the latest pairing code's link as a QR code",
    responses(
        (status = 200, description = "An SVG image", content_type = "image/svg+xml", body = String),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 404, description = "No code a phone can still use", body = ErrorBody)
    )
)]
pub async fn codex_pairing_qr(
    _: Session,
    State(state): State<AppState>,
) -> Result<([(HeaderName, &'static str); 1], String), ApiError> {
    let svg = state
        .codex_remote
        .open_pairing_qr()
        .ok_or(ApiError::NotFound("no pairing code a phone can still use"))?
        .map_err(internal)?;
    Ok(([(header::CONTENT_TYPE, "image/svg+xml")], svg))
}

#[utoipa::path(
    get,
    path = "/api/v1/remote-control/codex/phones",
    operation_id = "listCodexPhones",
    tag = "remote-control",
    summary = "List the phones paired with Codex",
    responses(
        (status = 200, description = "Paired phones", body = Vec<PairedPhone>),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 409, description = "Codex is not running or not connected to ChatGPT", body = ErrorBody),
        (status = 502, description = "Codex refused or did not answer", body = ErrorBody)
    )
)]
pub async fn codex_phones(
    _: Session,
    State(state): State<AppState>,
) -> Result<Json<Vec<PairedPhone>>, ApiError> {
    Ok(Json(state.codex_remote.phones().await?))
}

#[utoipa::path(
    delete,
    path = "/api/v1/remote-control/codex/phones/{id}",
    operation_id = "removeCodexPhone",
    tag = "remote-control",
    summary = "Remove a paired phone",
    description = "The phone can no longer reach this box.",
    params(("id" = String, Path, description = "The phone's id")),
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 409, description = "Codex is not running or not connected to ChatGPT", body = ErrorBody),
        (status = 502, description = "Codex refused or did not answer", body = ErrorBody)
    )
)]
pub async fn remove_codex_phone(
    _: Session,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let codex_remote = Arc::clone(&state.codex_remote);
    tokio::spawn(async move { codex_remote.remove_phone(id).await })
        .await
        .map_err(internal)??;
    Ok(StatusCode::NO_CONTENT)
}

impl ServerLogTail {
    async fn read(log: ServerLog) -> Result<Self, ApiError> {
        let path = log.debug_file().display().to_string();
        let lines = tokio::task::spawn_blocking(move || log.tail())
            .await
            .map_err(internal)?
            .map_err(internal)?;
        Ok(Self { path, lines })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use axum::http::StatusCode;
    use tokio::time::sleep;

    use super::super::test_support::{ResponseExt, TestManager};
    use super::*;
    use crate::manager::remote_control::ServerState;

    #[tokio::test]
    async fn every_server_is_listed_once_logged_in() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        assert_eq!(
            manager.get("/api/v1/remote-control", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let overview: RemoteControlOverview = manager
            .get("/api/v1/remote-control", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(overview.projects.state, ServerState::Waiting);
        assert!(overview.folders.is_empty());
        assert_eq!(overview.codex.state, ServerState::Waiting);
    }

    #[tokio::test]
    async fn the_codex_log_has_the_lines_codex_printed() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        assert_eq!(
            manager
                .get("/api/v1/remote-control/codex/log", None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let empty: ServerLogTail = manager
            .get("/api/v1/remote-control/codex/log", Some(&cookie))
            .await
            .json()
            .await;
        assert!(empty.lines.is_empty());
        assert!(
            empty
                .path
                .ends_with("/ezra/remote-control/codex/server.log"),
            "{}",
            empty.path
        );

        let log = &manager.state.codex_remote.log;
        fs::create_dir_all(&log.0).expect("the log folder is created");
        fs::write(log.debug_file(), "one\ntwo\n").expect("the log is written");
        let tail: ServerLogTail = manager
            .get("/api/v1/remote-control/codex/log", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(tail.lines, ["one", "two"]);
    }

    #[tokio::test]
    async fn the_projects_log_is_empty_before_the_server_ever_ran() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        assert_eq!(
            manager
                .get("/api/v1/remote-control/log", None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let tail: ServerLogTail = manager
            .get("/api/v1/remote-control/log", Some(&cookie))
            .await
            .json()
            .await;
        assert!(tail.lines.is_empty());
        assert!(
            tail.path
                .ends_with("/ezra/remote-control/projects/server.log"),
            "{}",
            tail.path
        );
    }

    #[tokio::test]
    async fn a_folder_log_needs_a_folder_server() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        for name in ["app", "..", "%2E%2E"] {
            assert_eq!(
                manager
                    .get(
                        &format!("/api/v1/folders/{name}/remote-control/log"),
                        Some(&cookie)
                    )
                    .await
                    .status(),
                StatusCode::NOT_FOUND,
                "{name}"
            );
        }

        fs::create_dir_all(manager.state.projects.folder("app")).expect("folder is created");
        manager
            .state
            .change_folder_choice("app", |choice| choice.serve = true)
            .await
            .expect("the choice is saved");
        let supervisor = tokio::spawn(manager.state.clone().supervise_remote_control());
        let app = manager.state.projects.folder("app");
        while manager.state.remote_control.status_of(&app).is_none() {
            sleep(Duration::from_millis(20)).await;
        }
        let tail: ServerLogTail = manager
            .get("/api/v1/folders/app/remote-control/log", Some(&cookie))
            .await
            .json()
            .await;
        assert!(
            tail.path
                .ends_with("/remote-control/folders/app/server.log"),
            "{}",
            tail.path
        );

        manager.state.remote_control.supervision.begin_shut_down();
        supervisor.await.expect("supervisor stops");
    }
}
