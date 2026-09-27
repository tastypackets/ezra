use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, Session, internal};
use crate::manager::agents::Agent;
use crate::manager::events::Topic;
use crate::manager::settings_file::{
    MAX_TEXT_BYTES, ParseProblem, SettingsFileError, SettingsFileText,
};

/// The largest request body.
pub const MAX_BODY_BYTES: usize = MAX_TEXT_BYTES * 2 + 4096;
const NO_SETTINGS_FILE: &str = "the agent's config directory is not set";

/// Text to save over the version it was edited from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SettingsFileSaveBody {
    pub text: String,
    /// The version of the file the text was edited from.
    pub version: String,
}

impl From<SettingsFileError> for ApiError {
    fn from(error: SettingsFileError) -> Self {
        match error {
            SettingsFileError::Invalid(problem) => Self::Invalid(problem),
            SettingsFileError::TooLarge => Self::TooLarge(error.to_string()),
            SettingsFileError::Changed(_) => Self::Conflict(error.to_string()),
            SettingsFileError::NotText(_)
            | SettingsFileError::NotRegular(_)
            | SettingsFileError::FileTooLarge(_) => Self::Unprocessable(error.to_string()),
            SettingsFileError::Io { .. } => Self::Internal(error.to_string()),
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/agents/{agent}/settings-file",
    operation_id = "getSettingsFile",
    tag = "agents",
    summary = "Get an agent's settings file",
    description = "Returns the agent's own settings file as text, empty when it does not exist.",
    params(("agent" = Agent, Path, description = "The agent")),
    responses(
        (status = 200, description = "The file", body = SettingsFileText),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 404, description = "The agent's config directory is not set", body = ErrorBody),
        (status = 422, description = "The file is not UTF-8 text, not a regular file or larger than 2 MiB", body = ErrorBody)
    )
)]
pub async fn read(
    _: Session,
    State(state): State<AppState>,
    Path(agent): Path<Agent>,
) -> Result<Json<SettingsFileText>, ApiError> {
    let file = state
        .install_paths
        .settings_file(agent)
        .ok_or(ApiError::NotFound(NO_SETTINGS_FILE))?;
    Ok(Json(
        tokio::task::spawn_blocking(move || file.read())
            .await
            .map_err(internal)??,
    ))
}

#[utoipa::path(
    put,
    path = "/api/v1/agents/{agent}/settings-file",
    operation_id = "updateSettingsFile",
    tag = "agents",
    summary = "Save an agent's settings file",
    description = "Writes the text byte for byte when it parses and the file is still at `version`.",
    params(("agent" = Agent, Path, description = "The agent")),
    request_body = SettingsFileSaveBody,
    responses(
        (status = 200, description = "Saved", body = SettingsFileText),
        (status = 400, description = "The text does not parse", body = ParseProblem),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 404, description = "The agent's config directory is not set", body = ErrorBody),
        (status = 409, description = "The file changed since that version", body = ErrorBody),
        (status = 413, description = "The text is larger than 2 MiB", body = ErrorBody),
        (status = 422, description = "The file is not a regular file or larger than 2 MiB", body = ErrorBody)
    )
)]
pub async fn save(
    _: Session,
    State(state): State<AppState>,
    Path(agent): Path<Agent>,
    body: Result<Json<SettingsFileSaveBody>, JsonRejection>,
) -> Result<Json<SettingsFileText>, ApiError> {
    let Json(body) = body.map_err(|rejection| match rejection.status() {
        StatusCode::PAYLOAD_TOO_LARGE => SettingsFileError::TooLarge.into(),
        _ => ApiError::Rejected(rejection),
    })?;
    let file = state
        .install_paths
        .settings_file(agent)
        .ok_or(ApiError::NotFound(NO_SETTINGS_FILE))?;
    let events = state.events.clone();
    Ok(Json(
        tokio::task::spawn_blocking(move || {
            let saved = file.save(body.text, &body.version)?;
            events.publish(Topic::SettingsFile);
            Ok::<_, SettingsFileError>(saved)
        })
        .await
        .map_err(internal)??,
    ))
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use futures_util::{Stream, StreamExt};
    use nix::sys::stat::Mode;
    use nix::unistd::mkfifo;
    use serde_json::{Value, json};

    use super::super::test_support::{ResponseExt, TestManager};
    use super::*;
    use crate::manager::events::ManagerEvent;
    use crate::manager::settings_file::{SettingsFileFormat, SettingsFileWatcher};

    const CLAUDE: &str = "/api/v1/agents/claude/settings-file";
    const CODEX: &str = "/api/v1/agents/codex/settings-file";

    fn body(text: &str, version: &str) -> String {
        json!({ "text": text, "version": version }).to_string()
    }

    fn path_of(manager: &TestManager, agent: Agent) -> PathBuf {
        manager
            .state
            .install_paths
            .settings_file(agent)
            .expect("the config directory is known")
            .path
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().expect("the path has a parent"))
            .expect("the directory is created");
        fs::write(path, bytes).expect("the file is written");
    }

    async fn published(events: &mut (impl Stream<Item = ManagerEvent> + Unpin), what: &str) {
        let wait = async {
            while let Some(event) = events.next().await {
                if let ManagerEvent::Changed {
                    topic: Topic::SettingsFile,
                    ..
                } = event
                {
                    return;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(4), wait)
            .await
            .unwrap_or_else(|_| panic!("{what} was not published"));
    }

    #[tokio::test]
    async fn settings_files_need_a_login() {
        let manager = TestManager::new();
        manager.logged_in().await;
        assert_eq!(
            manager.get(CLAUDE, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            manager.put(CODEX, &body("", ""), None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn the_exact_text_is_saved_and_published() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let missing: SettingsFileText = manager.get(CLAUDE, Some(&cookie)).await.json().await;
        assert_eq!(missing.format, SettingsFileFormat::Json);
        assert_eq!(missing.text, "");
        assert!(missing.path.ends_with("config/claude/settings.json"));

        let mut events = Box::pin(manager.state.events.stream());
        events.next().await;
        let text = "\u{feff}{\r\n\t\"permissions\" :  {\"allow\": []},\r\n  \"unknown\": 1\r\n}";
        let response = manager
            .put(CLAUDE, &body(text, &missing.version), Some(&cookie))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let saved: SettingsFileText = response.json().await;
        assert_eq!(saved.text, text);
        assert_ne!(saved.version, missing.version);
        assert_eq!(
            fs::read(path_of(&manager, Agent::Claude)).expect("the file is read"),
            text.as_bytes()
        );
        published(&mut events, "the save").await;
        let read: SettingsFileText = manager.get(CLAUDE, Some(&cookie)).await.json().await;
        assert_eq!(read, saved);

        let codex: SettingsFileText = manager.get(CODEX, Some(&cookie)).await.json().await;
        assert_eq!(codex.format, SettingsFileFormat::Toml);
        assert!(codex.path.ends_with("config/codex/config.toml"));
        let text = "model = \"gpt\"\r\n[tui]\r\nstatus = {\r\n  a = \"\\e\",\r\n}\r\n\r\n";
        let response = manager
            .put(CODEX, &body(text, &codex.version), Some(&cookie))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            fs::read(path_of(&manager, Agent::Codex)).expect("the file is read"),
            text.as_bytes()
        );
    }

    #[tokio::test]
    async fn text_that_does_not_parse_gets_its_line_and_column() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let empty: SettingsFileText = manager.get(CLAUDE, Some(&cookie)).await.json().await;
        for (path, text, expected) in [
            (CLAUDE, "{\"a\":1,}", ("trailing comma", 1, 8)),
            (
                CLAUDE,
                "{\n  \"\u{e9}\": 1\n  // no\n}",
                ("expected `,` or `}`", 3, 3),
            ),
            (CODEX, "a = 1\n[b]\na = 2\n[b]\n", ("duplicate key", 4, 2)),
        ] {
            let response = manager
                .put(path, &body(text, &empty.version), Some(&cookie))
                .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{text:?}");
            let problem: ParseProblem = response.json().await;
            assert_eq!(
                (problem.error.as_str(), problem.line, problem.column),
                expected
            );
        }
        assert!(!path_of(&manager, Agent::Claude).exists());
        assert!(!path_of(&manager, Agent::Codex).exists());
    }

    #[tokio::test]
    async fn a_stale_version_is_a_conflict() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let claude = path_of(&manager, Agent::Claude);
        let opened: SettingsFileText = manager.get(CLAUDE, Some(&cookie)).await.json().await;
        write(&claude, b"{\"created\":true}");
        let conflict = manager
            .put(CLAUDE, &body("{}", &opened.version), Some(&cookie))
            .await;
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        let error: Value = conflict.json().await;
        assert!(
            error["error"]
                .as_str()
                .is_some_and(|message| message.ends_with("changed since it was opened")),
            "{error}"
        );

        let opened: SettingsFileText = manager.get(CLAUDE, Some(&cookie)).await.json().await;
        fs::remove_file(&claude).expect("the file is removed");
        assert_eq!(
            manager
                .put(CLAUDE, &body("{}", &opened.version), Some(&cookie))
                .await
                .status(),
            StatusCode::CONFLICT
        );
        assert!(!claude.exists());
    }

    #[tokio::test]
    async fn text_over_2_mib_is_too_large() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let empty: SettingsFileText = manager.get(CLAUDE, Some(&cookie)).await.json().await;
        let escaped = format!("{{}}{}", "\n".repeat(MAX_TEXT_BYTES.saturating_sub(2)));
        let response = manager
            .put(CLAUDE, &body(&escaped, &empty.version), Some(&cookie))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let saved: SettingsFileText = response.json().await;

        let over = format!("{escaped} ");
        let response = manager
            .put(CLAUDE, &body(&over, &saved.version), Some(&cookie))
            .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let error: Value = response.json().await;
        assert_eq!(error["error"], "the text is larger than 2 MiB");

        let huge = "\n".repeat(MAX_BODY_BYTES);
        let response = manager
            .put(CLAUDE, &body(&huge, &saved.version), Some(&cookie))
            .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let error: Value = response.json().await;
        assert_eq!(error["error"], "the text is larger than 2 MiB");

        let response = manager.put(CLAUDE, "{\"text\":", Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_file_that_is_not_utf8_cannot_be_opened() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        write(&path_of(&manager, Agent::Claude), b"\xff\xfe{\x00}\x00");
        let response = manager.get(CLAUDE, Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let error: Value = response.json().await;
        assert!(
            error["error"]
                .as_str()
                .is_some_and(|message| message.ends_with("settings.json is not UTF-8 text")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_file_that_is_not_regular_or_too_large_cannot_be_opened_or_saved() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let claude = path_of(&manager, Agent::Claude);
        fs::create_dir_all(claude.parent().expect("the path has a parent"))
            .expect("the directory is created");
        mkfifo(&claude, Mode::S_IRUSR | Mode::S_IWUSR).expect("the pipe is created");
        let response = manager.get(CLAUDE, Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let error: Value = response.json().await;
        assert!(
            error["error"]
                .as_str()
                .is_some_and(|message| message.ends_with("settings.json is not a regular file")),
            "{error}"
        );
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(
            manager
                .put(CLAUDE, &body("{}", empty), Some(&cookie))
                .await
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );

        fs::remove_file(&claude).expect("the pipe is removed");
        File::create(&claude)
            .and_then(|file| file.set_len(4 * 1024 * 1024 * 1024))
            .expect("the sparse file is created");
        let response = manager.get(CLAUDE, Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let error: Value = response.json().await;
        assert!(
            error["error"]
                .as_str()
                .is_some_and(|message| message.ends_with("settings.json is larger than 2 MiB")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn changes_by_the_agents_are_published_and_make_saves_conflict() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let claude = path_of(&manager, Agent::Claude);
        let codex = path_of(&manager, Agent::Codex);
        let config = claude
            .parent()
            .and_then(Path::parent)
            .expect("the config directory has a parent");
        write(&claude, b"{}");
        let dotfiles = config.join("dotfiles");
        write(&dotfiles.join("config.toml"), b"model = \"gpt\"\n");
        fs::create_dir_all(codex.parent().expect("the path has a parent"))
            .expect("the directory is created");
        symlink(dotfiles.join("config.toml"), &codex).expect("the link is created");
        let watcher = SettingsFileWatcher::start(&manager.state.install_paths).await;
        let mut events = Box::pin(manager.state.events.stream());
        events.next().await;
        tokio::spawn(watcher.publish_changes(manager.state.events.clone()));

        let opened: SettingsFileText = manager.get(CLAUDE, Some(&cookie)).await.json().await;
        write(
            &config.join("claude/.cc-writes/.tmp.7.a1"),
            b"{\n  \"model\": \"opus\"\n}\n",
        );
        fs::rename(config.join("claude/.cc-writes/.tmp.7.a1"), &claude)
            .expect("Claude's save is renamed into place");
        published(&mut events, "Claude's save").await;
        assert_eq!(
            manager
                .put(CLAUDE, &body("{}", &opened.version), Some(&cookie))
                .await
                .status(),
            StatusCode::CONFLICT
        );

        let opened: SettingsFileText = manager.get(CODEX, Some(&cookie)).await.json().await;
        assert_eq!(opened.text, "model = \"gpt\"\n");
        write(
            &dotfiles.join(".tmpX4b9Qz"),
            b"model = \"gpt\"\n\n[projects.\"/projects/app\"]\ntrust_level = \"trusted\"\n",
        );
        fs::rename(dotfiles.join(".tmpX4b9Qz"), dotfiles.join("config.toml"))
            .expect("Codex's save is renamed into place");
        published(&mut events, "Codex's save through the link").await;
        assert_eq!(
            manager
                .put(CODEX, &body("", &opened.version), Some(&cookie))
                .await
                .status(),
            StatusCode::CONFLICT
        );
        let current: SettingsFileText = manager.get(CODEX, Some(&cookie)).await.json().await;
        assert!(current.text.contains("trust_level"));
    }
}
