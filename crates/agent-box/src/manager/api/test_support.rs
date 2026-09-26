use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use http_body_util::BodyExt;
use serde::de::DeserializeOwned;
use tower::ServiceExt;

use super::router;
use super::session::SessionStatus;
use crate::manager::agents::InstallPaths;
use crate::manager::settings::Settings;
use crate::manager::state::AppState;

pub const PASSWORD: &str = r#"{"password": "correct horse"}"#;

pub struct TestManager {
    router: Router,
    pub settings_path: PathBuf,
    _directory: tempfile::TempDir,
}

impl TestManager {
    pub fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let settings_path = directory.path().join("agent-box/settings.toml");
        let install_paths = InstallPaths::under_home(&directory.path().join("home"))
            .with_config_directories(
                directory.path().join("config/claude"),
                directory.path().join("config/codex"),
            );
        let router = router(AppState::new(
            settings_path.clone(),
            Settings::default(),
            install_paths,
        ));
        Self {
            router,
            settings_path,
            _directory: directory,
        }
    }

    /// Claims the manager and returns the session cookie.
    pub async fn logged_in(&self) -> String {
        let response = self.post("/api/v1/setup", PASSWORD, None).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        session_cookie_of(&response)
    }

    pub async fn post(&self, path: &str, body: &str, cookie: Option<&str>) -> Response {
        let request = Request::post(path).header(header::CONTENT_TYPE, "application/json");
        self.send(
            with_cookie(request, cookie)
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
    }

    pub async fn get(&self, path: &str, cookie: Option<&str>) -> Response {
        self.send(
            with_cookie(Request::get(path), cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    pub async fn session_status(&self, cookie: Option<&str>) -> SessionStatus {
        json_of(self.get("/api/v1/session", cookie).await).await
    }

    async fn send(&self, request: Request<Body>) -> Response {
        self.router.clone().oneshot(request).await.unwrap()
    }
}

fn with_cookie(
    request: axum::http::request::Builder,
    cookie: Option<&str>,
) -> axum::http::request::Builder {
    match cookie {
        Some(cookie) => request.header(header::COOKIE, cookie),
        None => request,
    }
}

pub async fn json_of<T: DeserializeOwned>(response: Response) -> T {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

/// The `name=value` part of the response's Set-Cookie header.
pub fn session_cookie_of(response: &Response) -> String {
    let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("Secure"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Strict"), "{set_cookie}");
    set_cookie.split(';').next().unwrap().to_owned()
}
