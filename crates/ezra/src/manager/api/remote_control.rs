use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, Session, internal};
use crate::manager::remote_control::{RemoteControlOverview, Served};
use crate::manager::supervision::ServerLog;

#[utoipa::path(
    get,
    path = "/api/v1/remote-control",
    operation_id = "getRemoteControl",
    tag = "remote-control",
    summary = "Get every Remote Control server's state",
    responses(
        (status = 200, description = "The servers for /projects and its folders", body = RemoteControlOverview),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn overview(_: Session, State(state): State<AppState>) -> Json<RemoteControlOverview> {
    Json(state.remote_control.overview(&state.projects.0))
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
    summary = "Get the end of the /projects server's log",
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
    params(("name" = String, Path, description = "The folder's name in /projects")),
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
