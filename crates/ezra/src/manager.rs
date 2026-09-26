mod agents;
mod api;
mod auth;
mod git;
mod login;
mod remote_control;
mod settings;
mod state;
mod status;
mod tls;
mod updates;
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
use axum_server::tls_rustls::RustlsConfig;
use serde::Deserialize;
use tokio::signal::unix::{SignalKind, signal};

use crate::environment_config::FromEnvironment;
use agents::TlsVerification;
use remote_control::RemoteControl;

const STATE_DIRECTORY: &str = "/config/ezra";
const TLS_VERIFY_VARIABLE: &str = "EZRA_TLS_VERIFY";
const DEFAULT_PORT: u16 = 8443;
const DEFAULT_WEB_DIRECTORY: &str = "/usr/local/share/ezra/web";
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
    let certificate_files =
        tls::CertificateFiles::ensure_self_signed(&state_directory.join("tls"), &hostname)
            .map_err(ManagerError::Certificate)?;
    let tls_config =
        RustlsConfig::from_pem_file(&certificate_files.certificate, &certificate_files.key)
            .await
            .map_err(ManagerError::Certificate)?;

    let handle = Handle::new();
    tracing::info!("manager listening on https://{hostname}:{port}");
    if !is_claimed {
        tracing::info!("no password is set, the first visitor chooses it");
    }
    let home = env::var_os("HOME").unwrap_or_else(|| "/home/dev".into());
    let state = state::AppState::new(
        settings_path,
        settings,
        agents::InstallPaths::under_home(Path::new(&home))
            .with_config_directories_from_environment(),
        tls_verification,
        git::GitTools::from_environment(Path::new(&home)),
    );
    tokio::spawn(state.clone().reinstall_configured_agents());
    tokio::spawn(state.clone().check_for_updates_regularly());
    tokio::spawn(state.clone().lend_github_sign_in_at_start());
    tokio::spawn(state.clone().supervise_remote_control());
    tokio::spawn(
        handle
            .clone()
            .shut_down_on_signal(Arc::clone(&state.remote_control)),
    );
    let remote_control = Arc::clone(&state.remote_control);
    let app = state.into_router(web_directory);
    let served =
        axum_server::bind_rustls(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)), tls_config)
            .handle(handle)
            .serve(app.into_make_service())
            .await
            .map_err(|source| ManagerError::Serve { port, source });
    remote_control.wait_until_stopped().await;
    served
}

impl state::AppState {
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
    /// On SIGTERM or Ctrl-C, stops Remote Control and gives open connections a few seconds.
    async fn shut_down_on_signal(self, remote_control: Arc<RemoteControl>);
}

impl HandleExt for Handle<SocketAddr> {
    async fn shut_down_on_signal(self, remote_control: Arc<RemoteControl>) {
        let Ok(mut terminate) = signal(SignalKind::terminate()) else {
            return;
        };
        tokio::select! {
            _ = terminate.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        remote_control.begin_shut_down();
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
