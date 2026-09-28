use std::path::Path;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::agents::TlsVerification;
use super::git::GitTools;
use crate::environment_config::FromEnvironment;
use crate::init::config::InitConfig;
use crate::init::setup_scripts::{SETUP_SCRIPTS_DIRECTORY, SetupScripts};
use crate::init::sudo::SudoPolicy;

/// The container settings that come from its environment, read once at start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentSettings {
    /// The container's hostname.
    pub hostname: String,
    /// `EZRA_PORT`, the port the manager listens on inside the container.
    pub port: u16,
    /// `EZRA_TLS_VERIFY`, whether agent downloads check the server's certificate.
    pub tls_verification: bool,
    /// `EZRA_SUDO`, whether agents may use sudo.
    pub sudo: bool,
    /// `EZRA_APT_PACKAGES`, installed at every start.
    pub apt_packages: Vec<String>,
    /// Scripts in /etc/ezra/setup.d, run as root at every start.
    pub setup_scripts: Vec<String>,
    /// `GH_HOST`, the GitHub host gh signs in to, github.com when unset.
    pub github_host: String,
    /// A variable in `github_token_variables` is set.
    pub github_token: bool,
    /// The variables gh takes a token from for `github_host`.
    pub github_token_variables: Vec<String>,
}

impl EnvironmentSettings {
    pub fn read(
        hostname: &str,
        port: u16,
        tls_verification: TlsVerification,
        git_tools: &GitTools,
    ) -> Self {
        let init = InitConfig::from_environment().unwrap_or_default();
        let setup_scripts = SetupScripts::in_directory(Path::new(SETUP_SCRIPTS_DIRECTORY))
            .map(|scripts| {
                scripts
                    .executable
                    .iter()
                    .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            hostname: hostname.to_owned(),
            port,
            tls_verification: tls_verification == TlsVerification::On,
            sudo: init.sudo_policy == SudoPolicy::Full,
            apt_packages: init.apt_packages.names().to_vec(),
            setup_scripts,
            github_host: git_tools.host().to_string(),
            github_token: git_tools.token_from_environment(),
            github_token_variables: Vec::from(
                git_tools.host().token_variables().map(str::to_owned),
            ),
        }
    }
}
