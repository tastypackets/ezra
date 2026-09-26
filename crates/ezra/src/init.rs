mod apt_packages;
mod config;
mod environment;
mod exec;
mod git;
mod groups;
mod privileges;
mod setup_scripts;
mod stdio;
mod sudo;

use std::env;
use std::ffi::{NulError, OsStr, OsString};
use std::path::Path;
use std::process::ExitCode;

use nix::errno::Errno;
use nix::unistd::{AccessFlags, Uid, User, access};

use crate::environment_config::FromEnvironment;
use apt_packages::{APT_PACKAGES_VARIABLE, RequestedPackages};
use config::InitConfig;
use environment::{AccountDetails, EnvironmentOverride, TemporaryHome};
use exec::Program;
use git::GlobalGitConfig;
use groups::SupplementaryGroups;
use privileges::{Capabilities, UserExt};
use setup_scripts::SetupScripts;
use stdio::StandardStreams;
use sudo::{SUDO_POLICY_VARIABLE, SudoPolicy};

const AGENT_USER_NAME: &str = "dev";
const DIRECTORIES_AGENT_MUST_WRITE: [&str; 2] = ["/config", "/projects"];

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("user {AGENT_USER_NAME:?} does not exist in /etc/passwd")]
    AgentUserMissing,
    #[error("user {AGENT_USER_NAME:?} must not have uid 0 or gid 0")]
    AgentUserIsPrivileged,
    #[error("could not look up {subject}: {source}")]
    Lookup { subject: String, source: Errno },
    #[error("user name {0:?} contains a NUL byte")]
    InvalidUserName(#[from] NulError),
    #[error(
        "could not switch to {AGENT_USER_NAME}: {step} failed: {source}; \
         if the container runs without CAP_SETUID/CAP_SETGID, start it with --user 1000:1000 instead"
    )]
    DropPrivileges { step: &'static str, source: Errno },
    #[error("root privileges could still be regained after switching to {AGENT_USER_NAME}")]
    PrivilegesStillRecoverable,
    #[error("could not clear the inheritable capability set: {0}")]
    ClearCapabilities(#[from] caps::errors::CapsError),
    #[error("invalid environment: {0}")]
    Configuration(#[from] ::config::ConfigError),
    #[error("could not set no_new_privs: {0}")]
    NoNewPrivileges(Errno),
    #[error("could not prepare {path} as HOME: {source}")]
    TemporaryHome {
        path: String,
        source: std::io::Error,
    },
}

pub fn run(program: &OsStr, arguments: &[OsString]) -> ExitCode {
    match prepare() {
        Ok(environment_overrides) => Program {
            name: program,
            arguments,
        }
        .replace_current_process(&environment_overrides),
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn prepare() -> Result<Vec<EnvironmentOverride>, InitError> {
    let InitConfig {
        sudo_policy,
        apt_packages: requested_packages,
    } = InitConfig::from_environment()?;
    let environment_overrides = if Uid::effective().is_root() {
        requested_packages.install_missing();
        SetupScripts::run_all();
        become_agent_user(sudo_policy)?
    } else {
        adopt_invoking_user(sudo_policy, &requested_packages)?
    };
    sudo_policy.restrict_process_tree()?;
    warn_about_unwritable_directories();
    if let Some(git_config) = GlobalGitConfig::from_environment() {
        git_config.create_directory();
    }
    Ok(environment_overrides)
}

fn become_agent_user(sudo_policy: SudoPolicy) -> Result<Vec<EnvironmentOverride>, InitError> {
    let agent = User::look_up_by_name(AGENT_USER_NAME)?.ok_or(InitError::AgentUserMissing)?;
    if agent.uid.is_root() || agent.gid.as_raw() == 0 {
        return Err(InitError::AgentUserIsPrivileged);
    }

    let supplementary_groups = SupplementaryGroups::for_agent(&agent)?;
    StandardStreams::hand_over_to(agent.uid);
    let environment_overrides =
        AccountDetails::from(&agent).agent_overrides(env::var_os("HOME").as_deref());
    sudo_policy.configure_sudoers(&agent.name);
    agent.switch_process_to(&supplementary_groups)?;
    Ok(environment_overrides)
}

fn adopt_invoking_user(
    sudo_policy: SudoPolicy,
    requested_packages: &RequestedPackages,
) -> Result<Vec<EnvironmentOverride>, InitError> {
    let uid = Uid::effective();
    let invoking_user = User::from_uid(uid).map_err(|source| InitError::Lookup {
        subject: format!("uid {uid}"),
        source,
    })?;

    let environment_overrides = match invoking_user {
        Some(user) => AccountDetails::from(&user).identity_overrides(),
        None if TemporaryHome::is_needed(env::var_os("HOME").as_deref()) => {
            let home = TemporaryHome::prepare(uid)?;
            tracing::info!(
                "uid {uid} has no passwd entry, so HOME is {}",
                home.display()
            );
            vec![EnvironmentOverride::new("HOME", home)]
        }
        None => Vec::new(),
    };

    if sudo_policy == SudoPolicy::Full {
        tracing::info!(
            "{SUDO_POLICY_VARIABLE}=full has no effect when the container starts as uid {uid}"
        );
    }
    if !requested_packages.is_empty() {
        tracing::warn!(
            "{APT_PACKAGES_VARIABLE} is ignored when the container starts as uid {uid}. Not installed: {}",
            requested_packages.names().join(" ")
        );
    }
    SetupScripts::warn_if_ignored(uid);
    Capabilities::clear_inheritable()?;
    Ok(environment_overrides)
}

fn warn_about_unwritable_directories() {
    for directory in DIRECTORIES_AGENT_MUST_WRITE {
        if let Err(problem) = access(Path::new(directory), AccessFlags::W_OK) {
            tracing::warn!(
                "{directory} is not writable by uid {} ({problem}). Check its ownership and mount options on the host.",
                Uid::effective()
            );
        }
    }
}
