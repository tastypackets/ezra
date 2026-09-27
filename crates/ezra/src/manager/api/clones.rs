use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use super::{ApiError, AppState, ErrorBody, Session};
use crate::manager::clones::{CloneError, CloneRequest, CloneStatus};

impl From<CloneError> for ApiError {
    fn from(error: CloneError) -> Self {
        match error {
            CloneError::NoRepository => Self::BadRequest("enter a repository"),
            CloneError::InvalidName => Self::BadRequest(
                "the folder name must not be empty, start with a dot or contain a slash",
            ),
            CloneError::Exists(name) => Self::Conflict(format!("~/projects/{name} already exists")),
            CloneError::Running(name) => Self::Conflict(format!("{name} is already being cloned")),
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/clones",
    operation_id = "listClones",
    tag = "folders",
    summary = "List the clones that are running or failed",
    responses(
        (status = 200, description = "Clones sorted by folder name", body = Vec<CloneStatus>),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn list(_: Session, State(state): State<AppState>) -> Json<Vec<CloneStatus>> {
    Json(state.clones.statuses())
}

#[utoipa::path(
    post,
    path = "/api/v1/clones",
    operation_id = "cloneRepository",
    tag = "folders",
    summary = "Clone a repository into a new folder in /home/dev/projects",
    description = "Starts the clone and returns at once.",
    request_body = CloneRequest,
    responses(
        (status = 202, description = "Cloning", body = CloneStatus),
        (status = 400, description = "No repository, or the folder name is not one folder", body = ErrorBody),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 409, description = "The folder exists or is being cloned into", body = ErrorBody)
    )
)]
pub async fn start(
    _: Session,
    State(state): State<AppState>,
    Json(request): Json<CloneRequest>,
) -> Result<(StatusCode, Json<CloneStatus>), ApiError> {
    Ok((StatusCode::ACCEPTED, Json(state.start_clone(request)?)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/clones/{name}",
    operation_id = "stopClone",
    tag = "folders",
    summary = "Stop a clone or clear its failure",
    description = "Stops a running clone and removes what it cloned, or clears a failed one.",
    params(("name" = String, Path, description = "The folder being cloned into")),
    responses(
        (status = 204, description = "Stopping or cleared"),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 404, description = "No such clone", body = ErrorBody)
    )
)]
pub async fn stop(
    _: Session,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    if state.clones.stop(&name) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("no such clone"))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path as FilePath;
    use std::process::Command;
    use std::time::Duration;

    use tokio::net::TcpListener;
    use tokio::time::{Instant, sleep};

    use super::super::test_support::{ResponseExt, TestManager};
    use super::*;
    use crate::manager::folders::{FolderStatus, ProjectsDirectory};
    use crate::process_ext::CommandStatusExt;

    fn repository_with_a_commit(directory: &FilePath) -> String {
        let repository = directory.join("source");
        Command::new("git")
            .args(["init", "--quiet", "--initial-branch=main"])
            .arg(&repository)
            .run_checked()
            .expect("repository is created");
        fs::write(repository.join("README.md"), "hello\n").expect("file is written");
        Command::new("git")
            .current_dir(&repository)
            .args(["add", "README.md"])
            .run_checked()
            .expect("file is added");
        Command::new("git")
            .current_dir(&repository)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--quiet",
                "--message=first",
            ])
            .run_checked()
            .expect("commit is made");
        format!("file://{}", repository.display())
    }

    async fn wait_for_clones(manager: &TestManager, cookie: &str) -> Vec<CloneStatus> {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(20))
            .expect("the deadline fits");
        loop {
            let clones: Vec<CloneStatus> = manager
                .get("/api/v1/clones", Some(cookie))
                .await
                .json()
                .await;
            if clones.iter().all(|clone| clone.error.is_some()) {
                return clones;
            }
            assert!(Instant::now() < deadline, "still cloning: {clones:?}");
            sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn clones_need_a_login() {
        let manager = TestManager::new();
        manager.logged_in().await;
        assert_eq!(
            manager.get("/api/v1/clones", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            manager
                .post("/api/v1/clones", r#"{"repository":"a/b","name":"b"}"#, None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn a_repository_is_cloned_with_its_choice() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let source = tempfile::tempdir().expect("temporary directory");
        let url = repository_with_a_commit(source.path());
        let ProjectsDirectory(projects) = &manager.state.projects;
        fs::create_dir_all(projects).expect("projects directory is created");

        let started = manager
            .post(
                "/api/v1/clones",
                &format!(r#"{{"repository":"{url}","name":"app","serve":false}}"#),
                Some(&cookie),
            )
            .await;
        assert_eq!(started.status(), StatusCode::ACCEPTED);
        let status: CloneStatus = started.json().await;
        assert_eq!((status.name.as_str(), status.percent), ("app", 0));

        assert_eq!(wait_for_clones(&manager, &cookie).await, []);
        assert_eq!(
            fs::read_to_string(projects.join("app/README.md")).expect("the clone has the file"),
            "hello\n"
        );
        let folders: Vec<FolderStatus> = manager
            .get("/api/v1/folders", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(folders.len(), 1);
        assert_eq!(
            folders
                .first()
                .map(|folder| (folder.folder.name.as_str(), folder.serve)),
            Some(("app", false))
        );

        let again = manager
            .post(
                "/api/v1/clones",
                &format!(r#"{{"repository":"{url}","name":"app"}}"#),
                Some(&cookie),
            )
            .await;
        assert_eq!(again.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn a_failed_clone_says_why_until_cleared() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let ProjectsDirectory(projects) = &manager.state.projects;
        fs::create_dir_all(projects).expect("projects directory is created");
        let missing = projects.join("missing-source");

        let started = manager
            .post(
                "/api/v1/clones",
                &format!(r#"{{"repository":"{}","name":"app"}}"#, missing.display()),
                Some(&cookie),
            )
            .await;
        assert_eq!(started.status(), StatusCode::ACCEPTED);
        let clones = wait_for_clones(&manager, &cookie).await;
        let error = clones
            .first()
            .and_then(|clone| clone.error.clone())
            .expect("the failure is kept");
        assert!(error.contains("does not exist"), "{error}");
        assert!(!error.contains("Cloning into"), "{error}");
        assert!(!projects.join("app").exists());
        assert!(!projects.join(".ezra-clone-app").exists());

        let cleared = manager.delete("/api/v1/clones/app", Some(&cookie)).await;
        assert_eq!(cleared.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            manager
                .delete("/api/v1/clones/app", Some(&cookie))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert!(manager.state.clones.statuses().is_empty());
    }

    #[tokio::test]
    async fn a_running_clone_can_be_stopped() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let ProjectsDirectory(projects) = &manager.state.projects;
        fs::create_dir_all(projects).expect("projects directory is created");
        let silent = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener binds");
        let address = silent.local_addr().expect("listener has an address");
        let staging = projects.join(".ezra-clone-app");

        let started = manager
            .post(
                "/api/v1/clones",
                &format!(r#"{{"repository":"http://{address}/app.git","name":"app"}}"#),
                Some(&cookie),
            )
            .await;
        assert_eq!(started.status(), StatusCode::ACCEPTED);
        let (_connection, _) = silent.accept().await.expect("git connects");
        assert_eq!(
            manager
                .post(
                    "/api/v1/clones",
                    &format!(r#"{{"repository":"http://{address}/app.git","name":"app"}}"#),
                    Some(&cookie),
                )
                .await
                .status(),
            StatusCode::CONFLICT
        );

        let stopped = manager.delete("/api/v1/clones/app", Some(&cookie)).await;
        assert_eq!(stopped.status(), StatusCode::NO_CONTENT);
        assert_eq!(wait_for_clones(&manager, &cookie).await, []);
        assert!(!staging.exists());
        assert!(!projects.join("app").exists());
    }

    #[tokio::test]
    async fn names_that_are_not_one_folder_are_refused() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        for name in ["", ".hidden", "..", "a/b", "../escape"] {
            let response = manager
                .post(
                    "/api/v1/clones",
                    &format!(r#"{{"repository":"zeke/app","name":"{name}"}}"#),
                    Some(&cookie),
                )
                .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
        }
        let empty = manager
            .post(
                "/api/v1/clones",
                r#"{"repository":"  ","name":"app"}"#,
                Some(&cookie),
            )
            .await;
        assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
        assert!(manager.state.clones.statuses().is_empty());
    }
}
