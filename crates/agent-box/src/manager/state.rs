use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::agents::{Agent, InstallPaths};
use super::auth::Sessions;
use super::login::LoginProcess;
use super::settings::Settings;

/// Shared by the API and the pages.
#[derive(Clone)]
pub struct AppState {
    pub settings_path: Arc<PathBuf>,
    pub settings: Arc<Mutex<Settings>>,
    pub sessions: Arc<Sessions>,
    pub install_paths: Arc<InstallPaths>,
    pub install_lock: Arc<Mutex<()>>,
    pub logins: Arc<Mutex<HashMap<Agent, LoginProcess>>>,
}

impl AppState {
    pub fn new(settings_path: PathBuf, settings: Settings, install_paths: InstallPaths) -> Self {
        Self {
            settings_path: Arc::new(settings_path),
            settings: Arc::new(Mutex::new(settings)),
            sessions: Arc::default(),
            install_paths: Arc::new(install_paths),
            install_lock: Arc::default(),
            logins: Arc::default(),
        }
    }

    pub fn is_session(&self, token: Option<&str>) -> bool {
        token.is_some_and(|token| self.sessions.is_active(token))
    }
}
