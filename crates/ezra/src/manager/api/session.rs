use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum_extra::extract::cookie::{Cookie, CookieJar};
use serde::{Deserialize, Serialize};

use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, internal};
use crate::manager::auth::{HashedPassword, SESSION_COOKIE};
use crate::manager::events::Topic;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct SessionStatus {
    /// A password has been set.
    pub claimed: bool,
    /// This request carries a signed-in session.
    pub authenticated: bool,
}

impl SessionStatus {
    pub async fn of(state: &AppState, cookies: &CookieJar) -> Self {
        Self {
            claimed: state.settings.lock().await.manager.password_hash.is_some(),
            authenticated: state
                .is_session(cookies.get(SESSION_COOKIE).map(|cookie| cookie.value())),
        }
    }
}

#[derive(Deserialize, ToSchema)]
pub struct PasswordBody {
    password: String,
}

#[utoipa::path(
    get,
    path = "/api/v1/session",
    operation_id = "getSession",
    tag = "session",
    summary = "Session status",
    description = "Returns whether a password is set and whether this request is signed in.",
    responses((status = 200, description = "Session status", body = SessionStatus))
)]
pub async fn status(State(state): State<AppState>, cookies: CookieJar) -> Json<SessionStatus> {
    Json(SessionStatus::of(&state, &cookies).await)
}

#[utoipa::path(
    post,
    path = "/api/v1/setup",
    operation_id = "setUpPassword",
    tag = "session",
    summary = "Set the password",
    description = "Sets the manager password when none is set yet, and signs this browser in.",
    request_body = PasswordBody,
    responses(
        (status = 204, description = "Password set and signed in"),
        (status = 400, description = "Empty password", body = ErrorBody),
        (status = 409, description = "A password is already set", body = ErrorBody)
    )
)]
pub async fn set_up_password(
    State(state): State<AppState>,
    cookies: CookieJar,
    Json(body): Json<PasswordBody>,
) -> Result<(CookieJar, StatusCode), ApiError> {
    if body.password.is_empty() {
        return Err(ApiError::BadRequest("the password must not be empty"));
    }
    let password_hash = HashedPassword::from_password(&body.password).map_err(internal)?;
    state
        .update_settings(|settings| {
            if settings.manager.password_hash.is_some() {
                return Err(ApiError::Conflict("a password is already set".to_owned()));
            }
            settings.manager.password_hash = Some(password_hash);
            Ok(())
        })
        .await?;
    state.start_session(cookies)
}

#[utoipa::path(
    post,
    path = "/api/v1/login",
    operation_id = "logIn",
    tag = "session",
    summary = "Sign in",
    description = "Checks the manager password and signs this browser in.",
    request_body = PasswordBody,
    responses(
        (status = 204, description = "Signed in"),
        (status = 401, description = "Wrong password", body = ErrorBody),
        (status = 409, description = "No password is set yet", body = ErrorBody)
    )
)]
pub async fn log_in(
    State(state): State<AppState>,
    cookies: CookieJar,
    Json(body): Json<PasswordBody>,
) -> Result<(CookieJar, StatusCode), ApiError> {
    let password_hash = state.settings.lock().await.manager.password_hash.clone();
    let Some(password_hash) = password_hash else {
        return Err(ApiError::Conflict("no password is set yet".to_owned()));
    };
    if !password_hash.matches(&body.password) {
        return Err(ApiError::Unauthorized("wrong password"));
    }
    state.start_session(cookies)
}

#[utoipa::path(
    post,
    path = "/api/v1/logout",
    operation_id = "logOut",
    tag = "session",
    summary = "Sign out",
    description = "Ends this browser's session.",
    responses((status = 204, description = "Signed out"))
)]
pub async fn log_out(State(state): State<AppState>, cookies: CookieJar) -> (CookieJar, StatusCode) {
    if let Some(cookie) = cookies.get(SESSION_COOKIE) {
        state.sessions.end(cookie.value());
        state.events.publish(Topic::Manager);
    }
    (
        cookies.remove(Cookie::build(SESSION_COOKIE).path("/")),
        StatusCode::NO_CONTENT,
    )
}

impl AppState {
    fn start_session(&self, cookies: CookieJar) -> Result<(CookieJar, StatusCode), ApiError> {
        let token = self.sessions.start().map_err(internal)?;
        self.events.publish(Topic::Manager);
        Ok((cookies.add(token.into_cookie()), StatusCode::NO_CONTENT))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{PASSWORD, ResponseExt, TestManager};
    use super::*;
    use crate::manager::settings::Settings;

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
        let cookie = response.session_cookie();

        assert_eq!(
            manager.session_status(Some(&cookie)).await,
            SessionStatus {
                claimed: true,
                authenticated: true
            }
        );
        let saved = Settings::load(&manager.settings_path).expect("settings load");
        let saved_hash = saved.manager.password_hash.expect("password hash is saved");
        assert!(saved_hash.matches("correct horse"));
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
        let cookie = right.session_cookie();
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
        let cookie = manager.logged_in().await;

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
