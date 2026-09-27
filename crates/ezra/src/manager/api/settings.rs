use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, Session};
use crate::manager::agents::{Agent, ReleaseChannel};
use crate::manager::codex_remote::CodexRemoteSettings;
use crate::manager::events::Topic;
use crate::manager::remote_control::RemoteControlSettings;

/// How the manager installs, updates and serves Claude Code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClaudeSettingsBody {
    pub release_channel: ReleaseChannel,
    pub remote_control: RemoteControlSettings,
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
        remote_control: settings.agents.claude.remote_control.clone(),
    })
}

#[utoipa::path(
    put,
    path = "/api/v1/agents/claude/settings",
    operation_id = "updateClaudeSettings",
    tag = "agents",
    summary = "Change Claude Code settings",
    description = "Saves the settings, never downgrading the installed version and restarting Remote Control when its settings change.",
    request_body = ClaudeSettingsBody,
    responses(
        (status = 200, description = "Saved", body = ClaudeSettingsBody),
        (status = 400, description = "A setting Claude cannot take", body = ErrorBody),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn update_claude(
    _: Session,
    State(state): State<AppState>,
    Json(body): Json<ClaudeSettingsBody>,
) -> Result<Json<ClaudeSettingsBody>, ApiError> {
    Ok(Json(state.apply_claude_settings(body).await?))
}

/// How the manager runs Codex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CodexSettingsBody {
    pub remote_control: CodexRemoteSettings,
}

#[utoipa::path(
    get,
    path = "/api/v1/agents/codex/settings",
    operation_id = "getCodexSettings",
    tag = "agents",
    summary = "Get Codex settings",
    responses(
        (status = 200, description = "Settings", body = CodexSettingsBody),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn codex(_: Session, State(state): State<AppState>) -> Json<CodexSettingsBody> {
    Json(CodexSettingsBody {
        remote_control: state.settings.lock().await.agents.codex.remote_control,
    })
}

#[utoipa::path(
    put,
    path = "/api/v1/agents/codex/settings",
    operation_id = "updateCodexSettings",
    tag = "agents",
    summary = "Change Codex settings",
    description = "Saves the settings, restarting Codex's remote control when they change.",
    request_body = CodexSettingsBody,
    responses(
        (status = 200, description = "Saved", body = CodexSettingsBody),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn update_codex(
    _: Session,
    State(state): State<AppState>,
    Json(body): Json<CodexSettingsBody>,
) -> Result<Json<CodexSettingsBody>, ApiError> {
    state
        .update_settings(|settings| {
            settings.agents.codex.remote_control = body.remote_control;
            Ok::<(), ApiError>(())
        })
        .await?;
    state.events.publish(Topic::CodexSettings);
    state.codex_remote.supervision.reconsider();
    Ok(Json(body))
}

impl AppState {
    async fn apply_claude_settings(
        &self,
        body: ClaudeSettingsBody,
    ) -> Result<ClaudeSettingsBody, ApiError> {
        if let Some(problem) = body.remote_control.problem() {
            return Err(ApiError::BadRequest(problem));
        }
        let body = ClaudeSettingsBody {
            remote_control: body.remote_control.trimmed(),
            ..body
        };
        let mut channel_changed = false;
        let mut remote_control_changed = false;
        self.update_settings(|settings| {
            let claude = &mut settings.agents.claude;
            channel_changed = claude.release_channel != body.release_channel;
            remote_control_changed = claude.remote_control != body.remote_control;
            claude.release_channel = body.release_channel;
            claude.remote_control = body.remote_control.clone();
            Ok::<(), ApiError>(())
        })
        .await?;
        self.events.publish(Topic::ClaudeSettings);
        if channel_changed {
            self.events.publish(Topic::Agents);
            self.recheck_claude_release();
        }
        if remote_control_changed {
            self.remote_control.supervision.reconsider();
            self.events.publish(Topic::Folders);
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use futures_util::StreamExt;

    use super::super::test_support::{EventStreamExt, ResponseExt, TestManager};
    use super::*;
    use crate::manager::codex_remote::{CodexApprovals, CodexSandbox};
    use crate::manager::remote_control::SpawnMode;
    use crate::manager::settings::Settings;

    const PATH: &str = "/api/v1/agents/claude/settings";
    const CODEX_PATH: &str = "/api/v1/agents/codex/settings";
    const CODEX_BODY: &str =
        r#"{"remote_control":{"enabled":false,"sandbox":"read-only","approvals":"never"}}"#;

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
                .put(PATH, r#"{"release_channel":"stable","remote_control":{"enabled":false,"permission_mode":" plan ","capacity":2}}"#, None)
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
            .put(PATH, r#"{"release_channel":"stable","remote_control":{"enabled":false,"permission_mode":" plan ","capacity":2}}"#, Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(saved.release_channel, ReleaseChannel::Stable);
        assert_eq!(
            saved.remote_control,
            RemoteControlSettings {
                enabled: false,
                permission_mode: "plan".to_owned(),
                capacity: Some(2),
                serve_repositories: true,
                spawn: SpawnMode::Worktree,
            }
        );
        let on_disk = Settings::load(&manager.settings_path).expect("settings load");
        assert_eq!(
            on_disk.release_channel(Agent::Claude),
            ReleaseChannel::Stable
        );
    }

    #[tokio::test]
    async fn remote_control_settings_claude_cannot_take_are_rejected() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        for remote_control in [
            r#"{"enabled":true,"permission_mode":"","capacity":4}"#,
            r#"{"enabled":true,"permission_mode":"auto --x","capacity":4}"#,
            r#"{"enabled":true,"permission_mode":"Auto","capacity":4}"#,
            r#"{"enabled":true,"permission_mode":"auto","capacity":0}"#,
        ] {
            let body =
                format!(r#"{{"release_channel":"latest","remote_control":{remote_control}}}"#);
            let response = manager.put(PATH, &body, Some(&cookie)).await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{remote_control}"
            );
        }
        let large = manager
            .put(
                PATH,
                r#"{"release_channel":"latest","remote_control":{"enabled":true,"permission_mode":"auto","capacity":64}}"#,
                Some(&cookie),
            )
            .await;
        assert_eq!(large.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_release_channel_is_rejected() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let response = manager
            .put(PATH, r#"{"release_channel":"nightly","remote_control":{"enabled":true,"permission_mode":"auto","capacity":4}}"#, Some(&cookie))
            .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn codex_settings_need_a_login() {
        let manager = TestManager::new();
        manager.logged_in().await;
        assert_eq!(
            manager.get(CODEX_PATH, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            manager.put(CODEX_PATH, CODEX_BODY, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn codex_settings_are_saved_and_published() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let defaults: CodexSettingsBody = manager.get(CODEX_PATH, Some(&cookie)).await.json().await;
        assert_eq!(
            defaults.remote_control,
            CodexRemoteSettings {
                enabled: true,
                sandbox: CodexSandbox::DangerFullAccess,
                approvals: CodexApprovals::OnRequest,
            }
        );

        manager
            .state
            .update_settings(|settings| {
                settings.agent_mut(Agent::Codex).configured = true;
                Ok::<(), ApiError>(())
            })
            .await
            .expect("configured saves");
        let mut events = Box::pin(manager.state.events.stream());
        events.next().await;
        let response = manager.put(CODEX_PATH, CODEX_BODY, Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let saved: CodexSettingsBody = response.json().await;
        let chosen = CodexRemoteSettings {
            enabled: false,
            sandbox: CodexSandbox::ReadOnly,
            approvals: CodexApprovals::Never,
        };
        assert_eq!(saved.remote_control, chosen);
        assert_eq!(events.published().await, [Topic::CodexSettings]);
        let stored = Settings::load(&manager.settings_path).expect("settings load");
        assert_eq!(stored.agents.codex.remote_control, chosen);
        assert!(stored.agent(Agent::Codex).configured);
        let read: CodexSettingsBody = manager.get(CODEX_PATH, Some(&cookie)).await.json().await;
        assert_eq!(read, saved);
    }

    #[tokio::test]
    async fn codex_settings_it_does_not_offer_are_rejected() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        for remote_control in [
            r#"{"enabled":true,"sandbox":"danger-full-access","approvals":"untrusted"}"#,
            r#"{"enabled":true,"sandbox":"danger-full-access","approvals":"on-failure"}"#,
            r#"{"enabled":true,"sandbox":"danger-full-access","approvals":"Never"}"#,
            r#"{"enabled":true,"sandbox":"danger-full-access","approvals":{"granular":{"mcp_elicitations":true,"rules":true,"sandbox_approval":true}}}"#,
            r#"{"enabled":true,"sandbox":"ReadOnly","approvals":"never"}"#,
        ] {
            let response = manager
                .put(
                    CODEX_PATH,
                    &format!(r#"{{"remote_control":{remote_control}}}"#),
                    Some(&cookie),
                )
                .await;
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{remote_control}"
            );
        }
        assert_eq!(
            Settings::load(&manager.settings_path)
                .expect("settings load")
                .agents
                .codex
                .remote_control,
            CodexRemoteSettings::default()
        );
    }
}
