use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ApiError, AppState, ErrorBody, Session, internal};
use crate::manager::auth::HashedPassword;
use crate::manager::environment::EnvironmentSettings;
use crate::manager::events::Topic;
use crate::manager::tls::CertificateStatus;

/// The manager's own settings and state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ManagerStatus {
    /// Absent when the manager serves no certificate of its own.
    pub certificate: Option<CertificateStatus>,
    pub environment: EnvironmentSettings,
}

#[derive(Deserialize, ToSchema)]
pub struct PasswordChangeBody {
    current_password: String,
    new_password: String,
}

#[utoipa::path(
    get,
    path = "/api/v1/manager",
    operation_id = "getManager",
    tag = "manager",
    summary = "Get the manager's certificate and environment",
    responses(
        (status = 200, description = "Manager state", body = ManagerStatus),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody)
    )
)]
pub async fn status(
    _: Session,
    State(state): State<AppState>,
) -> Result<Json<ManagerStatus>, ApiError> {
    let certificate = match &state.certificate {
        Some(certificate) => Some(certificate.status().map_err(internal)?),
        None => None,
    };
    Ok(Json(ManagerStatus {
        certificate,
        environment: state.environment.clone(),
    }))
}

#[utoipa::path(
    put,
    path = "/api/v1/manager/password",
    operation_id = "changePassword",
    tag = "manager",
    summary = "Change the manager password",
    description = "Signs out every other session.",
    request_body = PasswordChangeBody,
    responses(
        (status = 204, description = "Password changed"),
        (status = 400, description = "Empty new password", body = ErrorBody),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 403, description = "Wrong current password", body = ErrorBody)
    )
)]
pub async fn change_password(
    session: Session,
    State(state): State<AppState>,
    Json(body): Json<PasswordChangeBody>,
) -> Result<StatusCode, ApiError> {
    if body.new_password.is_empty() {
        return Err(ApiError::BadRequest("the new password must not be empty"));
    }
    let password_hash = HashedPassword::from_password(&body.new_password).map_err(internal)?;
    state
        .update_settings(|settings| {
            if !settings
                .manager
                .password_hash
                .as_ref()
                .is_some_and(|current| current.matches(&body.current_password))
            {
                return Err(ApiError::Forbidden("the current password is wrong"));
            }
            settings.manager.password_hash = Some(password_hash);
            Ok(())
        })
        .await?;
    state.sessions.end_others(&session.0);
    state.events.publish(Topic::Manager);
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/manager/certificate",
    operation_id = "regenerateCertificate",
    tag = "manager",
    summary = "Replace the certificate with a new self-signed one",
    description = "Browsers warn again until the new certificate is accepted.",
    responses(
        (status = 200, description = "The new certificate", body = CertificateStatus),
        (status = 401, description = "Not signed in to the manager", body = ErrorBody),
        (status = 409, description = "The manager serves no certificate of its own", body = ErrorBody)
    )
)]
pub async fn regenerate_certificate(
    _: Session,
    State(state): State<AppState>,
) -> Result<Json<CertificateStatus>, ApiError> {
    let Some(certificate) = &state.certificate else {
        return Err(ApiError::Conflict(
            "the manager serves no certificate of its own".to_owned(),
        ));
    };
    certificate.regenerate().await.map_err(internal)?;
    state.events.publish(Topic::Manager);
    certificate.status().map(Json).map_err(internal)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::super::test_support::{PASSWORD, ResponseExt, TestManager};
    use super::*;

    async fn second_session(manager: &TestManager) -> String {
        let response = manager.post("/api/v1/login", PASSWORD, None).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        response.session_cookie()
    }

    #[tokio::test]
    async fn status_needs_a_session() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        assert_eq!(
            manager.get("/api/v1/manager", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let status: ManagerStatus = manager
            .get("/api/v1/manager", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(status.certificate, None);
    }

    #[tokio::test]
    async fn changing_the_password_needs_the_current_one_and_ends_other_sessions() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        let other = second_session(&manager).await;
        let change = |current: &str, new: &str| {
            format!(r#"{{"current_password":"{current}","new_password":"{new}"}}"#)
        };

        let wrong = manager
            .put(
                "/api/v1/manager/password",
                &change("wrong", "battery staple"),
                Some(&cookie),
            )
            .await;
        assert_eq!(wrong.status(), StatusCode::FORBIDDEN);
        let empty = manager
            .put(
                "/api/v1/manager/password",
                &change("correct horse", ""),
                Some(&cookie),
            )
            .await;
        assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

        let changed = manager
            .put(
                "/api/v1/manager/password",
                &change("correct horse", "battery staple"),
                Some(&cookie),
            )
            .await;
        assert_eq!(changed.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            manager.get("/api/v1/manager", Some(&other)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            manager.get("/api/v1/manager", Some(&cookie)).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            manager.post("/api/v1/login", PASSWORD, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            manager
                .post("/api/v1/login", r#"{"password":"battery staple"}"#, None)
                .await
                .status(),
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn without_a_certificate_there_is_nothing_to_regenerate() {
        let manager = TestManager::new();
        let cookie = manager.logged_in().await;
        assert_eq!(
            manager
                .post("/api/v1/manager/certificate", "", Some(&cookie))
                .await
                .status(),
            StatusCode::CONFLICT
        );
    }
}
