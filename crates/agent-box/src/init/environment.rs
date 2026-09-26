use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nix::unistd::{Uid, User};

use super::InitError;

const PRIVATE_HOME_MODE: u32 = 0o700;

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

pub fn agent_user_overrides(
    current_home: Option<&OsStr>,
    agent: &AccountDetails,
) -> Vec<EnvironmentOverride> {
    let mut overrides = identity_overrides(agent);
    if current_home.is_none_or(|home| home == "/root") {
        overrides.push(EnvironmentOverride::new("HOME", &agent.home));
    }
    overrides
}

pub fn identity_overrides(account: &AccountDetails) -> Vec<EnvironmentOverride> {
    vec![
        EnvironmentOverride::new("USER", &account.name),
        EnvironmentOverride::new("LOGNAME", &account.name),
        EnvironmentOverride::new("SHELL", &account.shell),
    ]
}

pub fn home_is_unusable(current_home: Option<&OsStr>) -> bool {
    current_home.is_none_or(|home| home == "/")
}

pub fn prepare_private_home(uid: Uid) -> Result<PathBuf, InitError> {
    let home = std::env::temp_dir().join(format!("agent-box-home-{uid}"));
    ensure_private_directory(&home, uid).map_err(|source| InitError::TemporaryHome {
        path: home.display().to_string(),
        source,
    })?;
    Ok(home)
}

fn ensure_private_directory(directory: &Path, owner: Uid) -> io::Result<()> {
    match DirBuilder::new().mode(PRIVATE_HOME_MODE).create(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(directory)?;
            let is_private_and_ours = metadata.is_dir()
                && metadata.uid() == owner.as_raw()
                && metadata.permissions().mode() & 0o777 == PRIVATE_HOME_MODE;
            if is_private_and_ours {
                Ok(())
            } else {
                Err(io::Error::other(
                    "it already exists and is not a private directory owned by this uid",
                ))
            }
        }
        Err(error) => Err(error),
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
        let overrides = agent_user_overrides(Some(OsStr::new("/root")), &dev_account());

        assert_eq!(value_of(&overrides, "USER"), Some(OsStr::new("dev")));
        assert_eq!(value_of(&overrides, "LOGNAME"), Some(OsStr::new("dev")));
        assert_eq!(value_of(&overrides, "SHELL"), Some(OsStr::new("/bin/bash")));
    }

    #[test]
    fn agent_home_replaces_root_home() {
        let overrides = agent_user_overrides(Some(OsStr::new("/root")), &dev_account());

        assert_eq!(value_of(&overrides, "HOME"), Some(OsStr::new("/home/dev")));
    }

    #[test]
    fn agent_home_is_set_when_home_is_missing() {
        let overrides = agent_user_overrides(None, &dev_account());

        assert_eq!(value_of(&overrides, "HOME"), Some(OsStr::new("/home/dev")));
    }

    #[test]
    fn operator_home_is_kept() {
        let overrides = agent_user_overrides(Some(OsStr::new("/projects")), &dev_account());

        assert_eq!(value_of(&overrides, "HOME"), None);
    }

    #[test]
    fn identity_leaves_home_to_the_runtime() {
        let overrides = identity_overrides(&dev_account());

        assert_eq!(value_of(&overrides, "HOME"), None);
        assert_eq!(value_of(&overrides, "USER"), Some(OsStr::new("dev")));
    }

    #[test]
    fn filesystem_root_and_missing_home_are_unusable() {
        assert!(home_is_unusable(None));
        assert!(home_is_unusable(Some(OsStr::new("/"))));
        assert!(!home_is_unusable(Some(OsStr::new("/home/dev"))));
    }
}
