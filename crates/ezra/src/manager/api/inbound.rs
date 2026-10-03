use axum::Json;
use axum::extract::State;
use ezra::agent::Agent;
use ezra::inbound::InboundSettings;

use super::{ApiError, AppState, ErrorBody, Session};

#[utoipa::path(
    get, path = "/api/v1/inbound/settings", operation_id = "getInboundSettings", tag = "settings",
    summary = "Get inbound trigger settings",
    responses(
        (status = 200, description = "Settings", body = InboundSettings),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn settings(_: Session, State(state): State<AppState>) -> Json<InboundSettings> {
    Json(state.settings.lock().await.inbound.clone())
}

#[utoipa::path(
    put, path = "/api/v1/inbound/settings", operation_id = "updateInboundSettings", tag = "settings",
    summary = "Change inbound trigger settings",
    request_body = InboundSettings,
    responses(
        (status = 200, description = "Saved", body = InboundSettings),
        (status = 400, description = "Invalid trigger settings", body = ErrorBody),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn update_settings(
    _: Session,
    State(state): State<AppState>,
    Json(settings): Json<InboundSettings>,
) -> Result<Json<InboundSettings>, ApiError> {
    settings.match_shortcut("").map_err(|_| {
        ApiError::BadRequest("Shortcuts must be nonempty tokens without whitespace")
    })?;
    for shortcut in settings.shortcuts.values() {
        shortcut.validate().map_err(|_| {
            ApiError::BadRequest(
                "Model and effort must be nonempty identifiers of at most 512 bytes",
            )
        })?;
        if shortcut.agent != Agent::Codex {
            return Err(ApiError::BadRequest(
                "GitHub triggers currently support Codex",
            ));
        }
    }
    state
        .update_settings(|current| {
            current.inbound = settings.clone();
            Ok::<(), ApiError>(())
        })
        .await?;
    state
        .events
        .publish(crate::manager::events::Topic::InboundSettings);
    Ok(Json(settings))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{ResponseExt, TestManager};
    use super::*;
    use axum::http::StatusCode;

    const PATH: &str = "/api/v1/inbound/settings";

    #[tokio::test]
    async fn trigger_settings_require_sign_in_and_persist_values_and_global_scope() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        assert_eq!(
            manager.get(PATH, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            manager.put(PATH, "{}", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let defaults: InboundSettings = manager.get(PATH, Some(&cookie)).await.json().await;
        assert!(defaults.github.only_added_repositories);
        assert_eq!(defaults.waiting_expiry_hours.get(), 24);
        let saved: InboundSettings = manager.put(PATH,
            r#"{"github":{"only_added_repositories":false,"edit_comment_status":true,"react_on_status":false,"poll_interval_seconds":10},"shortcuts":{"/custom":{"model":"future-model","effort":"future-effort"}},"retention_days":120,"waiting_expiry_hours":48}"#,
            Some(&cookie)).await.json().await;
        assert!(!saved.github.only_added_repositories);
        assert_eq!(saved.retention_days, 120);
        assert_eq!(saved.waiting_expiry_hours.get(), 48);
        assert_eq!(saved.github.poll_interval_seconds.get(), 10);
        assert!(saved.github.edit_comment_status);
        assert!(!saved.github.react_on_status);
        let loaded = crate::manager::settings::Settings::load(&manager.settings_path)
            .expect("settings load");
        assert_eq!(loaded.inbound, saved);
    }

    #[tokio::test]
    async fn invalid_trigger_configuration_does_not_replace_saved_settings() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        for body in [
            r#"{"shortcuts":{"has space":{}}}"#,
            r#"{"shortcuts":{"/custom":{"model":""}}}"#,
        ] {
            assert_eq!(
                manager.put(PATH, body, Some(&cookie)).await.status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            manager
                .put(
                    PATH,
                    r#"{"shortcuts":{"/custom":{"agent":"unsupported"}}}"#,
                    Some(&cookie)
                )
                .await
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let saved: InboundSettings = manager.get(PATH, Some(&cookie)).await.json().await;
        assert_eq!(saved, InboundSettings::default());
    }
}
