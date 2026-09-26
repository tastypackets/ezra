mod agents;
mod session;
#[cfg(test)]
mod test_support;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::CookieJar;
use serde::Serialize;
use tokio::sync::Mutex;

use super::agents::InstallPaths;
use super::auth::{SESSION_COOKIE, Sessions};
use super::settings::Settings;

pub use agents::reinstall_configured_agents;

#[derive(Clone)]
pub struct AppState {
    settings_path: Arc<PathBuf>,
    settings: Arc<Mutex<Settings>>,
    sessions: Arc<Sessions>,
    install_paths: Arc<InstallPaths>,
    install_lock: Arc<Mutex<()>>,
}

impl AppState {
    pub fn new(settings_path: PathBuf, settings: Settings, install_paths: InstallPaths) -> Self {
        Self {
            settings_path: Arc::new(settings_path),
            settings: Arc::new(Mutex::new(settings)),
            sessions: Arc::default(),
            install_paths: Arc::new(install_paths),
            install_lock: Arc::default(),
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/session", get(session::status))
        .route("/api/v1/setup", post(session::set_up_password))
        .route("/api/v1/login", post(session::log_in))
        .route("/api/v1/logout", post(session::log_out))
        .route("/api/v1/agents", get(agents::list))
        .route("/api/v1/agents/{agent}/install", post(agents::install))
        .with_state(state)
}

/// Extracting this rejects requests that are not logged in.
pub struct Session;

impl FromRequestParts<AppState> for Session {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let cookies = CookieJar::from_headers(&parts.headers);
        match cookies.get(SESSION_COOKIE) {
            Some(cookie) if state.sessions.is_active(cookie.value()) => Ok(Self),
            _ => Err(ApiError::Unauthorized("log in first")),
        }
    }
}

#[derive(Debug)]
pub enum ApiError {
    BadRequest(&'static str),
    Unauthorized(&'static str),
    Conflict(&'static str),
    InstallFailed(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message.to_owned()),
            Self::Unauthorized(message) => (StatusCode::UNAUTHORIZED, message.to_owned()),
            Self::Conflict(message) => (StatusCode::CONFLICT, message.to_owned()),
            Self::InstallFailed(message) => {
                tracing::warn!("{message}");
                (StatusCode::BAD_GATEWAY, message)
            }
            Self::Internal(message) => {
                tracing::error!("{message}");
                (StatusCode::INTERNAL_SERVER_ERROR, message)
            }
        };
        (status, Json(ErrorBody { error: message })).into_response()
    }
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(error.to_string())
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}
