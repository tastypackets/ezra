use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use nix::sys::prctl;

use super::InitError;

pub const SUDO_POLICY_VARIABLE: &str = "AGENT_SUDO";
const SUDOERS_RULE_PATH: &str = "/etc/sudoers.d/agent-box";
// sudo skips files in sudoers.d whose names contain a dot, so a half-written file is never read.
const SUDOERS_RULE_STAGING_PATH: &str = "/etc/sudoers.d/.agent-box.new";
const SUDOERS_RULE_MODE: u32 = 0o440;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SudoPolicy {
    Full,
    Off,
}

impl SudoPolicy {
    pub fn from_environment_value(value: Option<&OsStr>) -> Result<Self, InitError> {
        match value.map(OsStr::as_encoded_bytes) {
            None | Some(b"" | b"off") => Ok(Self::Off),
            Some(b"full") => Ok(Self::Full),
            Some(_) => Err(InitError::InvalidSudoPolicy(
                value.unwrap_or_default().to_string_lossy().into_owned(),
            )),
        }
    }
}

/// Runs as root, before switching to the agent user.
pub fn configure_sudoers(policy: SudoPolicy, agent_user_name: &str) {
    let outcome = match policy {
        SudoPolicy::Full => write_sudoers_rule(&sudoers_rule_for(agent_user_name)),
        SudoPolicy::Off => remove_sudoers_rule(),
    };
    if let Err(error) = outcome {
        tracing::warn!("could not update {SUDOERS_RULE_PATH} ({error}); sudo is unavailable");
    }
}

/// Runs after switching to the agent user, so it also covers a non-root start.
pub fn restrict_process_tree(policy: SudoPolicy) -> Result<(), InitError> {
    match policy {
        SudoPolicy::Full => Ok(()),
        SudoPolicy::Off => prctl::set_no_new_privs().map_err(InitError::NoNewPrivileges),
    }
}

pub fn sudoers_rule_for(agent_user_name: &str) -> String {
    format!("{agent_user_name} ALL=(ALL:ALL) NOPASSWD: ALL\n")
}

fn write_sudoers_rule(rule: &str) -> io::Result<()> {
    let mut staging_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(SUDOERS_RULE_MODE)
        .open(SUDOERS_RULE_STAGING_PATH)?;
    staging_file.write_all(rule.as_bytes())?;
    staging_file.sync_all()?;
    fs::rename(SUDOERS_RULE_STAGING_PATH, SUDOERS_RULE_PATH)
}

fn remove_sudoers_rule() -> io::Result<()> {
    match fs::remove_file(Path::new(SUDOERS_RULE_PATH)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        outcome => outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_for(value: Option<&str>) -> Result<SudoPolicy, InitError> {
        SudoPolicy::from_environment_value(value.map(OsStr::new))
    }

    #[test]
    fn unset_or_empty_means_off() {
        assert_eq!(policy_for(None).ok(), Some(SudoPolicy::Off));
        assert_eq!(policy_for(Some("")).ok(), Some(SudoPolicy::Off));
    }

    #[test]
    fn full_and_off_are_accepted() {
        assert_eq!(policy_for(Some("full")).ok(), Some(SudoPolicy::Full));
        assert_eq!(policy_for(Some("off")).ok(), Some(SudoPolicy::Off));
    }

    #[test]
    fn anything_else_is_refused() {
        for value in ["FULL", "on", "true", "1", " full"] {
            assert!(policy_for(Some(value)).is_err(), "{value:?} was accepted");
        }
    }

    #[test]
    fn rule_grants_passwordless_sudo_to_the_agent() {
        assert_eq!(sudoers_rule_for("dev"), "dev ALL=(ALL:ALL) NOPASSWD: ALL\n");
    }
}
