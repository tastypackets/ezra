use std::path::Path;

use nix::sys::signal::Signal;
use nix::unistd::Pid;

use super::ServerBudget;
use super::control::ControlSocket;
use crate::manager::processes::{Process, ProcessFamily};

/// The folders in a Codex home that Codex's own daemon runs its copy of Codex from.
const DAEMON_PACKAGES: [&str; 2] = ["packages/app-server-daemon", "packages/standalone"];
const UPDATE_LOOP: &str = "pid-update-loop";

/// A Codex server ezra did not start, holding ezra's control socket.
pub struct ForeignServer;

impl ForeignServer {
    /// Stops the updaters of Codex's own daemon in `codex_home`, then the server on its control
    /// socket, then kills what they started.
    pub async fn stop_all_in(codex_home: &Path, budget: &ServerBudget) {
        let home = codex_home.to_path_buf();
        let updaters = tokio::task::spawn_blocking(move || DaemonUpdater::running_in(&home))
            .await
            .unwrap_or_default();
        let mut families = Vec::new();
        for updater in updaters {
            tracing::warn!("stopping the updater of Codex's own daemon, pid {updater}");
            families.extend(Self::stop(updater, budget).await);
        }
        if let Some(server) = Self::on(&ControlSocket::of(codex_home)).await {
            tracing::warn!("stopping another Codex server, pid {server}, on ezra's socket");
            families.extend(Self::stop(server, budget).await);
        }
        let _killed = tokio::task::spawn_blocking(move || {
            families.iter().for_each(ProcessFamily::kill_survivors);
        })
        .await;
    }

    /// The process answering on `socket`. None when nothing answers, or when that process cannot
    /// be told or is this one or pid 1.
    pub async fn on(socket: &ControlSocket) -> Option<Pid> {
        let (_, peer) = socket.connect().await.ok()?;
        let Some(peer) = peer else {
            tracing::warn!(
                "could not tell which process answers on {}",
                socket.0.display()
            );
            return None;
        };
        if peer.as_raw() <= 1 || peer == Pid::this() {
            tracing::warn!(
                "process {peer} answers on {} and is left running",
                socket.0.display()
            );
            return None;
        }
        Some(peer)
    }

    /// Stops `pid` like ezra's own server, with /proc polls instead of a child. Returns what it
    /// started as last read, which may still run.
    async fn stop(pid: Pid, budget: &ServerBudget) -> Option<ProcessFamily> {
        let mut family = tokio::task::spawn_blocking(move || ProcessFamily::of(pid))
            .await
            .ok()?;
        if let Some(leader) = family.leader() {
            leader.signal(Signal::SIGTERM).await;
            if !leader.exits_within(budget.drain).await {
                if let Ok(again) = tokio::task::spawn_blocking(move || ProcessFamily::of(pid)).await
                    && again.leader() == Some(leader)
                {
                    family = again;
                }
                leader.signal(Signal::SIGTERM).await;
                if !leader.exits_within(budget.force).await {
                    leader.signal(Signal::SIGKILL).await;
                }
            }
        }
        Some(family)
    }
}

/// The updater Codex's own daemon runs.
struct DaemonUpdater;

impl DaemonUpdater {
    /// The updaters running from the daemon's copies of Codex in `codex_home`. Blocks.
    fn running_in(codex_home: &Path) -> Vec<Pid> {
        DAEMON_PACKAGES
            .iter()
            .flat_map(|package| Process::running_inside(&codex_home.join(package)))
            .filter(|(process, _)| {
                process
                    .arguments()
                    .iter()
                    .any(|argument| argument == UPDATE_LOOP)
            })
            .filter_map(|(process, _)| process.id())
            .collect()
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    use nix::sys::signal::killpg;
    use tokio::time::Instant;

    use super::*;
    use crate::manager::api::test_support::{PidExt, ProgramExt, wait_until};
    use crate::manager::codex_remote::control::tests::home;
    use crate::manager::codex_remote::fake::{FakeControlServer, LinkedSocket};
    use crate::manager::codex_remote::run::tests::{LEFTOVER, counting_terms, pid_in, terms_seen};

    const WAIT: Duration = Duration::from_secs(10);
    const BUDGET: ServerBudget = ServerBudget {
        probe: Duration::ZERO,
        readiness: Duration::ZERO,
        drain: Duration::from_millis(500),
        force: Duration::from_millis(500),
        request: Duration::ZERO,
        mfa_retry: Duration::ZERO,
        usage: Duration::ZERO,
        update_deadline: Duration::ZERO,
        pairing_poll: Duration::ZERO,
    };
    const RELEASE: &str = "releases/0.157.1-x86_64-unknown-linux-musl/bin";
    const UPDATER: [&str; 3] = ["app-server", "daemon", UPDATE_LOOP];
    /// Waits on a child and dies at the first SIGTERM.
    pub const SLEEPS: &str = "sleep 600; :";
    /// Listens on the socket `$1`. At SIGTERM it adds `server` to `$CODEX_HOME/stops` and exits.
    const LISTEN: &str = r#"import os, signal, socket, sys, time
def stop(*_):
    with open(os.path.join(os.environ["CODEX_HOME"], "stops"), "a") as stops:
        stops.write("server\n")
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
listener = socket.socket(socket.AF_UNIX)
listener.bind(sys.argv[1])
listener.listen()
time.sleep(600)"#;

    /// A process the test started in a process group of its own, which is killed when dropped.
    pub struct Started(Child);

    impl Started {
        fn spawn(command: &mut Command) -> Self {
            Self(
                command
                    .process_group(0)
                    .stdin(Stdio::null())
                    .spawn()
                    .expect("the process starts"),
            )
        }

        /// Runs `script` with sh and `CODEX_HOME` set to `codex_home`, and returns once it
        /// created `$CODEX_HOME/ready`.
        async fn sh(codex_home: &Path, script: &str) -> Self {
            let started = Self::spawn(
                Command::new("sh")
                    .arg("-c")
                    .arg(script)
                    .env("CODEX_HOME", codex_home),
            );
            let ready = codex_home.join("ready");
            wait_until(WAIT, || ready.exists(), |ready| *ready).await;
            started
        }

        pub fn pid(&self) -> Pid {
            Pid::from_raw(i32::try_from(self.0.id()).expect("the pid fits"))
        }

        pub fn has_exited(&mut self) -> bool {
            self.0
                .try_wait()
                .expect("the process is readable")
                .is_some()
        }

        /// The children it has started so far.
        fn children(&self) -> Vec<Pid> {
            ProcessFamily::of(self.pid())
                .children()
                .filter_map(Process::id)
                .collect()
        }
    }

    impl Drop for Started {
        fn drop(&mut self) {
            let _already_gone = killpg(self.pid(), Signal::SIGKILL);
            let _reaped = self.0.wait();
        }
    }

    /// Runs `script` with a copy of sh in `folder`, `arguments` on its command line and
    /// `CODEX_HOME` set to `codex_home`. Returns once it started a child.
    async fn copy_of_sh(
        codex_home: &Path,
        folder: &Path,
        script: &str,
        arguments: &[&str],
    ) -> Started {
        fs::create_dir_all(folder).expect("the folder is created");
        let copy = folder.join("codex");
        let installed = fs::canonicalize("/bin/sh").expect("sh is installed");
        let copied = Command::new("cp").arg(&installed).arg(&copy).status();
        assert!(copied.expect("cp runs").success());
        let started = Started::spawn(
            Command::new(&copy)
                .args(["-c", script, "codex"])
                .args(arguments)
                .env("CODEX_HOME", codex_home),
        );
        wait_until(WAIT, || started.children(), |children| !children.is_empty()).await;
        started
    }

    /// Codex's own daemon updater in `codex_home`, faked with a copy of sh that runs `script`.
    pub async fn daemon_updater(codex_home: &Path, script: &str) -> Started {
        copy_of_sh(
            codex_home,
            &codex_home.join(DAEMON_PACKAGES[0]).join(RELEASE),
            script,
            &UPDATER,
        )
        .await
    }

    /// Waits until a process answers on the control socket of `codex_home`.
    async fn answering(codex_home: &Path) {
        let socket = ControlSocket::of(codex_home);
        wait_until(
            WAIT,
            || UnixStream::connect(&socket.0).is_ok(),
            |answers| *answers,
        )
        .await;
    }

    /// A Codex server ezra did not start, faked with python3 listening on the control socket of
    /// its Codex home.
    pub struct ForeignListener {
        pub process: Started,
        _socket: LinkedSocket,
    }

    impl ForeignListener {
        /// Returns once it answers.
        pub async fn in_home(codex_home: &Path) -> Self {
            let socket = LinkedSocket::of(codex_home);
            let process = Started::spawn(
                Command::new("python3")
                    .args(["-c", LISTEN])
                    .arg(&socket.path)
                    .env("CODEX_HOME", codex_home),
            );
            answering(codex_home).await;
            Self {
                process,
                _socket: socket,
            }
        }
    }

    #[tokio::test]
    async fn no_process_is_named_when_nothing_answers() {
        let folder = home();
        let socket = ControlSocket(folder.path().join("control"));
        assert_eq!(ForeignServer::on(&socket).await, None);

        drop(UnixListener::bind(&socket.0).expect("the socket is bound"));
        assert_eq!(ForeignServer::on(&socket).await, None);
    }

    #[tokio::test]
    async fn this_process_is_never_named() {
        let home = home();
        let _fake = FakeControlServer::bind(home.path());

        assert_eq!(
            ForeignServer::on(&ControlSocket::of(home.path())).await,
            None
        );
    }

    #[tokio::test]
    async fn another_process_on_the_socket_is_named_and_stopped() {
        if !"python3".is_installed() {
            return;
        }
        let home = home();
        let mut listener = ForeignListener::in_home(home.path()).await;

        let named = ForeignServer::on(&ControlSocket::of(home.path())).await;
        assert_eq!(named, Some(listener.process.pid()));
        let started = Instant::now();
        ForeignServer::stop_all_in(home.path(), &BUDGET).await;

        assert!(started.elapsed() < BUDGET.drain, "{:?}", started.elapsed());
        assert!(listener.process.has_exited());
        let stops = fs::read_to_string(home.path().join("stops")).expect("the stop is written");
        assert_eq!(stops, "server\n");
    }

    #[tokio::test]
    async fn a_server_that_ignores_both_terms_is_killed_after_the_budget() {
        let home = home();
        let mut server = Started::sh(home.path(), &counting_terms(3, "")).await;
        let helper = pid_in(home.path(), "helper");
        let started = Instant::now();

        let family = ForeignServer::stop(server.pid(), &BUDGET)
            .await
            .expect("the family is read");

        let waited = started.elapsed();
        assert!(
            waited >= BUDGET.drain.saturating_add(BUDGET.force),
            "{waited:?}"
        );
        let exit = server.0.wait().expect("the server is reaped");
        assert_eq!(exit.signal(), Some(Signal::SIGKILL as i32));
        assert_eq!(terms_seen(home.path()), 2);
        family.kill_survivors();
        helper.wait_until_gone().await;
    }

    #[tokio::test]
    async fn what_a_server_starts_while_it_drains_is_killed_too() {
        if !"setsid".is_installed() {
            return;
        }
        let home = home();
        let server = Started::sh(
            home.path(),
            &format!(
                "trap '[ -e \"$CODEX_HOME/leftover\" ] || {{ {LEFTOVER}; }}' TERM\n\
                 : > \"$CODEX_HOME/ready\"\n\
                 while :; do sleep 0.05; done"
            ),
        )
        .await;

        let family = ForeignServer::stop(server.pid(), &BUDGET)
            .await
            .expect("the family is read");

        let leftover = pid_in(home.path(), "leftover");
        family.kill_survivors();
        leftover.wait_until_gone().await;
    }

    #[tokio::test]
    async fn a_stop_returns_what_the_server_left_in_a_session_of_its_own() {
        if !"setsid".is_installed() {
            return;
        }
        let home = home();
        let mut server = Started::sh(home.path(), &counting_terms(1, LEFTOVER)).await;
        let leftover = pid_in(home.path(), "leftover");
        let started = Instant::now();

        let family = ForeignServer::stop(server.pid(), &BUDGET)
            .await
            .expect("the family is read");

        assert!(started.elapsed() < BUDGET.drain, "{:?}", started.elapsed());
        assert!(server.has_exited());
        assert_eq!(terms_seen(home.path()), 1);
        family.kill_survivors();
        leftover.wait_until_gone().await;
    }

    #[tokio::test]
    async fn only_updaters_inside_codexs_own_daemon_copies_are_found_and_stopped() {
        let home = home();
        let mut updater = daemon_updater(home.path(), SLEEPS).await;
        let mut legacy = copy_of_sh(
            home.path(),
            &home.path().join(DAEMON_PACKAGES[1]).join(RELEASE),
            SLEEPS,
            &UPDATER,
        )
        .await;
        let mut elsewhere = copy_of_sh(
            home.path(),
            &home.path().join("elsewhere"),
            SLEEPS,
            &UPDATER,
        )
        .await;
        let mut server = copy_of_sh(
            home.path(),
            &home
                .path()
                .join(DAEMON_PACKAGES[0])
                .join("releases/0.157.2-x86_64-unknown-linux-musl/bin"),
            SLEEPS,
            &["app-server", "--listen", "unix://"],
        )
        .await;
        let linked = tempfile::tempdir().expect("a folder is created");
        let link = linked.path().join("codex");
        symlink(home.path(), &link).expect("the Codex home is linked");

        let updaters = [updater.pid(), legacy.pid()];
        assert_eq!(DaemonUpdater::running_in(home.path()), updaters);
        assert_eq!(DaemonUpdater::running_in(&link), updaters);

        let sleeps = [updater.children(), legacy.children()].concat();
        ForeignServer::stop_all_in(home.path(), &BUDGET).await;

        assert!(updater.has_exited());
        assert!(legacy.has_exited());
        for sleep in sleeps {
            sleep.wait_until_gone().await;
        }
        assert!(!elsewhere.has_exited());
        assert!(!server.has_exited());
    }

    #[tokio::test]
    async fn the_updater_stops_first_and_the_server_it_started_still_drains() {
        if !"python3".is_installed() {
            return;
        }
        let home = home();
        let socket = LinkedSocket::of(home.path());
        let mut updater = daemon_updater(
            home.path(),
            &format!(
                "trap 'echo updater >> \"$CODEX_HOME/stops\"; exit 0' TERM\n\
                 python3 -c '{LISTEN}' '{}' &\n\
                 while :; do sleep 0.05; done",
                socket.path.display()
            ),
        )
        .await;
        answering(home.path()).await;
        let server = ForeignServer::on(&ControlSocket::of(home.path()))
            .await
            .expect("the server is named");
        assert!(updater.children().contains(&server));

        ForeignServer::stop_all_in(home.path(), &BUDGET).await;

        let stops = fs::read_to_string(home.path().join("stops")).expect("the stops are written");
        assert_eq!(stops, "updater\nserver\n");
        assert!(updater.has_exited());
        server.wait_until_gone().await;
    }
}
