use std::fs;
use std::path::{Path, PathBuf};

use super::foreign::{DAEMON_PACKAGE, DAEMON_STATE, DaemonCopies};
use crate::path_ext::PathExt;

/// The files Codex's own daemon keeps in its state folder, in its current and its legacy layout.
const DAEMON_FILES: [&str; 16] = [
    "daemon.pid",
    "daemon.pid.lock",
    "daemon.stderr.log",
    "daemon-updater.pid",
    "daemon-updater.pid.lock",
    "daemon-updater.stderr.log",
    "daemon-updater.sock",
    "app-server.pid",
    "app-server.pid.lock",
    "app-server.stderr.log",
    "app-server-updater.pid",
    "app-server-updater.pid.lock",
    "app-server-updater.stderr.log",
    "app-server-updater.sock",
    "daemon.lock",
    "settings.json",
];

/// What Codex's own daemon left in a Codex home: its copy of Codex and its files.
pub struct DaemonLeftovers {
    codex_home: PathBuf,
}

impl DaemonLeftovers {
    /// What the daemon left in `codex_home`.
    pub fn of(codex_home: &Path) -> Self {
        Self {
            codex_home: codex_home.to_path_buf(),
        }
    }

    /// Once nothing runs from a copy of Codex the daemon made, removes its copy and its files.
    pub async fn remove_when_unused(self) {
        let removed = tokio::task::spawn_blocking(move || {
            if DaemonCopies::running_in(&self.codex_home).next().is_some() {
                return Vec::new();
            }
            let package = self.codex_home.join(DAEMON_PACKAGE);
            let state = self.codex_home.join(DAEMON_STATE);
            let mut removed = Vec::new();
            for path in [package.clone()]
                .into_iter()
                .chain(DAEMON_FILES.iter().map(|name| state.join(name)))
            {
                if fs::symlink_metadata(&path).is_err() {
                    continue;
                }
                match path.remove_if_present() {
                    Ok(()) => removed.push(path),
                    Err(error) => tracing::warn!("could not remove {}: {error}", path.display()),
                }
            }
            if let Some(packages) = package.parent() {
                let _kept_unless_empty = fs::remove_dir(packages);
            }
            removed
        })
        .await
        .unwrap_or_default();
        for path in removed {
            tracing::info!("removed {}, left by Codex's own daemon", path.display());
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::manager::codex_remote::foreign::DAEMON_PACKAGES;
    use crate::manager::codex_remote::foreign::tests::{RELEASE, SLEEPS, copy_of_sh};

    const KEPT: [&str; 5] = [
        "app-server-daemon/loaded-threads.json",
        "app-server-daemon/notes.txt",
        "state_5.sqlite",
        "installation_id",
        "auth.json",
    ];

    /// Writes what Codex's own daemon leaves in `codex_home`, next to files ezra keeps.
    pub fn leave_daemon_files(codex_home: &Path) {
        let package = codex_home.join(DAEMON_PACKAGE);
        fs::create_dir_all(package.join(RELEASE)).expect("the daemon's copy is created");
        fs::write(package.join("auto-update-version"), "").expect("the daemon's copy is written");
        let state = codex_home.join(DAEMON_STATE);
        fs::create_dir_all(&state).expect("the daemon's state folder is created");
        for file in DAEMON_FILES
            .iter()
            .map(|name| state.join(name))
            .chain(KEPT.iter().map(|name| codex_home.join(name)))
        {
            fs::write(file, "").expect("the file is written");
        }
    }

    /// The daemon's copy and files still in `codex_home`.
    pub fn daemon_files_left(codex_home: &Path) -> Vec<PathBuf> {
        let package = codex_home.join(DAEMON_PACKAGE);
        [package
            .parent()
            .expect("the copy is in a folder")
            .to_path_buf()]
        .into_iter()
        .chain(
            DAEMON_FILES
                .iter()
                .map(|name| codex_home.join(DAEMON_STATE).join(name)),
        )
        .filter(|path| path.exists())
        .collect()
    }

    fn kept_left(codex_home: &Path) -> Vec<&'static str> {
        KEPT.into_iter()
            .filter(|name| codex_home.join(name).exists())
            .collect()
    }

    fn codex_home_with_daemon_files() -> TempDir {
        let home = tempfile::tempdir().expect("a Codex home is created");
        leave_daemon_files(home.path());
        home
    }

    #[tokio::test]
    async fn the_daemons_copy_and_files_are_removed_and_everything_else_is_kept() {
        let home = codex_home_with_daemon_files();
        assert_eq!(daemon_files_left(home.path()).len(), 17);

        DaemonLeftovers::of(home.path()).remove_when_unused().await;

        assert_eq!(daemon_files_left(home.path()), Vec::<PathBuf>::new());
        assert_eq!(kept_left(home.path()), KEPT);
    }

    #[tokio::test]
    async fn a_standalone_install_of_codex_is_kept() {
        let home = codex_home_with_daemon_files();
        let standalone = home.path().join(DAEMON_PACKAGES[1]).join(RELEASE);
        fs::create_dir_all(&standalone).expect("the standalone install is created");
        fs::write(standalone.join("codex"), "install").expect("the install is written");

        DaemonLeftovers::of(home.path()).remove_when_unused().await;

        assert!(standalone.join("codex").exists());
        assert!(!home.path().join(DAEMON_PACKAGE).exists());
        assert_eq!(kept_left(home.path()), KEPT);
    }

    #[tokio::test]
    async fn nothing_is_removed_while_a_daemon_copy_still_runs() {
        for package in DAEMON_PACKAGES {
            let home = codex_home_with_daemon_files();
            let running = copy_of_sh(
                home.path(),
                &home.path().join(package).join(RELEASE),
                SLEEPS,
                &[],
            )
            .await;
            let before = daemon_files_left(home.path());

            DaemonLeftovers::of(home.path()).remove_when_unused().await;
            assert_eq!(daemon_files_left(home.path()), before, "{package}");

            drop(running);
            DaemonLeftovers::of(home.path()).remove_when_unused().await;
            assert!(!home.path().join(DAEMON_PACKAGE).exists(), "{package}");
            assert_eq!(kept_left(home.path()), KEPT, "{package}");
        }
    }

    #[tokio::test]
    async fn a_home_without_daemon_files_is_left_as_it_is() {
        let home = tempfile::tempdir().expect("a Codex home is created");

        DaemonLeftovers::of(home.path()).remove_when_unused().await;

        assert!(home.path().exists());
        assert_eq!(
            fs::read_dir(home.path())
                .expect("the home is readable")
                .count(),
            0
        );
    }
}
