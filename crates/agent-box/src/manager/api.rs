use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::auth::{self, SESSION_COOKIE, Sessions};
use super::settings::{self, Settings};

#[derive(Clone)]
pub struct AppState {
    settings_path: Arc<PathBuf>,
    settings: Arc<Mutex<Settings>>,
    sessions: Arc<Sessions>,
}

impl AppState {
    pub fn new(settings_path: PathBuf, settings: Settings) -> Self {
        Self {
            settings_path: Arc::new(settings_path),
            settings: Arc::new(Mutex::new(settings)),
            sessions: Arc::default(),
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/session", get(session_status))
        .route("/api/v1/setup", post(set_up_password))
        .route("/api/v1/login", post(log_in))
        .route("/api/v1/logout", post(log_out))
        .with_state(state)
}

#[derive(Debug)]
enum ApiError {
    BadRequest(&'static str),
    Unauthorized(&'static str),
    Conflict(&'static str),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message.to_owned()),
            Self::Unauthorized(message) => (StatusCode::UNAUTHORIZED, message.to_owned()),
            Self::Conflict(message) => (StatusCode::CONFLICT, message.to_owned()),
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

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SessionStatus {
    claimed: bool,
    authenticated: bool,
}

#[derive(Deserialize)]
struct PasswordBody {
    password: String,
}

async fn session_status(State(state): State<AppState>, cookies: CookieJar) -> Json<SessionStatus> {
    let claimed = state.settings.lock().await.manager.password_hash.is_some();
    let authenticated = cookies
        .get(SESSION_COOKIE)
        .is_some_and(|cookie| state.sessions.is_active(cookie.value()));
    Json(SessionStatus {
        claimed,
        authenticated,
    })
}

/// The first visitor chooses the password.
async fn set_up_password(
    State(state): State<AppState>,
    cookies: CookieJar,
    Json(body): Json<PasswordBody>,
) -> Result<(CookieJar, StatusCode), ApiError> {
    if body.password.is_empty() {
        return Err(ApiError::BadRequest("the password must not be empty"));
    }
    let mut settings = state.settings.lock().await;
    if settings.manager.password_hash.is_some() {
        return Err(ApiError::Conflict("a password is already set"));
    }
    let mut updated_settings = settings.clone();
    updated_settings.manager.password_hash =
        Some(auth::hash_password(&body.password).map_err(internal)?);
    settings::save(&state.settings_path, &updated_settings).map_err(internal)?;
    *settings = updated_settings;
    start_session(&state, cookies)
}

async fn log_in(
    State(state): State<AppState>,
    cookies: CookieJar,
    Json(body): Json<PasswordBody>,
) -> Result<(CookieJar, StatusCode), ApiError> {
    let password_hash = state.settings.lock().await.manager.password_hash.clone();
    let Some(password_hash) = password_hash else {
        return Err(ApiError::Conflict("no password is set yet"));
    };
    if !auth::password_matches(&body.password, &password_hash) {
        return Err(ApiError::Unauthorized("wrong password"));
    }
    start_session(&state, cookies)
}

async fn log_out(State(state): State<AppState>, cookies: CookieJar) -> (CookieJar, StatusCode) {
    if let Some(cookie) = cookies.get(SESSION_COOKIE) {
        state.sessions.end(cookie.value());
    }
    (
        cookies.remove(Cookie::build(SESSION_COOKIE).path("/")),
        StatusCode::NO_CONTENT,
    )
}

fn start_session(
    state: &AppState,
    cookies: CookieJar,
) -> Result<(CookieJar, StatusCode), ApiError> {
    let token = state.sessions.start().map_err(internal)?;
    Ok((
        cookies.add(auth::session_cookie(token)),
        StatusCode::NO_CONTENT,
    ))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, header};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    struct TestManager {
        router: Router,
        settings_path: PathBuf,
        _directory: tempfile::TempDir,
    }

    impl TestManager {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let settings_path = directory.path().join("agent-box/settings.toml");
            let router = router(AppState::new(settings_path.clone(), Settings::default()));
            Self {
                router,
                settings_path,
                _directory: directory,
            }
        }

        async fn post(&self, path: &str, body: &str, cookie: Option<&str>) -> Response {
            let mut request = Request::post(path).header(header::CONTENT_TYPE, "application/json");
            if let Some(cookie) = cookie {
                request = request.header(header::COOKIE, cookie);
            }
            self.send(request.body(Body::from(body.to_owned())).unwrap())
                .await
        }

        async fn session_status(&self, cookie: Option<&str>) -> SessionStatus {
            let mut request = Request::get("/api/v1/session");
            if let Some(cookie) = cookie {
                request = request.header(header::COOKIE, cookie);
            }
            let response = self.send(request.body(Body::empty()).unwrap()).await;
            let body = response.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice(&body).unwrap()
        }

        async fn send(&self, request: Request<Body>) -> Response {
            self.router.clone().oneshot(request).await.unwrap()
        }
    }

    /// The `name=value` part of the response's Set-Cookie header.
    fn session_cookie_of(response: &Response) -> String {
        let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
        assert!(set_cookie.contains("Secure"), "{set_cookie}");
        assert!(set_cookie.contains("SameSite=Strict"), "{set_cookie}");
        set_cookie.split(';').next().unwrap().to_owned()
    }

    const PASSWORD: &str = r#"{"password": "correct horse"}"#;

    #[tokio::test]
    async fn fresh_manager_is_unclaimed() {
        let manager = TestManager::new();
        assert_eq!(
            manager.session_status(None).await,
            SessionStatus {
                claimed: false,
                authenticated: false
            }
        );
    }

    #[tokio::test]
    async fn setup_saves_the_password_and_logs_in() {
        let manager = TestManager::new();
        let response = manager.post("/api/v1/setup", PASSWORD, None).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let cookie = session_cookie_of(&response);

        assert_eq!(
            manager.session_status(Some(&cookie)).await,
            SessionStatus {
                claimed: true,
                authenticated: true
            }
        );
        let saved = settings::load(&manager.settings_path).unwrap();
        assert!(auth::password_matches(
            "correct horse",
            saved.manager.password_hash.as_deref().unwrap()
        ));
    }

    #[tokio::test]
    async fn setup_only_works_once() {
        let manager = TestManager::new();
        manager.post("/api/v1/setup", PASSWORD, None).await;
        let second = manager
            .post("/api/v1/setup", r#"{"password": "taken over"}"#, None)
            .await;
        assert_eq!(second.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn empty_password_is_refused() {
        let manager = TestManager::new();
        let response = manager
            .post("/api/v1/setup", r#"{"password": ""}"#, None)
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn login_checks_the_password() {
        let manager = TestManager::new();
        manager.post("/api/v1/setup", PASSWORD, None).await;

        let wrong = manager
            .post("/api/v1/login", r#"{"password": "wrong"}"#, None)
            .await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        let right = manager.post("/api/v1/login", PASSWORD, None).await;
        assert_eq!(right.status(), StatusCode::NO_CONTENT);
        let cookie = session_cookie_of(&right);
        assert!(manager.session_status(Some(&cookie)).await.authenticated);
    }

    #[tokio::test]
    async fn login_before_setup_is_a_conflict() {
        let manager = TestManager::new();
        let response = manager.post("/api/v1/login", PASSWORD, None).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn logout_ends_the_session() {
        let manager = TestManager::new();
        let setup = manager.post("/api/v1/setup", PASSWORD, None).await;
        let cookie = session_cookie_of(&setup);

        let logout = manager.post("/api/v1/logout", "", Some(&cookie)).await;
        assert_eq!(logout.status(), StatusCode::NO_CONTENT);
        assert!(!manager.session_status(Some(&cookie)).await.authenticated);
    }

    #[tokio::test]
    async fn made_up_cookie_is_not_a_session() {
        let manager = TestManager::new();
        manager.post("/api/v1/setup", PASSWORD, None).await;
        assert!(
            !manager
                .session_status(Some("session=0123abcd"))
                .await
                .authenticated
        );
    }
}
