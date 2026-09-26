use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const SETUP_SCRIPTS_DIRECTORY: &str = "/etc/agent-box/setup.d";
const AGENT_MISE_DIRECTORY_VARIABLES: [&str; 3] =
    ["MISE_DATA_DIR", "MISE_CONFIG_DIR", "MISE_STATE_DIR"];

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SetupScripts {
    pub executable: Vec<PathBuf>,
    pub not_executable: Vec<PathBuf>,
}

/// Regular files in name order, skipping names that start with a dot and dangling symlinks.
pub fn setup_scripts_in(directory: &Path) -> io::Result<SetupScripts> {
    let mut paths = match fs::read_dir(directory) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    paths.sort();

    let mut scripts = SetupScripts::default();
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
        if metadata.permissions().mode() & 0o111 == 0 {
            scripts.not_executable.push(path);
        } else {
            scripts.executable.push(path);
        }
    }
    Ok(scripts)
}

/// Runs as root, before switching to the agent user. Never fails the start: problems are logged.
pub fn run_all() {
    let scripts = match setup_scripts_in(Path::new(SETUP_SCRIPTS_DIRECTORY)) {
        Ok(scripts) => scripts,
        Err(error) => {
            tracing::warn!(
                "could not read {SETUP_SCRIPTS_DIRECTORY} ({error}); no setup scripts ran"
            );
            return;
        }
    };
    for path in &scripts.not_executable {
        tracing::warn!("skipping {} because it is not executable", path.display());
    }
    for path in &scripts.executable {
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
    use super::*;

    fn create_file(directory: &Path, name: &str, mode: u32) {
        let path = directory.join(name);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn executable_files_are_listed_in_name_order() {
        let directory = tempfile::tempdir().unwrap();
        create_file(directory.path(), "20-second", 0o755);
        create_file(directory.path(), "10-first.sh", 0o700);
        create_file(directory.path(), "30-third", 0o755);
        let scripts = setup_scripts_in(directory.path()).unwrap();
        assert_eq!(
            names(&scripts.executable),
            ["10-first.sh", "20-second", "30-third"]
        );
        assert!(scripts.not_executable.is_empty());
    }

    #[test]
    fn files_that_are_not_executable_are_reported() {
        let directory = tempfile::tempdir().unwrap();
        create_file(directory.path(), "10-forgot-chmod.sh", 0o644);
        let scripts = setup_scripts_in(directory.path()).unwrap();
        assert!(scripts.executable.is_empty());
        assert_eq!(names(&scripts.not_executable), ["10-forgot-chmod.sh"]);
    }

    #[test]
    fn hidden_files_directories_and_dangling_links_are_ignored() {
        let directory = tempfile::tempdir().unwrap();
        create_file(directory.path(), ".gitkeep", 0o644);
        create_file(directory.path(), ".hidden-script", 0o755);
        fs::create_dir(directory.path().join("20-directory")).unwrap();
        std::os::unix::fs::symlink("missing", directory.path().join("30-dangling")).unwrap();
        assert_eq!(
            setup_scripts_in(directory.path()).unwrap(),
            SetupScripts::default()
        );
    }

    #[test]
    fn missing_directory_means_no_scripts() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            setup_scripts_in(&directory.path().join("setup.d")).unwrap(),
            SetupScripts::default()
        );
    }
}
