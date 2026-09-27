use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nix::unistd::Pid;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep_until};

use super::control::{
    ControlClient, ControlError, ControlEvent, ControlNotification, ControlSocket, RelayWire,
    StatusRead,
};
use super::foreign::ForeignServer;
use super::launch::CodexLaunch;
use super::problem::CodexProblem;
use super::run::CodexServerRun;
use super::{CodexRemoteStatus, ServerBudget};
use crate::manager::agents::Agent;
use crate::manager::remote_control::ServerState;
use crate::manager::state::AppState;
use crate::manager::supervision::{
    Failures, FlagExt, RECHECK_INTERVAL, RunEnd, Signals, USAGE_INTERVAL, Verdict, Wake, Wanted,
};

/// How often the control socket is tried while the server starts.
const CONNECT_INTERVAL: Duration = Duration::from_millis(250);
const LONGEST_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const LARGEST_LOG: u64 = 10 * 1024 * 1024;

impl AppState {
    /// Runs Codex's remote control server while it is wanted, until shutdown.
    pub async fn supervise_codex_remote(self) {
        let codex_remote = Arc::clone(&self.codex_remote);
        let mut signals = codex_remote
            .supervision
            .signals(self.agent_checks.watch_sign_in(Agent::Codex));
        let mut failures = Failures::default();
        let mut look_for_foreign = true;
        while !signals.is_shutting_down() {
            if !codex_remote.is_held() && !self.check_sign_in(Agent::Codex, &mut signals).await {
                break;
            }
            let run = match self.wanted_codex_server().await {
                Wanted::Off => {
                    self.idle_codex_server(
                        |status| status.idle(ServerState::Off, None),
                        &mut signals,
                    )
                    .await
                }
                Wanted::Waiting(problem) => {
                    self.idle_codex_server(
                        |status| status.idle(ServerState::Waiting, problem),
                        &mut signals,
                    )
                    .await
                }
                Wanted::Unknown => {
                    self.idle_codex_server(CodexRemoteStatus::unsure, &mut signals)
                        .await
                }
                Wanted::Server(launch) if look_for_foreign => {
                    look_for_foreign = false;
                    let stopping =
                        ForeignServer::stop_all_in(&launch.codex_home, &codex_remote.budget);
                    tokio::select! {
                        () = stopping => RunEnd::Reconsidered,
                        () = signals.shutdown.until_set() => RunEnd::ShutDown,
                    }
                }
                Wanted::Server(launch) => {
                    let started = Instant::now();
                    let end = self.run_codex_server(&launch, &mut signals).await;
                    failures.forget_after_healthy_run(started);
                    end
                }
            };
            match run {
                RunEnd::Reconsidered => {}
                RunEnd::ShutDown => break,
                RunEnd::Failed(failure) => {
                    look_for_foreign = failure.problem == Some(CodexProblem::SocketInUse);
                    failures.add_one();
                    tracing::warn!("Codex remote control stopped: {}", failure.message);
                    codex_remote.status.update(|status| status.fail(failure));
                    if signals.wait_for_change(failures.retry_delay()).await == Wake::ShutDown {
                        break;
                    }
                }
            }
        }
        codex_remote.supervision.mark_stopped();
    }

    async fn idle_codex_server(
        &self,
        show: impl FnOnce(&mut CodexRemoteStatus),
        signals: &mut Signals,
    ) -> RunEnd<CodexProblem> {
        self.codex_remote.status.update(show);
        match signals.wait_for_change(RECHECK_INTERVAL).await {
            Wake::Changed => RunEnd::Reconsidered,
            Wake::ShutDown => RunEnd::ShutDown,
        }
    }

    async fn wanted_codex_server(&self) -> Wanted<CodexLaunch, Option<CodexProblem>> {
        let settings = self.settings.lock().await.agents.codex.remote_control;
        if !settings.enabled {
            return Wanted::Off;
        }
        let installed = self.install_paths.installed_command(Agent::Codex);
        let codex_home = self.install_paths.config_directory(Agent::Codex);
        let (Some((codex, version)), Some(codex_home)) = (installed, codex_home) else {
            return Wanted::Waiting(None);
        };
        if self.codex_remote.is_held() {
            return Wanted::Waiting(None);
        }
        let sign_in = match self.agent_checks.sign_in(Agent::Codex) {
            None => return Wanted::Unknown,
            Some(sign_in) if !sign_in.logged_in => return Wanted::Waiting(None),
            Some(sign_in) => sign_in.method,
        };
        if sign_in.is_some_and(|method| !method.uses_codex_backend()) {
            return Wanted::Waiting(Some(CodexProblem::NotChatGpt));
        }
        let codex_remote = &self.codex_remote;
        let flags = match codex_remote
            .flags
            .flags(&codex, &version, codex_home, &codex_remote.budget)
            .await
        {
            Ok(flags) => flags,
            Err(error) => {
                tracing::warn!("{error}");
                return Wanted::Unknown;
            }
        };
        if !flags.remote_control {
            return Wanted::Waiting(Some(CodexProblem::UnsupportedVersion));
        }
        Wanted::Server(CodexLaunch {
            codex,
            version,
            codex_home: codex_home.to_path_buf(),
            projects: self.projects.0.clone(),
            sandbox: settings.sandbox,
            approvals: settings.approvals,
            sign_in,
            managed_daemon: flags.managed_daemon,
            log: codex_remote.log.clone(),
        })
    }

    async fn run_codex_server(
        &self,
        launch: &CodexLaunch,
        signals: &mut Signals,
    ) -> RunEnd<CodexProblem> {
        signals.restarts.mark_unchanged();
        if signals.is_shutting_down() {
            return RunEnd::ShutDown;
        }
        let codex_remote = &self.codex_remote;
        if let Err(error) = launch.log.prepare().await {
            tracing::warn!("could not prepare {}: {error}", launch.log.0.display());
        }
        let mut server = match CodexServerRun::start(launch).await {
            Ok(server) => server,
            Err(error) => return RunEnd::Failed(format!("could not start: {error}").into()),
        };
        let leader = server
            .leader()
            .expect("a server that just started has a pid");
        codex_remote
            .status
            .update(|status| status.start(&launch.version));
        tracing::info!(
            "Codex remote control is starting on Codex {}",
            launch.version
        );
        let mut link = ControlLink::opening(
            launch,
            codex_remote.expected.pid(leader),
            codex_remote.budget,
        );
        let mut recheck = interval(RECHECK_INTERVAL);
        recheck.set_missed_tick_behavior(MissedTickBehavior::Delay);
        recheck.reset();
        let mut usage = interval(USAGE_INTERVAL);
        usage.set_missed_tick_behavior(MissedTickBehavior::Delay);
        usage.reset();
        loop {
            tokio::select! {
                exit = server.child.wait() => {
                    return RunEnd::Failed(server.finish(exit).await);
                }
                Some(line) = server.problems.recv() => {
                    codex_remote.status.update(|status| status.name_problem(line));
                    continue;
                }
                event = link.next() => {
                    match event {
                        LinkEvent::Ready(relay) => {
                            tracing::info!("Codex remote control answers as {}", relay.server_name);
                            server.sample_family().await;
                            codex_remote.status.update(|status| status.answer(relay));
                        }
                        LinkEvent::Control(ControlEvent::Notification(
                            ControlNotification::StatusChanged(relay),
                        )) => codex_remote.status.update(|status| status.show_relay(relay)),
                        LinkEvent::Control(_) => {}
                        LinkEvent::Lost => {
                            tracing::warn!("lost Codex's control connection, opening it again");
                            codex_remote.status.update(CodexRemoteStatus::lose_control);
                        }
                        LinkEvent::GaveUp(message) => {
                            if let Err(error) = server.stop(&codex_remote.budget).await {
                                tracing::warn!("could not wait for Codex to stop: {error}");
                            }
                            return RunEnd::Failed(message.into());
                        }
                    }
                    continue;
                }
                _ = signals.restarts.changed() => {
                    self.stop_codex_server(server).await;
                    return RunEnd::Reconsidered;
                }
                _ = signals.changes.changed() => {}
                _ = signals.sign_in.changed() => {}
                _ = recheck.tick() => {
                    if let Err(error) = launch.log.rotate_when_larger_than(LARGEST_LOG).await {
                        tracing::warn!("could not rotate {}: {error}", launch.log.debug_file().display());
                    }
                    let agent_checks = Arc::clone(&self.agent_checks);
                    tokio::spawn(async move {
                        agent_checks.check_sign_in(Agent::Codex, RECHECK_INTERVAL).await;
                    });
                }
                _ = usage.tick() => {
                    server.sample_family().await;
                    continue;
                }
                () = signals.shutdown.until_set() => {
                    self.stop_codex_server(server).await;
                    return RunEnd::ShutDown;
                }
            }
            let wanted = tokio::select! {
                wanted = self.wanted_codex_server() => wanted,
                () = signals.shutdown.until_set() => {
                    self.stop_codex_server(server).await;
                    return RunEnd::ShutDown;
                }
            };
            match wanted.verdict(launch) {
                Verdict::Keep => {
                    let unsupported = matches!(
                        wanted,
                        Wanted::Waiting(Some(CodexProblem::UnsupportedVersion))
                    );
                    codex_remote
                        .status
                        .update(|status| status.keep(unsupported));
                }
                Verdict::Unsure => {}
                Verdict::Stop => {
                    self.stop_codex_server(server).await;
                    return RunEnd::Reconsidered;
                }
                Verdict::Update(version) => {
                    tracing::info!("restarting Codex remote control on Codex {version}");
                    self.stop_codex_server(server).await;
                    return RunEnd::Reconsidered;
                }
            }
        }
    }

    async fn stop_codex_server(&self, server: CodexServerRun) {
        self.codex_remote
            .status
            .update(|status| status.enter(ServerState::Stopping));
        if let Err(error) = server.stop(&self.codex_remote.budget).await {
            tracing::warn!("could not wait for Codex to stop: {error}");
        }
    }
}

impl Wanted<CodexLaunch, Option<CodexProblem>> {
    /// What to do with the server running `running`. A version that cannot serve the ChatGPT app
    /// leaves the running one in place.
    fn verdict(&self, running: &CodexLaunch) -> Verdict {
        match self {
            Self::Server(wanted) if wanted == running => Verdict::Keep,
            Self::Server(wanted) if wanted.is_update_of(running) => {
                Verdict::Update(wanted.version.clone())
            }
            Self::Waiting(Some(CodexProblem::UnsupportedVersion)) => Verdict::Keep,
            Self::Unknown => Verdict::Unsure,
            Self::Server(_) | Self::Off | Self::Waiting(_) => Verdict::Stop,
        }
    }
}

/// What happened on the control connection.
#[derive(Debug)]
enum LinkEvent {
    /// The connection opened, with the relay as it was then.
    Ready(RelayWire),
    Control(ControlEvent),
    /// The connection closed, and opening it again started.
    Lost,
    /// Nothing opened it in time, for this reason.
    GaveUp(String),
}

/// The control connection to a running server, opened again when it is lost.
struct ControlLink {
    socket: ControlSocket,
    expected: Pid,
    codex_home: PathBuf,
    budget: ServerBudget,
    state: LinkState,
}

enum LinkState {
    Open {
        _client: ControlClient,
        events: mpsc::UnboundedReceiver<ControlEvent>,
    },
    Opening(Attempts),
}

impl ControlLink {
    /// Starts trying to open the connection to the server `launch` started, answered by
    /// `expected`.
    fn opening(launch: &CodexLaunch, expected: Pid, budget: ServerBudget) -> Self {
        Self {
            socket: ControlSocket::of(&launch.codex_home),
            expected,
            codex_home: launch.codex_home.clone(),
            budget,
            state: LinkState::Opening(Attempts::at_start(&budget)),
        }
    }

    /// Waits for the next event. Dropping the wait loses nothing.
    async fn next(&mut self) -> LinkEvent {
        loop {
            let attempts = match &mut self.state {
                LinkState::Open { events, .. } => {
                    let event = events.recv().await;
                    return match event {
                        Some(ControlEvent::Closed) | None => {
                            self.state = LinkState::Opening(Attempts::after_loss(&self.budget));
                            LinkEvent::Lost
                        }
                        Some(event) => LinkEvent::Control(event),
                    };
                }
                LinkState::Opening(attempts) => attempts,
            };
            let deadline = sleep_until(attempts.deadline);
            let Some(running) = attempts.running.as_mut() else {
                tokio::select! {
                    () = sleep_until(attempts.next) => {
                        attempts.running = Some(tokio::spawn(Opened::open(
                            self.socket.clone(),
                            self.expected,
                            self.codex_home.clone(),
                            self.budget,
                        )));
                    }
                    () = deadline => return LinkEvent::GaveUp(attempts.give_up()),
                }
                continue;
            };
            let opened = tokio::select! {
                joined = running => joined,
                () = deadline => return LinkEvent::GaveUp(attempts.give_up()),
            };
            attempts.running = None;
            match opened {
                Ok(Ok(Opened {
                    client,
                    events,
                    relay,
                })) => {
                    self.state = LinkState::Open {
                        _client: client,
                        events,
                    };
                    return LinkEvent::Ready(relay);
                }
                Ok(Err(error)) => attempts.failed(error.to_string()),
                Err(error) => attempts.failed(error.to_string()),
            }
        }
    }
}

/// A control connection that answered, and the relay it reported.
struct Opened {
    client: ControlClient,
    events: mpsc::UnboundedReceiver<ControlEvent>,
    relay: RelayWire,
}

impl Opened {
    async fn open(
        socket: ControlSocket,
        expected: Pid,
        codex_home: PathBuf,
        budget: ServerBudget,
    ) -> Result<Self, ControlError> {
        let (client, events) =
            ControlClient::connect(&socket, expected, &codex_home, &budget).await?;
        let relay = client.request(StatusRead).await?;
        Ok(Self {
            client,
            events,
            relay,
        })
    }
}

/// Tries to open the control connection until a deadline.
struct Attempts {
    /// Why the server failed when no try succeeds in time.
    failure: &'static str,
    longest: Duration,
    deadline: Instant,
    next: Instant,
    delay: Duration,
    longest_delay: Duration,
    running: Option<JoinHandle<Result<Opened, ControlError>>>,
    last_error: Option<String>,
}

impl Attempts {
    /// Tries at once and every 250 ms after, for the readiness budget.
    fn at_start(budget: &ServerBudget) -> Self {
        Self::until(
            budget.readiness,
            "did not answer on its control socket",
            Duration::ZERO,
            CONNECT_INTERVAL,
        )
    }

    /// Tries after 250 ms and then twice as long each time up to 5 s, for the readiness budget.
    fn after_loss(budget: &ServerBudget) -> Self {
        Self::until(
            budget.readiness,
            "lost its control connection, which did not open again",
            CONNECT_INTERVAL,
            LONGEST_RECONNECT_DELAY,
        )
    }

    fn until(
        longest: Duration,
        failure: &'static str,
        first_delay: Duration,
        longest_delay: Duration,
    ) -> Self {
        let now = Instant::now();
        Self {
            failure,
            longest,
            deadline: now.checked_add(longest).unwrap_or(now),
            next: now.checked_add(first_delay).unwrap_or(now),
            delay: CONNECT_INTERVAL,
            longest_delay,
            running: None,
            last_error: None,
        }
    }

    fn failed(&mut self, error: String) {
        let now = Instant::now();
        self.last_error = Some(error);
        self.next = now.checked_add(self.delay).unwrap_or(now);
        self.delay = self.delay.saturating_mul(2).min(self.longest_delay);
    }

    /// Stops the try still running, and says why the server failed.
    fn give_up(&mut self) -> String {
        if let Some(running) = self.running.take() {
            running.abort();
        }
        match &self.last_error {
            Some(error) => format!("{} within {:?}: {error}", self.failure, self.longest),
            None => format!("{} within {:?}", self.failure, self.longest),
        }
    }
}

impl Drop for Attempts {
    fn drop(&mut self) {
        if let Some(running) = &self.running {
            running.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use axum::http::StatusCode;
    use nix::sys::signal::{Signal, kill};
    use serde_json::json;
    use tokio::time::timeout;

    use super::*;
    use crate::manager::api::test_support::{
        EventStreamExt, PidExt, ProgramExt, ResponseExt, TestManager, wait_until,
    };
    use crate::manager::codex_remote::control::RelayStatusWire;
    use crate::manager::codex_remote::fake::{FakeControlServer, Reply};
    use crate::manager::codex_remote::foreign::tests::{
        ForeignListener, SLEEPS, Started, daemon_updater,
    };
    use crate::manager::codex_remote::launch::LaunchFlags;
    use crate::manager::codex_remote::problem::tests::relay_warning;
    use crate::manager::codex_remote::run::tests::{
        LEFTOVER, SOCKET_IN_USE, counting_terms, exits_when_told, terms_seen,
    };
    use crate::manager::codex_remote::{
        CodexApprovals, CodexRemoteSettings, CodexSandbox, ExpectedPeer, RelayState,
    };
    use crate::manager::events::Topic;
    use crate::manager::remote_control::RemoteControlOverview;
    use crate::manager::supervision::{FIRST_RETRY_DELAY, ServerLog};

    const WAIT: Duration = Duration::from_secs(10);
    const BUDGET: ServerBudget = ServerBudget {
        probe: Duration::from_secs(5),
        readiness: Duration::from_secs(5),
        drain: Duration::from_secs(1),
        force: Duration::from_secs(1),
        request: Duration::from_secs(5),
    };
    const SHORT_READINESS: ServerBudget = ServerBudget {
        readiness: Duration::from_secs(1),
        ..BUDGET
    };
    const SHORT_PROBE: ServerBudget = ServerBudget {
        probe: Duration::from_secs(1),
        ..BUDGET
    };
    const SETTINGS: &str = "/api/v1/agents/codex/settings";
    const TAKES_EVERY_FLAG: &str = "*--help) exit 0 ;;";
    const REMOTE_CONTROL_PROBE: &str = "app-server --remote-control --help";
    const REMOTE_CONTROL_HANGS: &str = "'app-server --remote-control --help') exec sleep 30 ;;";
    const SERVE: &str = "exec sleep 60";
    const CHATGPT: &str = "Logged in using ChatGPT";
    const ACCESS_TOKEN: &str = "Logged in using access token";
    const API_KEY: &str = "Logged in using an API key - sk-proj-***ABCDE";
    const BEDROCK: &str = "Logged in using Amazon Bedrock API key";
    const SIGNED_OUT: &str = "Not logged in";

    /// A manager whose Codex server is answered by a fake in the test process.
    fn manager(budget: ServerBudget) -> TestManager {
        let manager = TestManager::new().with_codex(budget, ExpectedPeer::Pid(Pid::this()));
        fs::create_dir_all(&manager.state.projects.0).expect("projects is created");
        manager
    }

    fn codex_home(manager: &TestManager) -> PathBuf {
        manager
            .state
            .install_paths
            .config_directory(Agent::Codex)
            .expect("the test manager has a Codex home")
            .to_path_buf()
    }

    /// Installs a `codex` as `version` that answers its flag probes with `probes`, is signed in as
    /// `$CODEX_HOME/sign-in` says, and whose `app-server` adds its pid to `$CODEX_HOME/servers`
    /// before it runs `server`.
    fn install_codex(manager: &TestManager, version: &str, probes: &str, server: &str) {
        manager.install_fake_version(
            Agent::Codex,
            version,
            &format!(
                "case \"$*\" in\n  \
                   {probes}\n  \
                   *--help) exit 0 ;;\n  \
                   'login status')\n    \
                     cat \"$CODEX_HOME/sign-in\" >&2\n    \
                     grep -q '^Logged in' \"$CODEX_HOME/sign-in\" ;;\n  \
                   app-server*)\n    \
                     echo $$ >> \"$CODEX_HOME/servers\"\n\
                     {server} ;;\n\
                 esac"
            ),
        );
    }

    fn sign_in(manager: &TestManager, status: &str) {
        let home = codex_home(manager);
        fs::create_dir_all(&home).expect("the Codex home is created");
        fs::write(home.join("sign-in"), format!("{status}\n")).expect("the sign-in is written");
    }

    async fn sign_in_now(manager: &TestManager, status: &str) {
        sign_in(manager, status);
        manager.state.agent_checks.refresh(Agent::Codex).await;
    }

    /// The pid of each server started, in order.
    fn servers(manager: &TestManager) -> Vec<Pid> {
        fs::read_to_string(codex_home(manager).join("servers"))
            .unwrap_or_default()
            .lines()
            .map(|pid| Pid::from_raw(pid.parse().expect("the pid is a number")))
            .collect()
    }

    /// The arguments of each server started, in order.
    fn server_arguments(manager: &TestManager) -> Vec<String> {
        manager
            .fake_cli_runs(Agent::Codex)
            .into_iter()
            .filter(|run| run.starts_with("app-server") && !run.ends_with("--help"))
            .collect()
    }

    async fn servers_started(manager: &TestManager, count: usize) -> Vec<Pid> {
        wait_until(WAIT, || servers(manager), |servers| servers.len() >= count).await
    }

    /// Waits until `app-server --remote-control --help` was asked `times` times.
    async fn remote_control_probed(manager: &TestManager, times: usize) {
        wait_until(
            WAIT,
            || {
                manager
                    .fake_cli_runs(Agent::Codex)
                    .iter()
                    .filter(|run| *run == REMOTE_CONTROL_PROBE)
                    .count()
            },
            |probes| *probes >= times,
        )
        .await;
    }

    async fn status_until(
        manager: &TestManager,
        wanted: impl Fn(&CodexRemoteStatus) -> bool,
    ) -> CodexRemoteStatus {
        wait_until(WAIT, || manager.state.codex_remote.status(), wanted).await
    }

    fn connected(status: &CodexRemoteStatus) -> bool {
        status.state == ServerState::Running && status.relay == Some(RelayState::Connected)
    }

    /// Serves the control socket and reports the relay as `relay` the way Codex does: pushed
    /// after `initialize`, then read.
    fn control(manager: &TestManager, relay: &str) -> FakeControlServer {
        let fake = FakeControlServer::bind(&codex_home(manager));
        fake.push_after("initialize", FakeControlServer::status_changed(relay));
        fake.reply(
            "remoteControl/status/read",
            [Reply::Result(FakeControlServer::relay(relay))],
        );
        fake
    }

    /// Serves the control socket once the first server started.
    async fn serve_control(manager: &TestManager, relay: &str) -> FakeControlServer {
        servers_started(manager, 1).await;
        control(manager, relay)
    }

    fn supervise(manager: &TestManager) -> JoinHandle<()> {
        tokio::spawn(manager.state.clone().supervise_codex_remote())
    }

    async fn shut_down(manager: &TestManager, supervisor: JoinHandle<()>) {
        manager.state.codex_remote.supervision.begin_shut_down();
        supervisor.await.expect("the supervisor stops");
    }

    async fn save(manager: &TestManager, cookie: &str, settings: CodexRemoteSettings) {
        let body = json!({ "remote_control": settings }).to_string();
        let response = manager.put(SETTINGS, &body, Some(cookie)).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// Starts a supervisor with `version` installed and signed in with ChatGPT, and waits until
    /// Codex is connected.
    async fn running(
        manager: &TestManager,
        version: &str,
    ) -> (JoinHandle<()>, FakeControlServer, Pid) {
        install_codex(manager, version, TAKES_EVERY_FLAG, SERVE);
        sign_in(manager, CHATGPT);
        let supervisor = supervise(manager);
        let fake = serve_control(manager, "connected").await;
        let running = status_until(manager, connected).await;
        assert_eq!(running.restarts, 0);
        let server = *servers(manager).first().expect("a server started");
        (supervisor, fake, server)
    }

    /// Like `running`, with a server that first leaves a helper running in a session of its own.
    /// Also returns the helper's pid.
    async fn running_with_a_helper(
        manager: &TestManager,
    ) -> (JoinHandle<()>, FakeControlServer, Pid, Pid) {
        install_codex(
            manager,
            "0.157.1",
            TAKES_EVERY_FLAG,
            &format!("{LEFTOVER}\n{SERVE}"),
        );
        sign_in(manager, CHATGPT);
        let supervisor = supervise(manager);
        let leftover = codex_home(manager).join("leftover");
        let helper = wait_until(
            WAIT,
            || {
                fs::read_to_string(&leftover)
                    .ok()
                    .and_then(|pid| pid.trim().parse::<i32>().ok())
            },
            Option::is_some,
        )
        .await
        .map(Pid::from_raw)
        .expect("the helper runs");
        let fake = control(manager, "connected");
        status_until(manager, connected).await;
        let server = *servers(manager).first().expect("a server started");
        (supervisor, fake, server, helper)
    }

    #[tokio::test]
    async fn codex_serves_the_chatgpt_app_once_installed_and_signed_in_with_chatgpt() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let mut events = Box::pin(manager.state.events.stream());
        let supervisor = supervise(&manager);
        wait_until(
            WAIT,
            || manager.state.agent_checks.sign_in(Agent::Codex),
            Option::is_some,
        )
        .await;

        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, CHATGPT);
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        let _fake = serve_control(&manager, "connected").await;

        let running = status_until(&manager, connected).await;
        assert_eq!(running.server_name.as_deref(), Some("ezra-dev"));
        assert_eq!(running.server_version.as_deref(), Some("0.157.1"));
        assert_eq!((running.problem, running.restarts), (None, 0));
        assert_eq!(
            server_arguments(&manager),
            [
                r#"app-server --remote-control --managed-daemon --listen unix:// -c sandbox_mode="danger-full-access" -c approval_policy="on-request""#
            ]
        );
        assert!(events.published().await.contains(&Topic::RemoteControl));
        let overview: RemoteControlOverview = manager
            .get("/api/v1/remote-control", Some(&cookie))
            .await
            .json()
            .await;
        assert_eq!(
            (
                overview.codex.state,
                overview.codex.relay,
                overview.codex.server_name.as_deref(),
                overview.codex.server_version.as_deref()
            ),
            (
                ServerState::Running,
                Some(RelayState::Connected),
                Some("ezra-dev"),
                Some("0.157.1")
            )
        );

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn codex_stops_while_its_switch_is_off_and_starts_when_it_is_on() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let (supervisor, _fake, first) = running(&manager, "0.157.1").await;

        let off = CodexRemoteSettings {
            enabled: false,
            ..CodexRemoteSettings::default()
        };
        save(&manager, &cookie, off).await;
        let stopped = status_until(&manager, |status| status.state == ServerState::Off).await;
        assert_eq!(
            (stopped.relay, stopped.server_version, stopped.restarts),
            (None, None, 0)
        );
        first.wait_until_gone().await;

        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        servers_started(&manager, 2).await;
        status_until(&manager, connected).await;

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn codex_waits_for_a_chatgpt_sign_in() {
        let manager = manager(BUDGET);
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, API_KEY);
        let supervisor = supervise(&manager);
        let with_api_key = status_until(&manager, |status| status.problem.is_some()).await;
        assert_eq!(
            (with_api_key.state, with_api_key.problem),
            (ServerState::Waiting, Some(CodexProblem::NotChatGpt))
        );

        sign_in_now(&manager, SIGNED_OUT).await;
        status_until(&manager, |status| {
            status.state == ServerState::Waiting && status.problem.is_none()
        })
        .await;
        assert!(servers(&manager).is_empty());

        sign_in_now(&manager, CHATGPT).await;
        let _fake = serve_control(&manager, "connected").await;
        let running = status_until(&manager, connected).await;
        assert_eq!(running.problem, None);
        assert_eq!(servers(&manager).len(), 1);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn codex_serves_with_an_access_token_but_not_with_bedrock() {
        let manager = manager(BUDGET);
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, BEDROCK);
        let supervisor = supervise(&manager);
        let with_bedrock = status_until(&manager, |status| status.problem.is_some()).await;
        assert_eq!(
            (with_bedrock.state, with_bedrock.problem),
            (ServerState::Waiting, Some(CodexProblem::NotChatGpt))
        );

        sign_in_now(&manager, ACCESS_TOKEN).await;
        let _fake = serve_control(&manager, "connected").await;
        let with_access_token = status_until(&manager, connected).await;
        assert_eq!(with_access_token.problem, None);

        sign_in_now(&manager, CHATGPT).await;
        let restarted = servers_started(&manager, 2).await;
        restarted
            .first()
            .expect("a server started")
            .wait_until_gone()
            .await;
        status_until(&manager, connected).await;

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_settings_change_restarts_codex_at_once_with_the_new_settings() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let (supervisor, _fake, first) = running(&manager, "0.157.1").await;

        let chosen = CodexRemoteSettings {
            enabled: true,
            sandbox: CodexSandbox::ReadOnly,
            approvals: CodexApprovals::Never,
        };
        save(&manager, &cookie, chosen).await;
        servers_started(&manager, 2).await;
        first.wait_until_gone().await;
        assert_eq!(
            server_arguments(&manager).last().map(String::as_str),
            Some(
                r#"app-server --remote-control --managed-daemon --listen unix:// -c sandbox_mode="read-only" -c approval_policy="never""#
            )
        );
        let running = status_until(&manager, connected).await;
        assert_eq!(running.restarts, 0);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn codex_shows_the_relay_and_the_first_line_of_a_problem_while_it_runs() {
        let manager = manager(BUDGET);
        let signed_out = relay_warning(
            "remote control requires ChatGPT authentication",
            "PermissionDenied",
        );
        let again = signed_out.replace("reconnect_attempt=3", "reconnect_attempt=4");
        install_codex(
            &manager,
            "0.157.1",
            TAKES_EVERY_FLAG,
            &format!(
                "printf '%s\\n' '{signed_out}' >&2\n\
                 while [ ! -e \"$CODEX_HOME/again\" ]; do sleep 0.05; done\n\
                 printf '%s\\n' '{again}' >&2\n\
                 {SERVE}"
            ),
        );
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        let fake = serve_control(&manager, "errored").await;

        let running = status_until(&manager, |status| {
            status.state == ServerState::Running && status.problem.is_some()
        })
        .await;
        assert_eq!(
            (
                running.relay,
                running.problem,
                running.last_error.as_deref()
            ),
            (
                Some(RelayState::Errored),
                Some(CodexProblem::SignedOut),
                Some(signed_out.as_str())
            )
        );

        let mut events = Box::pin(manager.state.events.stream());
        fs::write(codex_home(&manager).join("again"), "").expect("the server is told to print");
        wait_until(
            WAIT,
            || manager.state.codex_remote.log.tail().unwrap_or_default(),
            |lines| lines.iter().any(|line| line.ends_with(&again)),
        )
        .await;
        assert!(!events.published().await.contains(&Topic::RemoteControl));
        assert_eq!(manager.state.codex_remote.status(), running);

        fake.push(FakeControlServer::status_changed("connected"));
        let connected = status_until(&manager, connected).await;
        assert_eq!((connected.problem, connected.last_error), (None, None));

        fake.push(FakeControlServer::status_changed("errored"));
        let errored =
            status_until(&manager, |status| status.relay == Some(RelayState::Errored)).await;
        assert_eq!((errored.state, errored.restarts), (ServerState::Running, 0));

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_codex_server_that_exits_is_retried_and_its_output_logged() {
        let manager = manager(BUDGET);
        install_codex(
            &manager,
            "0.157.1",
            TAKES_EVERY_FLAG,
            "echo 'Error: remote control is disabled by managed requirements' >&2\nexit 1",
        );
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);

        let retrying = status_until(&manager, |status| status.state == ServerState::Retrying).await;
        assert_eq!(retrying.restarts, 1);
        assert_eq!(retrying.problem, Some(CodexProblem::NotAllowed));
        assert!(
            retrying.last_error.as_deref().is_some_and(|error| error
                .ends_with("Error: remote control is disabled by managed requirements")),
            "{retrying:?}"
        );
        let logged = manager
            .state
            .codex_remote
            .log
            .tail()
            .expect("the log is readable");
        assert!(
            logged.iter().any(|line| line
                .ends_with("[OUTPUT] Error: remote control is disabled by managed requirements")),
            "{logged:?}"
        );

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_codex_server_that_exits_cleanly_is_retried_too() {
        let manager = manager(BUDGET);
        install_codex(
            &manager,
            "0.157.1",
            TAKES_EVERY_FLAG,
            "echo 'shutting down' >&2\nexit 0",
        );
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);

        let retrying = status_until(&manager, |status| status.state == ServerState::Retrying).await;
        assert_eq!(retrying.restarts, 1);
        assert_eq!(
            retrying.last_error.as_deref(),
            Some("exit status: 0: shutting down")
        );

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn stopping_codex_kills_what_its_server_left_running() {
        if !"setsid".is_installed() {
            return;
        }
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let (supervisor, _fake, server, helper) = running_with_a_helper(&manager).await;

        let off = CodexRemoteSettings {
            enabled: false,
            ..CodexRemoteSettings::default()
        };
        save(&manager, &cookie, off).await;
        server.wait_until_gone().await;
        helper.wait_until_gone().await;

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_codex_server_that_dies_leaves_nothing_running() {
        if !"setsid".is_installed() {
            return;
        }
        let manager = manager(BUDGET);
        let (supervisor, _fake, server, helper) = running_with_a_helper(&manager).await;

        kill(server, Signal::SIGKILL).expect("the server is killed");
        let retrying = status_until(&manager, |status| status.state == ServerState::Retrying).await;
        assert_eq!(retrying.restarts, 1);
        helper.wait_until_gone().await;

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_server_that_never_answers_on_its_socket_is_stopped_and_retried() {
        let manager = manager(SHORT_READINESS);
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        let first = *servers_started(&manager, 1)
            .await
            .first()
            .expect("a server started");

        let retrying = status_until(&manager, |status| status.state == ServerState::Retrying).await;
        assert_eq!(retrying.restarts, 1);
        assert!(
            retrying.last_error.as_deref().is_some_and(
                |error| error.starts_with("did not answer on its control socket within 1s")
            ),
            "{retrying:?}"
        );
        first.wait_until_gone().await;
        wait_until(
            FIRST_RETRY_DELAY.saturating_add(WAIT),
            || servers(&manager).len(),
            |started| *started == 2,
        )
        .await;

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn another_process_on_the_socket_is_never_taken_for_the_server() {
        let manager =
            TestManager::new().with_codex(SHORT_READINESS, ExpectedPeer::Pid(Pid::from_raw(1)));
        fs::create_dir_all(&manager.state.projects.0).expect("projects is created");
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, CHATGPT);
        let fake = control(&manager, "connected");
        let supervisor = supervise(&manager);

        let retrying = status_until(&manager, |status| status.state == ServerState::Retrying).await;
        assert!(
            retrying
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains(&format!(
                    "another Codex server, pid {}, answers on the control socket",
                    Pid::this()
                ))),
            "{retrying:?}"
        );
        assert!(fake.requests().is_empty(), "{:?}", fake.requests());

        shut_down(&manager, supervisor).await;
    }

    /// Starts a supervisor whose first server fails with `line` while Codex's own daemon updater
    /// runs, and waits until the next server started. Returns the updater and the failure.
    async fn after_a_failure_with(
        manager: &TestManager,
        line: &str,
    ) -> (JoinHandle<()>, Started, CodexRemoteStatus) {
        install_codex(
            manager,
            "0.157.1",
            TAKES_EVERY_FLAG,
            &exits_when_told("", line),
        );
        sign_in(manager, CHATGPT);
        let supervisor = supervise(manager);
        servers_started(manager, 1).await;
        let updater = daemon_updater(&codex_home(manager), SLEEPS).await;
        fs::write(codex_home(manager).join("exit"), "").expect("the server is told to fail");
        let retrying = status_until(manager, |status| status.state == ServerState::Retrying).await;
        wait_until(
            FIRST_RETRY_DELAY.saturating_add(WAIT),
            || servers(manager).len(),
            |started| *started == 2,
        )
        .await;
        (supervisor, updater, retrying)
    }

    #[tokio::test]
    async fn a_listener_in_the_manager_itself_is_left_running_and_codex_starts() {
        let manager = manager(BUDGET);
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, CHATGPT);
        let fake = control(&manager, "connected");
        assert_eq!(
            ForeignServer::on(&ControlSocket::of(&codex_home(&manager))).await,
            None
        );

        let supervisor = supervise(&manager);
        let running = status_until(&manager, connected).await;
        assert_eq!(running.restarts, 0);
        assert_eq!(servers(&manager).len(), 1);
        assert_eq!(
            fake.requests().first().map(|(method, _)| method.as_str()),
            Some("initialize")
        );

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_codex_server_ezra_did_not_start_is_stopped_before_codex_starts() {
        if !"python3".is_installed() {
            return;
        }
        let manager = manager(BUDGET);
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, CHATGPT);
        let mut foreign = ForeignListener::in_home(&codex_home(&manager)).await;

        let supervisor = supervise(&manager);
        servers_started(&manager, 1).await;
        assert!(foreign.process.has_exited());

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_socket_in_use_failure_stops_codexs_own_daemon_before_the_next_start() {
        let manager = manager(BUDGET);
        let (supervisor, mut updater, retrying) =
            after_a_failure_with(&manager, SOCKET_IN_USE).await;

        assert_eq!(retrying.problem, Some(CodexProblem::SocketInUse));
        assert!(updater.has_exited());

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_failure_for_another_reason_leaves_codexs_own_daemon_running() {
        let manager = manager(BUDGET);
        let (supervisor, mut updater, retrying) = after_a_failure_with(
            &manager,
            "Error: remote control is disabled by managed requirements",
        )
        .await;

        assert_eq!(retrying.problem, Some(CodexProblem::NotAllowed));
        assert!(!updater.has_exited());

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn codexs_own_daemon_is_left_alone_until_codex_is_about_to_start() {
        let manager = manager(BUDGET);
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, API_KEY);
        let mut updater = daemon_updater(&codex_home(&manager), SLEEPS).await;
        let supervisor = supervise(&manager);
        status_until(&manager, |status| {
            status.problem == Some(CodexProblem::NotChatGpt)
        })
        .await;
        assert!(!updater.has_exited());

        sign_in_now(&manager, CHATGPT).await;
        servers_started(&manager, 1).await;
        assert!(updater.has_exited());

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn shutting_down_does_not_wait_for_codexs_own_daemon_to_stop() {
        let manager = manager(ServerBudget {
            drain: Duration::from_secs(30),
            force: Duration::from_secs(30),
            ..BUDGET
        });
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, CHATGPT);
        let home = codex_home(&manager);
        let _updater = daemon_updater(&home, &counting_terms(3, "")).await;
        let supervisor = supervise(&manager);
        wait_until(WAIT, || terms_seen(&home), |terms| *terms == 1).await;
        let supervision = &manager.state.codex_remote.supervision;
        supervision.begin_shut_down();

        assert!(supervision.wait_until_stopped(Duration::from_secs(2)).await);
        supervisor.await.expect("the supervisor stops");
        assert!(servers(&manager).is_empty());
    }

    #[tokio::test]
    async fn a_lost_control_connection_is_opened_again_with_a_new_snapshot() {
        let manager = manager(BUDGET);
        let (supervisor, fake, first) = running(&manager, "0.157.1").await;

        fake.disconnect();
        let requests = wait_until(
            WAIT,
            || fake.requests(),
            |requests| {
                requests
                    .iter()
                    .filter(|(method, _)| method == "remoteControl/status/read")
                    .count()
                    >= 2
            },
        )
        .await;
        let methods: Vec<&str> = requests.iter().map(|(method, _)| method.as_str()).collect();
        assert_eq!(
            methods,
            [
                "initialize",
                "remoteControl/status/read",
                "initialize",
                "remoteControl/status/read"
            ]
        );
        let running = status_until(&manager, connected).await;
        assert_eq!(running.restarts, 0);
        assert_eq!(servers(&manager), [first]);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_control_connection_that_stays_lost_stops_codex() {
        let manager = manager(SHORT_READINESS);
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, CHATGPT);
        let fake = control(&manager, "connected");
        let supervisor = supervise(&manager);
        let running = status_until(&manager, connected).await;
        assert_eq!(running.restarts, 0);
        let first = *servers_started(&manager, 1)
            .await
            .first()
            .expect("a server started");

        drop(fake);
        let retrying = status_until(&manager, |status| status.state == ServerState::Retrying).await;
        assert_eq!(retrying.restarts, 1);
        assert!(
            retrying
                .last_error
                .as_deref()
                .is_some_and(|error| error.starts_with("lost its control connection")),
            "{retrying:?}"
        );
        first.wait_until_gone().await;

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn shutting_down_stops_codex() {
        let manager = manager(BUDGET);
        let (supervisor, _fake, first) = running(&manager, "0.157.1").await;

        shut_down(&manager, supervisor).await;

        assert!(
            manager
                .state
                .codex_remote
                .supervision
                .wait_until_stopped(Duration::ZERO)
                .await
        );
        first.wait_until_gone().await;
    }

    #[tokio::test]
    async fn shutting_down_does_not_wait_for_a_flag_probe() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let (supervisor, _fake, first) = running(&manager, "0.157.1").await;

        install_codex(&manager, "0.157.2", REMOTE_CONTROL_HANGS, SERVE);
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        remote_control_probed(&manager, 2).await;
        let supervision = &manager.state.codex_remote.supervision;
        supervision.begin_shut_down();

        assert!(supervision.wait_until_stopped(BUDGET.longest_stop()).await);
        supervisor.await.expect("the supervisor stops");
        first.wait_until_gone().await;
    }

    #[tokio::test]
    async fn a_new_codex_version_restarts_the_server_on_it() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let (supervisor, _fake, first) = running(&manager, "0.157.1").await;

        install_codex(&manager, "0.157.2", TAKES_EVERY_FLAG, SERVE);
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        servers_started(&manager, 2).await;
        first.wait_until_gone().await;
        let running = status_until(&manager, |status| {
            connected(status) && status.server_version.as_deref() == Some("0.157.2")
        })
        .await;
        assert_eq!(running.restarts, 0);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_new_version_keeps_the_running_server_until_its_flag_probe_answers() {
        let manager = manager(SHORT_PROBE);
        let cookie = manager.logged_in().await;
        let (supervisor, _fake, first) = running(&manager, "0.157.1").await;

        install_codex(&manager, "0.157.2", REMOTE_CONTROL_HANGS, SERVE);
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        remote_control_probed(&manager, 2).await;
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        remote_control_probed(&manager, 3).await;
        let kept = manager.state.codex_remote.status();
        assert_eq!(
            (kept.state, kept.server_version.as_deref(), kept.problem),
            (ServerState::Running, Some("0.157.1"), None)
        );
        assert_eq!(servers(&manager), [first]);

        install_codex(&manager, "0.157.2", TAKES_EVERY_FLAG, SERVE);
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        servers_started(&manager, 2).await;
        first.wait_until_gone().await;
        status_until(&manager, |status| {
            connected(status) && status.server_version.as_deref() == Some("0.157.2")
        })
        .await;

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn codex_waits_while_its_flag_probe_does_not_answer() {
        let manager = manager(SHORT_PROBE);
        let cookie = manager.logged_in().await;
        install_codex(&manager, "0.157.1", REMOTE_CONTROL_HANGS, SERVE);
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        remote_control_probed(&manager, 1).await;
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        remote_control_probed(&manager, 2).await;
        let waiting = manager.state.codex_remote.status();
        assert_eq!(
            (waiting.state, waiting.problem),
            (ServerState::Waiting, None)
        );
        assert!(servers(&manager).is_empty());

        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        let _fake = serve_control(&manager, "connected").await;
        status_until(&manager, connected).await;

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_version_without_remote_control_leaves_the_running_server_in_place() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let (supervisor, _fake, first) = running(&manager, "0.157.1").await;

        install_codex(
            &manager,
            "0.100.0",
            "'app-server --remote-control --help') exit 2 ;;",
            SERVE,
        );
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        let kept = status_until(&manager, |status| status.problem.is_some()).await;
        assert_eq!(kept.problem, Some(CodexProblem::UnsupportedVersion));
        assert_eq!(kept.state, ServerState::Running);
        assert_eq!(kept.server_version.as_deref(), Some("0.157.1"));
        assert_eq!(servers(&manager), [first]);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    #[ignore = "runs the real codex that EZRA_TEST_CODEX names"]
    async fn real_codex_runs_signed_out() {
        let codex = PathBuf::from(
            std::env::var_os("EZRA_TEST_CODEX").expect("EZRA_TEST_CODEX names a real codex"),
        );
        let home = tempfile::Builder::new()
            .tempdir_in("/tmp")
            .expect("a Codex home is created");
        let projects = tempfile::tempdir().expect("a projects folder is created");
        let logs = tempfile::tempdir().expect("a log folder is created");
        let budget = ServerBudget::default();
        let flags = LaunchFlags::probe(&codex, home.path(), &budget)
            .await
            .expect("codex answers the flag probes");
        assert!(flags.remote_control);
        let launch = CodexLaunch {
            codex,
            version: "real".to_owned(),
            codex_home: home.path().to_path_buf(),
            projects: projects.path().to_path_buf(),
            sandbox: CodexSandbox::default(),
            approvals: CodexApprovals::default(),
            sign_in: None,
            managed_daemon: flags.managed_daemon,
            log: ServerLog(logs.path().to_path_buf()),
        };
        let mut server = CodexServerRun::start(&launch).await.expect("codex starts");
        let mut link = ControlLink::opening(&launch, server.leader().expect("codex runs"), budget);

        let event = link.next().await;
        let LinkEvent::Ready(relay) = event else {
            panic!("{event:?}");
        };
        let socket = fs::read_link(ControlSocket::of(home.path()).0)
            .expect("Codex links its control socket");
        assert!(
            matches!(
                relay.status,
                RelayStatusWire::Errored | RelayStatusWire::Connecting
            ),
            "{relay:?}"
        );
        let problem = timeout(Duration::from_secs(20), server.problems.recv())
            .await
            .expect("a problem line arrives in time")
            .expect("the output is read");
        assert_eq!(problem.problem, CodexProblem::SignedOut, "{}", problem.line);
        let stopping = Instant::now();
        let exit = server
            .stop(&ServerBudget {
                drain: Duration::from_secs(5),
                ..budget
            })
            .await
            .expect("codex is reaped");
        let stopped_after = stopping.elapsed();
        let startup_lock = socket.with_extension("lock");
        if startup_lock.exists() {
            fs::remove_file(&startup_lock).expect("the startup lock is removed");
        }
        assert!(exit.success(), "{exit}");
        assert!(stopped_after < Duration::from_secs(5), "{stopped_after:?}");

        let files = Command::new("find")
            .arg(home.path())
            .output()
            .expect("find runs");
        eprintln!(
            "The Codex home after the run:\n{}",
            String::from_utf8_lossy(&files.stdout)
        );
        eprintln!(
            "The server's output:\n{}",
            launch.log.tail().expect("the log is readable").join("\n")
        );
    }
}
