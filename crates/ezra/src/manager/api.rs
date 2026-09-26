mod agents;
mod folders;
mod git;
mod login;
pub mod session;
mod settings;
#[cfg(test)]
pub mod test_support;

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use axum::{Json, Router};
use axum_extra::extract::cookie::CookieJar;
use serde::Serialize;
use utoipa::{OpenApi, ToSchema};

use super::auth::SESSION_COOKIE;
use super::settings::SettingsError;
use super::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/session", get(session::status))
        .route("/api/v1/setup", post(session::set_up_password))
        .route("/api/v1/login", post(session::log_in))
        .route("/api/v1/logout", post(session::log_out))
        .route("/api/v1/agents", get(agents::list))
        .route("/api/v1/agents/{agent}/install", post(agents::install))
        .route("/api/v1/agents/{agent}/login", post(login::start))
        .route(
            "/api/v1/agents/{agent}/login/code",
            post(login::submit_code),
        )
        .route("/api/v1/agents/{agent}/logout", post(login::log_out))
        .route(
            "/api/v1/agents/claude/settings",
            get(settings::claude).put(settings::update_claude),
        )
        .route("/api/v1/folders", get(folders::list))
        .route(
            "/api/v1/folders/{name}/remote-control",
            put(folders::choose_to_serve),
        )
        .route("/api/v1/git", get(git::status))
        .route("/api/v1/git/github/login", post(git::start_github_login))
        .route("/api/v1/git/github/logout", post(git::log_out_of_github))
        .route("/api/v1/git/identity", put(git::update_identity))
        .route("/api", any(not_found))
        .route("/api/", any(not_found))
        .route("/api/{*path}", any(not_found))
        .with_state(state)
}

/// Extracting this rejects requests that are not logged in.
pub struct Session;

impl FromRequestParts<AppState> for Session {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let cookies = CookieJar::from_headers(&parts.headers);
        if state.is_session(cookies.get(SESSION_COOKIE).map(|cookie| cookie.value())) {
            Ok(Self)
        } else {
            Err(ApiError::Unauthorized("log in first"))
        }
    }
}

#[derive(Debug)]
pub enum ApiError {
    BadRequest(&'static str),
    Unauthorized(&'static str),
    NotFound(&'static str),
    Conflict(String),
    AgentFailed(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message.to_owned()),
            Self::Unauthorized(message) => (StatusCode::UNAUTHORIZED, message.to_owned()),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message.to_owned()),
            Self::Conflict(message) => (StatusCode::CONFLICT, message),
            Self::AgentFailed(message) => {
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

impl From<SettingsError> for ApiError {
    fn from(error: SettingsError) -> Self {
        Self::Internal(error.to_string())
    }
}

async fn not_found() -> ApiError {
    ApiError::NotFound("no such endpoint")
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(error.to_string())
}

/// Why a request failed.
#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    error: String,
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "EZ Remote Agent",
        description = "Installs, signs in and runs coding agents."
    ),
    paths(
        session::status,
        session::set_up_password,
        session::log_in,
        session::log_out,
        agents::list,
        agents::install,
        login::start,
        login::submit_code,
        login::log_out,
        settings::claude,
        settings::update_claude,
        git::status,
        git::start_github_login,
        git::log_out_of_github,
        git::update_identity,
        folders::list,
        folders::choose_to_serve,
    )
)]
pub struct ApiDoc;

impl ApiDoc {
    /// The API description the web client is generated from, as pretty JSON with a trailing newline.
    pub fn to_json() -> Result<String, serde_json::Error> {
        let mut json = Self::openapi().to_pretty_json()?;
        json.push('\n');
        Ok(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_spec_is_fresh() {
        let committed = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../openapi.json"));
        let generated = ApiDoc::to_json().expect("the API description serializes");
        assert!(
            generated == committed,
            "openapi.json is out of date, run `mise run generate-schema`"
        );
    }
}
