mod apt_packages;
mod environment;
mod exec;
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

use apt_packages::APT_PACKAGES_VARIABLE;
use environment::{AccountDetails, EnvironmentOverride};
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
    #[error("{SUDO_POLICY_VARIABLE} must be \"full\" or \"off\" (unset means off), not {0:?}")]
    InvalidSudoPolicy(String),
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
        Ok(environment_overrides) => {
            exec::replace_process(program, arguments, &environment_overrides)
        }
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn prepare() -> Result<Vec<EnvironmentOverride>, InitError> {
    let sudo_policy =
        SudoPolicy::from_environment_value(env::var_os(SUDO_POLICY_VARIABLE).as_deref())?;
    let requested_packages = requested_apt_packages();
    let environment_overrides = if Uid::effective().is_root() {
        apt_packages::install_missing(&requested_packages);
        setup_scripts::run_all();
        become_agent_user(sudo_policy)?
    } else {
        adopt_invoking_user(sudo_policy, &requested_packages)?
    };
    sudo::restrict_process_tree(sudo_policy)?;
    warn_about_unwritable_directories();
    Ok(environment_overrides)
}

fn become_agent_user(sudo_policy: SudoPolicy) -> Result<Vec<EnvironmentOverride>, InitError> {
    let agent = look_up_user_by_name(AGENT_USER_NAME)?.ok_or(InitError::AgentUserMissing)?;
    if agent.uid.is_root() || agent.gid.as_raw() == 0 {
        return Err(InitError::AgentUserIsPrivileged);
    }

    let supplementary_groups = groups::supplementary_groups_for(&agent)?;
    stdio::hand_over_to(agent.uid);
    let environment_overrides = environment::agent_user_overrides(
        env::var_os("HOME").as_deref(),
        &AccountDetails::from(&agent),
    );
    sudo::configure_sudoers(sudo_policy, &agent.name);
    privileges::drop_to(&agent, &supplementary_groups)?;
    Ok(environment_overrides)
}

fn requested_apt_packages() -> Vec<String> {
    let environment_value = env::var_os(APT_PACKAGES_VARIABLE).unwrap_or_default();
    apt_packages::requested_packages(&environment_value.to_string_lossy())
}

fn adopt_invoking_user(
    sudo_policy: SudoPolicy,
    requested_packages: &[String],
) -> Result<Vec<EnvironmentOverride>, InitError> {
    let uid = Uid::effective();
    let invoking_user = User::from_uid(uid).map_err(|source| InitError::Lookup {
        subject: format!("uid {uid}"),
        source,
    })?;

    let environment_overrides = match invoking_user {
        Some(user) => environment::identity_overrides(&AccountDetails::from(&user)),
        None if environment::home_is_unusable(env::var_os("HOME").as_deref()) => {
            let home = environment::prepare_temporary_home(uid)?;
            tracing::info!(
                "uid {uid} has no passwd entry; using {} as HOME",
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
            "{APT_PACKAGES_VARIABLE} is ignored when the container starts as uid {uid}; not installed: {}",
            requested_packages.join(" ")
        );
    }
    warn_about_ignored_setup_scripts(uid);
    privileges::clear_inheritable_capabilities()?;
    Ok(environment_overrides)
}

fn warn_about_ignored_setup_scripts(uid: Uid) {
    let directory = Path::new(setup_scripts::SETUP_SCRIPTS_DIRECTORY);
    let has_scripts = setup_scripts::setup_scripts_in(directory)
        .is_ok_and(|scripts| !scripts.executable.is_empty());
    if has_scripts {
        tracing::warn!(
            "{} is ignored when the container starts as uid {uid}",
            directory.display()
        );
    }
}

fn look_up_user_by_name(user_name: &str) -> Result<Option<User>, InitError> {
    User::from_name(user_name).map_err(|source| InitError::Lookup {
        subject: format!("user {user_name:?}"),
        source,
    })
}

fn warn_about_unwritable_directories() {
    for directory in DIRECTORIES_AGENT_MUST_WRITE {
        if let Err(problem) = access(Path::new(directory), AccessFlags::W_OK) {
            tracing::warn!(
                "{directory} is not writable by uid {} ({problem}); fix the ownership or mount options of the mounted directory on the host",
                Uid::effective()
            );
        }
    }
}
