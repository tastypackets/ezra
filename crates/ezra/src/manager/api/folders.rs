use axum::Json;
use axum::extract::State;

use super::{ApiError, AppState, ErrorBody, Session, internal};
use crate::manager::folders::Folder;

#[utoipa::path(
    get,
    path = "/api/v1/folders",
    operation_id = "listFolders",
    tag = "folders",
    summary = "List the folders in /projects",
    responses(
        (status = 200, description = "Folders sorted by name", body = Vec<Folder>),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn list(
    _: Session,
    State(state): State<AppState>,
) -> Result<Json<Vec<Folder>>, ApiError> {
    state.folders().await.map(Json).map_err(internal)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use axum::http::StatusCode;

    use super::super::test_support::{ResponseExt, TestManager};
    use super::*;
    use crate::manager::folders::ProjectsDirectory;

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
        let none: Vec<Folder> = manager
            .get("/api/v1/folders", Some(&cookie))
            .await
            .json()
            .await;
        assert!(none.is_empty());

        let ProjectsDirectory(projects) = &manager.state.projects;
        fs::create_dir_all(projects.join("app")).expect("folder is created");
        let folders: Vec<Folder> = manager
            .get("/api/v1/folders", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(
            folders,
            [Folder {
                name: "app".to_owned(),
                git: None,
            }]
        );
    }
}
