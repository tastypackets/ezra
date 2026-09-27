use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nix::unistd::Pid;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, MissedTickBehavior, interval, sleep_until};

use super::chats::Chats;
use super::control::{
    AccountRead, AccountWire, ControlClient, ControlError, ControlEvent, ControlNotification,
    ControlSocket, Disable, Enable, LoadedThreads, RelayStatusWire, RelayWire, StatusRead,
    ThreadId, ThreadRead, ThreadUnsubscribe, ThreadWire, UnsubscribeStatus,
};
use super::foreign::ForeignServer;
use super::launch::CodexLaunch;
use super::problem::{CodexProblem, ProblemLine};
use super::run::CodexServerRun;
use super::{CodexRemote, CodexRemoteStatus, CodexUsage, Control, RelayState, ServerBudget};
use crate::manager::agents::Agent;
use crate::manager::checks::AgentChecks;
use crate::manager::remote_control::ServerState;
use crate::manager::state::AppState;
use crate::manager::supervision::{
    Failures, FlagExt, RECHECK_INTERVAL, RunEnd, Signals, UpdateWait, Verdict, Wake, Wanted,
};

/// How often the control socket is tried while the server starts.
const CONNECT_INTERVAL: Duration = Duration::from_millis(250);
const LONGEST_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const LARGEST_LOG: u64 = 10 * 1024 * 1024;
const TURNED_OFF: &str = "Codex turned remote control off";
const LOADED_THREAD_PAGES: usize = 10;

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
                    codex_remote.with_control(|control| *control = Control::default());
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
        let mut run = CodexRun {
            link: ControlLink::opening(
                launch,
                codex_remote.expected.pid(leader),
                codex_remote.budget,
            ),
            asking: Asking::default(),
            mfa_retry_at: None,
            chats: Chats::default(),
            memory_bytes: None,
            update: None,
        };
        let mut recheck = interval(RECHECK_INTERVAL);
        recheck.set_missed_tick_behavior(MissedTickBehavior::Delay);
        recheck.reset();
        let mut usage = interval(codex_remote.budget.usage);
        usage.set_missed_tick_behavior(MissedTickBehavior::Delay);
        usage.reset();
        loop {
            run.plan_mfa_retry(codex_remote);
            let mfa_retry_at = run.mfa_retry_at;
            let update_due_at = run.update.as_ref().and_then(UpdateWait::wakes_at);
            let mut turned_off = false;
            tokio::select! {
                exit = server.child.wait() => {
                    return RunEnd::Failed(server.finish(exit).await);
                }
                Some(line) = server.problems.recv() => {
                    run.name_problem(codex_remote, line);
                    continue;
                }
                event = run.link.next() => {
                    match event {
                        LinkEvent::Ready { relay, client } => {
                            run.memory_bytes = server.sample_family().await.or(run.memory_bytes);
                            run.ready(self, relay, client);
                        }
                        LinkEvent::Control(ControlEvent::Notification(notification)) => {
                            run.notified(self, notification);
                        }
                        LinkEvent::Control(ControlEvent::ServerRequest { thread_id: Some(thread_id), .. }) => {
                            run.asked_about(codex_remote, thread_id);
                        }
                        LinkEvent::Control(ControlEvent::ServerRequest { thread_id: None, .. } | ControlEvent::Closed) => {}
                        LinkEvent::Lost => {
                            tracing::warn!("lost Codex's control connection, opening it again");
                            codex_remote.with_control(|control| control.client = None);
                            codex_remote.status.update(CodexRemoteStatus::lose_control);
                        }
                        LinkEvent::GaveUp(message) => {
                            if let Err(error) = server.stop(&codex_remote.budget).await {
                                tracing::warn!("could not wait for Codex to stop: {error}");
                            }
                            return RunEnd::Failed(message.into());
                        }
                    }
                    if !run.update_is_due() {
                        continue;
                    }
                }
                Some(answer) = run.asking.next() => {
                    turned_off = run.answered(codex_remote, answer);
                    if !turned_off && !run.update_is_due() {
                        continue;
                    }
                }
                () = sleep_until(mfa_retry_at.unwrap_or_else(Instant::now)), if mfa_retry_at.is_some() => {
                    run.retry_mfa(codex_remote);
                    continue;
                }
                () = sleep_until(update_due_at.unwrap_or_else(Instant::now)), if update_due_at.is_some() => {}
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
                    run.memory_bytes = server.sample_family().await.or(run.memory_bytes);
                    run.show_usage(codex_remote);
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
                Verdict::Keep | Verdict::Unsure | Verdict::Update(_) if turned_off => {
                    self.stop_codex_server(server).await;
                    return RunEnd::Failed(TURNED_OFF.to_owned().into());
                }
                Verdict::Keep => {
                    run.update = None;
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
                    let busy = run.chats.are_busy();
                    let waiting = run.update.get_or_insert_with(|| {
                        UpdateWait::starting_now(
                            version.clone(),
                            codex_remote.budget.update_deadline,
                        )
                    });
                    waiting.version = version;
                    if waiting.is_due(busy) {
                        tracing::info!(
                            "restarting Codex remote control on Codex {}",
                            waiting.version
                        );
                        self.stop_codex_server(server).await;
                        self.remove_unused_versions(Agent::Codex).await;
                        return RunEnd::Reconsidered;
                    }
                    let pending = waiting.pending();
                    codex_remote
                        .status
                        .update(|status| status.wait_for_update(pending));
                }
            }
        }
    }

    async fn stop_codex_server(&self, server: CodexServerRun) {
        self.codex_remote
            .with_control(|control| control.client = None);
        self.codex_remote
            .status
            .update(|status| status.enter(ServerState::Stopping));
        if let Err(error) = server.stop(&self.codex_remote.budget).await {
            tracing::warn!("could not wait for Codex to stop: {error}");
        }
    }
}

/// What the supervisor tracks while one server runs, beyond the status it shows.
struct CodexRun {
    link: ControlLink,
    asking: Asking,
    /// When to turn the relay on again while ChatGPT asks for multi-factor authentication.
    mfa_retry_at: Option<Instant>,
    chats: Chats,
    /// From the last reading of the server's processes.
    memory_bytes: Option<u64>,
    /// A newer Codex to restart on once no chat runs.
    update: Option<UpdateWait>,
}

impl CodexRun {
    /// Shows a line that names a problem, and turns the relay off when Codex cannot enroll.
    fn name_problem(&mut self, codex_remote: &CodexRemote, line: ProblemLine) {
        let problem = line.problem;
        codex_remote
            .status
            .update(|status| status.name_problem(line));
        if problem.turns_relay_off() {
            self.turn_relay_off(codex_remote);
        }
    }

    /// The connection opened with `relay` as Codex reported it.
    fn ready(&mut self, state: &AppState, relay: RelayWire, client: Arc<ControlClient>) {
        let codex_remote = &state.codex_remote;
        let off = codex_remote.with_control(|control| control.relay_off);
        match relay.status {
            RelayStatusWire::Disabled if !off => {
                self.asking.turned_off(Arc::clone(&state.agent_checks));
            }
            RelayStatusWire::Connected => {
                codex_remote.with_control(|control| control.relay_off = false);
            }
            RelayStatusWire::Connecting | RelayStatusWire::Errored if off => {
                self.asking.relay(Arc::clone(&client), true);
            }
            RelayStatusWire::Disabled | RelayStatusWire::Connecting | RelayStatusWire::Errored => {}
        }
        tracing::info!("Codex remote control answers as {}", relay.server_name);
        codex_remote.with_control(|control| control.client = Some(Arc::clone(&client)));
        self.chats = Chats::default();
        let usage = self.usage();
        codex_remote.status.update(|status| {
            status.answer(relay);
            status.usage = usage;
        });
        self.asking.threads(Arc::clone(&client));
        self.asking.account(client, None);
    }

    fn usage(&self) -> Option<CodexUsage> {
        Some(CodexUsage {
            chats: self.chats.count(),
            running_chats: self.chats.running(),
            memory_bytes: self.memory_bytes?,
        })
    }

    fn show_usage(&self, codex_remote: &CodexRemote) {
        if let Some(usage) = self.usage() {
            codex_remote
                .status
                .update(|status| status.usage = Some(usage));
        }
    }

    /// Codex asked ezra about a chat, so ezra's connection follows it.
    fn asked_about(&mut self, codex_remote: &CodexRemote, thread_id: ThreadId) {
        let leave = self.chats.asked_about(thread_id);
        self.leave(codex_remote, leave);
        self.show_usage(codex_remote);
    }

    /// Unsubscribes from the chat `leave` names, if any.
    fn leave(&mut self, codex_remote: &CodexRemote, leave: Option<ThreadId>) {
        let Some(thread_id) = leave else {
            return;
        };
        match codex_remote.client() {
            Some(client) => self.asking.leave(client, thread_id),
            None => self.chats.left(&thread_id, None),
        }
    }

    fn notified(&mut self, state: &AppState, notification: ControlNotification) {
        let codex_remote = &state.codex_remote;
        match notification {
            ControlNotification::StatusChanged(relay) => {
                match relay.status {
                    RelayStatusWire::Disabled
                        if !codex_remote.with_control(|control| control.relay_off) =>
                    {
                        self.asking.turned_off(Arc::clone(&state.agent_checks));
                    }
                    RelayStatusWire::Connected => {
                        codex_remote.with_control(|control| control.relay_off = false);
                    }
                    RelayStatusWire::Disabled
                    | RelayStatusWire::Connecting
                    | RelayStatusWire::Errored => {}
                }
                codex_remote
                    .status
                    .update(|status| status.show_relay(relay));
            }
            ControlNotification::AccountUpdated {} => {
                if let Some(client) = codex_remote.client() {
                    self.asking
                        .account(client, Some(Arc::clone(&state.agent_checks)));
                }
            }
            ControlNotification::ThreadStarted { thread } => {
                let leave = self.chats.started(thread);
                self.leave(codex_remote, leave);
                self.show_usage(codex_remote);
            }
            ControlNotification::ThreadStatusChanged { thread_id, status } => {
                let leave = self.chats.changed(thread_id, status);
                self.leave(codex_remote, leave);
                self.show_usage(codex_remote);
            }
            ControlNotification::ThreadClosed { thread_id } => {
                self.chats.closed(&thread_id);
                self.show_usage(codex_remote);
            }
        }
    }

    /// True when Codex turned the relay off by itself and still has it off, with Codex's own
    /// sign-in checked again since.
    fn answered(&mut self, codex_remote: &CodexRemote, answer: Answer) -> bool {
        match answer {
            Answer::Account { account, changed } => match account.problem() {
                Some(problem) => {
                    codex_remote
                        .status
                        .update(|status| status.name_sign_in_problem(problem));
                    self.turn_relay_off(codex_remote);
                }
                None if changed
                    && codex_remote.status.read(|status| {
                        status
                            .problem
                            .is_some_and(CodexProblem::is_about_the_sign_in)
                    }) =>
                {
                    self.turn_relay_on(codex_remote);
                }
                None => {}
            },
            Answer::RelayStays { off } => {
                codex_remote.with_control(|control| control.relay_off = off);
            }
            Answer::TurnedOff => {
                return codex_remote.status.read(|status| status.relay)
                    == Some(RelayState::Disabled)
                    && !codex_remote.with_control(|control| control.relay_off);
            }
            Answer::Threads(listed) => {
                let unread = self.chats.listed(listed);
                if let Some(client) = codex_remote.client() {
                    for thread_id in unread {
                        self.asking.read(Arc::clone(&client), thread_id);
                    }
                }
                self.show_usage(codex_remote);
            }
            Answer::Thread(thread) => {
                self.chats.read(thread);
                self.show_usage(codex_remote);
            }
            Answer::Left { thread_id, status } => self.chats.left(&thread_id, status),
            Answer::Nothing => {}
        }
        false
    }

    fn turn_relay_off(&mut self, codex_remote: &CodexRemote) {
        if !codex_remote.with_control(Control::turn_relay_off) {
            return;
        }
        if let Some(client) = codex_remote.client() {
            self.asking.relay(client, true);
        }
    }

    /// Asks Codex to turn the relay on again when ezra turned it off.
    fn turn_relay_on(&mut self, codex_remote: &CodexRemote) {
        if let Some(client) = codex_remote.with_control(Control::turn_relay_on) {
            self.asking.relay(client, false);
        }
    }

    /// Plans turning the relay on again while ezra holds it off and ChatGPT asks for multi-factor
    /// authentication, and drops the plan otherwise.
    fn plan_mfa_retry(&mut self, codex_remote: &CodexRemote) {
        let waiting = codex_remote.with_control(|control| control.relay_off)
            && codex_remote.status.read(|status| status.problem) == Some(CodexProblem::MfaRequired);
        if !waiting {
            self.mfa_retry_at = None;
        } else if self.mfa_retry_at.is_none() {
            self.mfa_retry_at = Instant::now().checked_add(codex_remote.budget.mfa_retry);
        }
    }

    /// Whether the restart on a newer Codex waits no longer.
    fn update_is_due(&self) -> bool {
        self.update
            .as_ref()
            .is_some_and(|update| update.is_due(self.chats.are_busy()))
    }

    fn retry_mfa(&mut self, codex_remote: &CodexRemote) {
        self.mfa_retry_at = None;
        if codex_remote.status.read(|status| status.problem) == Some(CodexProblem::MfaRequired) {
            self.turn_relay_on(codex_remote);
        }
    }
}

/// Requests to Codex that run beside the supervisor, and end with the run.
#[derive(Default)]
struct Asking(JoinSet<Answer>);

/// What Codex's answer to a request running beside the supervisor tells it.
enum Answer {
    /// The server's sign-in, read again after it changed when `changed`.
    Account {
        account: AccountWire,
        changed: bool,
    },
    /// Codex did not do what ezra asked, so the relay stays off, or on when not `off`.
    RelayStays {
        off: bool,
    },
    /// Codex turned the relay off by itself, and its sign-in was checked again since.
    TurnedOff,
    /// The chats the server has loaded.
    Threads(Vec<ThreadId>),
    /// A chat as read.
    Thread(ThreadWire),
    /// Codex answered an unsubscribe with `status`, absent when it failed.
    Left {
        thread_id: ThreadId,
        status: Option<UnsubscribeStatus>,
    },
    Nothing,
}

impl Asking {
    /// Asks Codex to turn the relay off, or on when not `off`.
    fn relay(&mut self, client: Arc<ControlClient>, off: bool) {
        self.0.spawn(async move {
            let asked = if off {
                client.request(Disable { ephemeral: true }).await
            } else {
                client.request(Enable { ephemeral: true }).await
            };
            match asked {
                Ok(_) | Err(ControlError::Closed) => Answer::Nothing,
                Err(error) => {
                    let turn = if off { "off" } else { "on" };
                    tracing::warn!("Codex did not turn remote control {turn}: {error}");
                    Answer::RelayStays { off: !off }
                }
            }
        });
    }

    /// Checks Codex's own sign-in again, after Codex turned the relay off by itself.
    fn turned_off(&mut self, agent_checks: Arc<AgentChecks>) {
        self.0.spawn(async move {
            agent_checks
                .check_sign_in(Agent::Codex, Duration::ZERO)
                .await;
            Answer::TurnedOff
        });
    }

    /// Reads the server's sign-in, after checking Codex's own when `agent_checks` is given.
    fn account(&mut self, client: Arc<ControlClient>, agent_checks: Option<Arc<AgentChecks>>) {
        let changed = agent_checks.is_some();
        self.0.spawn(async move {
            if let Some(agent_checks) = agent_checks {
                agent_checks.refresh(Agent::Codex).await;
            }
            match client.request(AccountRead {}).await {
                Ok(account) => Answer::Account { account, changed },
                Err(error) => {
                    tracing::warn!("could not read Codex's sign-in: {error}");
                    Answer::Nothing
                }
            }
        });
    }

    /// Lists the chats the server has loaded, from at most ten pages, unless the connection
    /// closes first.
    fn threads(&mut self, client: Arc<ControlClient>) {
        self.0.spawn(async move {
            let mut listed = Vec::new();
            let mut cursor = None;
            for _ in 0..LOADED_THREAD_PAGES {
                match client.request(LoadedThreads { cursor }).await {
                    Ok(page) => {
                        listed.extend(page.data);
                        cursor = page.next_cursor;
                    }
                    Err(ControlError::Closed) => return Answer::Nothing,
                    Err(error) => {
                        tracing::warn!("could not list Codex's chats: {error}");
                        break;
                    }
                }
                if cursor.is_none() {
                    break;
                }
            }
            Answer::Threads(listed)
        });
    }

    fn read(&mut self, client: Arc<ControlClient>, thread_id: ThreadId) {
        self.0.spawn(async move {
            match client.request(ThreadRead { thread_id }).await {
                Ok(read) => Answer::Thread(read.thread),
                Err(error) => {
                    tracing::warn!("could not read a Codex chat: {error}");
                    Answer::Nothing
                }
            }
        });
    }

    /// Unsubscribes ezra's connection from a chat.
    fn leave(&mut self, client: Arc<ControlClient>, thread_id: ThreadId) {
        self.0.spawn(async move {
            let unsubscribe = ThreadUnsubscribe {
                thread_id: thread_id.clone(),
            };
            let status = match client.request(unsubscribe).await {
                Ok(left) => Some(left.status),
                Err(error) => {
                    tracing::warn!("could not unsubscribe from a Codex chat: {error}");
                    None
                }
            };
            Answer::Left { thread_id, status }
        });
    }

    /// Waits for the next answer. None while nothing is asked.
    async fn next(&mut self) -> Option<Answer> {
        while let Some(joined) = self.0.join_next().await {
            if let Ok(answer) = joined {
                return Some(answer);
            }
        }
        None
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
    Ready {
        relay: RelayWire,
        client: Arc<ControlClient>,
    },
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
        _client: Arc<ControlClient>,
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
                    let client = Arc::new(client);
                    self.state = LinkState::Open {
                        _client: Arc::clone(&client),
                        events,
                    };
                    return LinkEvent::Ready { relay, client };
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
    use std::os::unix::fs::FileTypeExt;
    use std::process::Command;

    use axum::http::StatusCode;
    use nix::sys::signal::{Signal, kill};
    use serde_json::{Value, json};
    use time::OffsetDateTime;
    use tokio::time::{sleep, timeout};

    use super::*;
    use crate::manager::api::test_support::{
        EventStreamExt, PidExt, ProgramExt, ResponseExt, TestManager, wait_until,
    };
    use crate::manager::codex_remote::fake::{FakeControlServer, Reply};
    use crate::manager::codex_remote::foreign::tests::{
        ForeignListener, SLEEPS, Started, daemon_updater,
    };
    use crate::manager::codex_remote::launch::LaunchFlags;
    use crate::manager::codex_remote::problem::tests::{
        mfa_warning, relay_warning, unavailable_warning,
    };
    use crate::manager::codex_remote::run::tests::{
        LEFTOVER, SOCKET_IN_USE, counting_terms, exits_when_told, terms_seen,
    };
    use crate::manager::codex_remote::{
        CodexApprovals, CodexRemoteSettings, CodexSandbox, ExpectedPeer, RelayState,
    };
    use crate::manager::events::Topic;
    use crate::manager::remote_control::RemoteControlOverview;
    use crate::manager::supervision::{
        FIRST_RETRY_DELAY, PendingUpdate, ServerLog, UPDATE_RESTART_DEADLINE,
    };

    const WAIT: Duration = Duration::from_secs(10);
    const BUDGET: ServerBudget = ServerBudget {
        probe: Duration::from_secs(5),
        readiness: Duration::from_secs(5),
        drain: Duration::from_secs(1),
        force: Duration::from_secs(1),
        request: Duration::from_secs(5),
        mfa_retry: Duration::from_secs(10 * 60),
        usage: Duration::from_secs(15),
        update_deadline: UPDATE_RESTART_DEADLINE,
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

    /// A server that prints `line` twice to stderr each time `$CODEX_HOME/print` appears, which
    /// it removes.
    fn prints_when_told(line: &str) -> String {
        format!(
            "while :; do\n  \
               if [ -e \"$CODEX_HOME/print\" ]; then\n    \
                 rm \"$CODEX_HOME/print\"\n    \
                 printf '%s\\n' '{line}' '{line}' >&2\n  \
               fi\n  \
               sleep 0.05\n\
             done"
        )
    }

    fn tell_to_print(manager: &TestManager) {
        fs::write(codex_home(manager).join("print"), "").expect("the server is told to print");
    }

    /// Serves the control socket like `control`, and turns the relay off and on like Codex.
    fn relay_control(manager: &TestManager, relay: &str) -> FakeControlServer {
        let fake = control(manager, relay);
        fake.reply(
            "remoteControl/disable",
            [Reply::Result(FakeControlServer::relay("disabled"))],
        );
        fake.push_after(
            "remoteControl/disable",
            FakeControlServer::status_changed("disabled"),
        );
        fake.reply(
            "remoteControl/enable",
            [Reply::Result(FakeControlServer::relay("connecting"))],
        );
        fake.push_after(
            "remoteControl/enable",
            FakeControlServer::status_changed("connecting"),
        );
        fake
    }

    fn turned_off(status: &CodexRemoteStatus) -> bool {
        status.relay == Some(RelayState::Disabled)
    }

    const TURN_OFF: &str = "remoteControl/disable";
    const TURN_ON: &str = "remoteControl/enable";
    const RETRY: &str = "/api/v1/remote-control/codex/retry";

    /// Starts a supervisor whose server prints the MFA warning when told, and waits until the
    /// warning turned the relay off.
    async fn asking_for_mfa(
        manager: &TestManager,
        control: impl FnOnce(&TestManager) -> FakeControlServer,
    ) -> (JoinHandle<()>, FakeControlServer) {
        install_codex(
            manager,
            "0.157.1",
            TAKES_EVERY_FLAG,
            &prints_when_told(&mfa_warning()),
        );
        sign_in(manager, CHATGPT);
        let supervisor = supervise(manager);
        servers_started(manager, 1).await;
        let fake = control(manager);
        status_until(manager, |status| status.state == ServerState::Running).await;
        tell_to_print(manager);
        let off = status_until(manager, turned_off).await;
        assert_eq!(
            (off.state, off.problem, off.restarts),
            (ServerState::Running, Some(CodexProblem::MfaRequired), 0)
        );
        assert_eq!(off.last_error, Some(mfa_warning()));
        (supervisor, fake)
    }

    #[tokio::test]
    async fn chatgpt_asking_for_mfa_turns_the_relay_off_once_and_codex_keeps_running() {
        let manager = manager(BUDGET);
        let (supervisor, fake) =
            asking_for_mfa(&manager, |manager| relay_control(manager, "errored")).await;

        tell_to_print(&manager);
        wait_until(
            WAIT,
            || manager.state.codex_remote.log.tail().unwrap_or_default(),
            |lines| {
                lines
                    .iter()
                    .filter(|line| line.ends_with(&mfa_warning()))
                    .count()
                    == 4
            },
        )
        .await;
        sleep(Duration::from_millis(200)).await;
        assert_eq!(fake.requests_of(TURN_OFF), [json!({"ephemeral": true})]);
        let off = manager.state.codex_remote.status();
        assert_eq!(
            (off.state, off.relay, off.restarts),
            (ServerState::Running, Some(RelayState::Disabled), 0)
        );
        assert_eq!(servers(&manager).len(), 1);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn the_disabled_status_may_arrive_before_or_after_the_answer_to_turning_it_off() {
        for before in [true, false] {
            let manager = manager(BUDGET);
            let (supervisor, fake) = asking_for_mfa(&manager, |manager| {
                let fake = control(manager, "errored");
                fake.reply(
                    TURN_OFF,
                    [Reply::Result(FakeControlServer::relay("disabled"))],
                );
                let disabled = FakeControlServer::status_changed("disabled");
                if before {
                    fake.push_before(TURN_OFF, disabled);
                } else {
                    fake.push_after(TURN_OFF, disabled);
                }
                fake
            })
            .await;

            sleep(Duration::from_millis(200)).await;
            let off = manager.state.codex_remote.status();
            assert_eq!(
                (off.state, off.relay, off.restarts),
                (ServerState::Running, Some(RelayState::Disabled), 0),
                "before: {before}"
            );
            assert_eq!(fake.requests_of(TURN_OFF).len(), 1);
            assert_eq!(servers(&manager).len(), 1);

            shut_down(&manager, supervisor).await;
        }
    }

    #[tokio::test]
    async fn an_mfa_warning_before_codex_answers_turns_the_relay_off_once_it_does() {
        let manager = manager(BUDGET);
        install_codex(
            &manager,
            "0.157.1",
            TAKES_EVERY_FLAG,
            &format!("printf '%s\\n' '{}' >&2\n{SERVE}", mfa_warning()),
        );
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        let starting = status_until(&manager, |status| status.problem.is_some()).await;
        assert_eq!(
            (starting.state, starting.problem),
            (ServerState::Starting, Some(CodexProblem::MfaRequired))
        );

        let fake = relay_control(&manager, "errored");
        let off = status_until(&manager, turned_off).await;
        assert_eq!(
            (off.state, off.problem, off.restarts),
            (ServerState::Running, Some(CodexProblem::MfaRequired), 0)
        );
        assert_eq!(fake.requests_of(TURN_OFF), [json!({"ephemeral": true})]);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_disabled_status_while_codex_stops_changes_nothing() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        install_codex(
            &manager,
            "0.157.1",
            TAKES_EVERY_FLAG,
            &counting_terms(2, ""),
        );
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        servers_started(&manager, 1).await;
        let fake = control(&manager, "connected");
        status_until(&manager, connected).await;

        let off = CodexRemoteSettings {
            enabled: false,
            ..CodexRemoteSettings::default()
        };
        save(&manager, &cookie, off).await;
        let home = codex_home(&manager);
        wait_until(WAIT, || terms_seen(&home), |terms| *terms == 1).await;
        fake.push(FakeControlServer::status_changed("disabled"));
        let stopped = status_until(&manager, |status| status.state == ServerState::Off).await;
        assert_eq!(
            (stopped.relay, stopped.problem, stopped.restarts),
            (None, None, 0)
        );
        assert_eq!(terms_seen(&home), 2);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn codex_turning_the_relay_off_by_itself_restarts_it_after_a_delay() {
        let manager = manager(BUDGET);
        let (supervisor, fake, first) = running(&manager, "0.157.1").await;

        fake.push(FakeControlServer::status_changed("disabled"));
        let retrying = status_until(&manager, |status| status.state == ServerState::Retrying).await;
        let failed = Instant::now();
        assert_eq!(
            (retrying.restarts, retrying.last_error.as_deref()),
            (1, Some("Codex turned remote control off"))
        );
        first.wait_until_gone().await;
        wait_until(
            FIRST_RETRY_DELAY.saturating_add(WAIT),
            || servers(&manager).len(),
            |started| *started == 2,
        )
        .await;
        assert!(
            failed.elapsed() >= FIRST_RETRY_DELAY.saturating_sub(Duration::from_millis(500)),
            "{:?}",
            failed.elapsed()
        );

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn trying_again_clears_the_problem_before_codex_turns_the_relay_on() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let (supervisor, fake) =
            asking_for_mfa(&manager, |manager| relay_control(manager, "errored")).await;
        fake.reply(
            TURN_ON,
            [Reply::Late(
                Duration::from_millis(500),
                FakeControlServer::relay("connecting"),
            )],
        );
        assert_eq!(
            manager.post(RETRY, "", None).await.status(),
            StatusCode::UNAUTHORIZED
        );

        let (response, (cleared, answered_then)) =
            tokio::join!(manager.post(RETRY, "", Some(&cookie)), async {
                let cleared = status_until(&manager, |status| status.problem.is_none()).await;
                (cleared, fake.answered(TURN_ON))
            });

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(cleared.last_error, None);
        assert!(!answered_then);
        assert!(fake.answered(TURN_ON));
        assert_eq!(fake.requests_of(TURN_ON), [json!({"ephemeral": true})]);
        let connecting = status_until(&manager, |status| {
            status.relay == Some(RelayState::Connecting)
        })
        .await;
        assert_eq!((connecting.problem, connecting.restarts), (None, 0));

        tell_to_print(&manager);
        let off = status_until(&manager, turned_off).await;
        assert_eq!(
            (off.problem, off.last_error),
            (Some(CodexProblem::MfaRequired), Some(mfa_warning()))
        );
        assert_eq!(fake.requests_of(TURN_OFF).len(), 2);

        shut_down(&manager, supervisor).await;
    }

    /// Codex's answer when the account changed while it turned the relay on.
    fn authentication_changed() -> Reply {
        Reply::Error {
            code: -32603,
            message: "remote control authentication changed".to_owned(),
        }
    }

    #[tokio::test]
    async fn trying_again_when_codex_refuses_shows_the_problem_again_and_tries_later() {
        let manager = manager(ServerBudget {
            mfa_retry: Duration::from_secs(1),
            ..BUDGET
        });
        let cookie = manager.logged_in().await;
        let (supervisor, fake) = asking_for_mfa(&manager, |manager| {
            let fake = relay_control(manager, "errored");
            fake.reply(TURN_ON, [authentication_changed()]);
            fake
        })
        .await;

        let response = manager.post(RETRY, "", Some(&cookie)).await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let shown = manager.state.codex_remote.status();
        assert_eq!(
            (shown.relay, shown.problem, shown.last_error),
            (
                Some(RelayState::Disabled),
                Some(CodexProblem::MfaRequired),
                Some(mfa_warning())
            )
        );
        let tried = fake.requests_of(TURN_ON).len();
        fake.until_requested(TURN_ON, tried.saturating_add(1)).await;
        assert_eq!(manager.state.codex_remote.status().restarts, 0);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn trying_again_while_the_relay_is_on_only_clears_the_problem() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        install_codex(
            &manager,
            "0.157.1",
            TAKES_EVERY_FLAG,
            &prints_when_told(&unavailable_warning()),
        );
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        servers_started(&manager, 1).await;
        let fake = relay_control(&manager, "errored");
        status_until(&manager, |status| status.state == ServerState::Running).await;
        tell_to_print(&manager);
        status_until(&manager, |status| {
            status.problem == Some(CodexProblem::RelayUnavailable)
        })
        .await;

        let response = manager.post(RETRY, "", Some(&cookie)).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let cleared = manager.state.codex_remote.status();
        assert_eq!(
            (cleared.relay, cleared.problem, cleared.last_error),
            (Some(RelayState::Errored), None, None)
        );
        assert!(fake.requests_of(TURN_ON).is_empty());

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn trying_again_needs_a_running_codex() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;

        let response = manager.post(RETRY, "", Some(&cookie)).await;

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn the_relay_is_turned_on_again_while_chatgpt_asks_for_mfa() {
        let manager = manager(ServerBudget {
            mfa_retry: Duration::from_secs(1),
            ..BUDGET
        });
        let (supervisor, fake) =
            asking_for_mfa(&manager, |manager| relay_control(manager, "errored")).await;

        fake.until_requested(TURN_ON, 1).await;
        let trying = status_until(&manager, |status| {
            status.relay == Some(RelayState::Connecting)
        })
        .await;
        assert_eq!(trying.problem, Some(CodexProblem::MfaRequired));
        tell_to_print(&manager);
        fake.until_requested(TURN_OFF, 2).await;
        status_until(&manager, turned_off).await;
        fake.until_requested(TURN_ON, 2).await;

        fake.push(FakeControlServer::status_changed("connected"));
        let connected = status_until(&manager, connected).await;
        assert_eq!((connected.problem, connected.last_error), (None, None));
        sleep(Duration::from_millis(1500)).await;
        assert_eq!(fake.requests_of(TURN_ON).len(), 2);
        assert_eq!(fake.requests_of(TURN_OFF).len(), 2);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_sign_in_codex_cannot_serve_with_turns_the_relay_off() {
        for (account, requires_openai_auth, problem) in [
            (
                json!({"type": "apiKey"}),
                true,
                Some(CodexProblem::NotChatGpt),
            ),
            (Value::Null, true, Some(CodexProblem::SignedOut)),
            (Value::Null, false, None),
            (
                json!({"type": "amazonBedrock", "usesCodexManagedCredentials": true}),
                false,
                None,
            ),
            (
                json!({"type": "chatgpt", "email": "dev@example.com", "planType": "plus"}),
                true,
                None,
            ),
        ] {
            let manager = manager(BUDGET);
            install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
            sign_in(&manager, CHATGPT);
            let supervisor = supervise(&manager);
            servers_started(&manager, 1).await;
            let fake = relay_control(&manager, "connecting");
            fake.reply(
                "account/read",
                [Reply::Result(json!({
                    "account": account,
                    "requiresOpenaiAuth": requires_openai_auth,
                }))],
            );

            fake.until_requested("account/read", 1).await;
            let shown = match problem {
                Some(_) => {
                    let off = status_until(&manager, turned_off).await;
                    assert_eq!(fake.requests_of(TURN_OFF).len(), 1, "{account}");
                    off
                }
                None => {
                    sleep(Duration::from_millis(200)).await;
                    assert!(fake.requests_of(TURN_OFF).is_empty(), "{account}");
                    manager.state.codex_remote.status()
                }
            };
            assert_eq!(
                (shown.state, shown.problem, shown.last_error),
                (ServerState::Running, problem, None),
                "{account}"
            );

            shut_down(&manager, supervisor).await;
        }
    }

    #[tokio::test]
    async fn a_changed_sign_in_is_checked_and_read_again() {
        let manager = manager(BUDGET);
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        servers_started(&manager, 1).await;
        let fake = relay_control(&manager, "connecting");
        fake.reply(
            "account/read",
            [
                Reply::Result(json!({"account": null, "requiresOpenaiAuth": true})),
                Reply::Result(json!({
                    "account": {"type": "chatgpt", "email": "dev@example.com", "planType": "plus"},
                    "requiresOpenaiAuth": true,
                })),
            ],
        );
        let signed_out = status_until(&manager, turned_off).await;
        assert_eq!(signed_out.problem, Some(CodexProblem::SignedOut));
        let checks = || {
            manager
                .fake_cli_runs(Agent::Codex)
                .iter()
                .filter(|run| *run == "login status")
                .count()
        };
        let checked = checks();

        fake.push(signed_in_with_chatgpt());
        fake.until_requested("account/read", 2).await;
        wait_until(WAIT, checks, |count| *count > checked).await;
        fake.until_requested(TURN_ON, 1).await;
        assert_eq!(fake.requests_of(TURN_ON), [json!({"ephemeral": true})]);
        let connecting = status_until(&manager, |status| {
            status.relay == Some(RelayState::Connecting)
        })
        .await;
        assert_eq!(
            (connecting.state, connecting.problem),
            (ServerState::Running, Some(CodexProblem::SignedOut))
        );

        fake.push(FakeControlServer::status_changed("connected"));
        let signed_in = status_until(&manager, connected).await;
        assert_eq!((signed_in.problem, signed_in.restarts), (None, 0));

        shut_down(&manager, supervisor).await;
    }

    /// The notification Codex sends when its sign-in becomes ChatGPT's, and to each new
    /// connection while that sign-in has workspace routing.
    fn signed_in_with_chatgpt() -> Value {
        json!({
            "method": "account/updated",
            "params": {"authMode": "chatgpt", "planType": "plus"},
        })
    }

    fn chatgpt_account() -> Reply {
        Reply::Result(json!({
            "account": {"type": "chatgpt", "email": "dev@example.com", "planType": "plus"},
            "requiresOpenaiAuth": true,
        }))
    }

    #[tokio::test]
    async fn a_changed_sign_in_codex_refuses_to_serve_leaves_the_relay_off_and_the_problem_shown() {
        let manager = manager(BUDGET);
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        servers_started(&manager, 1).await;
        let fake = relay_control(&manager, "connecting");
        fake.reply(
            "account/read",
            [
                Reply::Result(json!({"account": null, "requiresOpenaiAuth": true})),
                chatgpt_account(),
            ],
        );
        fake.reply(TURN_ON, [authentication_changed()]);
        status_until(&manager, turned_off).await;

        fake.push(signed_in_with_chatgpt());
        fake.until_requested(TURN_ON, 1).await;
        sleep(Duration::from_millis(200)).await;
        let refused = manager.state.codex_remote.status();
        assert_eq!(
            (refused.state, refused.relay, refused.problem),
            (
                ServerState::Running,
                Some(RelayState::Disabled),
                Some(CodexProblem::SignedOut)
            )
        );

        fake.push(signed_in_with_chatgpt());
        fake.until_requested(TURN_ON, 2).await;

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn codex_reporting_its_sign_in_on_each_connection_changes_nothing() {
        let manager = manager(BUDGET);
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        servers_started(&manager, 1).await;
        let fake = relay_control(&manager, "connected");
        fake.reply("account/read", [chatgpt_account()]);
        fake.push_after("remoteControl/status/read", signed_in_with_chatgpt());

        fake.until_requested("account/read", 2).await;
        fake.disconnect();
        fake.until_requested("account/read", 4).await;
        sleep(Duration::from_millis(200)).await;
        let running = status_until(&manager, connected).await;
        assert_eq!((running.problem, running.restarts), (None, 0));
        assert!(fake.requests_of(TURN_OFF).is_empty());
        assert!(fake.requests_of(TURN_ON).is_empty());
        assert_eq!(servers(&manager).len(), 1);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn signing_out_while_codex_serves_stops_it_without_a_failure() {
        for disabled_first in [true, false] {
            let manager = manager(BUDGET);
            let (supervisor, fake, first) = running(&manager, "0.157.1").await;

            sign_in(&manager, SIGNED_OUT);
            let mut pushes = [
                FakeControlServer::status_changed("disabled"),
                json!({
                    "method": "account/updated",
                    "params": {"authMode": null, "planType": null},
                }),
            ];
            if !disabled_first {
                pushes.reverse();
            }
            for frame in pushes {
                fake.push(frame);
            }

            let waiting =
                status_until(&manager, |status| status.state == ServerState::Waiting).await;
            assert_eq!(
                (waiting.problem, waiting.last_error, waiting.restarts),
                (None, None, 0),
                "disabled first: {disabled_first}"
            );
            first.wait_until_gone().await;
            assert_eq!(servers(&manager).len(), 1);

            shut_down(&manager, supervisor).await;
        }
    }

    #[tokio::test]
    async fn a_warning_about_reaching_chatgpt_leaves_the_relay_on() {
        let manager = manager(BUDGET);
        install_codex(
            &manager,
            "0.157.1",
            TAKES_EVERY_FLAG,
            &prints_when_told(&unavailable_warning()),
        );
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        servers_started(&manager, 1).await;
        let fake = relay_control(&manager, "errored");
        status_until(&manager, |status| status.state == ServerState::Running).await;

        tell_to_print(&manager);
        let unavailable = status_until(&manager, |status| status.problem.is_some()).await;
        assert_eq!(unavailable.problem, Some(CodexProblem::RelayUnavailable));
        sleep(Duration::from_millis(200)).await;
        assert!(fake.requests_of(TURN_OFF).is_empty());
        assert_eq!(
            manager.state.codex_remote.status().relay,
            Some(RelayState::Errored)
        );

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn an_error_is_published_once_and_shown_until_codex_connects() {
        let manager = manager(BUDGET);
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        servers_started(&manager, 1).await;
        let fake = control(&manager, "connecting");
        status_until(&manager, |status| status.state == ServerState::Running).await;
        let mut events = Box::pin(manager.state.events.stream());

        for relay in ["connecting", "errored", "connecting", "errored"] {
            fake.push(FakeControlServer::status_changed(relay));
        }
        let published = events.published().await;
        assert_eq!(
            published
                .iter()
                .filter(|topic| **topic == Topic::RemoteControl)
                .count(),
            1,
            "{published:?}"
        );
        assert_eq!(
            manager.state.codex_remote.status().relay,
            Some(RelayState::Errored)
        );

        fake.push(FakeControlServer::status_changed("connected"));
        status_until(&manager, connected).await;

        shut_down(&manager, supervisor).await;
    }

    const UNSUBSCRIBE: &str = "thread/unsubscribe";

    fn chats(status: &CodexRemoteStatus) -> Option<(u32, u32)> {
        status.usage.map(|usage| (usage.chats, usage.running_chats))
    }

    /// Starts a supervisor whose Codex lists `loaded` chats, reads them with `reads` and
    /// unsubscribes with `left` in turn, and waits until Codex is connected.
    async fn with_chats(
        manager: &TestManager,
        loaded: &[&str],
        reads: Vec<Reply>,
        left: &[&str],
    ) -> (JoinHandle<()>, FakeControlServer) {
        install_codex(manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(manager, CHATGPT);
        let supervisor = supervise(manager);
        servers_started(manager, 1).await;
        let fake = control(manager, "connected");
        fake.reply(
            "thread/loaded/list",
            [Reply::Result(json!({"data": loaded, "nextCursor": null}))],
        );
        fake.reply("thread/read", reads);
        fake.reply(
            UNSUBSCRIBE,
            left.iter()
                .map(|status| Reply::Result(json!({"status": status}))),
        );
        status_until(manager, connected).await;
        (supervisor, fake)
    }

    #[tokio::test]
    async fn chats_are_counted_from_the_loaded_list_and_what_codex_reports() {
        let manager = manager(BUDGET);
        let reads = vec![
            Reply::Result(json!({"thread": FakeControlServer::thread(
                "thread-1",
                json!({"type": "active", "activeFlags": []}),
            )})),
            Reply::Result(json!({"thread": FakeControlServer::thread(
                "thread-2",
                json!({"type": "idle"}),
            )})),
        ];
        let (supervisor, fake) = with_chats(
            &manager,
            &["thread-1", "thread-2"],
            reads,
            &["notSubscribed"],
        )
        .await;

        let read = status_until(&manager, |status| chats(status) == Some((2, 1))).await;
        assert!(
            read.usage.is_some_and(|usage| usage.memory_bytes > 0),
            "{read:?}"
        );
        assert_eq!(fake.requests_of("thread/read").len(), 2);

        fake.push(FakeControlServer::thread_status_changed("thread-1", "idle"));
        status_until(&manager, |status| chats(status) == Some((2, 0))).await;
        fake.push(json!({"method": "thread/closed", "params": {"threadId": "thread-2"}}));
        status_until(&manager, |status| chats(status) == Some((1, 0))).await;
        assert_eq!(
            fake.requests_of(UNSUBSCRIBE),
            [json!({"threadId": "thread-1"})]
        );

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn ezra_unsubscribes_from_a_chat_another_client_started_once() {
        let manager = manager(BUDGET);
        let (supervisor, fake) = with_chats(&manager, &[], Vec::new(), &["notSubscribed"]).await;

        fake.push(json!({
            "method": "thread/started",
            "params": {"thread": FakeControlServer::thread("thread-1", json!({"type": "idle"}))},
        }));
        status_until(&manager, |status| chats(status) == Some((1, 0))).await;
        fake.until_requested(UNSUBSCRIBE, 1).await;
        wait_until(WAIT, || fake.answered(UNSUBSCRIBE), |answered| *answered).await;

        fake.push(FakeControlServer::thread_status_changed(
            "thread-1", "active",
        ));
        status_until(&manager, |status| chats(status) == Some((1, 1))).await;
        fake.push(FakeControlServer::thread_status_changed("thread-1", "idle"));
        status_until(&manager, |status| chats(status) == Some((1, 0))).await;
        sleep(Duration::from_millis(200)).await;
        assert_eq!(
            fake.requests_of(UNSUBSCRIBE),
            [json!({"threadId": "thread-1"})]
        );

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn codex_asking_about_a_chat_unsubscribes_from_it() {
        let manager = manager(BUDGET);
        let (supervisor, fake) = with_chats(&manager, &[], Vec::new(), &["unsubscribed"]).await;

        fake.push(json!({
            "id": 0,
            "method": "item/commandExecution/requestApproval",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "item-1",
                "startedAtMs": 1_790_000_000_000_i64,
            },
        }));

        fake.until_requested(UNSUBSCRIBE, 1).await;
        assert_eq!(
            fake.requests_of(UNSUBSCRIBE),
            [json!({"threadId": "thread-1"})]
        );
        let asked = status_until(&manager, |status| chats(status) == Some((1, 1))).await;
        assert_eq!(asked.state, ServerState::Running);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_chat_codex_does_not_read_counts_as_running_until_codex_says_otherwise() {
        let manager = manager(BUDGET);
        let (supervisor, fake) = with_chats(
            &manager,
            &["thread-1"],
            vec![Reply::Never],
            &["notSubscribed"],
        )
        .await;

        fake.until_requested("thread/read", 1).await;
        status_until(&manager, |status| chats(status) == Some((1, 1))).await;

        fake.push(FakeControlServer::thread_status_changed("thread-1", "idle"));
        status_until(&manager, |status| chats(status) == Some((1, 0))).await;

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn chats_are_counted_again_after_a_lost_control_connection() {
        let manager = manager(BUDGET);
        let reads = vec![Reply::Result(json!({"thread": FakeControlServer::thread(
            "thread-1",
            json!({"type": "active", "activeFlags": []}),
        )}))];
        let (supervisor, fake) =
            with_chats(&manager, &["thread-1"], reads, &["notSubscribed"]).await;
        status_until(&manager, |status| chats(status) == Some((1, 1))).await;
        fake.reply(
            "thread/loaded/list",
            [Reply::Result(json!({"data": [], "nextCursor": null}))],
        );

        fake.disconnect();
        fake.until_requested("thread/loaded/list", 2).await;
        let recounted = status_until(&manager, |status| {
            connected(status) && chats(status) == Some((0, 0))
        })
        .await;
        assert_eq!(recounted.restarts, 0);
        sleep(Duration::from_millis(200)).await;
        assert_eq!(chats(&manager.state.codex_remote.status()), Some((0, 0)));

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn the_memory_codex_uses_is_read_again_while_it_runs() {
        let manager = manager(ServerBudget {
            usage: Duration::from_millis(100),
            ..BUDGET
        });
        install_codex(
            &manager,
            "0.157.1",
            TAKES_EVERY_FLAG,
            "mkfifo \"$CODEX_HOME/grow\"\n\
             ( read _ < \"$CODEX_HOME/grow\"; sleep 60 & sleep 60 & sleep 60 & wait ) &\n\
             exec sleep 60",
        );
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        let _fake = serve_control(&manager, "connected").await;
        let grow = codex_home(&manager).join("grow");
        wait_until(
            WAIT,
            || fs::metadata(&grow).is_ok_and(|grow| grow.file_type().is_fifo()),
            |made| *made,
        )
        .await;
        sleep(Duration::from_millis(500)).await;
        let before = status_until(&manager, |status| status.usage.is_some())
            .await
            .usage
            .map(|usage| usage.memory_bytes)
            .expect("the memory was read");

        fs::write(&grow, "\n").expect("the server is told to start more processes");

        status_until(&manager, |status| {
            status
                .usage
                .is_some_and(|usage| usage.memory_bytes > before)
        })
        .await;

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
        fake.until_requested("account/read", 2).await;
        let requests = fake.requests();
        let methods: Vec<&str> = requests
            .iter()
            .map(|(method, _)| method.as_str())
            .filter(|method| ["initialize", "remoteControl/status/read"].contains(method))
            .collect();
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
    async fn a_relay_ezra_turned_off_stays_off_across_a_lost_control_connection() {
        for answered in [false, true] {
            let manager = manager(BUDGET);
            let (supervisor, fake) = asking_for_mfa(&manager, |manager| {
                let fake = FakeControlServer::bind(&codex_home(manager));
                fake.reply(
                    "remoteControl/status/read",
                    [
                        Reply::Result(FakeControlServer::relay("errored")),
                        Reply::Result(FakeControlServer::relay("disabled")),
                    ],
                );
                let disabled = FakeControlServer::status_changed("disabled");
                if answered {
                    fake.reply(
                        TURN_OFF,
                        [Reply::Result(FakeControlServer::relay("disabled"))],
                    );
                    fake.push_after(TURN_OFF, disabled);
                } else {
                    fake.reply(TURN_OFF, [Reply::Never]);
                    fake.push_before(TURN_OFF, disabled);
                }
                fake
            })
            .await;

            fake.disconnect();
            fake.until_requested("remoteControl/status/read", 2).await;
            status_until(&manager, turned_off).await;
            sleep(Duration::from_millis(500)).await;
            let off = manager.state.codex_remote.status();
            assert_eq!(
                (off.state, off.relay, off.problem, off.restarts),
                (
                    ServerState::Running,
                    Some(RelayState::Disabled),
                    Some(CodexProblem::MfaRequired),
                    0
                ),
                "answered: {answered}"
            );
            assert_eq!(fake.requests_of(TURN_OFF).len(), 1, "answered: {answered}");
            assert_eq!(servers(&manager).len(), 1);

            shut_down(&manager, supervisor).await;
        }
    }

    #[tokio::test]
    async fn an_mfa_warning_while_the_control_connection_is_lost_turns_the_relay_off_once_it_opens()
    {
        let manager = manager(BUDGET);
        install_codex(
            &manager,
            "0.157.1",
            TAKES_EVERY_FLAG,
            &prints_when_told(&mfa_warning()),
        );
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        servers_started(&manager, 1).await;
        let fake = relay_control(&manager, "errored");
        status_until(&manager, |status| status.state == ServerState::Running).await;
        fake.reply(
            "remoteControl/status/read",
            [Reply::Late(
                Duration::from_secs(1),
                FakeControlServer::relay("errored"),
            )],
        );

        fake.disconnect();
        status_until(&manager, |status| status.relay.is_none()).await;
        tell_to_print(&manager);
        status_until(&manager, |status| {
            status.problem == Some(CodexProblem::MfaRequired)
        })
        .await;
        assert!(fake.requests_of(TURN_OFF).is_empty());

        fake.until_requested(TURN_OFF, 1).await;
        let off = status_until(&manager, turned_off).await;
        assert_eq!((off.state, off.restarts), (ServerState::Running, 0));
        sleep(Duration::from_millis(200)).await;
        assert_eq!(fake.requests_of(TURN_OFF), [json!({"ephemeral": true})]);

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

    fn connected_on(version: &str) -> impl Fn(&CodexRemoteStatus) -> bool {
        move |status| connected(status) && status.server_version.as_deref() == Some(version)
    }

    fn waiting_for(version: &str) -> impl Fn(&CodexRemoteStatus) -> bool {
        move |status| {
            status
                .update
                .as_ref()
                .is_some_and(|update| update.version == version)
        }
    }

    #[tokio::test]
    async fn an_idle_codex_restarts_on_a_new_version_at_once_and_the_old_version_is_removed() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        install_codex(
            &manager,
            "0.157.1",
            TAKES_EVERY_FLAG,
            "cp /bin/sleep \"${0%/*}/sleep\" && exec \"${0%/*}/sleep\" 60",
        );
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        let _fake = serve_control(&manager, "connected").await;
        status_until(&manager, connected).await;
        let first = *servers(&manager).first().expect("a server started");
        let versions = manager.state.install_paths.versions_directory(Agent::Codex);

        install_codex(&manager, "0.157.2", TAKES_EVERY_FLAG, SERVE);
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        servers_started(&manager, 2).await;
        first.wait_until_gone().await;
        let running = status_until(&manager, connected_on("0.157.2")).await;
        assert_eq!((running.update, running.restarts), (None, 0));
        assert!(!versions.join("0.157.1").exists());
        assert!(versions.join("0.157.2").exists());

        shut_down(&manager, supervisor).await;
    }

    /// Starts a supervisor whose Codex 0.157.1 runs one chat, and waits until the chat counts.
    async fn with_a_running_chat(
        manager: &TestManager,
    ) -> (JoinHandle<()>, FakeControlServer, Pid) {
        let reads = vec![Reply::Result(json!({"thread": FakeControlServer::thread(
            "thread-1",
            json!({"type": "active", "activeFlags": []}),
        )}))];
        let (supervisor, fake) =
            with_chats(manager, &["thread-1"], reads, &["notSubscribed"]).await;
        status_until(manager, |status| chats(status) == Some((1, 1))).await;
        let server = *servers(manager).first().expect("a server started");
        (supervisor, fake, server)
    }

    /// Installs Codex 0.157.2, and waits until the server on 0.157.1 waits to restart on it.
    async fn an_update_waits(manager: &TestManager, cookie: &str) -> PendingUpdate {
        install_codex(manager, "0.157.2", TAKES_EVERY_FLAG, SERVE);
        save(manager, cookie, CodexRemoteSettings::default()).await;
        let waiting = status_until(manager, |status| status.update.is_some()).await;
        assert_eq!(
            (waiting.state, waiting.server_version.as_deref()),
            (ServerState::Running, Some("0.157.1"))
        );
        let update = waiting.update.expect("an update waits");
        assert_eq!(update.version, "0.157.2");
        update
    }

    #[tokio::test]
    async fn a_codex_with_a_running_chat_restarts_on_a_new_version_once_no_chat_runs() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let (supervisor, fake, first) = with_a_running_chat(&manager).await;
        let versions = manager.state.install_paths.versions_directory(Agent::Codex);
        fake.push(FakeControlServer::thread_status_changed(
            "thread-1", "active",
        ));
        fake.until_requested(UNSUBSCRIBE, 1).await;
        wait_until(WAIT, || fake.answered(UNSUBSCRIBE), |answered| *answered).await;

        let update = an_update_waits(&manager, &cookie).await;
        let now = OffsetDateTime::now_utc();
        assert!(
            (now.saturating_add(time::Duration::hours(5))
                ..=now.saturating_add(time::Duration::hours(6)))
                .contains(&update.restart_by),
            "{}",
            update.restart_by
        );
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        sleep(Duration::from_millis(200)).await;
        assert_eq!(servers(&manager), [first]);
        assert_eq!(manager.state.codex_remote.status().update, Some(update));
        assert!(versions.join("0.157.1").exists());

        fake.push(FakeControlServer::thread_status_changed("thread-1", "idle"));
        servers_started(&manager, 2).await;
        assert_eq!(fake.requests_of(UNSUBSCRIBE).len(), 1);
        first.wait_until_gone().await;
        let running = status_until(&manager, connected_on("0.157.2")).await;
        assert_eq!((running.update, running.restarts), (None, 0));
        assert!(!versions.join("0.157.1").exists());

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_newer_codex_takes_the_place_of_a_waiting_update_and_keeps_its_deadline() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let (supervisor, fake, first) = with_a_running_chat(&manager).await;
        let versions = manager.state.install_paths.versions_directory(Agent::Codex);
        let update = an_update_waits(&manager, &cookie).await;

        install_codex(&manager, "0.157.3", TAKES_EVERY_FLAG, SERVE);
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        let waiting = status_until(&manager, waiting_for("0.157.3")).await;
        assert_eq!(
            waiting.update.map(|waiting| waiting.restart_by),
            Some(update.restart_by)
        );
        assert_eq!(servers(&manager), [first]);

        fake.push(FakeControlServer::thread_status_changed("thread-1", "idle"));
        servers_started(&manager, 2).await;
        first.wait_until_gone().await;
        status_until(&manager, connected_on("0.157.3")).await;
        assert!(!versions.join("0.157.1").exists());
        assert!(!versions.join("0.157.2").exists());

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_codex_with_a_running_chat_restarts_on_a_new_version_at_the_deadline() {
        let deadline = Duration::from_secs(1);
        let manager = manager(ServerBudget {
            update_deadline: deadline,
            ..BUDGET
        });
        let cookie = manager.logged_in().await;
        let (supervisor, _fake, first) = with_a_running_chat(&manager).await;

        let asked = Instant::now();
        let update = an_update_waits(&manager, &cookie).await;
        assert!(
            update.restart_by <= OffsetDateTime::now_utc().saturating_add(time::Duration::SECOND)
        );
        servers_started(&manager, 2).await;
        assert!(asked.elapsed() >= deadline, "{:?}", asked.elapsed());
        first.wait_until_gone().await;
        let running = status_until(&manager, connected_on("0.157.2")).await;
        assert_eq!((running.update, running.restarts), (None, 0));

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn turning_codex_off_stops_it_at_once_even_with_a_running_chat() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let (supervisor, _fake, first) = with_a_running_chat(&manager).await;
        an_update_waits(&manager, &cookie).await;

        let off = CodexRemoteSettings {
            enabled: false,
            ..CodexRemoteSettings::default()
        };
        save(&manager, &cookie, off).await;
        let stopped = status_until(&manager, |status| status.state == ServerState::Off).await;
        assert_eq!(
            (stopped.update, stopped.usage, stopped.restarts),
            (None, None, 0)
        );
        first.wait_until_gone().await;
        assert_eq!(servers(&manager), [first]);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_restart_on_a_new_version_waits_until_codex_lists_its_chats() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        install_codex(&manager, "0.157.1", TAKES_EVERY_FLAG, SERVE);
        sign_in(&manager, CHATGPT);
        let supervisor = supervise(&manager);
        servers_started(&manager, 1).await;
        let fake = control(&manager, "connected");
        fake.reply(
            "thread/loaded/list",
            [Reply::Late(
                Duration::from_secs(3),
                json!({"data": [], "nextCursor": null}),
            )],
        );
        status_until(&manager, connected).await;

        an_update_waits(&manager, &cookie).await;
        assert_eq!(servers(&manager).len(), 1);
        assert!(!fake.answered("thread/loaded/list"));

        servers_started(&manager, 2).await;
        assert!(fake.answered("thread/loaded/list"));
        status_until(&manager, connected_on("0.157.2")).await;

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn codex_turning_the_relay_off_by_itself_restarts_it_while_an_update_waits() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let (supervisor, fake, first) = with_a_running_chat(&manager).await;
        an_update_waits(&manager, &cookie).await;

        fake.push(FakeControlServer::status_changed("disabled"));
        let retrying = status_until(&manager, |status| status.state == ServerState::Retrying).await;
        assert_eq!(
            (
                retrying.update,
                retrying.restarts,
                retrying.last_error.as_deref()
            ),
            (None, 1, Some(TURNED_OFF))
        );
        first.wait_until_gone().await;

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_list_of_chats_cut_off_by_a_lost_connection_keeps_an_update_waiting() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let (supervisor, fake, first) = with_a_running_chat(&manager).await;
        an_update_waits(&manager, &cookie).await;

        fake.reply("thread/loaded/list", [Reply::Never]);
        fake.disconnect();
        fake.until_requested("thread/loaded/list", 2).await;
        fake.disconnect();
        fake.until_requested("thread/loaded/list", 3).await;
        let waiting = status_until(&manager, connected).await;
        assert!(waiting_for("0.157.2")(&waiting));
        assert_eq!(servers(&manager), [first]);

        shut_down(&manager, supervisor).await;
    }

    #[tokio::test]
    async fn a_version_without_remote_control_drops_a_waiting_update_and_a_later_one_waits_anew() {
        let manager = manager(BUDGET);
        let cookie = manager.logged_in().await;
        let (supervisor, _fake, first) = with_a_running_chat(&manager).await;
        let dropped = an_update_waits(&manager, &cookie).await;

        install_codex(
            &manager,
            "0.100.0",
            "'app-server --remote-control --help') exit 2 ;;",
            SERVE,
        );
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        let kept = status_until(&manager, |status| status.update.is_none()).await;
        assert_eq!(
            (kept.state, kept.problem, kept.server_version.as_deref()),
            (
                ServerState::Running,
                Some(CodexProblem::UnsupportedVersion),
                Some("0.157.1")
            )
        );
        assert_eq!(servers(&manager), [first]);

        install_codex(&manager, "0.157.3", TAKES_EVERY_FLAG, SERVE);
        save(&manager, &cookie, CodexRemoteSettings::default()).await;
        let waiting = status_until(&manager, waiting_for("0.157.3")).await;
        assert_eq!(waiting.problem, None);
        assert!(
            waiting
                .update
                .is_some_and(|update| update.restart_by > dropped.restart_by)
        );
        assert_eq!(servers(&manager), [first]);

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
        status_until(&manager, connected_on("0.157.2")).await;

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
        let LinkEvent::Ready { relay, client } = event else {
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
        let account = client
            .request(AccountRead {})
            .await
            .expect("codex reads its sign-in");
        assert_eq!(
            account.problem(),
            Some(CodexProblem::SignedOut),
            "{account:?}"
        );
        let turned_off = client
            .request(Disable { ephemeral: true })
            .await
            .expect("codex turns the relay off");
        assert_eq!(turned_off.status, RelayStatusWire::Disabled);
        timeout(Duration::from_secs(5), async {
            loop {
                if let LinkEvent::Control(ControlEvent::Notification(
                    ControlNotification::StatusChanged(relay),
                )) = link.next().await
                    && relay.status == RelayStatusWire::Disabled
                {
                    break;
                }
            }
        })
        .await
        .expect("codex reports the relay turned off");
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
