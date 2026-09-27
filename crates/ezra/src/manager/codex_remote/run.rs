use std::io;
use std::process::ExitStatus;

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::process::Child;
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::ServerBudget;
use super::launch::CodexLaunch;
use super::problem::{CodexProblem, ProblemLine};
use crate::manager::processes::ProcessFamily;
use crate::manager::supervision::{Failure, LineWatcher, ServerOutput};

/// A running Codex server, the processes it started when last read, and its output.
pub struct CodexServerRun {
    pub child: Child,
    /// The lines of its output that name a known problem.
    pub problems: mpsc::UnboundedReceiver<ProblemLine>,
    family: Option<ProcessFamily>,
    output: ServerOutput,
}

impl CodexServerRun {
    /// Without `--managed-daemon`, first removes the threads an earlier server saved.
    pub async fn start(launch: &CodexLaunch) -> io::Result<Self> {
        if !launch.managed_daemon {
            let saved_threads = launch.saved_threads();
            match tokio::fs::remove_file(&saved_threads).await {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    tracing::warn!("could not remove {}: {error}", saved_threads.display());
                }
                _ => {}
            }
        }
        let mut child = launch.command().spawn()?;
        let (sender, problems) = mpsc::unbounded_channel();
        let output = ServerOutput::read(&mut child, &launch.log, || ProblemLines(sender.clone()));
        Ok(Self {
            child,
            problems,
            family: None,
            output,
        })
    }

    /// Reads which processes the server started, and returns the memory they use. Keeps the
    /// earlier reading when the server exited meanwhile.
    pub async fn sample_family(&mut self) -> Option<u64> {
        let leader = self.leader()?;
        let (family, memory_bytes) = tokio::task::spawn_blocking(move || {
            let family = ProcessFamily::of(leader);
            let memory_bytes = family.memory_bytes();
            (family, memory_bytes)
        })
        .await
        .ok()?;
        if !matches!(self.child.try_wait(), Ok(None)) {
            return None;
        }
        self.family = Some(family);
        Some(memory_bytes)
    }

    /// Sends SIGTERM, a second SIGTERM after the drain budget, and SIGKILL after the force
    /// budget. Then kills what the server left running.
    pub async fn stop(mut self, budget: &ServerBudget) -> io::Result<ExitStatus> {
        self.signal(Signal::SIGTERM);
        if timeout(budget.drain, self.child.wait()).await.is_err() {
            self.sample_family().await;
            self.signal(Signal::SIGTERM);
            if timeout(budget.force, self.child.wait()).await.is_err() {
                let _already_gone = self.child.start_kill();
            }
        }
        let exit = self.child.wait().await;
        self.kill_survivors().await;
        self.output.stop_reading().await;
        exit
    }

    /// After the server exited by itself: kills what it left and describes the exit.
    pub async fn finish(mut self, exit: io::Result<ExitStatus>) -> Failure<CodexProblem> {
        self.kill_survivors().await;
        self.output.stop_reading().await;
        self.output.describe_exit(exit, CodexProblem::in_output)
    }

    /// The server's pid until its exit is collected.
    pub fn leader(&self) -> Option<Pid> {
        let id = self.child.id()?;
        Some(Pid::from_raw(i32::try_from(id).ok()?))
    }

    fn signal(&self, signal: Signal) {
        if let Some(leader) = self.leader() {
            let _already_gone = kill(leader, signal);
        }
    }

    async fn kill_survivors(&mut self) {
        if let Some(family) = self.family.take() {
            let _killed = tokio::task::spawn_blocking(move || family.kill_survivors()).await;
        }
    }
}

/// Sends each line that names a known problem to the supervisor.
struct ProblemLines(mpsc::UnboundedSender<ProblemLine>);

impl LineWatcher for ProblemLines {
    fn watch(&mut self, line: &str) -> bool {
        if let Some(problem) = CodexProblem::in_line(line) {
            let _supervisor_gone = self.0.send(ProblemLine {
                problem,
                line: line.to_owned(),
            });
        }
        true
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;
    use std::time::Duration;

    use nix::sys::wait::{Id, WaitPidFlag, waitid};
    use nix::unistd::getpgid;
    use tokio::time::{Instant, sleep};

    use super::*;
    use crate::manager::agents::Agent;
    use crate::manager::api::test_support::{PidExt, ProgramExt, TestManager, wait_until};
    use crate::manager::codex_remote::problem::tests::{coloured, mfa_warning};
    use crate::manager::processes::Process;

    const WAIT: Duration = Duration::from_secs(10);
    const BUDGET: ServerBudget = ServerBudget {
        probe: Duration::from_secs(1),
        readiness: Duration::from_secs(1),
        drain: Duration::from_millis(500),
        force: Duration::from_millis(500),
        request: Duration::from_secs(1),
        mfa_retry: Duration::ZERO,
        usage: Duration::ZERO,
    };

    /// A fake `codex` whose `app-server` runs `server`.
    fn fake_codex(server: &str) -> (TestManager, CodexLaunch) {
        let manager = TestManager::without_web_app();
        manager.install_fake_cli(
            Agent::Codex,
            &format!("case \"$1\" in\n  app-server)\n{server}\n  ;;\nesac"),
        );
        let launch = CodexLaunch::of_fake(&manager);
        (manager, launch)
    }

    /// Starts a fake `codex` whose `app-server` runs `server`, and waits until `server` created
    /// `$CODEX_HOME/ready`.
    async fn started(server: &str) -> (TestManager, CodexLaunch, CodexServerRun) {
        let (manager, launch) = fake_codex(server);
        let run = CodexServerRun::start(&launch)
            .await
            .expect("the fake starts");
        let ready = launch.codex_home.join("ready");
        wait_until(WAIT, || ready.exists(), |ready| *ready).await;
        (manager, launch, run)
    }

    /// A server that counts each SIGTERM in `$CODEX_HOME/terms` and exits on the `exits_on`th.
    /// The helper it runs until then writes its pid to `$CODEX_HOME/helper`.
    pub fn counting_terms(exits_on: u32, before: &str) -> String {
        format!(
            "terms=0\n\
             trap 'echo term >> \"$CODEX_HOME/terms\"; terms=$((terms + 1)); \
             [ $terms -ge {exits_on} ] && {{ kill $!; exit 0; }}' TERM\n\
             {before}\n\
             sleep 60 > /dev/null 2>&1 &\n\
             echo $! > \"$CODEX_HOME/helper\"\n\
             : > \"$CODEX_HOME/ready\"\n\
             while :; do wait $!; done"
        )
    }

    pub fn terms_seen(codex_home: &Path) -> usize {
        fs::read_to_string(codex_home.join("terms"))
            .unwrap_or_default()
            .lines()
            .count()
    }

    /// Starts a helper in a session of its own, and writes its pid to `$CODEX_HOME/leftover`.
    pub const LEFTOVER: &str =
        "setsid sleep 60 > /dev/null 2>&1 &\necho $! > \"$CODEX_HOME/leftover\"";

    /// Codex's line when another server holds the control socket.
    pub const SOCKET_IN_USE: &str = "Error: app-server control socket is already in use at \
                                     $CODEX_HOME/app-server-control/app-server-control.sock";

    /// A server that runs `before`, then prints `line` and fails once `$CODEX_HOME/exit` exists,
    /// which it removes.
    pub fn exits_when_told(before: &str, line: &str) -> String {
        format!(
            "{before}\n\
             : > \"$CODEX_HOME/ready\"\n\
             while [ ! -e \"$CODEX_HOME/exit\" ]; do sleep 0.05; done\n\
             rm \"$CODEX_HOME/exit\"\n\
             echo \"{line}\" >&2\n\
             exit 1"
        )
    }

    /// The pid the server wrote to `$CODEX_HOME/<name>`.
    pub fn pid_in(codex_home: &Path, name: &str) -> Pid {
        let text = fs::read_to_string(codex_home.join(name)).expect("pid is written");
        Pid::from_raw(text.trim().parse().expect("the pid is a number"))
    }

    #[tokio::test]
    async fn the_server_leads_a_process_group_of_its_own() {
        let (_manager, _launch, run) = started(": > \"$CODEX_HOME/ready\"\nexec sleep 60").await;
        let server = run.leader().expect("the server runs");

        assert_eq!(getpgid(Some(server)), Ok(server));
        run.stop(&BUDGET).await.expect("the server is reaped");
    }

    #[tokio::test]
    async fn only_a_server_without_managed_daemon_starts_without_the_saved_threads() {
        let (_manager, launch) = fake_codex("exec sleep 60");
        let saved_threads = launch.saved_threads();
        fs::create_dir_all(saved_threads.parent().expect("the file has a folder"))
            .expect("the folder is created");
        fs::write(&saved_threads, "{}").expect("threads are saved");
        let without_managed_daemon = CodexLaunch {
            managed_daemon: false,
            ..launch.clone()
        };

        for launch in [launch, without_managed_daemon] {
            let run = CodexServerRun::start(&launch)
                .await
                .expect("the fake starts");
            assert_eq!(saved_threads.exists(), launch.managed_daemon);
            run.stop(&BUDGET).await.expect("the server is reaped");
        }
    }

    #[tokio::test]
    async fn a_server_that_drains_quickly_gets_one_term() {
        let (_manager, launch, run) = started(&counting_terms(1, "")).await;
        let started = Instant::now();

        let exit = run.stop(&BUDGET).await.expect("the server is reaped");

        assert!(started.elapsed() < BUDGET.drain, "{:?}", started.elapsed());
        assert!(exit.success(), "{exit}");
        assert_eq!(terms_seen(&launch.codex_home), 1);
    }

    #[tokio::test]
    async fn a_second_term_forces_the_stop_after_the_drain() {
        let (_manager, launch, run) = started(&counting_terms(2, "")).await;
        let helper = Process::with_id(pid_in(&launch.codex_home, "helper"));
        let started = Instant::now();

        let stopping = tokio::spawn(run.stop(&BUDGET));
        sleep(BUDGET.drain.saturating_sub(Duration::from_millis(250))).await;
        assert!(helper.is_running(), "the helper keeps working in the drain");
        let exit = stopping
            .await
            .expect("the stop ends")
            .expect("the server is reaped");

        let waited = started.elapsed();
        assert!(
            (BUDGET.drain..BUDGET.drain.saturating_add(BUDGET.force)).contains(&waited),
            "{waited:?}"
        );
        assert!(exit.success(), "{exit}");
        assert_eq!(terms_seen(&launch.codex_home), 2);
    }

    #[tokio::test]
    async fn a_server_that_ignores_both_terms_is_killed() {
        let (_manager, launch, run) = started(&counting_terms(3, "")).await;
        let started = Instant::now();

        let exit = run.stop(&BUDGET).await.expect("the server is reaped");

        assert!(started.elapsed() >= BUDGET.drain.saturating_add(BUDGET.force));
        assert_eq!(exit.signal(), Some(Signal::SIGKILL as i32));
        assert_eq!(terms_seen(&launch.codex_home), 2);
    }

    #[tokio::test]
    async fn a_stop_kills_what_the_server_left_in_its_own_session() {
        if !"setsid".is_installed() {
            return;
        }
        let (_manager, launch, run) = started(&counting_terms(2, LEFTOVER)).await;
        let leftover = pid_in(&launch.codex_home, "leftover");

        run.stop(&BUDGET).await.expect("the server is reaped");

        leftover.wait_until_gone().await;
    }

    #[tokio::test]
    async fn a_server_that_exits_by_itself_leaves_nothing_running() {
        if !"setsid".is_installed() {
            return;
        }
        let (_manager, launch, mut run) = started(&exits_when_told(LEFTOVER, SOCKET_IN_USE)).await;
        let leftover = pid_in(&launch.codex_home, "leftover");
        run.sample_family().await;
        fs::write(launch.codex_home.join("exit"), "").expect("the server is told to exit");

        let exit = run.child.wait().await;
        let failure = run.finish(exit).await;

        leftover.wait_until_gone().await;
        assert_eq!(
            failure,
            Failure {
                message: format!(
                    "exit status: 1: Error: app-server control socket is already in use at {}",
                    launch
                        .codex_home
                        .join("app-server-control/app-server-control.sock")
                        .display()
                ),
                problem: Some(CodexProblem::SocketInUse),
            }
        );
    }

    #[tokio::test]
    async fn a_sample_after_the_exit_keeps_the_family_read_before_it() {
        if !"setsid".is_installed() {
            return;
        }
        let (_manager, launch, mut run) = started(&exits_when_told(LEFTOVER, SOCKET_IN_USE)).await;
        let leftover = pid_in(&launch.codex_home, "leftover");
        run.sample_family().await;
        let server = run.leader().expect("the server runs");
        fs::write(launch.codex_home.join("exit"), "").expect("the server is told to exit");
        tokio::task::spawn_blocking(move || {
            waitid(Id::Pid(server), WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT)
        })
        .await
        .expect("the wait runs")
        .expect("the server exits");

        run.sample_family().await;
        let exit = run.child.wait().await;
        run.finish(exit).await;

        leftover.wait_until_gone().await;
    }

    #[tokio::test]
    async fn problem_lines_reach_the_supervisor_without_colour() {
        let warning = mfa_warning();
        let (_manager, launch, mut run) = started(&format!(
            "printf 'starting\\n'\n\
             printf '%s\\n' '{}' >&2\n\
             : > \"$CODEX_HOME/ready\"\n\
             exec sleep 60",
            coloured(&warning)
        ))
        .await;

        let problem = timeout(WAIT, run.problems.recv())
            .await
            .expect("a problem arrives")
            .expect("the watcher is alive");
        assert_eq!(
            problem,
            ProblemLine {
                problem: CodexProblem::MfaRequired,
                line: warning,
            }
        );
        run.stop(&BUDGET).await.expect("the server is reaped");
        let logged = launch.log.tail().expect("the log is readable");
        assert!(
            logged
                .iter()
                .any(|line| line.ends_with("[OUTPUT] starting")),
            "{logged:?}"
        );
    }
}
