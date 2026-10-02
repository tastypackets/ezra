use axum::Json;
use axum::extract::State;
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
                "Agent, model and effort must be nonempty identifiers of at most 512 bytes",
            )
        })?;
        if shortcut
            .agent
            .as_deref()
            .is_some_and(|agent| agent != "codex")
        {
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
