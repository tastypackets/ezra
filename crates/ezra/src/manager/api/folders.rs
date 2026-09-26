use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, Session, internal};
use crate::manager::folders::{FolderChoiceError, FolderStatus};
use crate::manager::remote_control::SpawnMode;

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
    state
        .change_folder_choice(&name, |choice| choice.serve = body.serve)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Where a folder's sessions work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SpawnModeBody {
    pub spawn: SpawnMode,
}

#[utoipa::path(
    put,
    path = "/api/v1/folders/{name}/spawn-mode",
    operation_id = "chooseFolderSpawnMode",
    tag = "folders",
    summary = "Choose where a folder's sessions work",
    description = "Restarts the folder's Remote Control server if it is running.",
    params(("name" = String, Path, description = "The folder's name in /projects")),
    request_body = SpawnModeBody,
    responses(
        (status = 204, description = "Saved"),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 404, description = "No such folder", body = ErrorBody),
        (status = 409, description = "Worktrees need a git repository", body = ErrorBody)
    )
)]
pub async fn choose_spawn_mode(
    _: Session,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<SpawnModeBody>,
) -> Result<StatusCode, ApiError> {
    state
        .change_folder_choice(&name, |choice| choice.spawn = body.spawn)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

impl From<FolderChoiceError> for ApiError {
    fn from(error: FolderChoiceError) -> Self {
        match error {
            FolderChoiceError::NoSuchFolder => Self::NotFound("no such folder"),
            FolderChoiceError::NotARepository => Self::Conflict(error.to_string()),
            FolderChoiceError::Scan(_) | FolderChoiceError::Settings(_) => internal(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::test_support::{ResponseExt, TestManager};
    use super::*;
    use std::collections::BTreeMap;

    use crate::manager::folders::{Folder, GitDetails, ProjectsDirectory};
    use crate::manager::settings::{FolderChoice, SettingsError};

    impl TestManager {
        async fn choice_of(&self, name: &str) -> FolderChoice {
            let folder = self.state.projects.find(name).expect("folder is found");
            self.state
                .settings
                .lock()
                .await
                .agents
                .claude
                .folder_choice(&folder)
        }
    }

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
                    spawn: SpawnMode::SameDir,
                },
                FolderStatus {
                    folder: Folder {
                        name: "repo".to_owned(),
                        git: Some(GitDetails {
                            branch: Some("main".to_owned()),
                            repository: None,
                            worktrees: 0,
                        }),
                    },
                    serve: true,
                    spawn: SpawnMode::SameDir,
                },
            ]
        );
    }

    #[tokio::test]
    async fn new_repositories_get_the_default_and_keep_it() {
        let manager = TestManager::new();
        let ProjectsDirectory(projects) = &manager.state.projects;
        fs::create_dir_all(projects.join("notes")).expect("folder is created");
        fs::create_dir_all(projects.join("repo/.git")).expect("repository is created");
        manager
            .state
            .update_settings(|settings| {
                settings
                    .agents
                    .claude
                    .folders
                    .insert("gone".to_owned(), FolderChoice::default());
                Ok::<(), SettingsError>(())
            })
            .await
            .expect("settings save");
        let folders = manager.state.folders().await.expect("folders are listed");
        assert!(
            manager
                .state
                .record_folder_choices(&folders)
                .await
                .expect("recorded")
        );
        assert_eq!(
            manager.state.settings.lock().await.agents.claude.folders,
            BTreeMap::from([(
                "repo".to_owned(),
                FolderChoice {
                    serve: true,
                    spawn: SpawnMode::SameDir
                }
            )])
        );

        manager
            .state
            .update_settings(|settings| {
                settings.agents.claude.remote_control.serve_repositories = false;
                Ok::<(), SettingsError>(())
            })
            .await
            .expect("settings save");
        assert!(
            !manager
                .state
                .record_folder_choices(&folders)
                .await
                .expect("recorded")
        );
        assert!(manager.choice_of("repo").await.serve);
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
        assert!(manager.choice_of("app").await.serve);

        let unknown = manager
            .put(
                "/api/v1/folders/gone/remote-control",
                r#"{"serve":true}"#,
                Some(&cookie),
            )
            .await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn only_repositories_can_use_worktrees() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let ProjectsDirectory(projects) = &manager.state.projects;
        fs::create_dir_all(projects.join("notes")).expect("folder is created");
        fs::create_dir_all(projects.join("repo/.git")).expect("repository is created");
        let worktree = r#"{"spawn":"worktree"}"#;
        assert_eq!(
            manager
                .put("/api/v1/folders/repo/spawn-mode", worktree, None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let chosen = manager
            .put("/api/v1/folders/repo/spawn-mode", worktree, Some(&cookie))
            .await;
        assert_eq!(chosen.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            manager.choice_of("repo").await,
            FolderChoice {
                serve: true,
                spawn: SpawnMode::Worktree
            }
        );
        let folders: Vec<FolderStatus> = manager
            .get("/api/v1/folders", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(
            folders
                .iter()
                .map(|folder| folder.spawn)
                .collect::<Vec<_>>(),
            [SpawnMode::SameDir, SpawnMode::Worktree]
        );

        for (name, status) in [
            ("notes", StatusCode::CONFLICT),
            ("gone", StatusCode::NOT_FOUND),
        ] {
            let refused = manager
                .put(
                    &format!("/api/v1/folders/{name}/spawn-mode"),
                    worktree,
                    Some(&cookie),
                )
                .await;
            assert_eq!(refused.status(), status, "{name}");
        }
        assert_eq!(manager.choice_of("notes").await, FolderChoice::default());
    }
}
