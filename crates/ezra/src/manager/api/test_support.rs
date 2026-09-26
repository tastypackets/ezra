use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::request::Builder;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use http_body_util::BodyExt;
use serde::de::DeserializeOwned;
use tower::ServiceExt;

use super::session::SessionStatus;
use crate::manager::agents::{Agent, InstallPaths, TlsVerification};
use crate::manager::folders::ProjectsDirectory;
use crate::manager::git::GitTools;
use crate::manager::settings::Settings;
use crate::manager::state::AppState;
use crate::manager::status::AgentStatus;
use crate::path_ext::PathExt;

pub const PASSWORD: &str = r#"{"password": "correct horse"}"#;

pub struct TestManager {
    router: Router,
    pub state: AppState,
    pub settings_path: PathBuf,
    directory: tempfile::TempDir,
}

impl TestManager {
    pub fn new() -> Self {
        let manager = Self::without_web_app();
        let web = manager.directory.path().join("web");
        fs::create_dir_all(web.join("assets")).expect("assets directory is created");
        fs::write(
            web.join("index.html"),
            "<html><head></head><body></body></html>",
        )
        .expect("index is written");
        fs::write(web.join("assets/app-1a2b.js"), "").expect("asset is written");
        manager
    }

    pub fn without_web_app() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let settings_path = directory.path().join("ezra/settings.toml");
        let install_paths = InstallPaths::under_home(&directory.path().join("home"))
            .with_config_directories(
                directory.path().join("config/claude"),
                directory.path().join("config/codex"),
            );
        let mut state = AppState::new(
            settings_path.clone(),
            Settings::default(),
            install_paths,
            TlsVerification::default(),
            GitTools::under(directory.path()),
        );
        state.projects = ProjectsDirectory(directory.path().join("projects"));
        Self {
            router: state.clone().into_router(directory.path().join("web")),
            state,
            settings_path,
            directory,
        }
    }

    /// Makes `script` the agent's command, installed as version 9.9.9. Each run is logged for
    /// `fake_cli_runs`. Returns the script's path.
    pub fn install_fake_cli(&self, agent: Agent, script: &str) -> PathBuf {
        let fakes = self.directory.path().join("fakes");
        let version = fakes.join(agent.command_name()).join("9.9.9");
        let command = match agent {
            Agent::Claude => version,
            Agent::Codex => version.join("bin/codex"),
        };
        fs::create_dir_all(command.parent().expect("the command has a parent"))
            .expect("fake directory is created");
        let runs = fakes.join(format!("{agent}.runs"));
        fs::write(
            &command,
            format!("#!/bin/sh\necho \"$*\" >> '{}'\n{script}\n", runs.display()),
        )
        .expect("fake is written");
        fs::set_permissions(&command, fs::Permissions::from_mode(0o755))
            .expect("fake is executable");
        self.state
            .install_paths
            .command(agent)
            .replace_symlink(&command)
            .expect("command link is created");
        command
    }

    /// The arguments of each run of the fake CLI.
    pub fn fake_cli_runs(&self, agent: Agent) -> Vec<String> {
        let runs = self
            .directory
            .path()
            .join("fakes")
            .join(format!("{agent}.runs"));
        fs::read_to_string(runs)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub async fn agent_status(&self, agent: Agent, cookie: &str) -> AgentStatus {
        let listing: Vec<AgentStatus> = self.get("/api/v1/agents", Some(cookie)).await.json().await;
        listing
            .into_iter()
            .find(|status| status.agent == agent)
            .expect("every agent is listed")
    }

    /// Claims the manager and returns the session cookie.
    pub async fn logged_in(&self) -> String {
        let response = self.post("/api/v1/setup", PASSWORD, None).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        response.session_cookie()
    }

    pub async fn post(&self, path: &str, body: &str, cookie: Option<&str>) -> Response {
        let request = Request::post(path).header(header::CONTENT_TYPE, "application/json");
        self.send(
            request
                .with_cookie(cookie)
                .body(Body::from(body.to_owned()))
                .expect("request builds"),
        )
        .await
    }

    pub async fn put(&self, path: &str, body: &str, cookie: Option<&str>) -> Response {
        let request = Request::put(path).header(header::CONTENT_TYPE, "application/json");
        self.send(
            request
                .with_cookie(cookie)
                .body(Body::from(body.to_owned()))
                .expect("request builds"),
        )
        .await
    }

    pub async fn get(&self, path: &str, cookie: Option<&str>) -> Response {
        self.send(
            Request::get(path)
                .with_cookie(cookie)
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
    }

    pub async fn delete(&self, path: &str, cookie: Option<&str>) -> Response {
        self.send(
            Request::delete(path)
                .with_cookie(cookie)
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
    }

    pub async fn session_status(&self, cookie: Option<&str>) -> SessionStatus {
        self.get("/api/v1/session", cookie).await.json().await
    }

    pub async fn send(&self, request: Request<Body>) -> Response {
        self.router
            .clone()
            .oneshot(request)
            .await
            .expect("router responds")
    }
}

trait RequestBuilderExt {
    fn with_cookie(self, cookie: Option<&str>) -> Self;
}

impl RequestBuilderExt for Builder {
    fn with_cookie(self, cookie: Option<&str>) -> Self {
        match cookie {
            Some(cookie) => self.header(header::COOKIE, cookie),
            None => self,
        }
    }
}

pub trait ResponseExt {
    async fn json<T: DeserializeOwned>(self) -> T;

    /// The `name=value` part of the Set-Cookie header, after checking its attributes.
    fn session_cookie(&self) -> String;
}

impl ResponseExt for Response {
    async fn json<T: DeserializeOwned>(self) -> T {
        let body = self
            .into_body()
            .collect()
            .await
            .expect("body is readable")
            .to_bytes();
        serde_json::from_slice(&body).expect("body is the expected JSON")
    }

    fn session_cookie(&self) -> String {
        let set_cookie = self
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .expect("response sets a cookie");
        for attribute in ["HttpOnly", "Secure", "SameSite=Strict"] {
            assert!(set_cookie.contains(attribute), "{set_cookie}");
        }
        set_cookie
            .split_once(';')
            .map_or(set_cookie, |(name_and_value, _attributes)| name_and_value)
            .to_owned()
    }
}
