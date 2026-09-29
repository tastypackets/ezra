use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, Session, internal};
use crate::manager::folders::{FolderChoiceError, FolderDeleteError, FolderStatus};
use crate::manager::git::UnsavedWork;
use crate::manager::remote_control::ClaudeOptions;

#[utoipa::path(
    get,
    path = "/api/v1/folders",
    operation_id = "listFolders",
    tag = "folders",
    summary = "List the folders in /home/dev/projects",
    description = "Hidden folders, symbolic links and linked Git worktrees are omitted.",
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
    params(("name" = String, Path, description = "The folder's name in /home/dev/projects")),
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

#[utoipa::path(
    put,
    path = "/api/v1/folders/{name}/claude-options",
    operation_id = "chooseFolderClaudeOptions",
    tag = "folders",
    summary = "Choose a folder's own Claude Code options",
    description = "Restarts the folder's Remote Control server if it is running.",
    params(("name" = String, Path, description = "The folder's name in /home/dev/projects")),
    request_body = ClaudeOptions,
    responses(
        (status = 204, description = "Saved"),
        (status = 400, description = "Claude Code cannot take the options", body = ErrorBody),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 404, description = "No such folder", body = ErrorBody),
        (status = 409, description = "Worktree sessions are offered only in git repositories", body = ErrorBody)
    )
)]
pub async fn choose_claude_options(
    _: Session,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<ClaudeOptions>,
) -> Result<StatusCode, ApiError> {
    let options = body.trimmed();
    if let Some(problem) = options.problem() {
        return Err(ApiError::BadRequest(problem));
    }
    state
        .change_folder_choice(&name, |choice| choice.options = options)
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

#[utoipa::path(
    get,
    path = "/api/v1/folders/{name}/unsaved-work",
    operation_id = "getUnsavedWork",
    tag = "folders",
    summary = "Count the work in a folder's repository that no remote has",
    description = "A folder that is not a repository has none.",
    params(("name" = String, Path, description = "The folder's name in /home/dev/projects")),
    responses(
        (status = 200, description = "Unsaved work", body = UnsavedWork),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 404, description = "No such folder", body = ErrorBody),
        (status = 502, description = "git could not read the repository", body = ErrorBody)
    )
)]
pub async fn unsaved_work(
    _: Session,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<UnsavedWork>, ApiError> {
    state
        .unsaved_work(&name)
        .await?
        .map(Json)
        .ok_or(ApiError::NotFound("no such folder"))
}

#[utoipa::path(
    delete,
    path = "/api/v1/folders/{name}",
    operation_id = "deleteFolder",
    tag = "folders",
    summary = "Delete a folder and everything in it",
    description = "Stops the folder's Remote Control server first.",
    params(("name" = String, Path, description = "The folder's name in /home/dev/projects")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 404, description = "No such folder", body = ErrorBody),
        (status = 409, description = "The folder's Remote Control server did not stop", body = ErrorBody)
    )
)]
pub async fn delete(
    _: Session,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let deleting = state.clone();
    match tokio::spawn(async move { deleting.delete_folder(&name).await })
        .await
        .map_err(internal)?
    {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(FolderDeleteError::NoSuchFolder) => Err(ApiError::NotFound("no such folder")),
        Err(error @ FolderDeleteError::ServerStillRunning) => {
            Err(ApiError::Conflict(error.to_string()))
        }
        Err(error) => Err(internal(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path as FilePath;
    use std::process::Command;

    use super::super::test_support::{ResponseExt, TestManager};
    use super::*;
    use crate::manager::remote_control::SpawnMode;
    use std::collections::BTreeMap;

    use crate::manager::folders::{Folder, GitDetails, ProjectsDirectory};
    use crate::manager::settings::{FolderChoice, SettingsError};
    use crate::process_ext::CommandStatusExt;

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

    fn git(repository: &FilePath, arguments: &[&str]) {
        Command::new("git")
            .current_dir(repository)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(arguments)
            .run_checked()
            .expect("git runs");
    }

    #[tokio::test]
    async fn unsaved_work_is_counted() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let ProjectsDirectory(projects) = &manager.state.projects;
        let origin = projects.join("origin");
        let app = projects.join("app");
        fs::create_dir_all(&origin).expect("folder is created");
        git(&origin, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(origin.join("README.md"), "hello\n").expect("file is written");
        git(&origin, &["add", "README.md"]);
        git(&origin, &["commit", "--quiet", "--message=first"]);
        git(projects, &["clone", "--quiet", "origin", "app"]);
        fs::create_dir_all(projects.join("notes")).expect("folder is created");

        let clean: UnsavedWork = manager
            .get("/api/v1/folders/app/unsaved-work", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(clean, UnsavedWork::default());

        fs::write(app.join("README.md"), "changed\n").expect("file is written");
        fs::write(app.join("new.txt"), "new\n").expect("file is written");
        git(&app, &["stash", "--quiet"]);
        fs::write(app.join("README.md"), "changed again\n").expect("file is written");
        git(&app, &["commit", "--quiet", "--all", "--message=local"]);
        git(
            &app,
            &["commit", "--quiet", "--allow-empty", "--message=local too"],
        );
        fs::write(app.join("README.md"), "changed once more\n").expect("file is written");
        manager
            .state
            .git_tools
            .ignore_claude_worktrees()
            .await
            .expect("worktrees are ignored");
        git(&app, &["notes", "add", "--message=note"]);
        git(
            &app,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree-bridge-1",
                ".claude/worktrees/bridge-1",
            ],
        );
        fs::write(app.join(".claude/worktrees/bridge-1/work.txt"), "work\n")
            .expect("file is written");
        let unsaved: UnsavedWork = manager
            .get("/api/v1/folders/app/unsaved-work", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(
            unsaved,
            UnsavedWork {
                uncommitted_changes: 3,
                unpushed_commits: 2,
                stashes: 1,
            }
        );

        let plain: UnsavedWork = manager
            .get("/api/v1/folders/notes/unsaved-work", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(plain, UnsavedWork::default());
        assert_eq!(
            manager
                .get("/api/v1/folders/gone/unsaved-work", Some(&cookie))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_folder_is_deleted_with_its_choice_and_nothing_outside_it() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let ProjectsDirectory(projects) = &manager.state.projects;
        let outside = manager.state.settings_path.with_file_name("outside");
        fs::create_dir_all(&outside).expect("folder is created");
        fs::write(outside.join("keep.txt"), "keep\n").expect("file is written");
        fs::create_dir_all(projects.join("app/nested")).expect("folder is created");
        std::os::unix::fs::symlink(&outside, projects.join("app/nested/link"))
            .expect("link is created");
        std::os::unix::fs::symlink(&outside, projects.join("linked")).expect("link is created");
        manager
            .state
            .change_folder_choice("app", |choice| choice.serve = true)
            .await
            .expect("the choice is saved");

        for name in ["linked", "..", ".", "gone"] {
            assert_eq!(
                manager
                    .delete(&format!("/api/v1/folders/{name}"), Some(&cookie))
                    .await
                    .status(),
                StatusCode::NOT_FOUND,
                "{name}"
            );
        }
        let deleted = manager.delete("/api/v1/folders/app", Some(&cookie)).await;
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        assert!(!projects.join("app").exists());
        assert!(outside.join("keep.txt").exists());
        assert!(
            manager
                .state
                .settings
                .lock()
                .await
                .agents
                .claude
                .folders
                .is_empty()
        );
    }

    #[tokio::test]
    async fn deleting_needs_a_login() {
        let manager = TestManager::new();
        manager.logged_in().await;
        let ProjectsDirectory(projects) = &manager.state.projects;
        fs::create_dir_all(projects.join("app")).expect("folder is created");
        assert_eq!(
            manager.delete("/api/v1/folders/app", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert!(projects.join("app").exists());
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
                    claude: ClaudeOptions::default(),
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
                    claude: ClaudeOptions::default(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn linked_worktrees_are_not_listed_or_changed_through_folder_routes() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let projects = &manager.state.projects;
        let app = projects.folder("app");
        let feature = projects.folder("feature");
        fs::create_dir_all(&app).expect("repository folder is created");
        fs::create_dir_all(&feature).expect("feature folder is created");
        git(&app, &["init", "--quiet", "--initial-branch=main"]);
        git(
            &app,
            &["commit", "--quiet", "--allow-empty", "--message=first"],
        );
        manager
            .state
            .change_folder_choice("feature", |choice| choice.serve = true)
            .await
            .expect("the old folder choice is saved");
        git(
            &app,
            &["worktree", "add", "--quiet", "--detach", "../feature"],
        );

        let folders: Vec<FolderStatus> = manager
            .get("/api/v1/folders", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].folder.name, "app");
        assert_eq!(
            folders[0]
                .folder
                .git
                .as_ref()
                .expect("app is a repository")
                .worktrees,
            1
        );
        for (route, body) in [
            ("remote-control", r#"{"serve":true}"#),
            ("claude-options", r#"{"spawn":"same-dir"}"#),
        ] {
            assert_eq!(
                manager
                    .put(
                        &format!("/api/v1/folders/feature/{route}"),
                        body,
                        Some(&cookie)
                    )
                    .await
                    .status(),
                StatusCode::NOT_FOUND,
                "{route}"
            );
        }
        assert_eq!(
            manager
                .get("/api/v1/folders/feature/unsaved-work", Some(&cookie))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            manager
                .delete("/api/v1/folders/feature", Some(&cookie))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert!(feature.join(".git").is_file());
        let present = projects.folders().expect("projects are listed");
        manager
            .state
            .record_folder_choices(&present)
            .await
            .expect("folder choices are reconciled");
        assert!(
            !manager
                .state
                .settings
                .lock()
                .await
                .agents
                .claude
                .folders
                .contains_key("feature")
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
                    ..FolderChoice::default()
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
    async fn a_folder_keeps_its_own_claude_options() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let ProjectsDirectory(projects) = &manager.state.projects;
        fs::create_dir_all(projects.join("notes")).expect("folder is created");
        fs::create_dir_all(projects.join("repo/.git")).expect("repository is created");
        let options = r#"{"spawn":"worktree","permission_mode":" plan ","capacity":2}"#;
        assert_eq!(
            manager
                .put("/api/v1/folders/repo/claude-options", options, None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let chosen = manager
            .put(
                "/api/v1/folders/repo/claude-options",
                options,
                Some(&cookie),
            )
            .await;
        assert_eq!(chosen.status(), StatusCode::NO_CONTENT);
        let expected = ClaudeOptions {
            spawn: Some(SpawnMode::Worktree),
            permission_mode: Some("plan".to_owned()),
            capacity: Some(2),
        };
        assert_eq!(manager.choice_of("repo").await.options, expected);
        let folders: Vec<FolderStatus> = manager
            .get("/api/v1/folders", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(
            folders
                .into_iter()
                .map(|folder| folder.claude)
                .collect::<Vec<_>>(),
            [ClaudeOptions::default(), expected]
        );

        let followed = manager
            .put(
                "/api/v1/folders/repo/claude-options",
                r#"{"permission_mode":""}"#,
                Some(&cookie),
            )
            .await;
        assert_eq!(followed.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            manager.choice_of("repo").await.options,
            ClaudeOptions::default()
        );

        for (name, body, status) in [
            ("notes", r#"{"spawn":"worktree"}"#, StatusCode::CONFLICT),
            ("gone", r#"{"spawn":"same-dir"}"#, StatusCode::NOT_FOUND),
            (
                "repo",
                r#"{"spawn":"same-dir","capacity":0}"#,
                StatusCode::BAD_REQUEST,
            ),
            (
                "repo",
                r#"{"spawn":"same-dir","permission_mode":"two words"}"#,
                StatusCode::BAD_REQUEST,
            ),
            (
                "repo",
                r#"{"spawn":"same-dir","permission_mode":"yolo"}"#,
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let refused = manager
                .put(
                    &format!("/api/v1/folders/{name}/claude-options"),
                    body,
                    Some(&cookie),
                )
                .await;
            assert_eq!(refused.status(), status, "{name} {body}");
        }
        assert_eq!(manager.choice_of("notes").await, FolderChoice::default());
    }
}
