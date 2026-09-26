use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, Session, internal};
use crate::manager::folders::{FolderChoiceError, FolderStatus};

#[utoipa::path(
    get,
    path = "/api/v1/folders",
    operation_id = "listFolders",
    tag = "folders",
    summary = "List the folders in /projects",
    responses(
        (status = 200, description = "Folders sorted by name", body = Vec<FolderStatus>),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn list(
    _: Session,
    State(state): State<AppState>,
) -> Result<Json<Vec<FolderStatus>>, ApiError> {
    state.folder_statuses().await.map(Json).map_err(internal)
}

/// Whether the Claude app lists a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServeBody {
    pub serve: bool,
}

#[utoipa::path(
    put,
    path = "/api/v1/folders/{name}/remote-control",
    operation_id = "chooseToServeFolder",
    tag = "folders",
    summary = "Choose whether the Claude app lists a folder",
    description = "Starts or stops the folder's Remote Control server.",
    params(("name" = String, Path, description = "The folder's name in /projects")),
    request_body = ServeBody,
    responses(
        (status = 204, description = "Saved"),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 404, description = "No such folder", body = ErrorBody)
    )
)]
pub async fn choose_to_serve(
    _: Session,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<ServeBody>,
) -> Result<StatusCode, ApiError> {
    match state.choose_to_serve_folder(&name, body.serve).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(FolderChoiceError::NoSuchFolder) => Err(ApiError::NotFound("no such folder")),
        Err(error) => Err(internal(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::test_support::{ResponseExt, TestManager};
    use super::*;
    use crate::manager::folders::{Folder, GitDetails, ProjectsDirectory};

    #[tokio::test]
    async fn folders_need_a_login() {
        let manager = TestManager::new();
        manager.logged_in().await;
        assert_eq!(
            manager.get("/api/v1/folders", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn folders_are_listed_and_a_missing_directory_has_none() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let none: Vec<FolderStatus> = manager
            .get("/api/v1/folders", Some(&cookie))
            .await
            .json()
            .await;
        assert!(none.is_empty());

        let ProjectsDirectory(projects) = &manager.state.projects;
        fs::create_dir_all(projects.join("app")).expect("folder is created");
        fs::create_dir_all(projects.join("repo/.git")).expect("repository is created");
        fs::write(projects.join("repo/.git/HEAD"), "ref: refs/heads/main\n")
            .expect("HEAD is written");
        let folders: Vec<FolderStatus> = manager
            .get("/api/v1/folders", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(
            folders,
            [
                FolderStatus {
                    folder: Folder {
                        name: "app".to_owned(),
                        git: None,
                    },
                    serve: false,
                    remote_control: None,
                },
                FolderStatus {
                    folder: Folder {
                        name: "repo".to_owned(),
                        git: Some(GitDetails {
                            branch: Some("main".to_owned()),
                            repository: None,
                        }),
                    },
                    serve: true,
                    remote_control: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_folder_can_be_served_and_unknown_folders_cannot() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let ProjectsDirectory(projects) = &manager.state.projects;
        fs::create_dir_all(projects.join("app")).expect("folder is created");

        let chosen = manager
            .put(
                "/api/v1/folders/app/remote-control",
                r#"{"serve":true}"#,
                Some(&cookie),
            )
            .await;
        assert_eq!(chosen.status(), StatusCode::NO_CONTENT);
        let app = manager.state.projects.find("app").expect("folder is found");
        assert!(manager.state.serves_folder(&app).await);

        let unknown = manager
            .put(
                "/api/v1/folders/gone/remote-control",
                r#"{"serve":true}"#,
                Some(&cookie),
            )
            .await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }
}
