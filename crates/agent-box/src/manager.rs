mod agents;
mod api;
mod auth;
mod login;
mod pages;
mod settings;
mod state;
mod status;
mod tls;

use std::env;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use axum_server::Handle;
use axum_server::tls_rustls::RustlsConfig;
use tokio::signal::unix::{SignalKind, signal};

const STATE_DIRECTORY: &str = "/config/agent-box";
const PORT_VARIABLE: &str = "MANAGER_PORT";
const DEFAULT_PORT: u16 = 8443;
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("{PORT_VARIABLE} must be a port number, not {0:?}")]
    InvalidPort(String),
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
    let ManagerOptions { port } = ManagerOptions::from_environment()?;
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
    tokio::spawn(handle.clone().shut_down_on_signal());
    tracing::info!("manager listening on https://{hostname}:{port}");
    if !is_claimed {
        tracing::info!("no password is set yet; the first visitor chooses it");
    }
    let home = env::var_os("HOME").unwrap_or_else(|| "/home/dev".into());
    let state = state::AppState::new(
        settings_path,
        settings,
        agents::InstallPaths::under_home(Path::new(&home))
            .with_config_directories_from_environment(),
    );
    tokio::spawn(state.clone().reinstall_configured_agents());
    let app = api::router(state.clone()).merge(pages::router(state));
    axum_server::bind_rustls(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)), tls_config)
        .handle(handle)
        .serve(app.into_make_service())
        .await
        .map_err(|source| ManagerError::Serve { port, source })
}

/// How the manager is started, from its environment.
struct ManagerOptions {
    port: u16,
}

impl ManagerOptions {
    fn from_environment() -> Result<Self, ManagerError> {
        let port = match env::var(PORT_VARIABLE) {
            Ok(value) => value
                .parse()
                .map_err(|_| ManagerError::InvalidPort(value))?,
            Err(_) => DEFAULT_PORT,
        };
        Ok(Self { port })
    }
}

trait HandleExt {
    /// Stops accepting connections on SIGTERM or Ctrl-C and gives open ones a few seconds.
    async fn shut_down_on_signal(self);
}

impl HandleExt for Handle<SocketAddr> {
    async fn shut_down_on_signal(self) {
        let Ok(mut terminate) = signal(SignalKind::terminate()) else {
            return;
        };
        tokio::select! {
            _ = terminate.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        self.graceful_shutdown(Some(SHUTDOWN_GRACE_PERIOD));
    }
}
