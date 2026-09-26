use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use nix::unistd::Uid;

use crate::path_ext::PathExt;

pub const SETUP_SCRIPTS_DIRECTORY: &str = "/etc/ezra/setup.d";
const AGENT_MISE_DIRECTORY_VARIABLES: [&str; 3] =
    ["MISE_DATA_DIR", "MISE_CONFIG_DIR", "MISE_STATE_DIR"];
const EXECUTE_BITS: u32 = 0o111;

/// The operator's scripts in a setup directory, in name order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SetupScripts {
    pub executable: Vec<PathBuf>,
    pub not_executable: Vec<PathBuf>,
}

impl SetupScripts {
    /// Regular files, skipping names that start with a dot and dangling symlinks.
    pub fn in_directory(directory: &Path) -> io::Result<Self> {
        let mut paths = directory.entries_or_empty()?;
        paths.sort();

        let mut scripts = Self::default();
        for path in paths {
            let is_hidden = path
                .file_name()
                .is_some_and(|name| name.as_encoded_bytes().starts_with(b"."));
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if is_hidden || !metadata.is_file() {
                continue;
            }
            if metadata.permissions().mode() & EXECUTE_BITS == 0 {
                scripts.not_executable.push(path);
            } else {
                scripts.executable.push(path);
            }
        }
        Ok(scripts)
    }

    /// Runs as root, before switching to the agent user. Never fails the start: problems are logged.
    pub fn run_all() {
        let scripts = match Self::in_directory(Path::new(SETUP_SCRIPTS_DIRECTORY)) {
            Ok(scripts) => scripts,
            Err(error) => {
                tracing::warn!(
                    "could not read {SETUP_SCRIPTS_DIRECTORY} ({error}), so no setup scripts ran"
                );
                return;
            }
        };
        for path in &scripts.not_executable {
            tracing::warn!("skipping {} because it is not executable", path.display());
        }
        for path in &scripts.executable {
            Self::run(path);
        }
    }

    /// Warns that a non-root start skips the scripts, when there are any.
    pub fn warn_if_ignored(uid: Uid) {
        let directory = Path::new(SETUP_SCRIPTS_DIRECTORY);
        let has_scripts =
            Self::in_directory(directory).is_ok_and(|scripts| !scripts.executable.is_empty());
        if has_scripts {
            tracing::warn!(
                "{} is ignored when the container starts as uid {uid}",
                directory.display()
            );
        }
    }

    /// Without the agent's mise variables, so root never writes into the agent's /config/mise.
    fn run(path: &Path) {
        tracing::info!("running {}", path.display());
        let mut script = Command::new(path);
        for variable in AGENT_MISE_DIRECTORY_VARIABLES {
            script.env_remove(variable);
        }
        match script.stdin(Stdio::null()).status() {
            Ok(status) if status.success() => {}
            Ok(status) => tracing::warn!("{} failed with {status}", path.display()),
            Err(error) => tracing::warn!("could not run {} ({error})", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn create_file(directory: &Path, name: &str, mode: u32) {
        let path = directory.join(name);
        fs::write(&path, "#!/bin/sh\n").expect("script is written");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("script mode is set");
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect()
    }

    fn scripts_in(directory: &Path) -> SetupScripts {
        SetupScripts::in_directory(directory).expect("setup directory is readable")
    }

    #[test]
    fn executable_files_are_listed_in_name_order() {
        let directory = tempfile::tempdir().expect("temporary directory");
        create_file(directory.path(), "20-second", 0o755);
        create_file(directory.path(), "10-first.sh", 0o700);
        create_file(directory.path(), "30-third", 0o755);
        let scripts = scripts_in(directory.path());
        assert_eq!(
            names(&scripts.executable),
            ["10-first.sh", "20-second", "30-third"]
        );
        assert!(scripts.not_executable.is_empty());
    }

    #[test]
    fn files_that_are_not_executable_are_reported() {
        let directory = tempfile::tempdir().expect("temporary directory");
        create_file(directory.path(), "10-forgot-chmod.sh", 0o644);
        let scripts = scripts_in(directory.path());
        assert!(scripts.executable.is_empty());
        assert_eq!(names(&scripts.not_executable), ["10-forgot-chmod.sh"]);
    }

    #[test]
    fn hidden_files_directories_and_dangling_links_are_ignored() {
        let directory = tempfile::tempdir().expect("temporary directory");
        create_file(directory.path(), ".gitkeep", 0o644);
        create_file(directory.path(), ".hidden-script", 0o755);
        fs::create_dir(directory.path().join("20-directory")).expect("directory is created");
        symlink("missing", directory.path().join("30-dangling")).expect("link is created");
        assert_eq!(scripts_in(directory.path()), SetupScripts::default());
    }

    #[test]
    fn missing_directory_means_no_scripts() {
        let directory = tempfile::tempdir().expect("temporary directory");
        assert_eq!(
            scripts_in(&directory.path().join("setup.d")),
            SetupScripts::default()
        );
    }
}
