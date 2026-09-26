mod agents;
mod api;
mod auth;
mod settings;
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
    let port = port_from_environment()?;
    let state_directory = Path::new(STATE_DIRECTORY);
    let settings_path = state_directory.join("settings.toml");
    let settings = settings::load(&settings_path)?;
    let is_claimed = settings.manager.password_hash.is_some();

    let hostname = nix::unistd::gethostname()
        .map(|hostname| hostname.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "localhost".to_owned());
    let certificate_files = tls::ensure_certificate(&state_directory.join("tls"), &hostname)
        .map_err(ManagerError::Certificate)?;
    let tls_config =
        RustlsConfig::from_pem_file(&certificate_files.certificate, &certificate_files.key)
            .await
            .map_err(ManagerError::Certificate)?;

    let handle = Handle::new();
    tokio::spawn(shut_down_on_signal(handle.clone()));
    tracing::info!("manager listening on https://{hostname}:{port}");
    if !is_claimed {
        tracing::info!("no password is set yet; the first visitor chooses it");
    }
    let home = env::var_os("HOME").unwrap_or_else(|| "/home/dev".into());
    let state = api::AppState::new(
        settings_path,
        settings,
        agents::InstallPaths::under_home(Path::new(&home)),
    );
    tokio::spawn(api::reinstall_configured_agents(state.clone()));
    let app = api::router(state);
    axum_server::bind_rustls(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)), tls_config)
        .handle(handle)
        .serve(app.into_make_service())
        .await
        .map_err(|source| ManagerError::Serve { port, source })
}

fn port_from_environment() -> Result<u16, ManagerError> {
    match env::var(PORT_VARIABLE) {
        Ok(value) => value.parse().map_err(|_| ManagerError::InvalidPort(value)),
        Err(_) => Ok(DEFAULT_PORT),
    }
}

async fn shut_down_on_signal(handle: Handle<SocketAddr>) {
    let Ok(mut terminate) = signal(SignalKind::terminate()) else {
        return;
    };
    tokio::select! {
        _ = terminate.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    handle.graceful_shutdown(Some(Duration::from_secs(5)));
}
