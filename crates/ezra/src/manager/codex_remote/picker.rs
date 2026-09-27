use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex as SyncMutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::time::timeout;
use utoipa::ToSchema;

use super::launch::{CodexLaunch, ConfigOverride};
use super::{CodexRemote, CodexSandbox, ServerBudget};
use crate::manager::agents::Agent;
use crate::manager::login::StrExt;

/// What the ChatGPT app's folder picker runs first, to find the home folder.
const FIND_HOME: [&str; 3] = ["/bin/sh", "-lc", r#"cd "$HOME" && pwd -P"#];
const LOGGED_LINES: usize = 5;

/// Whether the ChatGPT app's folder picker is expected to work in this box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FolderPicker {
    /// It is expected to work.
    Works,
    /// It is expected to fail, because Codex's sandbox cannot run in this container.
    Blocked,
}

impl FolderPicker {
    /// Runs what the picker runs first in Codex's read-only sandbox. Logs the last lines of a
    /// refusal to the server's log.
    pub async fn probe(launch: &CodexLaunch, budget: &ServerBudget) -> io::Result<Self> {
        let output = timeout(budget.folder_picker, launch.folder_picker_probe().output())
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "codex sandbox did not exit within {:?}",
                        budget.folder_picker
                    ),
                )
            })??;
        if output.status.code().is_none() {
            return Err(io::Error::other(format!(
                "codex sandbox ended with {}",
                output.status
            )));
        }
        let found = String::from_utf8_lossy(&output.stdout)
            .lines()
            .last()
            .is_some_and(|home| Path::new(home.trim()).is_absolute());
        if output.status.success() && found {
            return Ok(Self::Works);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<String> = stderr
            .lines()
            .map(StrExt::without_terminal_codes)
            .filter(|line| !line.trim().is_empty())
            .collect();
        let skipped = lines.len().saturating_sub(LOGGED_LINES);
        for line in lines.iter().skip(skipped) {
            if let Err(error) = launch
                .log
                .append_output(&format!("codex sandbox: {line}"))
                .await
            {
                tracing::debug!("could not log to {}: {error}", launch.log.0.display());
            }
        }
        Ok(Self::Blocked)
    }
}

impl CodexLaunch {
    /// Codex's read-only sandbox finding the home folder, with the server's settings before it.
    fn folder_picker_probe(&self) -> Command {
        let mut command = Command::new(&self.codex);
        command
            .current_dir("/")
            .arg("sandbox")
            .args(self.config_overrides())
            .arg("-c")
            .arg(CodexSandbox::ReadOnly.config_override())
            .args(FIND_HOME)
            .env(Agent::Codex.config_directory_variable(), &self.codex_home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
}

/// The folder picker answer for each Codex version, and the versions still being asked.
#[derive(Debug, Default)]
pub struct FolderPickers(SyncMutex<HashMap<String, Option<FolderPicker>>>);

impl FolderPickers {
    /// The answer for `version`, absent until one came.
    pub fn answer(&self, version: &str) -> Option<FolderPicker> {
        self.lock().get(version).copied().flatten()
    }

    /// Marks `version` as being asked. False when it was asked already.
    fn ask(&self, version: &str) -> bool {
        let mut answers = self.lock();
        if answers.contains_key(version) {
            return false;
        }
        answers.insert(version.to_owned(), None);
        true
    }

    /// Keeps `answer` for `version`, or forgets that it was asked when there is none.
    fn answered(&self, version: &str, answer: Option<FolderPicker>) {
        let mut answers = self.lock();
        match answer {
            Some(answer) => answers.insert(version.to_owned(), Some(answer)),
            None => answers.remove(version),
        };
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Option<FolderPicker>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl CodexRemote {
    /// Asks in the background whether the ChatGPT app's folder picker works with `launch`'s
    /// Codex version, unless that version was asked already.
    pub fn check_folder_picker(self: &Arc<Self>, launch: &CodexLaunch) {
        if !self.folder_pickers.ask(&launch.version) {
            return;
        }
        let codex_remote = Arc::clone(self);
        let launch = launch.clone();
        tokio::spawn(async move {
            let answer = match FolderPicker::probe(&launch, &codex_remote.budget).await {
                Ok(answer) => Some(answer),
                Err(error) => {
                    tracing::warn!("could not check the ChatGPT app's folder picker: {error}");
                    None
                }
            };
            codex_remote
                .folder_pickers
                .answered(&launch.version, answer);
            if let Some(answer) = answer {
                tracing::info!(
                    "the ChatGPT app's folder picker with Codex {}: {answer:?}",
                    launch.version
                );
                codex_remote
                    .status
                    .update(|status| status.show_folder_picker(&launch.version, answer));
            }
        });
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    use nix::unistd::Pid;

    use super::*;
    use crate::manager::api::test_support::{PidExt, TestManager};
    use crate::manager::codex_remote::CodexApprovals;
    use crate::manager::supervision::ServerLog;

    const BUDGET: ServerBudget = ServerBudget {
        probe: Duration::ZERO,
        readiness: Duration::ZERO,
        drain: Duration::ZERO,
        force: Duration::ZERO,
        request: Duration::ZERO,
        mfa_retry: Duration::ZERO,
        usage: Duration::ZERO,
        update_deadline: Duration::ZERO,
        pairing_poll: Duration::ZERO,
        folder_picker: Duration::from_millis(500),
    };
    /// What bubblewrap prints when the container does not allow user namespaces.
    pub const NO_NAMESPACES: &str = "bwrap: No permissions to create a new namespace, likely \
                                     because the kernel does not allow non-privileged user \
                                     namespaces. On e.g. debian this can be enabled with 'sysctl \
                                     kernel.unprivileged_userns_clone=1'.";

    /// A launch of a fake `codex` whose `sandbox` runs `sandbox`.
    fn fake_codex(sandbox: &str) -> (TestManager, CodexLaunch) {
        let manager = TestManager::without_web_app();
        manager.install_fake_cli(
            Agent::Codex,
            &format!("case \"$1\" in\n  sandbox)\n{sandbox}\n  ;;\nesac"),
        );
        let launch = CodexLaunch::of_fake(&manager);
        (manager, launch)
    }

    #[test]
    fn the_probe_finds_the_home_folder_read_only_after_the_servers_settings() {
        let (_manager, launch) = fake_codex("");
        let launch = CodexLaunch {
            sandbox: CodexSandbox::WorkspaceWrite,
            approvals: CodexApprovals::Never,
            ..launch
        };
        let command = launch.folder_picker_probe();
        let command = command.as_std();

        assert_eq!(command.get_program(), launch.codex.as_os_str());
        let arguments: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(
            arguments,
            [
                "sandbox",
                "-c",
                r#"sandbox_mode="workspace-write""#,
                "-c",
                r#"approval_policy="never""#,
                "-c",
                r#"sandbox_mode="read-only""#,
                "/bin/sh",
                "-lc",
                r#"cd "$HOME" && pwd -P"#,
            ]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/")));
        let environment: HashMap<&OsStr, Option<&OsStr>> = command.get_envs().collect();
        assert_eq!(
            environment.get(OsStr::new("CODEX_HOME")),
            Some(&Some(launch.codex_home.as_os_str()))
        );
    }

    #[tokio::test]
    async fn the_picker_works_when_the_sandbox_finds_the_home_folder() {
        let (_manager, launch) = fake_codex("echo 'Welcome to the box'\necho /home/dev");

        let answer = FolderPicker::probe(&launch, &BUDGET).await;

        assert_eq!(answer.expect("the fake answers"), FolderPicker::Works);
        assert!(launch.log.tail().expect("the log is readable").is_empty());
    }

    #[tokio::test]
    async fn the_picker_is_blocked_when_the_sandbox_cannot_start_and_the_last_lines_are_logged() {
        let (_manager, launch) = fake_codex(&format!(
            "for line in 1 2 3 4 5 6; do echo \"line $line\" >&2; done\n\
             echo \"{NO_NAMESPACES}\" >&2\n\
             exit 1"
        ));

        let answer = FolderPicker::probe(&launch, &BUDGET).await;

        assert_eq!(answer.expect("the fake answers"), FolderPicker::Blocked);
        let logged: Vec<String> = launch
            .log
            .tail()
            .expect("the log is readable")
            .iter()
            .filter_map(|line| line.split_once(" [OUTPUT] "))
            .map(|(_, line)| line.to_owned())
            .collect();
        assert_eq!(
            logged,
            [
                "codex sandbox: line 3",
                "codex sandbox: line 4",
                "codex sandbox: line 5",
                "codex sandbox: line 6",
                &format!("codex sandbox: {NO_NAMESPACES}"),
            ]
        );
    }

    #[tokio::test]
    async fn the_picker_is_blocked_when_the_sandbox_finds_no_home_folder() {
        for sandbox in ["exit 0", "echo home"] {
            let (_manager, launch) = fake_codex(sandbox);

            let answer = FolderPicker::probe(&launch, &BUDGET).await;

            assert_eq!(
                answer.expect("the fake answers"),
                FolderPicker::Blocked,
                "{sandbox}"
            );
        }
    }

    #[tokio::test]
    async fn a_probe_that_hangs_is_stopped_with_no_answer() {
        let (_manager, launch) = fake_codex("echo $$ > \"$CODEX_HOME/probe\"\nexec sleep 60");

        let error = FolderPicker::probe(&launch, &BUDGET)
            .await
            .expect_err("the fake never answers");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let probe =
            fs::read_to_string(launch.codex_home.join("probe")).expect("the pid is written");
        Pid::from_raw(probe.trim().parse().expect("the pid is a number"))
            .wait_until_gone()
            .await;
    }

    #[tokio::test]
    async fn a_probe_killed_by_a_signal_gives_no_answer() {
        let (_manager, launch) = fake_codex("kill -KILL $$");

        let error = FolderPicker::probe(&launch, &BUDGET)
            .await
            .expect_err("the fake is killed");

        assert_eq!(error.kind(), io::ErrorKind::Other);
    }

    #[tokio::test]
    #[ignore = "runs the real codex that EZRA_TEST_CODEX names"]
    async fn real_codex_answers_whether_the_folder_picker_works() {
        let codex = PathBuf::from(
            std::env::var_os("EZRA_TEST_CODEX").expect("EZRA_TEST_CODEX names a real codex"),
        );
        let home = tempfile::tempdir().expect("a Codex home is created");
        let logs = tempfile::tempdir().expect("a log folder is created");
        let launch = CodexLaunch {
            codex,
            version: "real".to_owned(),
            codex_home: home.path().to_path_buf(),
            projects: PathBuf::from("/"),
            sandbox: CodexSandbox::default(),
            approvals: CodexApprovals::default(),
            sign_in: None,
            managed_daemon: false,
            log: ServerLog(logs.path().to_path_buf()),
        };

        let answer = FolderPicker::probe(&launch, &ServerBudget::default())
            .await
            .expect("codex answers");

        eprintln!("The folder picker here: {answer:?}");
        eprintln!(
            "The log:\n{}",
            launch.log.tail().expect("the log is readable").join("\n")
        );
    }

    #[test]
    fn each_version_is_asked_once_until_it_answers() {
        let pickers = FolderPickers::default();
        assert!(pickers.ask("0.157.1"));
        assert!(!pickers.ask("0.157.1"));
        assert_eq!(pickers.answer("0.157.1"), None);
        assert!(pickers.ask("0.157.2"));

        pickers.answered("0.157.1", None);
        assert!(pickers.ask("0.157.1"));
        pickers.answered("0.157.1", Some(FolderPicker::Blocked));
        assert!(!pickers.ask("0.157.1"));
        assert_eq!(pickers.answer("0.157.1"), Some(FolderPicker::Blocked));
        assert_eq!(pickers.answer("0.157.2"), None);
    }
}
