use std::io;
use std::mem;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex as SyncMutex, PoisonError};
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::PrintableExt;
use super::sessions_api::SessionId;
use crate::manager::login::AgentCli;
use crate::manager::remote_control::VARIABLES_THAT_DISABLE_REMOTE_CONTROL;

const OUTPUT_LIMIT: usize = 1024 * 1024;
const OUTPUT_DRAIN: Duration = Duration::from_secs(2);
const TEXT_LIMIT: usize = 300;
/// Parts of Claude Code 2.1's messages about a sign-in that cannot reach sessions, in lowercase.
const SIGN_IN_HINTS: [&str; 5] = [
    "session expired",
    "login",
    "log in",
    "logged in",
    "not enabled for your account",
];

/// `claude -p --cloud <session>`, which queues a message in a session and exits without waiting
/// for the reply. The message goes on stdin.
pub struct FollowUp<'a> {
    pub session: &'a SessionId,
    pub message: &'a str,
    pub directory: &'a Path,
}

/// What the command prints on stdout with `--output-format json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FollowUpResult {
    pub ok: bool,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Why Claude Code refused a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The session is archived, or Claude does not know it.
    Gone,
    /// The sign-in cannot send to sessions.
    SignIn,
    Other,
}

#[derive(Debug, thiserror::Error)]
pub enum FollowUpError {
    #[error("could not start Claude Code: {0}")]
    Spawn(io::Error),
    #[error("could not hand the message to Claude Code: {0}")]
    Input(io::Error),
    #[error("could not wait for Claude Code: {0}")]
    Wait(io::Error),
    #[error("Claude Code did not finish within {0:?}")]
    TimedOut(Duration),
    #[error("Claude Code printed no result and {0}")]
    NoResult(ExitStatus),
}

impl FollowUp<'_> {
    pub fn command(&self, cli: &AgentCli) -> Command {
        let mut command = cli.command();
        command
            .current_dir(self.directory)
            .args(["-p", "--cloud", self.session.as_str()])
            .args(["--output-format", "json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        for variable in VARIABLES_THAT_DISABLE_REMOTE_CONTROL {
            command.env_remove(variable);
        }
        command
    }

    /// Runs the command for at most `within`. Stdin closes only after the whole message is
    /// written, and the command's process group is killed first when anything goes wrong, so
    /// Claude Code never sends part of a message.
    pub async fn run(
        &self,
        cli: &AgentCli,
        within: Duration,
    ) -> Result<FollowUpResult, FollowUpError> {
        let mut child = self.command(cli).spawn().map_err(FollowUpError::Spawn)?;
        let mut stdin = child.stdin.take();
        let group = GroupKill::of(&child);
        let stdout = Capture::start(child.stdout.take());
        let stderr = Capture::start(child.stderr.take());
        let ran = timeout(within, async {
            if let Some(input) = stdin.as_mut() {
                input
                    .write_all(self.message.as_bytes())
                    .await
                    .map_err(FollowUpError::Input)?;
            }
            drop(stdin.take());
            child.wait().await.map_err(FollowUpError::Wait)
        })
        .await
        .unwrap_or(Err(FollowUpError::TimedOut(within)));
        drop(group);
        let status = ran?;
        let (stdout, stderr) = tokio::join!(stdout.finish(), stderr.finish());
        FollowUpResult::from_stdout(&stdout).ok_or_else(|| {
            let last_line: String = stderr
                .lines()
                .last()
                .unwrap_or_default()
                .chars()
                .take(TEXT_LIMIT)
                .collect();
            tracing::warn!(stderr = %last_line.printable(), "Claude Code printed no follow-up result");
            FollowUpError::NoResult(status)
        })
    }
}

impl FollowUpError {
    /// Claude Code did not get the whole message, so it sent nothing.
    pub fn sent_nothing(&self) -> bool {
        matches!(self, Self::Spawn(_) | Self::Input(_))
    }
}

impl FollowUpResult {
    /// The last JSON object on stdout that has `ok`.
    pub fn from_stdout(stdout: &str) -> Option<Self> {
        std::iter::once(stdout)
            .chain(stdout.lines().rev())
            .map(str::trim)
            .filter(|candidate| candidate.starts_with('{'))
            .find_map(|candidate| serde_json::from_str(candidate).ok())
    }

    pub fn refusal(&self) -> Option<Refusal> {
        if self.ok {
            return None;
        }
        let error = self.error.as_deref().unwrap_or_default();
        let refusal = if error.contains("is archived")
            || error.starts_with("Session not found")
            || error.contains("invalid session ID")
        {
            Refusal::Gone
        } else if SIGN_IN_HINTS
            .iter()
            .any(|hint| error.to_ascii_lowercase().contains(hint))
        {
            Refusal::SignIn
        } else {
            Refusal::Other
        };
        Some(refusal)
    }

    /// The error Claude Code gave, shortened.
    pub fn reason(&self) -> String {
        self.error
            .as_deref()
            .unwrap_or("Claude Code gave no reason")
            .chars()
            .take(TEXT_LIMIT)
            .collect::<String>()
            .printable()
    }
}

/// Kills a child's process group when dropped. Once the leader is reaped, the group's id stays
/// taken while anything in the group runs.
struct GroupKill(Option<Pid>);

impl GroupKill {
    fn of(child: &Child) -> Self {
        Self(
            child
                .id()
                .and_then(|id| i32::try_from(id).ok())
                .map(Pid::from_raw),
        )
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        if let Some(group) = self.0.take() {
            let _already_gone = killpg(group, Signal::SIGKILL);
        }
    }
}

/// Up to `OUTPUT_LIMIT` bytes of one output stream, read in the background.
struct Capture {
    kept: Arc<SyncMutex<Vec<u8>>>,
    reader: JoinHandle<()>,
}

impl Capture {
    fn start(output: Option<impl AsyncRead + Unpin + Send + 'static>) -> Self {
        let kept = Arc::new(SyncMutex::new(Vec::new()));
        let reading = Arc::clone(&kept);
        let reader = tokio::spawn(async move {
            let Some(mut output) = output else { return };
            let mut chunk = [0; 8192];
            while let Ok(read @ 1..) = output.read(&mut chunk).await {
                let mut kept = reading.lock().unwrap_or_else(PoisonError::into_inner);
                let room = OUTPUT_LIMIT.saturating_sub(kept.len());
                kept.extend_from_slice(chunk.get(..read.min(room)).unwrap_or_default());
            }
        });
        Self { kept, reader }
    }

    /// What was read by the time the stream closed, or shortly after the command exited when
    /// something it left behind still holds the stream open.
    async fn finish(mut self) -> String {
        let _still_open = timeout(OUTPUT_DRAIN, &mut self.reader).await;
        let kept = mem::take(&mut *self.kept.lock().unwrap_or_else(PoisonError::into_inner));
        String::from_utf8_lossy(&kept).into_owned()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;
    use tokio::time::Instant;

    use super::*;
    use crate::manager::agents::Agent;
    use crate::manager::api::test_support::{PidExt, TestManager};
    use crate::manager::processes::Process;

    const WITHIN: Duration = Duration::from_secs(20);
    const OK: &str = r#"{"ok":true,"session_id":"session_01AB","url":"https://claude.ai/code/session_01AB?from=cli&m=0"}"#;

    struct Fixture {
        manager: TestManager,
        out: TempDir,
        session: SessionId,
    }

    impl Fixture {
        /// A `claude` that runs `follow_up` for `-p`, with `$OUT` set to a directory the test reads.
        fn new(follow_up: &str) -> Self {
            let manager = TestManager::new();
            let out = tempfile::tempdir().expect("temporary directory");
            manager.install_fake_cli(
                Agent::Claude,
                &format!(
                    "OUT='{}'\ncase \"$1\" in\n  -p) {follow_up} ;;\nesac",
                    out.path().display()
                ),
            );
            Self {
                manager,
                out,
                session: SessionId::parse("session_01AB").expect("the id is valid"),
            }
        }

        fn cli(&self) -> AgentCli {
            AgentCli::installed(Agent::Claude, &self.manager.state.install_paths)
                .expect("claude is installed")
        }

        fn follow_up<'a>(&'a self, message: &'a str) -> FollowUp<'a> {
            FollowUp {
                session: &self.session,
                message,
                directory: self.out.path(),
            }
        }

        fn written(&self, name: &str) -> Vec<u8> {
            fs::read(self.out.path().join(name)).expect("the fake wrote the file")
        }

        fn pid(&self, name: &str) -> Pid {
            let written = String::from_utf8(self.written(name)).expect("a pid is text");
            Pid::from_raw(written.trim().parse().expect("a pid"))
        }
    }

    #[tokio::test]
    async fn the_message_goes_on_stdin_with_the_cloud_arguments() {
        let fixture = Fixture::new(&format!(
            "cat > \"$OUT/stdin\"; pwd > \"$OUT/pwd\"; printf %s \"$CLAUDE_CONFIG_DIR\" > \"$OUT/config\"; echo '{OK}'"
        ));
        let message = format!(
            "--help\n# Fix this\n\n```sh\necho $(whoami) `id` \"$HOME\" '\\n'\n```\n> quoted é ✓\n{}",
            "a line of Markdown text\n".repeat(4000)
        );
        let result = fixture
            .follow_up(&message)
            .run(&fixture.cli(), WITHIN)
            .await
            .expect("the follow-up runs");
        assert_eq!(
            result,
            FollowUpResult {
                ok: true,
                session_id: Some("session_01AB".to_owned()),
                error: None,
            }
        );
        assert_eq!(fixture.written("stdin"), message.as_bytes());
        assert_eq!(
            fixture.manager.fake_cli_runs(Agent::Claude),
            ["-p --cloud session_01AB --output-format json"]
        );
        assert_eq!(
            PathBuf::from(
                String::from_utf8(fixture.written("pwd"))
                    .expect("text")
                    .trim()
            ),
            fs::canonicalize(fixture.out.path()).expect("the directory exists")
        );
        assert_eq!(
            PathBuf::from(String::from_utf8(fixture.written("config")).expect("text")),
            fixture
                .manager
                .state
                .install_paths
                .config_directory(Agent::Claude)
                .expect("Claude has a config directory")
        );
    }

    #[tokio::test]
    async fn the_command_removes_variables_that_change_the_account_or_server() {
        let fixture = Fixture::new("true");
        let command = fixture.follow_up("message").command(&fixture.cli());
        let environment: HashMap<&OsStr, Option<&OsStr>> = command.as_std().get_envs().collect();
        for variable in VARIABLES_THAT_DISABLE_REMOTE_CONTROL {
            assert_eq!(
                environment.get(OsStr::new(variable)),
                Some(&None),
                "{variable}"
            );
        }
        assert_eq!(
            command.as_std().get_args().collect::<Vec<_>>(),
            ["-p", "--cloud", "session_01AB", "--output-format", "json"]
        );
    }

    #[tokio::test]
    async fn the_result_is_the_last_json_object_on_stdout() {
        let fixture = Fixture::new(
            "cat > /dev/null; echo 'Queuing message'; echo '{\"ok\":false,\"error\":\"first\"}'; echo '{\"ok\":false,\"session_id\":\"session_01AB\",\"error\":\"cloud session session_01AB is archived and cannot accept new messages\"}'; echo 'Error: failed' >&2; exit 1",
        );
        let result = fixture
            .follow_up("message")
            .run(&fixture.cli(), WITHIN)
            .await
            .expect("the follow-up runs");
        assert_eq!(result.refusal(), Some(Refusal::Gone));
    }

    #[test]
    fn results_are_found_in_tolerant_output() {
        for (stdout, expected) in [
            (OK, Some(true)),
            (
                "{\n  \"ok\": true,\n  \"future\": {\"field\": 1}\n}\n",
                Some(true),
            ),
            ("log\n{\"ok\":false,\"error\":\"e\"}\n\n", Some(false)),
            ("{\"ok\":true}\n{\"no\":\"ok\"}", Some(true)),
            ("", None),
            ("Error: something", None),
            ("{\"ok\":", None),
            ("{\"session_id\":\"session_01AB\"}", None),
        ] {
            assert_eq!(
                FollowUpResult::from_stdout(stdout).map(|result| result.ok),
                expected,
                "{stdout}"
            );
        }
    }

    #[test]
    fn refusals_are_sorted_by_what_ezra_can_do_about_them() {
        let refusal = |error: &str| {
            FollowUpResult {
                ok: false,
                session_id: None,
                error: Some(error.to_owned()),
            }
            .refusal()
        };
        for gone in [
            "cloud session session_01AB is archived and cannot accept new messages",
            "invalid session ID: must be a cse_… or session_… tagged ID",
            "Session not found: session_01AB",
        ] {
            assert_eq!(refusal(gone), Some(Refusal::Gone), "{gone}");
        }
        for sign_in in [
            "Session expired. Please run /login to sign in again.",
            "Claude login not accepted \u{b7} Run /login, then try again",
            "Attaching to an existing cloud session is not enabled for your account.",
            "Not logged in",
        ] {
            assert_eq!(refusal(sign_in), Some(Refusal::SignIn), "{sign_in}");
        }
        for other in ["rate limited", "the author of the OAuth app"] {
            assert_eq!(refusal(other), Some(Refusal::Other), "{other}");
        }
        assert_eq!(
            FollowUpResult {
                ok: false,
                session_id: None,
                error: None
            }
            .refusal(),
            Some(Refusal::Other)
        );
        assert_eq!(
            FollowUpResult::from_stdout(OK).expect("a result").refusal(),
            None
        );
        assert_eq!(refusal(&"x".repeat(1000)).map(|_| ()), Some(()));
        assert_eq!(
            FollowUpResult {
                ok: false,
                session_id: None,
                error: Some("x".repeat(1000))
            }
            .reason()
            .len(),
            TEXT_LIMIT
        );
    }

    #[tokio::test]
    async fn output_without_a_result_is_an_error() {
        let fixture =
            Fixture::new("cat > /dev/null; echo 'not json'; echo 'Error: boom' >&2; exit 3");
        let error = fixture
            .follow_up("message")
            .run(&fixture.cli(), WITHIN)
            .await
            .expect_err("there is no result");
        assert!(
            matches!(&error, FollowUpError::NoResult(status) if status.code() == Some(3)),
            "{error}"
        );
        assert!(!error.sent_nothing());
    }

    #[tokio::test]
    async fn a_command_that_runs_too_long_is_killed_with_everything_it_started() {
        let fixture = Fixture::new(
            "echo $$ > \"$OUT/leader\"; sleep 60 & echo $! > \"$OUT/child\"; cat > /dev/null; wait",
        );
        let started = Instant::now();
        let error = fixture
            .follow_up("message")
            .run(&fixture.cli(), Duration::from_secs(1))
            .await
            .expect_err("the command is too slow");
        assert!(matches!(error, FollowUpError::TimedOut(_)), "{error}");
        assert!(!error.sent_nothing());
        assert!(started.elapsed() < Duration::from_secs(10));
        fixture.pid("child").wait_until_gone().await;
        fixture.pid("leader").wait_until_gone().await;
    }

    #[tokio::test]
    async fn processes_left_in_the_group_are_killed_once_the_command_exits() {
        let fixture = Fixture::new(&format!(
            "cat > /dev/null; echo '{OK}'; sleep 60 & echo $! > \"$OUT/leftover\"; exit 0"
        ));
        let started = Instant::now();
        let result = fixture
            .follow_up("message")
            .run(&fixture.cli(), WITHIN)
            .await
            .expect("the follow-up runs");
        assert!(result.ok);
        assert!(started.elapsed() < OUTPUT_DRAIN);
        fixture.pid("leftover").wait_until_gone().await;
    }

    #[tokio::test]
    async fn a_process_that_left_the_group_holds_up_the_result_only_briefly() {
        let fixture = Fixture::new(&format!(
            "cat > /dev/null; echo '{OK}'; setsid sh -c 'echo $$ > \"$1\"; exec sleep 6' sh \"$OUT/escaped\" & while [ ! -s \"$OUT/escaped\" ]; do sleep 0.01; done; exit 0"
        ));
        let started = Instant::now();
        let result = fixture
            .follow_up("message")
            .run(&fixture.cli(), WITHIN)
            .await
            .expect("the follow-up runs");
        assert!(result.ok);
        let waited = started.elapsed();
        assert!(
            waited >= OUTPUT_DRAIN && waited < Duration::from_secs(6),
            "{waited:?}"
        );
        assert!(Process::with_id(fixture.pid("escaped")).is_running());
    }

    #[tokio::test]
    async fn nothing_is_sent_when_claude_does_not_take_the_whole_message() {
        let fixture = Fixture::new(&format!("echo '{OK}'; exit 0"));
        let message = "x".repeat(1024 * 1024);
        let error = fixture
            .follow_up(&message)
            .run(&fixture.cli(), WITHIN)
            .await
            .expect_err("the message is not read");
        assert!(matches!(error, FollowUpError::Input(_)), "{error}");
        assert!(error.sent_nothing());
    }

    #[tokio::test]
    async fn a_command_that_cannot_start_sends_nothing() {
        let fixture = Fixture::new("true");
        let cli = fixture.cli();
        fs::remove_file(fixture.manager.state.install_paths.command(Agent::Claude))
            .expect("the command is removed");
        let error = fixture
            .follow_up("message")
            .run(&cli, WITHIN)
            .await
            .expect_err("the command is missing");
        assert!(matches!(error, FollowUpError::Spawn(_)), "{error}");
        assert!(error.sent_nothing());
    }
}
