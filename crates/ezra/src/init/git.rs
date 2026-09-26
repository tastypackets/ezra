use std::env;
use std::fs;
use std::path::PathBuf;

const GIT_CONFIG_GLOBAL_VARIABLE: &str = "GIT_CONFIG_GLOBAL";

/// Git writes `GIT_CONFIG_GLOBAL` but never creates the directory that holds it.
pub struct GlobalGitConfig(PathBuf);

impl GlobalGitConfig {
    pub fn from_environment() -> Option<Self> {
        env::var_os(GIT_CONFIG_GLOBAL_VARIABLE).map(|path| Self(PathBuf::from(path)))
    }

    pub fn create_directory(&self) {
        let Some(directory) = self.0.parent() else {
            return;
        };
        if let Err(error) = fs::create_dir_all(directory) {
            tracing::warn!(
                "could not create {} for git's settings ({error})",
                directory.display()
            );
        }
    }
}
