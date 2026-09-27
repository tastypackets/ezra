use std::fmt::Debug;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::request::Builder;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use futures_util::{Stream, StreamExt};
use http_body_util::BodyExt;
use serde::de::DeserializeOwned;
use tokio::time::{Instant, sleep, timeout};
use tower::ServiceExt;

use super::session::SessionStatus;
use crate::manager::agents::{Agent, InstallPaths, TlsVerification};
use crate::manager::events::{ManagerEvent, Topic};
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

    /// Makes `script` the agent's command, installed as version 9.9.9 outside the versions
    /// directory. Each run is logged for `fake_cli_runs`. Returns the script's path.
    pub fn install_fake_cli(&self, agent: Agent, script: &str) -> PathBuf {
        let version = self
            .directory
            .path()
            .join("fakes")
            .join(agent.command_name())
            .join("9.9.9");
        self.install_fake(agent, &version, script)
    }

    /// Makes `script` the agent's command, installed as `version` in the versions directory like a
    /// release. Each run is logged for `fake_cli_runs`. Returns the script's path.
    pub fn install_fake_version(&self, agent: Agent, version: &str, script: &str) -> PathBuf {
        let version = self
            .state
            .install_paths
            .versions_directory(agent)
            .join(version);
        self.install_fake(agent, &version, script)
    }

    /// Writes the script where a release in `version` keeps the agent's command, without this
    /// process holding it open for writing, and links the command to it.
    fn install_fake(&self, agent: Agent, version: &Path, script: &str) -> PathBuf {
        let command = match agent {
            Agent::Claude => version.to_path_buf(),
            Agent::Codex => version.join("bin/codex"),
        };
        let fakes = self.directory.path().join("fakes");
        for directory in [&fakes, command.parent().expect("the command has a parent")] {
            fs::create_dir_all(directory).expect("fake directory is created");
        }
        let runs = fakes.join(format!("{agent}.runs"));
        let mut writer = Command::new("sh")
            .args(["-c", "cat > \"$0\" && chmod 755 \"$0\""])
            .arg(&command)
            .stdin(Stdio::piped())
            .spawn()
            .expect("sh starts");
        writer
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(
                format!("#!/bin/sh\necho \"$*\" >> '{}'\n{script}\n", runs.display()).as_bytes(),
            )
            .expect("script is sent");
        assert!(writer.wait().expect("sh ends").success());
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

pub trait EventStreamExt {
    /// The topics published until nothing more arrives for 100 ms.
    async fn published(&mut self) -> Vec<Topic>;
}

impl<S: Stream<Item = ManagerEvent> + Unpin> EventStreamExt for S {
    async fn published(&mut self) -> Vec<Topic> {
        let mut topics = Vec::new();
        while let Ok(Some(event)) = timeout(Duration::from_millis(100), self.next()).await {
            if let ManagerEvent::Changed { topic, .. } = event {
                topics.push(topic);
            }
        }
        topics
    }
}

/// Reads `current` every 50 ms until `wanted` holds for it, and fails the test after `longest`.
pub async fn wait_until<T: Debug>(
    longest: Duration,
    mut current: impl FnMut() -> T,
    wanted: impl Fn(&T) -> bool,
) -> T {
    let deadline = Instant::now()
        .checked_add(longest)
        .expect("the deadline fits");
    loop {
        let value = current();
        if wanted(&value) {
            return value;
        }
        assert!(Instant::now() < deadline, "still {value:?}");
        sleep(Duration::from_millis(50)).await;
    }
}
