mod chats;
mod control;
#[cfg(test)]
mod fake;
mod foreign;
mod launch;
mod pairing;
mod problem;
mod run;
mod supervisor;

use std::mem;
use std::sync::{Arc, Mutex as SyncMutex, PoisonError};
use std::time::Duration;

use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use utoipa::ToSchema;

use super::events::{Events, Topic};
use super::remote_control::ServerState;
use super::supervision::{
    Failure, OUTPUT_DRAIN_TIMEOUT, PendingUpdate, Published, ServerLog, Supervision,
    UPDATE_RESTART_DEADLINE, USAGE_INTERVAL,
};
pub use control::ControlError;
use control::{ControlClient, Enable, RelayStatusWire, RelayWire};
use launch::LaunchFlagCache;
use pairing::Pairing;
pub use pairing::{CodexPairing, CodexPairingState, PairedPhone, PairingError};
use problem::{CodexProblem, ProblemLine};

const HOLD_SLACK: Duration = Duration::from_secs(1);

/// How Codex serves this box to the ChatGPT app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct CodexRemoteSettings {
    /// Serve this box to the ChatGPT app while Codex is signed in with ChatGPT.
    #[schema(required = true)]
    pub enabled: bool,
    /// The sandbox Codex runs commands in.
    #[schema(required = true)]
    pub sandbox: CodexSandbox,
    /// When Codex asks for approval.
    #[schema(required = true)]
    pub approvals: CodexApprovals,
}

impl Default for CodexRemoteSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            sandbox: CodexSandbox::default(),
            approvals: CodexApprovals::default(),
        }
    }
}

/// How long each step with the Codex server may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerBudget {
    /// For each `--help` flag probe.
    pub probe: Duration,
    /// For the control socket to answer after a start, and again after the connection was lost.
    pub readiness: Duration,
    /// After the first SIGTERM, for running turns to finish.
    pub drain: Duration,
    /// After the second SIGTERM, before SIGKILL.
    pub force: Duration,
    /// For each answer on the control socket, and for its WebSocket handshake.
    pub request: Duration,
    /// After ezra turned the relay off because ChatGPT asks for multi-factor authentication,
    /// before it turns it on again.
    pub mfa_retry: Duration,
    /// Between readings of what the server and its chats use.
    pub usage: Duration,
    /// While chats run, before the server restarts on a newer Codex anyway.
    pub update_deadline: Duration,
    /// Between checks whether a phone used the pairing code.
    pub pairing_poll: Duration,
}

impl ServerBudget {
    /// The longest a stop takes, including reading the last output.
    pub fn longest_stop(&self) -> Duration {
        self.drain
            .saturating_add(self.force)
            .saturating_add(OUTPUT_DRAIN_TIMEOUT)
    }
}

impl Default for ServerBudget {
    fn default() -> Self {
        Self {
            probe: Duration::from_secs(5),
            readiness: Duration::from_secs(30),
            drain: Duration::from_secs(20),
            force: Duration::from_secs(10),
            request: Duration::from_secs(45),
            mfa_retry: Duration::from_secs(10 * 60),
            usage: USAGE_INTERVAL,
            update_deadline: UPDATE_RESTART_DEADLINE,
            pairing_poll: Duration::from_secs(5),
        }
    }
}

/// The process that must answer on the control socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedPeer {
    /// The server the supervisor started.
    Child,
    /// A fake control server in the test process.
    #[cfg(test)]
    Pid(Pid),
}

impl ExpectedPeer {
    /// The process that must answer when `server` is the one started.
    fn pid(self, server: Pid) -> Pid {
        match self {
            Self::Child => server,
            #[cfg(test)]
            Self::Pid(pid) => pid,
        }
    }
}

/// Codex's remote control server as the API reads it, and what its supervisor runs it with.
#[derive(Debug)]
pub struct CodexRemote {
    pub supervision: Supervision,
    pub log: ServerLog,
    pub budget: ServerBudget,
    events: Events,
    status: Published<CodexRemoteStatus>,
    flags: LaunchFlagCache,
    expected: ExpectedPeer,
    /// Sign-ins in progress, which keep the server stopped.
    holds: watch::Sender<u32>,
    control: SyncMutex<Control>,
    /// The pairing code asked for last.
    pairing: SyncMutex<Option<Pairing>>,
}

/// The running server's control connection, and whether ezra turned its relay off.
#[derive(Debug, Default)]
struct Control {
    client: Option<Arc<ControlClient>>,
    /// ezra turned the relay off, or will once a connection opens.
    relay_off: bool,
}

impl Control {
    /// Marks the relay as turned off by ezra. False when it was already.
    fn turn_relay_off(&mut self) -> bool {
        !mem::replace(&mut self.relay_off, true)
    }

    /// Marks the relay as on again when ezra turned it off and a connection is open to ask
    /// Codex on, and returns that connection.
    fn turn_relay_on(&mut self) -> Option<Arc<ControlClient>> {
        let client = self.client.clone().filter(|_| self.relay_off)?;
        self.relay_off = false;
        Some(client)
    }
}

impl CodexRemote {
    /// Publishes every status change to `events` and adds the server's output to `log`.
    pub fn new(
        events: Events,
        log: ServerLog,
        budget: ServerBudget,
        expected: ExpectedPeer,
    ) -> Self {
        Self {
            supervision: Supervision::default(),
            log,
            budget,
            events: events.clone(),
            status: Published::new(CodexRemoteStatus::default(), events, Topic::RemoteControl),
            flags: LaunchFlagCache::default(),
            expected,
            holds: watch::Sender::new(0),
            control: SyncMutex::default(),
            pairing: SyncMutex::default(),
        }
    }

    /// The status as the API shows it.
    pub fn status(&self) -> CodexRemoteStatus {
        self.status.read(Clone::clone)
    }

    fn is_held(&self) -> bool {
        *self.holds.borrow() > 0
    }

    /// Stops the server until the hold is dropped, and waits until it has stopped.
    pub async fn hold_for_sign_in(self: &Arc<Self>) -> SignInHold {
        let hold = SignInHold::take(Arc::clone(self));
        let longest = self.budget.longest_stop().saturating_add(HOLD_SLACK);
        if !self
            .status
            .wait_until(|status| !status.has_server(), longest)
            .await
        {
            tracing::warn!("Codex remote control did not stop within {longest:?} for a sign-in");
        }
        hold
    }

    /// Shows the server starting on `version`. False, with nothing shown, while a sign-in holds
    /// it stopped.
    fn start_unless_held(&self, version: &str) -> bool {
        self.status.update(|status| {
            let held = self.is_held();
            if !held {
                status.start(version);
            }
            !held
        })
    }

    /// Clears the problem shown, and asks Codex to turn the relay on again when ezra turned it
    /// off. Shows the problem again when Codex does not.
    pub async fn retry(&self) -> Result<(), ControlError> {
        let (client, was_off) = self
            .with_control(|control| {
                let client = control.client.clone()?;
                Some((client, mem::replace(&mut control.relay_off, false)))
            })
            .ok_or(ControlError::Closed)?;
        let shown = self.status.update(CodexRemoteStatus::take_problem);
        if !was_off {
            return Ok(());
        }
        match client.request(Enable { ephemeral: true }).await {
            Ok(_relay) => Ok(()),
            Err(ControlError::Closed) => Err(ControlError::Closed),
            Err(error) => {
                self.with_control(|control| control.relay_off = true);
                self.status.update(|status| status.show_again(shown));
                Err(error)
            }
        }
    }

    fn with_control<R>(&self, change: impl FnOnce(&mut Control) -> R) -> R {
        change(&mut self.control.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn client(&self) -> Option<Arc<ControlClient>> {
        self.with_control(|control| control.client.clone())
    }
}

/// Keeps Codex's remote control server stopped while Codex signs in or out, until dropped.
#[derive(Debug)]
pub struct SignInHold(Arc<CodexRemote>);

impl SignInHold {
    fn take(codex_remote: Arc<CodexRemote>) -> Self {
        codex_remote
            .holds
            .send_modify(|holds| *holds = holds.saturating_add(1));
        codex_remote.supervision.reconsider();
        Self(codex_remote)
    }
}

impl Drop for SignInHold {
    fn drop(&mut self) {
        self.0
            .holds
            .send_modify(|holds| *holds = holds.saturating_sub(1));
        self.0.supervision.reconsider();
    }
}

/// The manager's view of Codex's remote control server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CodexRemoteStatus {
    pub state: ServerState,
    /// Whether Codex is connected to ChatGPT, absent while the server does not answer.
    pub relay: Option<RelayState>,
    /// The name the ChatGPT app lists this box under, absent while the server does not answer.
    pub server_name: Option<String>,
    /// The Codex version the server runs, absent while none runs.
    pub server_version: Option<String>,
    /// What keeps Codex from serving the ChatGPT app, absent when nothing known does.
    pub problem: Option<CodexProblem>,
    /// The line that named the problem or the last output before a stop, until Codex connects.
    pub last_error: Option<String>,
    /// Unexpected stops since the manager started.
    pub restarts: u32,
    /// What the server and its chats use, absent while no server runs.
    pub usage: Option<CodexUsage>,
    /// A newer Codex the server restarts on once no chat runs, absent while none waits.
    pub update: Option<PendingUpdate>,
}

/// What a running Codex server and its chats use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CodexUsage {
    /// Chats the server has loaded.
    pub chats: u32,
    /// Chats running a turn or waiting for an answer, and chats whose state is not known yet.
    pub running_chats: u32,
    /// Memory the server, its chats and their commands use, with shared memory counted once.
    pub memory_bytes: u64,
}

impl CodexRemoteStatus {
    /// Shows `state` with no server answering.
    fn enter(&mut self, state: ServerState) {
        self.state = state;
        self.relay = None;
        self.server_name = None;
        self.server_version = None;
        self.usage = None;
        self.update = None;
    }

    /// Shows no server running, and the problem keeping it from starting.
    fn idle(&mut self, state: ServerState, problem: Option<CodexProblem>) {
        self.enter(state);
        self.problem = problem;
    }

    /// Whether a server may be running.
    fn has_server(&self) -> bool {
        matches!(
            self.state,
            ServerState::Starting | ServerState::Running | ServerState::Stopping
        )
    }

    /// Whether to start or stop is not known yet, so a running server shows as waiting.
    fn unsure(&mut self) {
        if self.has_server() {
            self.enter(ServerState::Waiting);
        }
    }

    fn start(&mut self, version: &str) {
        self.enter(ServerState::Starting);
        self.server_version = Some(version.to_owned());
    }

    /// The server answered on its control socket, with `relay` as it was then.
    fn answer(&mut self, relay: RelayWire) {
        self.state = ServerState::Running;
        self.show_relay(relay);
    }

    /// The control connection closed, so the relay is unknown until it opens again.
    fn lose_control(&mut self) {
        self.relay = None;
    }

    /// Shows the relay, and keeps an error shown while Codex tries again. A connection clears
    /// every problem but the installed version's.
    fn show_relay(&mut self, relay: RelayWire) {
        let state = RelayState::from(relay.status);
        if !(state == RelayState::Connecting && self.relay == Some(RelayState::Errored)) {
            self.relay = Some(state);
        }
        self.server_name = Some(relay.server_name);
        if state == RelayState::Connected {
            self.clear_problem();
        }
    }

    /// Forgets the problem shown and its line, unless the installed version is the problem.
    fn clear_problem(&mut self) {
        self.last_error = None;
        self.problem = self
            .problem
            .filter(|problem| *problem == CodexProblem::UnsupportedVersion);
    }

    /// Clears the problem like `clear_problem`, and returns the problem and line it showed.
    fn take_problem(&mut self) -> (Option<CodexProblem>, Option<String>) {
        let shown = (self.problem, self.last_error.clone());
        self.clear_problem();
        shown
    }

    /// Shows a problem taken earlier again, unless another was named since.
    fn show_again(&mut self, (problem, last_error): (Option<CodexProblem>, Option<String>)) {
        if self.problem.is_none_or(|shown| Some(shown) == problem) && self.last_error.is_none() {
            self.problem = problem;
            self.last_error = last_error;
        }
    }

    /// Shows the first line that names a problem, until another problem replaces it.
    fn name_problem(&mut self, ProblemLine { problem, line }: ProblemLine) {
        if self.problem != Some(problem) {
            self.problem = Some(problem);
            self.last_error = Some(line);
        }
    }

    /// Shows a problem the server's sign-in names, with no line.
    fn name_sign_in_problem(&mut self, problem: CodexProblem) {
        if self.problem != Some(problem) {
            self.problem = Some(problem);
            self.last_error = None;
        }
    }

    /// The running server is kept with no restart waiting, and the installed version cannot
    /// replace it when `unsupported`.
    fn keep(&mut self, unsupported: bool) {
        self.update = None;
        if unsupported {
            self.problem = Some(CodexProblem::UnsupportedVersion);
        } else if self.problem == Some(CodexProblem::UnsupportedVersion) {
            self.problem = None;
        }
    }

    /// The running server waits to restart on `update`, which can replace it.
    fn wait_for_update(&mut self, update: PendingUpdate) {
        self.keep(false);
        self.update = Some(update);
    }

    fn fail(&mut self, Failure { message, problem }: Failure<CodexProblem>) {
        self.enter(ServerState::Retrying);
        self.last_error = Some(message);
        self.problem = problem;
        self.restarts = self.restarts.saturating_add(1);
    }
}

/// Whether Codex is connected to ChatGPT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelayState {
    /// Turned off for now.
    Disabled,
    /// Connecting to ChatGPT.
    Connecting,
    /// Connected, so the ChatGPT app can reach this box.
    Connected,
    /// The last try failed, and Codex keeps trying.
    Errored,
}

impl From<RelayStatusWire> for RelayState {
    fn from(status: RelayStatusWire) -> Self {
        match status {
            RelayStatusWire::Disabled => Self::Disabled,
            RelayStatusWire::Connecting => Self::Connecting,
            RelayStatusWire::Connected => Self::Connected,
            RelayStatusWire::Errored => Self::Errored,
        }
    }
}

/// What Codex's commands can change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CodexSandbox {
    /// Commands can read files but not change them.
    ReadOnly,
    /// Commands can write in the chat's folder, /tmp and $TMPDIR, with network off by default.
    WorkspaceWrite,
    /// No sandbox.
    #[default]
    DangerFullAccess,
}

/// When Codex asks for approval.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CodexApprovals {
    /// Codex asks when it decides it needs to.
    #[default]
    OnRequest,
    /// Codex never asks.
    Never,
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::path::PathBuf;
    use std::time::Instant;

    use serde::de::DeserializeOwned;
    use time::macros::datetime;
    use tokio::time::{sleep, timeout};

    use super::*;
    use crate::manager::codex_remote::fake::FakeControlServer;
    use crate::manager::codex_remote::problem::tests::PROBLEMS;
    use crate::manager::remote_control::RemoteControl;

    const SANDBOXES: [(CodexSandbox, &str); 3] = [
        (CodexSandbox::ReadOnly, "read-only"),
        (CodexSandbox::WorkspaceWrite, "workspace-write"),
        (CodexSandbox::DangerFullAccess, "danger-full-access"),
    ];
    const APPROVALS: [(CodexApprovals, &str); 2] = [
        (CodexApprovals::OnRequest, "on-request"),
        (CodexApprovals::Never, "never"),
    ];

    fn round_trips<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: T, name: &str) {
        let json = format!("\"{name}\"");
        assert_eq!(
            serde_json::to_string(&value).expect("value serializes"),
            json
        );
        assert_eq!(
            serde_json::from_str::<T>(&json).expect("value parses"),
            value
        );
    }

    #[test]
    fn every_value_is_named_in_kebab_case() {
        for (sandbox, name) in SANDBOXES {
            round_trips(sandbox, name);
        }
        for (approvals, name) in APPROVALS {
            round_trips(approvals, name);
        }
    }

    #[test]
    fn values_ezra_does_not_offer_are_rejected() {
        for approvals in ["untrusted", "on-failure", "Never", "OnRequest"] {
            assert!(
                serde_json::from_str::<CodexApprovals>(&format!("\"{approvals}\"")).is_err(),
                "{approvals}"
            );
        }
        assert!(
            serde_json::from_str::<CodexApprovals>(
                r#"{"granular":{"mcp_elicitations":true,"rules":true,"sandbox_approval":true}}"#
            )
            .is_err()
        );
        for sandbox in ["ReadOnly", "read_only", "danger_full_access"] {
            assert!(
                serde_json::from_str::<CodexSandbox>(&format!("\"{sandbox}\"")).is_err(),
                "{sandbox}"
            );
        }
    }

    #[test]
    fn the_defaults_serve_with_no_sandbox_asking_on_request() {
        assert_eq!(
            CodexRemoteSettings::default(),
            CodexRemoteSettings {
                enabled: true,
                sandbox: CodexSandbox::DangerFullAccess,
                approvals: CodexApprovals::OnRequest,
            }
        );
        assert_eq!(
            serde_json::from_str::<CodexRemoteSettings>("{}").expect("empty settings parse"),
            CodexRemoteSettings::default()
        );
    }

    const STATES: [ServerState; 6] = [
        ServerState::Off,
        ServerState::Waiting,
        ServerState::Starting,
        ServerState::Running,
        ServerState::Retrying,
        ServerState::Stopping,
    ];

    /// Codex's report of the relay as `status`.
    fn reported(status: &str) -> RelayWire {
        serde_json::from_value(FakeControlServer::relay(status)).expect("the relay parses")
    }

    /// A running server whose relay errored, showing `problem`.
    fn errored_with(problem: CodexProblem) -> CodexRemoteStatus {
        CodexRemoteStatus {
            state: ServerState::Running,
            relay: Some(RelayState::Errored),
            server_name: Some("ezra-dev".to_owned()),
            server_version: Some("0.157.1".to_owned()),
            problem: Some(problem),
            last_error: Some("the line that named it".to_owned()),
            restarts: 1,
            usage: Some(CodexUsage {
                chats: 2,
                running_chats: 1,
                memory_bytes: 400_000_000,
            }),
            update: Some(PendingUpdate {
                version: "0.157.2".to_owned(),
                restart_by: datetime!(2026-09-27 18:00 UTC),
            }),
        }
    }

    fn line(problem: CodexProblem, line: &str) -> ProblemLine {
        ProblemLine {
            problem,
            line: line.to_owned(),
        }
    }

    #[test]
    fn a_connected_relay_clears_every_problem_but_the_installed_versions() {
        for problem in PROBLEMS {
            let mut status = errored_with(problem);
            status.show_relay(reported("connected"));
            assert_eq!(
                status,
                CodexRemoteStatus {
                    relay: Some(RelayState::Connected),
                    problem: (problem == CodexProblem::UnsupportedVersion).then_some(problem),
                    last_error: None,
                    ..errored_with(problem)
                },
                "{problem:?}"
            );
        }
    }

    #[test]
    fn a_relay_that_is_not_connected_leaves_the_problem_shown() {
        for (reported_status, shown) in [
            ("disabled", RelayState::Disabled),
            ("connecting", RelayState::Connecting),
            ("errored", RelayState::Errored),
        ] {
            let mut status = CodexRemoteStatus {
                relay: None,
                server_name: None,
                ..errored_with(CodexProblem::MfaRequired)
            };
            status.show_relay(reported(reported_status));
            assert_eq!(
                status,
                CodexRemoteStatus {
                    relay: Some(shown),
                    ..errored_with(CodexProblem::MfaRequired)
                },
                "{reported_status}"
            );
        }
    }

    #[test]
    fn an_error_stays_shown_while_codex_tries_again() {
        let mut status = CodexRemoteStatus {
            relay: Some(RelayState::Connecting),
            ..errored_with(CodexProblem::RelayUnavailable)
        };
        for (reported_status, shown) in [
            ("errored", RelayState::Errored),
            ("connecting", RelayState::Errored),
            ("errored", RelayState::Errored),
            ("disabled", RelayState::Disabled),
            ("connecting", RelayState::Connecting),
            ("errored", RelayState::Errored),
            ("connected", RelayState::Connected),
            ("connecting", RelayState::Connecting),
        ] {
            status.show_relay(reported(reported_status));
            assert_eq!(status.relay, Some(shown), "{reported_status}");
        }
    }

    #[test]
    fn clearing_the_problem_keeps_only_the_installed_versions() {
        for problem in PROBLEMS {
            let mut status = errored_with(problem);
            status.clear_problem();
            assert_eq!(
                status,
                CodexRemoteStatus {
                    problem: (problem == CodexProblem::UnsupportedVersion).then_some(problem),
                    last_error: None,
                    ..errored_with(problem)
                },
                "{problem:?}"
            );
        }
    }

    #[test]
    fn a_taken_problem_is_shown_again_unless_another_was_named() {
        for problem in PROBLEMS {
            let mut status = errored_with(problem);
            let shown = status.take_problem();
            status.show_again(shown);
            assert_eq!(status, errored_with(problem), "{problem:?}");
        }

        let mut status = errored_with(CodexProblem::MfaRequired);
        let shown = status.take_problem();
        status.name_problem(line(CodexProblem::RelayUnavailable, "since"));
        status.show_again(shown);
        assert_eq!(
            (status.problem, status.last_error.as_deref()),
            (Some(CodexProblem::RelayUnavailable), Some("since"))
        );
    }

    #[test]
    fn a_sign_in_problem_replaces_the_line_of_another_problem() {
        let mut status = errored_with(CodexProblem::SignedOut);
        status.name_sign_in_problem(CodexProblem::SignedOut);
        assert_eq!(status, errored_with(CodexProblem::SignedOut));

        status.name_sign_in_problem(CodexProblem::NotChatGpt);
        assert_eq!(
            status,
            CodexRemoteStatus {
                last_error: None,
                ..errored_with(CodexProblem::NotChatGpt)
            }
        );
    }

    #[test]
    fn only_a_problem_of_another_kind_replaces_the_line_shown() {
        let mut status = CodexRemoteStatus::default();
        status.name_problem(line(CodexProblem::SignedOut, "first"));
        status.name_problem(line(CodexProblem::SignedOut, "second"));
        assert_eq!(
            (status.problem, status.last_error.as_deref()),
            (Some(CodexProblem::SignedOut), Some("first"))
        );

        status.name_problem(line(CodexProblem::RelayUnavailable, "third"));
        assert_eq!(
            (status.problem, status.last_error.as_deref()),
            (Some(CodexProblem::RelayUnavailable), Some("third"))
        );
    }

    #[test]
    fn a_kept_server_waits_for_no_update_and_shows_whether_the_installed_version_can_replace_it() {
        let kept = |problem| CodexRemoteStatus {
            update: None,
            ..errored_with(problem)
        };
        let mut status = errored_with(CodexProblem::RelayUnavailable);
        status.keep(false);
        assert_eq!(status, kept(CodexProblem::RelayUnavailable));

        status.keep(true);
        assert_eq!(status, kept(CodexProblem::UnsupportedVersion));

        status.keep(false);
        assert_eq!(
            status,
            CodexRemoteStatus {
                problem: None,
                ..kept(CodexProblem::UnsupportedVersion)
            }
        );
    }

    #[test]
    fn a_server_waiting_for_an_update_shows_it_and_no_version_problem() {
        let mut status = errored_with(CodexProblem::UnsupportedVersion);
        let update = status
            .update
            .take()
            .expect("the fixture waits for an update");
        status.wait_for_update(update);
        assert_eq!(
            status,
            CodexRemoteStatus {
                problem: None,
                ..errored_with(CodexProblem::UnsupportedVersion)
            }
        );

        let mut status = errored_with(CodexProblem::RelayUnavailable);
        let update = status
            .update
            .take()
            .expect("the fixture waits for an update");
        status.wait_for_update(update);
        assert_eq!(status, errored_with(CodexProblem::RelayUnavailable));
    }

    #[test]
    fn an_unsure_wake_shows_only_a_server_on_its_way_as_waiting() {
        for state in STATES {
            let before = CodexRemoteStatus {
                state,
                ..errored_with(CodexProblem::MfaRequired)
            };
            let mut status = before.clone();
            status.unsure();
            let expected = match state {
                ServerState::Starting | ServerState::Running | ServerState::Stopping => {
                    CodexRemoteStatus {
                        state: ServerState::Waiting,
                        relay: None,
                        server_name: None,
                        server_version: None,
                        usage: None,
                        update: None,
                        ..before
                    }
                }
                ServerState::Off | ServerState::Waiting | ServerState::Retrying => before,
            };
            assert_eq!(status, expected, "{state:?}");
        }
    }

    fn codex_remote(budget: ServerBudget) -> Arc<CodexRemote> {
        Arc::new(CodexRemote::new(
            Events::default(),
            ServerLog(PathBuf::from("/nonexistent")),
            budget,
            ExpectedPeer::Child,
        ))
    }

    #[tokio::test]
    async fn codex_cannot_start_until_every_sign_in_hold_is_dropped() {
        let codex_remote = codex_remote(ServerBudget::default());
        let (_sign_in, sign_in) = watch::channel(None);
        let mut signals = codex_remote.supervision.signals(sign_in);
        let mut reconsidered = || {
            let changed = signals.changes.has_changed().expect("the sender is alive");
            signals.changes.mark_unchanged();
            changed
        };

        let first = codex_remote.hold_for_sign_in().await;
        assert!(reconsidered());
        let second = codex_remote.hold_for_sign_in().await;
        assert!(reconsidered());
        assert!(!codex_remote.start_unless_held("0.157.1"));
        assert_eq!(codex_remote.status(), CodexRemoteStatus::default());

        drop(first);
        assert!(reconsidered());
        assert!(!codex_remote.start_unless_held("0.157.1"));

        drop(second);
        assert!(reconsidered());
        assert!(codex_remote.start_unless_held("0.157.1"));
        let starting = codex_remote.status();
        assert_eq!(
            (starting.state, starting.server_version.as_deref()),
            (ServerState::Starting, Some("0.157.1"))
        );
    }

    #[tokio::test]
    async fn a_sign_in_hold_waits_until_the_server_has_stopped() {
        let codex_remote = codex_remote(ServerBudget::default());
        codex_remote.status.update(|status| status.start("0.157.1"));
        let holding = tokio::spawn({
            let codex_remote = Arc::clone(&codex_remote);
            async move { codex_remote.hold_for_sign_in().await }
        });

        for state in [
            ServerState::Starting,
            ServerState::Running,
            ServerState::Stopping,
        ] {
            codex_remote.status.update(|status| status.state = state);
            sleep(Duration::from_millis(100)).await;
            assert!(!holding.is_finished(), "{state:?}");
        }
        codex_remote
            .status
            .update(|status| status.idle(ServerState::Waiting, None));
        let hold = timeout(Duration::from_secs(10), holding)
            .await
            .expect("the hold is taken once the server stopped")
            .expect("the hold is taken");
        assert!(codex_remote.is_held());
        drop(hold);
        assert!(!codex_remote.is_held());
    }

    #[tokio::test]
    async fn a_sign_in_hold_waits_for_a_server_that_does_not_stop_only_as_long_as_a_stop_takes() {
        let budget = ServerBudget {
            drain: Duration::ZERO,
            force: Duration::ZERO,
            ..ServerBudget::default()
        };
        let codex_remote = codex_remote(budget);
        codex_remote.status.update(|status| status.start("0.157.1"));
        let started = Instant::now();

        let _hold = timeout(Duration::from_secs(10), codex_remote.hold_for_sign_in())
            .await
            .expect("the hold is taken in time");

        assert!(started.elapsed() >= budget.longest_stop().saturating_add(HOLD_SLACK));
        assert!(codex_remote.is_held());
    }

    #[test]
    fn the_default_stop_fits_in_claudes() {
        let longest = ServerBudget::default().longest_stop();
        assert_eq!(longest, Duration::from_secs(31));
        assert!(longest < RemoteControl::LONGEST_STOP);
    }
}
