use axum::Json;
use axum::extract::State;

use super::{AppState, ErrorBody, Session};
use crate::manager::remote_control::RemoteControlOverview;

#[utoipa::path(
    get,
    path = "/api/v1/remote-control",
    operation_id = "getRemoteControl",
    tag = "remote-control",
    summary = "Get every Remote Control server's state",
    responses(
        (status = 200, description = "The servers for /projects and its folders", body = RemoteControlOverview),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn overview(_: Session, State(state): State<AppState>) -> Json<RemoteControlOverview> {
    Json(state.remote_control.overview(&state.projects.0))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::super::test_support::{ResponseExt, TestManager};
    use super::*;
    use crate::manager::remote_control::ServerState;

    #[tokio::test]
    async fn every_server_is_listed_once_logged_in() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        assert_eq!(
            manager.get("/api/v1/remote-control", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let overview: RemoteControlOverview = manager
            .get("/api/v1/remote-control", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(overview.projects.state, ServerState::Waiting);
        assert!(overview.folders.is_empty());
    }
}
