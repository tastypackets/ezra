use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::agents::{Agent, InstallPaths, InstallProgress, TlsVerification};
use super::auth::Sessions;
use super::login::LoginProcess;
use super::settings::{Settings, SettingsError};

/// Shared by the API and the pages.
#[derive(Clone)]
pub struct AppState {
    pub settings_path: Arc<PathBuf>,
    pub settings: Arc<Mutex<Settings>>,
    pub sessions: Arc<Sessions>,
    pub install_paths: Arc<InstallPaths>,
    pub install_lock: Arc<Mutex<()>>,
    pub logins: Arc<Mutex<HashMap<Agent, LoginProcess>>>,
    pub download_tls_verification: TlsVerification,
    pub installs_in_progress: Arc<Mutex<HashMap<Agent, Arc<InstallProgress>>>>,
    pub latest_versions: Arc<Mutex<HashMap<Agent, String>>>,
}

impl AppState {
    pub fn new(
        settings_path: PathBuf,
        settings: Settings,
        install_paths: InstallPaths,
        download_tls_verification: TlsVerification,
    ) -> Self {
        Self {
            settings_path: Arc::new(settings_path),
            settings: Arc::new(Mutex::new(settings)),
            sessions: Arc::default(),
            install_paths: Arc::new(install_paths),
            install_lock: Arc::default(),
            logins: Arc::default(),
            download_tls_verification,
            installs_in_progress: Arc::default(),
            latest_versions: Arc::default(),
        }
    }

    /// Applies and saves a change, which becomes visible only once it is on disk.
    pub async fn update_settings<E: From<SettingsError>>(
        &self,
        change: impl FnOnce(&mut Settings) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut settings = self.settings.lock().await;
        let mut updated_settings = settings.clone();
        change(&mut updated_settings)?;
        updated_settings.save(&self.settings_path)?;
        *settings = updated_settings;
        Ok(())
    }

    pub fn is_session(&self, token: Option<&str>) -> bool {
        token.is_some_and(|token| self.sessions.is_active(token))
    }
}
