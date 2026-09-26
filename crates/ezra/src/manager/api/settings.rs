use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, Session};
use crate::manager::agents::{Agent, ReleaseChannel};

/// How the manager installs and updates Claude Code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClaudeSettingsBody {
    pub release_channel: ReleaseChannel,
}

#[utoipa::path(
    get,
    path = "/api/v1/agents/claude/settings",
    operation_id = "getClaudeSettings",
    tag = "agents",
    summary = "Get Claude Code settings",
    responses(
        (status = 200, description = "Settings", body = ClaudeSettingsBody),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn claude(_: Session, State(state): State<AppState>) -> Json<ClaudeSettingsBody> {
    let settings = state.settings.lock().await;
    Json(ClaudeSettingsBody {
        release_channel: settings.release_channel(Agent::Claude),
    })
}

#[utoipa::path(
    put,
    path = "/api/v1/agents/claude/settings",
    operation_id = "updateClaudeSettings",
    tag = "agents",
    summary = "Change Claude Code settings",
    description = "Saves the settings. Changing the release channel never downgrades the installed version.",
    request_body = ClaudeSettingsBody,
    responses(
        (status = 200, description = "Saved", body = ClaudeSettingsBody),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn update_claude(
    _: Session,
    State(state): State<AppState>,
    Json(body): Json<ClaudeSettingsBody>,
) -> Result<Json<ClaudeSettingsBody>, ApiError> {
    state
        .set_claude_release_channel(body.release_channel)
        .await?;
    Ok(Json(body))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::super::test_support::{ResponseExt, TestManager};
    use super::*;
    use crate::manager::settings::Settings;

    const PATH: &str = "/api/v1/agents/claude/settings";

    #[tokio::test]
    async fn claude_settings_need_a_login() {
        let manager = TestManager::new();
        manager.logged_in().await;
        assert_eq!(
            manager.get(PATH, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            manager
                .put(PATH, r#"{"release_channel":"stable"}"#, None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn release_channel_is_saved() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let defaults: ClaudeSettingsBody = manager.get(PATH, Some(&cookie)).await.json().await;
        assert_eq!(defaults.release_channel, ReleaseChannel::Latest);

        let saved: ClaudeSettingsBody = manager
            .put(PATH, r#"{"release_channel":"stable"}"#, Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(saved.release_channel, ReleaseChannel::Stable);
        let on_disk = Settings::load(&manager.settings_path).expect("settings load");
        assert_eq!(
            on_disk.release_channel(Agent::Claude),
            ReleaseChannel::Stable
        );
    }

    #[tokio::test]
    async fn unknown_release_channel_is_rejected() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let response = manager
            .put(PATH, r#"{"release_channel":"nightly"}"#, Some(&cookie))
            .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
