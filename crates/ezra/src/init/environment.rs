use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::PathBuf;

use nix::unistd::{Uid, User};

use super::InitError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentOverride {
    pub name: &'static str,
    pub value: OsString,
}

impl EnvironmentOverride {
    pub fn new(name: &'static str, value: impl Into<OsString>) -> Self {
        Self {
            name,
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDetails {
    pub name: String,
    pub home: PathBuf,
    pub shell: PathBuf,
}

impl From<&User> for AccountDetails {
    fn from(user: &User) -> Self {
        Self {
            name: user.name.clone(),
            home: user.dir.clone(),
            shell: user.shell.clone(),
        }
    }
}

impl AccountDetails {
    /// The agent's identity, plus its home unless the operator chose one.
    pub fn agent_overrides(&self, current_home: Option<&OsStr>) -> Vec<EnvironmentOverride> {
        let mut overrides = self.identity_overrides();
        if current_home.is_none_or(|home| home == "/root") {
            overrides.push(EnvironmentOverride::new("HOME", &self.home));
        }
        overrides
    }

    pub fn identity_overrides(&self) -> Vec<EnvironmentOverride> {
        vec![
            EnvironmentOverride::new("USER", &self.name),
            EnvironmentOverride::new("LOGNAME", &self.name),
            EnvironmentOverride::new("SHELL", &self.shell),
        ]
    }
}

/// A private HOME under /tmp for a uid the image has no account for.
pub struct TemporaryHome;

impl TemporaryHome {
    pub fn is_needed(current_home: Option<&OsStr>) -> bool {
        current_home.is_none_or(|home| home == "/")
    }

    pub fn prepare(uid: Uid) -> Result<PathBuf, InitError> {
        let home = std::env::temp_dir().join(format!("ezra-home-{uid}"));
        fs::create_dir_all(&home).map_err(|source| InitError::TemporaryHome {
            path: home.display().to_string(),
            source,
        })?;
        Ok(home)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev_account() -> AccountDetails {
        AccountDetails {
            name: "dev".to_owned(),
            home: PathBuf::from("/home/dev"),
            shell: PathBuf::from("/bin/bash"),
        }
    }

    fn value_of<'a>(overrides: &'a [EnvironmentOverride], name: &str) -> Option<&'a OsStr> {
        overrides
            .iter()
            .find(|variable| variable.name == name)
            .map(|variable| variable.value.as_os_str())
    }

    #[test]
    fn agent_gets_its_identity() {
        let overrides = dev_account().agent_overrides(Some(OsStr::new("/root")));

        assert_eq!(value_of(&overrides, "USER"), Some(OsStr::new("dev")));
        assert_eq!(value_of(&overrides, "LOGNAME"), Some(OsStr::new("dev")));
        assert_eq!(value_of(&overrides, "SHELL"), Some(OsStr::new("/bin/bash")));
    }

    #[test]
    fn agent_home_replaces_root_home() {
        let overrides = dev_account().agent_overrides(Some(OsStr::new("/root")));

        assert_eq!(value_of(&overrides, "HOME"), Some(OsStr::new("/home/dev")));
    }

    #[test]
    fn agent_home_is_set_when_home_is_missing() {
        let overrides = dev_account().agent_overrides(None);

        assert_eq!(value_of(&overrides, "HOME"), Some(OsStr::new("/home/dev")));
    }

    #[test]
    fn operator_home_is_kept() {
        let overrides = dev_account().agent_overrides(Some(OsStr::new("/projects")));

        assert_eq!(value_of(&overrides, "HOME"), None);
    }

    #[test]
    fn identity_leaves_home_to_the_runtime() {
        let overrides = dev_account().identity_overrides();

        assert_eq!(value_of(&overrides, "HOME"), None);
        assert_eq!(value_of(&overrides, "USER"), Some(OsStr::new("dev")));
    }

    #[test]
    fn filesystem_root_and_missing_home_are_unusable() {
        assert!(TemporaryHome::is_needed(None));
        assert!(TemporaryHome::is_needed(Some(OsStr::new("/"))));
        assert!(!TemporaryHome::is_needed(Some(OsStr::new("/home/dev"))));
    }
}
