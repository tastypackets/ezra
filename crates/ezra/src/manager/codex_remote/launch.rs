use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex as SyncMutex, PoisonError};

use serde::Serialize;
use tokio::process::Command;
use tokio::time::timeout;

use super::{CodexApprovals, CodexSandbox, ServerBudget};
use crate::manager::agents::Agent;
use crate::manager::login::SignInMethod;
use crate::manager::supervision::ServerLog;

/// Every error, plus remote control's warnings.
const SERVER_LOG_FILTER: &str = "error,codex_app_server_transport::transport::remote_control=warn";
const CODEX_INTERNAL_VARIABLES: [&str; 2] = [
    "CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED",
    "CODEX_DAEMON_TELEMETRY_HANDOFF",
];

/// A setting Codex takes as a `-c key=value` override.
pub trait ConfigOverride: Serialize {
    const KEY: &'static str;

    /// The `key=value` argument, with the value quoted as TOML.
    fn config_override(&self) -> String {
        let value = toml::Value::try_from(self).expect("a setting is a TOML string");
        format!("{}={value}", Self::KEY)
    }
}

impl ConfigOverride for CodexSandbox {
    const KEY: &'static str = "sandbox_mode";
}

impl ConfigOverride for CodexApprovals {
    const KEY: &'static str = "approval_policy";
}

/// One Codex app-server to start. A new Codex version is a different launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexLaunch {
    /// The file the `codex` command links to.
    pub codex: PathBuf,
    pub version: String,
    pub codex_home: PathBuf,
    pub projects: PathBuf,
    pub sandbox: CodexSandbox,
    pub approvals: CodexApprovals,
    /// How Codex is signed in, absent for a method this build does not know. Another method is a
    /// different launch.
    pub sign_in: Option<SignInMethod>,
    pub managed_daemon: bool,
    pub log: ServerLog,
}

impl CodexLaunch {
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.codex);
        command
            .current_dir(&self.projects)
            .args(["app-server", "--remote-control"])
            .args(self.managed_daemon.then_some("--managed-daemon"))
            .args(["--listen", "unix://"])
            .arg("-c")
            .arg(self.sandbox.config_override())
            .arg("-c")
            .arg(self.approvals.config_override())
            .env(Agent::Codex.config_directory_variable(), &self.codex_home)
            .env("RUST_LOG", SERVER_LOG_FILTER)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        for variable in CODEX_INTERNAL_VARIABLES {
            command.env_remove(variable);
        }
        command
    }

    /// Where a server started with `--managed-daemon` saves its loaded threads when it stops.
    pub fn saved_threads(&self) -> PathBuf {
        self.codex_home
            .join("app-server-daemon")
            .join("loaded-threads.json")
    }

    /// The same server on a different Codex version.
    pub fn is_update_of(&self, running: &Self) -> bool {
        self.version != running.version
            && *running
                == Self {
                    codex: running.codex.clone(),
                    version: running.version.clone(),
                    managed_daemon: running.managed_daemon,
                    ..self.clone()
                }
    }
}

/// Which hidden `app-server` flags a Codex version takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchFlags {
    pub remote_control: bool,
    pub managed_daemon: bool,
}

/// Codex gave no answer, so it is asked again next time.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("could not ask Codex which flags it takes: {0}")]
    Retryable(io::Error),
}

impl LaunchFlags {
    /// Asks `codex app-server <flag> --help` for each flag. `--managed-daemon` is only asked
    /// when `--remote-control` is taken.
    pub async fn probe(
        codex: &Path,
        codex_home: &Path,
        budget: &ServerBudget,
    ) -> Result<Self, ProbeError> {
        let remote_control = Self::takes(codex, codex_home, "--remote-control", budget).await?;
        let managed_daemon =
            remote_control && Self::takes(codex, codex_home, "--managed-daemon", budget).await?;
        Ok(Self {
            remote_control,
            managed_daemon,
        })
    }

    /// Whether `--help` after the flag exits 0.
    async fn takes(
        codex: &Path,
        codex_home: &Path,
        flag: &str,
        budget: &ServerBudget,
    ) -> Result<bool, ProbeError> {
        let status = timeout(
            budget.probe,
            Command::new(codex)
                .args(["app-server", flag, "--help"])
                .env(Agent::Codex.config_directory_variable(), codex_home)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await
        .map_err(|_| {
            ProbeError::Retryable(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "app-server {flag} --help did not exit within {:?}",
                    budget.probe
                ),
            ))
        })?
        .map_err(ProbeError::Retryable)?;
        match status.code() {
            Some(code) => Ok(code == 0),
            None => Err(ProbeError::Retryable(io::Error::other(format!(
                "app-server {flag} --help ended with {status}"
            )))),
        }
    }
}

/// The flags each Codex version takes, once it answered.
#[derive(Debug, Default)]
pub struct LaunchFlagCache(SyncMutex<HashMap<String, LaunchFlags>>);

impl LaunchFlagCache {
    /// The flags `codex` takes, asked only when its version has no answer yet.
    pub async fn flags(
        &self,
        codex: &Path,
        version: &str,
        codex_home: &Path,
        budget: &ServerBudget,
    ) -> Result<LaunchFlags, ProbeError> {
        let known = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(version)
            .copied();
        if let Some(flags) = known {
            return Ok(flags);
        }
        let flags = LaunchFlags::probe(codex, codex_home, budget).await?;
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(version.to_owned(), flags);
        Ok(flags)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::time::Duration;

    use nix::unistd::Pid;

    use super::*;
    use crate::manager::api::test_support::{PidExt, TestManager};

    impl CodexLaunch {
        /// A launch of the agent's installed fake `codex`, with the test manager's directories.
        pub fn of_fake(manager: &TestManager) -> Self {
            let paths = &manager.state.install_paths;
            let (codex, version) = paths
                .installed_command(Agent::Codex)
                .expect("a fake codex is installed");
            let codex_home = paths
                .config_directory(Agent::Codex)
                .expect("the test manager has a Codex home")
                .to_path_buf();
            let projects = manager.state.projects.0.clone();
            let log = ServerLog(manager.settings_path.with_file_name("codex-log"));
            for directory in [&codex_home, &projects, &log.0] {
                fs::create_dir_all(directory).expect("directory is created");
            }
            Self {
                codex,
                version,
                codex_home,
                projects,
                sandbox: CodexSandbox::default(),
                approvals: CodexApprovals::default(),
                sign_in: Some(SignInMethod::ChatGpt),
                managed_daemon: true,
                log,
            }
        }
    }

    const PROBE: ServerBudget = ServerBudget {
        probe: Duration::from_millis(300),
        readiness: Duration::ZERO,
        drain: Duration::ZERO,
        force: Duration::ZERO,
        request: Duration::ZERO,
        mfa_retry: Duration::ZERO,
    };

    fn arguments(command: &Command) -> Vec<&str> {
        command
            .as_std()
            .get_args()
            .map(|argument| argument.to_str().expect("arguments are text"))
            .collect()
    }

    fn launch() -> (TestManager, CodexLaunch) {
        let manager = TestManager::without_web_app();
        manager.install_fake_cli(Agent::Codex, "");
        let launch = CodexLaunch::of_fake(&manager);
        (manager, launch)
    }

    #[test]
    fn settings_are_passed_as_toml_strings() {
        assert_eq!(
            CodexSandbox::DangerFullAccess.config_override(),
            r#"sandbox_mode="danger-full-access""#
        );
        assert_eq!(
            CodexApprovals::OnRequest.config_override(),
            r#"approval_policy="on-request""#
        );
    }

    #[test]
    fn the_server_serves_remote_control_on_the_control_socket_with_the_settings() {
        let (_manager, launch) = launch();
        let launch = CodexLaunch {
            sandbox: CodexSandbox::ReadOnly,
            approvals: CodexApprovals::Never,
            ..launch
        };
        assert_eq!(
            arguments(&launch.command()),
            [
                "app-server",
                "--remote-control",
                "--managed-daemon",
                "--listen",
                "unix://",
                "-c",
                r#"sandbox_mode="read-only""#,
                "-c",
                r#"approval_policy="never""#,
            ]
        );
        let without_managed_daemon = CodexLaunch {
            managed_daemon: false,
            ..launch
        };
        assert_eq!(
            arguments(&without_managed_daemon.command()),
            [
                "app-server",
                "--remote-control",
                "--listen",
                "unix://",
                "-c",
                r#"sandbox_mode="read-only""#,
                "-c",
                r#"approval_policy="never""#,
            ]
        );
    }

    #[test]
    fn the_server_runs_in_projects_with_its_home_and_log_filter() {
        let (_manager, launch) = launch();
        let command = launch.command();
        let command = command.as_std();
        assert_eq!(command.get_program(), launch.codex.as_os_str());
        assert_eq!(command.get_current_dir(), Some(launch.projects.as_path()));
        let environment: HashMap<&OsStr, Option<&OsStr>> = command.get_envs().collect();
        assert_eq!(
            environment.get(OsStr::new("CODEX_HOME")),
            Some(&Some(launch.codex_home.as_os_str()))
        );
        assert_eq!(
            environment.get(OsStr::new("RUST_LOG")),
            Some(&Some(OsStr::new(
                "error,codex_app_server_transport::transport::remote_control=warn"
            )))
        );
        for variable in [
            "CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED",
            "CODEX_DAEMON_TELEMETRY_HANDOFF",
        ] {
            assert_eq!(environment.get(OsStr::new(variable)), Some(&None));
        }
        assert!(!environment.contains_key(OsStr::new("LOG_FORMAT")));
    }

    #[test]
    fn only_a_new_version_is_an_update() {
        let (_manager, running) = launch();
        let newer = CodexLaunch {
            codex: PathBuf::from("/cache/agents/codex/0.158.0/bin/codex"),
            version: "0.158.0".to_owned(),
            managed_daemon: false,
            ..running.clone()
        };
        assert!(newer.is_update_of(&running));
        assert!(!running.is_update_of(&running));
        let changed = [
            CodexLaunch {
                sandbox: CodexSandbox::WorkspaceWrite,
                ..newer.clone()
            },
            CodexLaunch {
                approvals: CodexApprovals::Never,
                ..newer.clone()
            },
            CodexLaunch {
                sign_in: Some(SignInMethod::AccessToken),
                ..newer.clone()
            },
            CodexLaunch {
                sign_in: None,
                ..newer.clone()
            },
            CodexLaunch {
                projects: PathBuf::from("/elsewhere"),
                ..newer.clone()
            },
        ];
        for launch in changed {
            assert!(!launch.is_update_of(&running), "{launch:?}");
        }
    }

    #[tokio::test]
    async fn answers_are_kept_per_version() {
        let manager = TestManager::without_web_app();
        let codex = manager.install_fake_cli(
            Agent::Codex,
            "case \"$*\" in\n  \
               'app-server --remote-control --help') : > \"$CODEX_HOME/probed\"; exit 0 ;;\n  \
               'app-server --managed-daemon --help') exit 2 ;;\n\
             esac\nexit 1",
        );
        let codex_home = CodexLaunch::of_fake(&manager).codex_home;
        let cache = LaunchFlagCache::default();

        for _ in 0..2 {
            let flags = cache
                .flags(&codex, "0.157.1", &codex_home, &PROBE)
                .await
                .expect("the fake answers");
            assert_eq!(
                flags,
                LaunchFlags {
                    remote_control: true,
                    managed_daemon: false
                }
            );
        }
        assert!(codex_home.join("probed").exists());
        assert_eq!(
            manager.fake_cli_runs(Agent::Codex),
            [
                "app-server --remote-control --help",
                "app-server --managed-daemon --help"
            ]
        );

        cache
            .flags(&codex, "0.158.0", &codex_home, &PROBE)
            .await
            .expect("the fake answers");
        assert_eq!(manager.fake_cli_runs(Agent::Codex).len(), 4);
    }

    #[tokio::test]
    async fn a_version_without_remote_control_is_kept_too() {
        let manager = TestManager::without_web_app();
        let codex = manager.install_fake_cli(Agent::Codex, "exit 2");
        let codex_home = CodexLaunch::of_fake(&manager).codex_home;
        let cache = LaunchFlagCache::default();

        for _ in 0..2 {
            let flags = cache
                .flags(&codex, "0.100.0", &codex_home, &PROBE)
                .await
                .expect("the fake answers");
            assert_eq!(
                flags,
                LaunchFlags {
                    remote_control: false,
                    managed_daemon: false
                }
            );
        }
        assert_eq!(
            manager.fake_cli_runs(Agent::Codex),
            ["app-server --remote-control --help"]
        );
    }

    #[tokio::test]
    async fn a_probe_that_hangs_is_stopped_and_asked_again() {
        let manager = TestManager::without_web_app();
        let codex = manager.install_fake_cli(
            Agent::Codex,
            "echo $$ >> \"$CODEX_HOME/probes\"\nexec sleep 60",
        );
        let codex_home = CodexLaunch::of_fake(&manager).codex_home;
        let cache = LaunchFlagCache::default();

        for _ in 0..2 {
            let error = cache
                .flags(&codex, "0.157.1", &codex_home, &PROBE)
                .await
                .expect_err("the fake never answers");
            let ProbeError::Retryable(error) = error;
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            let probes = fs::read_to_string(codex_home.join("probes")).expect("pids are written");
            for probe in probes.lines() {
                Pid::from_raw(probe.parse().expect("the pid is a number"))
                    .wait_until_gone()
                    .await;
            }
        }
        assert_eq!(manager.fake_cli_runs(Agent::Codex).len(), 2);
    }

    #[tokio::test]
    async fn a_probe_killed_by_a_signal_is_asked_again() {
        let manager = TestManager::without_web_app();
        let codex = manager.install_fake_cli(
            Agent::Codex,
            "if [ ! -e \"$CODEX_HOME/killed\" ]; then\n  \
               : > \"$CODEX_HOME/killed\"\n  \
               kill -KILL $$\n\
             fi\n\
             exit 0",
        );
        let codex_home = CodexLaunch::of_fake(&manager).codex_home;
        let cache = LaunchFlagCache::default();

        let error = cache
            .flags(&codex, "0.157.1", &codex_home, &PROBE)
            .await
            .expect_err("the first probe is killed");
        let ProbeError::Retryable(error) = error;
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let flags = cache
            .flags(&codex, "0.157.1", &codex_home, &PROBE)
            .await
            .expect("the fake answers");
        assert_eq!(
            flags,
            LaunchFlags {
                remote_control: true,
                managed_daemon: true
            }
        );
        assert_eq!(
            manager.fake_cli_runs(Agent::Codex),
            [
                "app-server --remote-control --help",
                "app-server --remote-control --help",
                "app-server --managed-daemon --help"
            ]
        );
    }

    #[tokio::test]
    async fn a_missing_codex_is_asked_again() {
        let manager = TestManager::without_web_app();
        let codex = manager.install_fake_cli(Agent::Codex, "exit 0");
        let codex_home = CodexLaunch::of_fake(&manager).codex_home;
        let moved = codex.with_file_name("moved");
        fs::rename(&codex, &moved).expect("the fake is moved away");
        let cache = LaunchFlagCache::default();

        let error = cache
            .flags(&codex, "0.157.1", &codex_home, &PROBE)
            .await
            .expect_err("nothing runs");
        let ProbeError::Retryable(error) = error;
        assert_eq!(error.kind(), io::ErrorKind::NotFound);

        fs::rename(&moved, &codex).expect("the fake is back");
        let flags = cache
            .flags(&codex, "0.157.1", &codex_home, &PROBE)
            .await
            .expect("the fake answers");
        assert_eq!(
            flags,
            LaunchFlags {
                remote_control: true,
                managed_daemon: true
            }
        );
    }
}
