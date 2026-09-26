use std::ffi::OsStr;
use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use nix::sys::prctl;

use super::InitError;
use crate::path_ext::PathExt;

pub const SUDO_POLICY_VARIABLE: &str = "AGENT_SUDO";
const SUDOERS_RULE_PATH: &str = "/etc/sudoers.d/agent-box";
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

impl SudoPolicy {
    /// Runs as root, before switching to the agent user.
    pub fn configure_sudoers(self, agent_user_name: &str) {
        let outcome = match self {
            Self::Full => Self::write_sudoers_rule(&Self::sudoers_rule_for(agent_user_name)),
            Self::Off => Self::remove_sudoers_rule(),
        };
        if let Err(error) = outcome {
            tracing::warn!("could not update {SUDOERS_RULE_PATH} ({error}); sudo is unavailable");
        }
    }

    /// Runs after switching to the agent user, so it also covers a non-root start.
    pub fn restrict_process_tree(self) -> Result<(), InitError> {
        match self {
            Self::Full => Ok(()),
            Self::Off => prctl::set_no_new_privs().map_err(InitError::NoNewPrivileges),
        }
    }

    fn sudoers_rule_for(agent_user_name: &str) -> String {
        format!("{agent_user_name} ALL=(ALL:ALL) NOPASSWD: ALL\n")
    }

    fn write_sudoers_rule(rule: &str) -> io::Result<()> {
        fs::write(SUDOERS_RULE_PATH, rule)?;
        fs::set_permissions(SUDOERS_RULE_PATH, Permissions::from_mode(SUDOERS_RULE_MODE))
    }

    fn remove_sudoers_rule() -> io::Result<()> {
        Path::new(SUDOERS_RULE_PATH).remove_if_present()
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
        assert_eq!(
            SudoPolicy::sudoers_rule_for("dev"),
            "dev ALL=(ALL:ALL) NOPASSWD: ALL\n"
        );
    }
}
