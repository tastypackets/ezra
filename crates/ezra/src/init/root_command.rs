use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::process::Command;

const ROOT_PATH: &str =
    "/usr/local/share/ezra/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const ROOT_HOME: &str = "/root";
const ROOT_TEMPORARY_DIRECTORY: &str = "/root/tmp";
/// Variables that point programs at configuration the agent can write.
const AGENT_CONFIG_VARIABLES: [&str; 8] = [
    "MISE_DATA_DIR",
    "MISE_CONFIG_DIR",
    "MISE_STATE_DIR",
    "MISE_TRUSTED_CONFIG_PATHS",
    "GIT_CONFIG_GLOBAL",
    "GH_CONFIG_DIR",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
];

pub trait RootCommandExt {
    /// A system PATH, root's home and temporary directory, `/` as the working directory and none
    /// of the agent's configuration variables.
    fn apart_from_agent(&mut self) -> &mut Self;
}

impl RootCommandExt for Command {
    fn apart_from_agent(&mut self) -> &mut Self {
        self.env("PATH", ROOT_PATH)
            .env("HOME", ROOT_HOME)
            .env("TMPDIR", ROOT_TEMPORARY_DIRECTORY)
            .current_dir("/");
        for variable in AGENT_CONFIG_VARIABLES {
            self.env_remove(variable);
        }
        self
    }
}

/// The TMPDIR of root's commands.
pub struct RootTemporaryDirectory;

impl RootTemporaryDirectory {
    pub fn create() {
        if let Err(error) = DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(ROOT_TEMPORARY_DIRECTORY)
        {
            tracing::warn!("could not create {ROOT_TEMPORARY_DIRECTORY} ({error})");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::*;

    #[test]
    fn root_commands_leave_out_everything_the_agent_can_write() {
        let mut command = Command::new("dpkg-query");
        command.env("GIT_CONFIG_GLOBAL", "/config/git/config");
        command.apart_from_agent();
        let value_of = |name: &str| {
            command
                .get_envs()
                .find(|(key, _)| *key == OsStr::new(name))
                .map(|(_, value)| value)
        };
        assert_eq!(value_of("PATH"), Some(Some(OsStr::new(ROOT_PATH))));
        assert_eq!(value_of("HOME"), Some(Some(OsStr::new("/root"))));
        assert_eq!(value_of("TMPDIR"), Some(Some(OsStr::new("/root/tmp"))));
        for variable in AGENT_CONFIG_VARIABLES {
            assert_eq!(value_of(variable), Some(None), "{variable}");
        }
        assert_eq!(command.get_current_dir(), Some(Path::new("/")));
        for directory in ROOT_PATH.split(':') {
            assert!(
                !directory.starts_with("/home") && !directory.starts_with("/config"),
                "{directory}"
            );
        }
    }
}
