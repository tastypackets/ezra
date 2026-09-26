use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Mutex, watch};

use super::agents::{Agent, InstallPaths, InstallProgress, TlsVerification};
use super::auth::Sessions;
use super::checks::AgentChecks;
use super::environment::EnvironmentSettings;
use super::events::Events;
use super::folders::{PROJECTS_DIRECTORY, ProjectsDirectory};
use super::git::GitTools;
use super::login::LoginProcess;
use super::remote_control::RemoteControl;
use super::settings::{Settings, SettingsError};
use super::tls::ServedCertificate;
use super::updates::LatestRelease;

/// Shared by the API and the pages.
#[derive(Clone)]
pub struct AppState {
    pub settings_path: Arc<PathBuf>,
    pub settings: Arc<Mutex<Settings>>,
    pub sessions: Arc<Sessions>,
    pub install_paths: Arc<InstallPaths>,
    pub agent_checks: Arc<AgentChecks>,
    pub install_lock: Arc<Mutex<()>>,
    pub logins: Arc<Mutex<HashMap<Agent, LoginProcess>>>,
    pub download_tls_verification: TlsVerification,
    pub installs_in_progress: Arc<Mutex<HashMap<Agent, Arc<InstallProgress>>>>,
    pub latest_releases: Arc<Mutex<HashMap<Agent, LatestRelease>>>,
    pub git_tools: Arc<GitTools>,
    pub github_login: Arc<Mutex<Option<LoginProcess>>>,
    pub remote_control: Arc<RemoteControl>,
    pub events: Events,
    /// Absent in tests, which serve plain HTTP.
    pub certificate: Option<ServedCertificate>,
    pub environment: EnvironmentSettings,
    /// The GitHub account gh is signed in as, from the last check. None when signed out.
    pub github_account: Arc<watch::Sender<Option<String>>>,
    pub projects: ProjectsDirectory,
}

impl AppState {
    pub fn new(
        settings_path: PathBuf,
        settings: Settings,
        install_paths: InstallPaths,
        download_tls_verification: TlsVerification,
        git_tools: GitTools,
    ) -> Self {
        let events = Events::default();
        let install_paths = Arc::new(install_paths);
        Self {
            settings_path: Arc::new(settings_path),
            settings: Arc::new(Mutex::new(settings)),
            sessions: Arc::default(),
            agent_checks: Arc::new(AgentChecks::new(Arc::clone(&install_paths), events.clone())),
            install_paths,
            install_lock: Arc::default(),
            logins: Arc::default(),
            download_tls_verification,
            installs_in_progress: Arc::default(),
            latest_releases: Arc::default(),
            git_tools: Arc::new(git_tools),
            github_login: Arc::default(),
            remote_control: Arc::new(RemoteControl::new(events.clone())),
            events,
            certificate: None,
            environment: EnvironmentSettings::default(),
            github_account: Arc::new(watch::Sender::new(None)),
            projects: ProjectsDirectory(PathBuf::from(PROJECTS_DIRECTORY)),
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
        if updated_settings != *settings {
            updated_settings.save(&self.settings_path)?;
            *settings = updated_settings;
        }
        Ok(())
    }

    pub fn is_session(&self, token: Option<&str>) -> bool {
        token.is_some_and(|token| self.sessions.is_active(token))
    }
}
