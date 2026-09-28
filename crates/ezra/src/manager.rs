mod agents;
mod api;
mod auth;
mod checks;
mod clones;
mod codex_remote;
mod environment;
mod events;
mod folders;
mod git;
mod github_host;
mod https_redirect;
mod login;
mod processes;
mod remote_control;
mod settings;
mod settings_file;
mod state;
mod status;
mod supervision;
mod tls;
mod updates;
mod watcher;
mod web;

use std::env;
use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use axum_server::Handle;
use serde::Deserialize;
use tokio::signal::unix::{SignalKind, signal};

use crate::environment_config::FromEnvironment;
use agents::TlsVerification;
pub use folders::PROJECTS_DIRECTORY;
use processes::OpenFileLimit;
use remote_control::RemoteControl;
use settings_file::SettingsFileWatcher;
use state::AppState;

const STATE_DIRECTORY: &str = "/config/ezra";
const TLS_VERIFY_VARIABLE: &str = "EZRA_TLS_VERIFY";
const DEFAULT_PORT: u16 = 8443;
const DEFAULT_WEB_DIRECTORY: &str = "/usr/local/share/ezra/web";
const AGENT_VERSIONS_DIRECTORY: &str = "/cache/agents";
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("invalid environment: {0}")]
    Configuration(#[from] config::ConfigError),
    #[error(transparent)]
    Settings(#[from] settings::SettingsError),
    #[error("could not prepare the TLS certificate: {0}")]
    Certificate(io::Error),
    #[error("could not serve on port {port}: {source}")]
    Serve { port: u16, source: io::Error },
}

#[tokio::main(flavor = "current_thread")]
pub async fn run() -> ExitCode {
    match serve().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn serve() -> Result<(), ManagerError> {
    match OpenFileLimit::current().and_then(OpenFileLimit::raise) {
        Ok(limit) => tracing::info!("agent servers can open {} files", limit.soft),
        Err(error) => tracing::warn!("could not raise the open file limit: {error}"),
    }
    let ManagerOptions {
        port,
        tls_verification,
        web_directory,
    } = ManagerOptions::from_environment()?;
    if tls_verification == TlsVerification::Off {
        tracing::warn!(
            "{TLS_VERIFY_VARIABLE}=off, so agent downloads skip certificate verification"
        );
    }
    let state_directory = Path::new(STATE_DIRECTORY);
    let settings_path = state_directory.join("settings.toml");
    let settings = settings::Settings::load(&settings_path)?;
    let is_claimed = settings.manager.password_hash.is_some();

    let hostname = nix::unistd::gethostname()
        .map(|hostname| hostname.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "localhost".to_owned());
    let certificate = tls::ServedCertificate::load(&state_directory.join("tls"), &hostname)
        .await
        .map_err(ManagerError::Certificate)?;

    let handle = Handle::new();
    tracing::info!("manager listening on https://{hostname}:{port}");
    if !is_claimed {
        tracing::info!("no password is set, the first visitor chooses it");
    }
    let home = env::var_os("HOME").unwrap_or_else(|| "/home/dev".into());
    let git_tools = git::GitTools::from_environment(Path::new(&home));
    let mut state = AppState::new(
        settings_path,
        settings,
        agents::InstallPaths::under_home(Path::new(&home))
            .with_versions_in(PathBuf::from(AGENT_VERSIONS_DIRECTORY))
            .with_config_directories_from_environment(),
        tls_verification,
        git_tools.clone(),
    );
    state.environment =
        environment::EnvironmentSettings::read(&hostname, port, tls_verification, &git_tools);
    let tls_config = certificate.config.clone();
    state.certificate = Some(certificate);
    let projects = state.projects.clone();
    if let Err(error) = tokio::task::spawn_blocking(move || projects.remove_unfinished_clones())
        .await
        .map_err(io::Error::other)
        .and_then(|removed| removed)
    {
        tracing::warn!("could not remove unfinished clones: {error}");
    }
    tokio::spawn(state.clone().reinstall_configured_agents());
    tokio::spawn(state.clone().check_for_updates_regularly());
    tokio::spawn(Arc::clone(&state.agent_checks).check_regularly());
    tokio::spawn(state.clone().prepare_git_at_start());
    tokio::spawn(state.clone().supervise_remote_control());
    tokio::spawn(state.clone().supervise_codex_remote());
    tokio::spawn(state.clone().describe_folders_regularly());
    let (install_paths, events) = (Arc::clone(&state.install_paths), state.events.clone());
    tokio::spawn(async move {
        SettingsFileWatcher::start(&install_paths)
            .await
            .publish_changes(events)
            .await;
    });
    tokio::spawn(handle.clone().shut_down_on_signal(state.clone()));
    let remote_control = Arc::clone(&state.remote_control);
    let codex_remote = Arc::clone(&state.codex_remote);
    let app = state.into_router(web_directory);
    let served = axum_server::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
        .acceptor(https_redirect::HttpsRedirectAcceptor::new(tls_config))
        .handle(handle)
        .serve(app.into_make_service())
        .await
        .map_err(|source| ManagerError::Serve { port, source });
    let (claude_stopped, codex_stopped) = tokio::join!(
        remote_control
            .supervision
            .wait_until_stopped(RemoteControl::LONGEST_STOP),
        codex_remote
            .supervision
            .wait_until_stopped(codex_remote.budget.longest_stop()),
    );
    if !claude_stopped {
        tracing::warn!("Claude Remote Control did not stop in time");
    }
    if !codex_stopped {
        tracing::warn!("Codex remote control did not stop in time");
    }
    served
}

impl AppState {
    /// The API and the web app.
    fn into_router(self, web_directory: PathBuf) -> axum::Router {
        api::router(self.clone()).merge(web::router(self, web_directory))
    }
}

/// How the manager is started, read from the environment.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct ManagerOptions {
    port: u16,
    #[serde(rename = "tls_verify")]
    tls_verification: TlsVerification,
    /// The built web app, served from disk.
    #[serde(rename = "web_dir")]
    web_directory: PathBuf,
}

impl Default for ManagerOptions {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            tls_verification: TlsVerification::default(),
            web_directory: PathBuf::from(DEFAULT_WEB_DIRECTORY),
        }
    }
}

impl FromEnvironment for ManagerOptions {}

trait HandleExt {
    /// On SIGTERM or Ctrl-C, stops both agents' remote control, ends event streams and gives open
    /// connections a few seconds.
    async fn shut_down_on_signal(self, state: AppState);
}

impl HandleExt for Handle<SocketAddr> {
    async fn shut_down_on_signal(self, state: AppState) {
        let Ok(mut terminate) = signal(SignalKind::terminate()) else {
            return;
        };
        tokio::select! {
            _ = terminate.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        state.remote_control.supervision.begin_shut_down();
        state.codex_remote.supervision.begin_shut_down();
        state.events.close();
        self.graceful_shutdown(Some(SHUTDOWN_GRACE_PERIOD));
    }
}

/// Writes the API description the web client is generated from.
pub fn write_openapi(output: &Path) -> ExitCode {
    let written = api::ApiDoc::to_json()
        .map_err(io::Error::other)
        .and_then(|json| fs::write(output, json));
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("could not write {}: {error}", output.display());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment_config::variables;

    fn options(pairs: &[(&str, &str)]) -> Result<ManagerOptions, config::ConfigError> {
        ManagerOptions::from_variables(variables(pairs))
    }

    #[test]
    fn defaults_are_port_8443_with_certificate_checks() {
        assert_eq!(
            options(&[]).expect("defaults load"),
            ManagerOptions {
                port: 8443,
                tls_verification: TlsVerification::On,
                web_directory: PathBuf::from("/usr/local/share/ezra/web"),
            }
        );
    }

    #[test]
    fn port_and_certificate_checks_are_typed() {
        let custom = options(&[("EZRA_PORT", "9443"), ("EZRA_TLS_VERIFY", "off")])
            .expect("valid values load");
        assert_eq!(custom.port, 9443);
        assert_eq!(custom.tls_verification, TlsVerification::Off);
        for (name, value) in [
            ("EZRA_PORT", "eighty"),
            ("EZRA_PORT", "70000"),
            ("EZRA_TLS_VERIFY", "maybe"),
        ] {
            assert!(
                options(&[(name, value)]).is_err(),
                "{name}={value} was accepted"
            );
        }
    }
}
