use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::{Stream, StreamExt};

use super::{AppState, ErrorBody, Session};
use crate::manager::events::ManagerEvent;

#[utoipa::path(
    get,
    path = "/api/v1/events",
    operation_id = "streamEvents",
    tag = "events",
    summary = "Stream what changes",
    description = "Sends `connected`, then each part that changes, until the manager shuts down.",
    responses(
        (status = 200, description = "Server-sent events", content_type = "text/event-stream", body = ManagerEvent),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn stream(
    _: Session,
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, axum::Error>>> {
    Sse::new(
        state
            .events
            .stream()
            .map(|event| Event::default().json_data(event)),
    )
    .keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use axum::http::{StatusCode, header};

    use super::super::test_support::TestManager;

    #[tokio::test]
    async fn events_stream_once_logged_in() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        assert_eq!(
            manager.get("/api/v1/events", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let events = manager.get("/api/v1/events", Some(&cookie)).await;
        assert_eq!(events.status(), StatusCode::OK);
        assert_eq!(
            events.headers().get(header::CONTENT_TYPE),
            Some(&header::HeaderValue::from_static("text/event-stream"))
        );
    }
}
