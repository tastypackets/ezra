use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex as SyncMutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nix::unistd::Pid;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use tokio_tungstenite::{WebSocketStream, client_async};

use super::ServerBudget;

/// A client name Codex does not take as the originator of its own requests.
const CLIENT_NAME: &str = "codex_app_server_daemon";
const CLIENT_TITLE: &str = "EZ Remote Agent";

/// The notifications ezra reads.
const FOLLOWED_NOTIFICATIONS: [&str; 5] = [
    "remoteControl/status/changed",
    "thread/started",
    "thread/status/changed",
    "thread/closed",
    "account/updated",
];

/// Every other notification Codex 0.157 sends.
const OPTED_OUT_NOTIFICATIONS: [&str; 80] = [
    "error",
    "thread/archived",
    "thread/deleted",
    "thread/unarchived",
    "thread/reverted",
    "skills/changed",
    "thread/name/updated",
    "thread/attachment/updated",
    "thread/goal/updated",
    "thread/goal/cleared",
    "thread/queue/changed",
    "project/changed",
    "thread/project/updated",
    "thread/environment/connected",
    "thread/environment/disconnected",
    "thread/settings/updated",
    "thread/tokenUsage/updated",
    "turn/started",
    "hook/started",
    "turn/completed",
    "hook/completed",
    "turn/diff/updated",
    "turn/plan/updated",
    "item/started",
    "item/autoApprovalReview/started",
    "item/autoApprovalReview/completed",
    "autoApprovalReview/strictReviewRequired",
    "item/completed",
    "rawResponseItem/completed",
    "rawResponse/completed",
    "item/agentMessage/delta",
    "item/plan/delta",
    "command/exec/outputDelta",
    "process/outputDelta",
    "process/exited",
    "item/commandExecution/outputDelta",
    "item/commandExecution/terminalInteraction",
    "item/fileChange/outputDelta",
    "item/fileChange/patchUpdated",
    "serverRequest/resolved",
    "item/mcpToolCall/progress",
    "mcpServer/oauthLogin/completed",
    "mcpServer/startupStatus/updated",
    "mcpServer/event/stream/notification",
    "account/gatewayOAuth/changed",
    "account/rateLimits/updated",
    "app/list/updated",
    "externalAgentConfig/import/progress",
    "externalAgentConfig/import/completed",
    "fs/changed",
    "item/reasoning/summaryTextDelta",
    "item/reasoning/summaryPartAdded",
    "item/reasoning/textDelta",
    "thread/compacted",
    "model/rerouted",
    "model/verification",
    "modelProvider/authRecoveryStarted",
    "modelProvider/authRecoveryCompleted",
    "turn/moderationMetadata",
    "model/safetyBuffering/updated",
    "warning",
    "guardianWarning",
    "deprecationNotice",
    "configWarning",
    "fuzzyFileSearch/sessionUpdated",
    "fuzzyFileSearch/sessionCompleted",
    "thread/realtime/started",
    "thread/realtime/itemAdded",
    "thread/realtime/item/started",
    "thread/realtime/item/transcript/delta",
    "thread/realtime/item/completed",
    "thread/realtime/transcript/delta",
    "thread/realtime/transcript/done",
    "thread/realtime/outputAudio/delta",
    "thread/realtime/sdp",
    "thread/realtime/error",
    "thread/realtime/closed",
    "windows/worldWritableWarning",
    "windowsSandbox/setupCompleted",
    "account/login/completed",
];

/// The socket a Codex server with this Codex home takes control connections on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlSocket(pub PathBuf);

impl ControlSocket {
    pub fn of(codex_home: &Path) -> Self {
        Self(
            codex_home
                .join("app-server-control")
                .join("app-server-control.sock"),
        )
    }

    /// Connects, and names the process that answers when it can.
    pub async fn connect(&self) -> io::Result<(UnixStream, Option<Pid>)> {
        let stream = UnixStream::connect(&self.0).await?;
        let peer = stream.peer_cred()?.pid().map(Pid::from_raw);
        Ok((stream, peer))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("Codex closed the control connection")]
    Closed,
    #[error("Codex did not answer in time")]
    TimedOut,
    #[error("another Codex server, pid {0}, answers on the control socket")]
    ForeignServer(Pid),
    #[error("{message}")]
    Codex { code: i64, message: String },
}

impl From<WebSocketError> for ControlError {
    fn from(error: WebSocketError) -> Self {
        match error {
            WebSocketError::Io(error) => Self::Io(error),
            error => Self::Io(io::Error::other(error)),
        }
    }
}

/// A request ezra sends Codex, and the answer it gets.
pub trait ControlRequest: Serialize {
    const METHOD: &'static str;
    type Response: DeserializeOwned;
}

/// Reads the relay status.
#[derive(Debug, Serialize)]
pub struct StatusRead;

impl ControlRequest for StatusRead {
    const METHOD: &'static str = "remoteControl/status/read";
    type Response = RelayWire;
}

/// Turns the relay on, for this server's lifetime only when `ephemeral`.
#[cfg(test)]
#[derive(Debug, Serialize)]
pub struct Enable {
    pub ephemeral: bool,
}

#[cfg(test)]
impl ControlRequest for Enable {
    const METHOD: &'static str = "remoteControl/enable";
    type Response = RelayWire;
}

/// Turns the relay off, for this server's lifetime only when `ephemeral`.
#[cfg(test)]
#[derive(Debug, Serialize)]
pub struct Disable {
    pub ephemeral: bool,
}

#[cfg(test)]
impl ControlRequest for Disable {
    const METHOD: &'static str = "remoteControl/disable";
    type Response = RelayWire;
}

/// Asks for a code a phone pairs with.
#[cfg(test)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingStart {
    pub manual_code: bool,
}

#[cfg(test)]
impl ControlRequest for PairingStart {
    const METHOD: &'static str = "remoteControl/pairing/start";
    type Response = PairingWire;
}

/// Asks whether a phone claimed a pairing code.
#[cfg(test)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingStatus {
    pub pairing_code: String,
}

#[cfg(test)]
impl ControlRequest for PairingStatus {
    const METHOD: &'static str = "remoteControl/pairing/status";
    type Response = PairingStatusWire;
}

/// Lists one page of the paired phones.
#[cfg(test)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientList {
    pub environment_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[cfg(test)]
impl ControlRequest for ClientList {
    const METHOD: &'static str = "remoteControl/client/list";
    type Response = ClientPageWire;
}

/// Removes a paired phone.
#[cfg(test)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientRevoke {
    pub environment_id: String,
    pub client_id: String,
}

#[cfg(test)]
impl ControlRequest for ClientRevoke {
    const METHOD: &'static str = "remoteControl/client/revoke";
    type Response = IgnoredAny;
}

/// Reads how the server is signed in.
#[cfg(test)]
#[derive(Debug, Serialize)]
pub struct AccountRead {}

#[cfg(test)]
impl ControlRequest for AccountRead {
    const METHOD: &'static str = "account/read";
    type Response = AccountWire;
}

/// Lists one page of the chats the server has loaded.
#[cfg(test)]
#[derive(Debug, Serialize)]
pub struct LoadedThreads {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[cfg(test)]
impl ControlRequest for LoadedThreads {
    const METHOD: &'static str = "thread/loaded/list";
    type Response = ThreadPageWire;
}

/// Reads one chat.
#[cfg(test)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRead {
    pub thread_id: ThreadId,
}

#[cfg(test)]
impl ControlRequest for ThreadRead {
    const METHOD: &'static str = "thread/read";
    type Response = ThreadReadWire;
}

/// Stops this connection following a chat.
#[cfg(test)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadUnsubscribe {
    pub thread_id: ThreadId,
}

#[cfg(test)]
impl ControlRequest for ThreadUnsubscribe {
    const METHOD: &'static str = "thread/unsubscribe";
    type Response = UnsubscribeWire;
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Initialize {
    client_info: ClientInfo,
    capabilities: Capabilities,
}

#[derive(Debug, Serialize)]
struct ClientInfo {
    name: &'static str,
    title: &'static str,
    version: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    experimental_api: bool,
    opt_out_notification_methods: &'static [&'static str],
}

impl Initialize {
    const EZRA: Self = Self {
        client_info: ClientInfo {
            name: CLIENT_NAME,
            title: CLIENT_TITLE,
            version: env!("CARGO_PKG_VERSION"),
        },
        capabilities: Capabilities {
            experimental_api: true,
            opt_out_notification_methods: &OPTED_OUT_NOTIFICATIONS,
        },
    };
}

impl ControlRequest for Initialize {
    const METHOD: &'static str = "initialize";
    type Response = InitializeWire;
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeWire {
    codex_home: PathBuf,
}

/// A Codex chat.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThreadId(pub String);

/// The relay status and the name the ChatGPT app shows.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayWire {
    pub status: RelayStatusWire,
    pub server_name: String,
    pub environment_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelayStatusWire {
    Disabled,
    Connecting,
    Connected,
    Errored,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingWire {
    pub pairing_code: String,
    pub manual_pairing_code: Option<String>,
    pub environment_id: String,
    /// Unix seconds.
    pub expires_at: i64,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct PairingStatusWire {
    pub claimed: bool,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientPageWire {
    pub data: Vec<ClientWire>,
    pub next_cursor: Option<String>,
}

/// A paired phone.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientWire {
    pub client_id: String,
    pub display_name: Option<String>,
    pub device_type: Option<String>,
    pub platform: Option<String>,
    pub os_version: Option<String>,
    pub device_model: Option<String>,
    pub app_version: Option<String>,
    /// Unix seconds.
    pub last_seen_at: Option<i64>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountWire {
    /// None when Codex has no OpenAI account to show.
    pub account: Option<AccountKindWire>,
    /// False when the model provider needs no OpenAI sign-in.
    pub requires_openai_auth: bool,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "type")]
pub enum AccountKindWire {
    #[serde(rename = "chatgpt")]
    ChatGpt,
    #[serde(rename = "apiKey")]
    ApiKey,
    #[serde(rename = "amazonBedrock")]
    AmazonBedrock,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPageWire {
    pub data: Vec<ThreadId>,
    pub next_cursor: Option<String>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ThreadReadWire {
    pub thread: ThreadWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ThreadWire {
    pub id: ThreadId,
    pub status: ThreadStatusWire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadStatusWire {
    NotLoaded,
    Idle,
    SystemError,
    /// Running a turn or waiting for an answer.
    Active,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct UnsubscribeWire {
    pub status: UnsubscribeStatus,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UnsubscribeStatus {
    NotLoaded,
    NotSubscribed,
    Unsubscribed,
}

/// A notification ezra reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ControlNotification {
    #[serde(rename = "remoteControl/status/changed")]
    StatusChanged(RelayWire),
    #[serde(rename = "thread/started")]
    ThreadStarted { thread: ThreadWire },
    #[serde(rename = "thread/status/changed", rename_all = "camelCase")]
    ThreadStatusChanged {
        thread_id: ThreadId,
        status: ThreadStatusWire,
    },
    #[serde(rename = "thread/closed", rename_all = "camelCase")]
    ThreadClosed { thread_id: ThreadId },
    #[serde(rename = "account/updated")]
    AccountUpdated {},
}

/// What the control connection tells the supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEvent {
    Notification(ControlNotification),
    /// Codex asks this connection something, such as an approval. ezra never answers.
    ServerRequest {
        method: String,
        thread_id: Option<ThreadId>,
    },
    /// The connection ended, and every request still waiting failed with `Closed`.
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct CodexErrorWire {
    code: i64,
    message: String,
}

impl From<CodexErrorWire> for ControlError {
    fn from(error: CodexErrorWire) -> Self {
        Self::Codex {
            code: error.code,
            message: error.message,
        }
    }
}

/// A frame from Codex, told apart by its fields in the order Codex does.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Incoming {
    Request {
        #[serde(rename = "id")]
        _id: IgnoredAny,
        method: String,
        #[serde(default)]
        params: ServerRequestParams,
    },
    Notification(ControlNotification),
    OtherNotification {
        method: String,
    },
    Response {
        id: i64,
        result: Value,
    },
    Error {
        id: i64,
        error: CodexErrorWire,
    },
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ServerRequestParams {
    thread_id: Option<ThreadId>,
}

#[derive(Debug, Serialize)]
struct Outgoing<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<i64>,
    method: &'a str,
    #[serde(skip_serializing_if = "Value::is_null")]
    params: Value,
}

type Reply = Result<Value, CodexErrorWire>;

/// The requests still waiting for Codex's answer.
#[derive(Debug, Default)]
struct PendingReplies(SyncMutex<Pending>);

#[derive(Debug, Default)]
struct Pending {
    waiting: HashMap<i64, oneshot::Sender<Reply>>,
    closed: bool,
}

impl PendingReplies {
    fn lock(&self) -> MutexGuard<'_, Pending> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Where the answer to `id` arrives. None once the connection ended.
    fn wait_for(&self, id: i64) -> Option<oneshot::Receiver<Reply>> {
        let mut pending = self.lock();
        if pending.closed {
            return None;
        }
        let (sender, receiver) = oneshot::channel();
        pending.waiting.insert(id, sender);
        Some(receiver)
    }

    fn answer(&self, id: i64, reply: Reply) {
        if let Some(waiting) = self.lock().waiting.remove(&id) {
            let _gave_up = waiting.send(reply);
        }
    }

    fn forget(&self, id: i64) {
        self.lock().waiting.remove(&id);
    }

    /// Fails every waiting request and every later one.
    fn close(&self) {
        let mut pending = self.lock();
        pending.closed = true;
        pending.waiting.clear();
    }
}

/// Reads and writes one control connection until either side ends it.
struct Connection {
    websocket: WebSocketStream<UnixStream>,
    outgoing: mpsc::UnboundedReceiver<String>,
    replies: Arc<PendingReplies>,
    events: mpsc::UnboundedSender<ControlEvent>,
}

impl Connection {
    async fn run(mut self) {
        loop {
            tokio::select! {
                text = self.outgoing.recv() => {
                    let Some(text) = text else {
                        let _already_closed = self.websocket.close(None).await;
                        break;
                    };
                    if self.websocket.send(Message::text(text)).await.is_err() {
                        break;
                    }
                }
                message = self.websocket.next() => match message {
                    Some(Ok(Message::Text(text))) => self.read(&text),
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
            }
        }
        self.replies.close();
        let _supervisor_gone = self.events.send(ControlEvent::Closed);
    }

    fn read(&self, text: &str) {
        let event = match serde_json::from_str::<Incoming>(text) {
            Ok(Incoming::Request { method, params, .. }) => ControlEvent::ServerRequest {
                method,
                thread_id: params.thread_id,
            },
            Ok(Incoming::Notification(notification)) => ControlEvent::Notification(notification),
            Ok(Incoming::OtherNotification { method }) => {
                if FOLLOWED_NOTIFICATIONS.contains(&method.as_str()) {
                    tracing::warn!("could not read Codex's {method} notification");
                }
                return;
            }
            Ok(Incoming::Response { id, result }) => {
                self.replies.answer(id, Ok(result));
                return;
            }
            Ok(Incoming::Error { id, error }) => {
                self.replies.answer(id, Err(error));
                return;
            }
            Err(error) => {
                tracing::warn!("could not read a message from Codex: {error}");
                return;
            }
        };
        let _supervisor_gone = self.events.send(event);
    }
}

/// A control connection to one Codex server.
#[derive(Debug)]
pub struct ControlClient {
    outgoing: mpsc::UnboundedSender<String>,
    replies: Arc<PendingReplies>,
    next_id: AtomicI64,
    answer_within: Duration,
}

impl ControlClient {
    /// Connects to the server `expected` with the Codex home `codex_home`, and introduces ezra.
    /// Events arrive from the first frame on, before this returns.
    pub async fn connect(
        socket: &ControlSocket,
        expected: Pid,
        codex_home: &Path,
        budget: &ServerBudget,
    ) -> Result<(Self, mpsc::UnboundedReceiver<ControlEvent>), ControlError> {
        let (stream, peer) = socket.connect().await?;
        let peer =
            peer.ok_or_else(|| io::Error::other("the control socket names no peer process"))?;
        if peer != expected {
            return Err(ControlError::ForeignServer(peer));
        }
        let (websocket, _response) =
            timeout(budget.request, client_async("ws://localhost/", stream))
                .await
                .map_err(|_| ControlError::TimedOut)??;
        let (events_sender, events) = mpsc::unbounded_channel();
        let (outgoing_sender, outgoing) = mpsc::unbounded_channel();
        let replies = Arc::new(PendingReplies::default());
        tokio::spawn(
            Connection {
                websocket,
                outgoing,
                replies: Arc::clone(&replies),
                events: events_sender,
            }
            .run(),
        );
        let client = Self {
            outgoing: outgoing_sender,
            replies,
            next_id: AtomicI64::new(1),
            answer_within: budget.request,
        };
        let answer = client.request(Initialize::EZRA).await?;
        let ours = tokio::fs::canonicalize(codex_home).await?;
        let theirs = tokio::fs::canonicalize(&answer.codex_home).await.ok();
        if theirs.as_ref() != Some(&ours) {
            return Err(ControlError::ForeignServer(peer));
        }
        client.send(&Outgoing {
            id: None,
            method: "initialized",
            params: Value::Null,
        })?;
        Ok((client, events))
    }

    pub async fn request<R: ControlRequest>(
        &self,
        request: R,
    ) -> Result<R::Response, ControlError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let reply = self.replies.wait_for(id).ok_or(ControlError::Closed)?;
        let sent = self.send(&Outgoing {
            id: Some(id),
            method: R::METHOD,
            params: serde_json::to_value(request).expect("a request is JSON"),
        });
        if let Err(error) = sent {
            self.replies.forget(id);
            return Err(error);
        }
        let Ok(reply) = timeout(self.answer_within, reply).await else {
            self.replies.forget(id);
            return Err(ControlError::TimedOut);
        };
        let result = reply.map_err(|_ended| ControlError::Closed)??;
        Ok(serde_json::from_value(result).map_err(io::Error::from)?)
    }

    fn send(&self, frame: &Outgoing<'_>) -> Result<(), ControlError> {
        let text = serde_json::to_string(frame).expect("a frame is JSON");
        self.outgoing
            .send(text)
            .map_err(|_ended| ControlError::Closed)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::os::unix::fs::symlink;

    use serde_json::json;
    use tempfile::TempDir;
    use tokio::net::UnixListener;
    use tokio::time::Instant;

    use super::*;
    use crate::manager::api::test_support::wait_until;
    use crate::manager::codex_remote::fake::{ENVIRONMENT, FakeControlServer, Reply, Seen};

    const WAIT: Duration = Duration::from_secs(10);
    const BUDGET: ServerBudget = ServerBudget {
        probe: Duration::ZERO,
        readiness: Duration::ZERO,
        drain: Duration::ZERO,
        force: Duration::ZERO,
        request: Duration::from_secs(5),
    };
    const ENABLE_FIRST: &str = "remote control pairing requires remote control to be enabled";

    type Events = mpsc::UnboundedReceiver<ControlEvent>;

    trait EventsExt {
        async fn next_event(&mut self) -> ControlEvent;
    }

    impl EventsExt for Events {
        async fn next_event(&mut self) -> ControlEvent {
            timeout(WAIT, self.recv())
                .await
                .expect("an event arrives")
                .expect("the connection is open")
        }
    }

    /// A Codex home short enough for its control socket path.
    fn home() -> TempDir {
        tempfile::Builder::new()
            .tempdir_in("/tmp")
            .expect("a Codex home is created")
    }

    /// A Codex home and a fake server on its control socket.
    fn served() -> (TempDir, FakeControlServer) {
        let home = home();
        let fake = FakeControlServer::bind(home.path());
        (home, fake)
    }

    async fn connected() -> (TempDir, FakeControlServer, ControlClient, Events) {
        let (home, fake) = served();
        let (client, events) = ControlClient::connect(
            &ControlSocket::of(home.path()),
            Pid::this(),
            home.path(),
            &BUDGET,
        )
        .await
        .expect("the fake answers");
        (home, fake, client, events)
    }

    fn relay(status: RelayStatusWire) -> RelayWire {
        RelayWire {
            status,
            server_name: "ezra-dev".to_owned(),
            environment_id: Some(ENVIRONMENT.to_owned()),
        }
    }

    fn thread_params(id: &str, status: Value) -> Value {
        json!({
            "id": id,
            "sessionId": id,
            "forkedFromId": null,
            "parentThreadId": null,
            "preview": "",
            "ephemeral": false,
            "modelProvider": "openai",
            "createdAt": 1_790_000_000,
            "updatedAt": 1_790_000_000,
            "status": status,
            "cwd": "/projects",
            "cliVersion": "0.157.1",
        })
    }

    fn thread() -> ThreadId {
        ThreadId("thread-1".to_owned())
    }

    fn requested(method: &str, params: Value) -> (String, Value) {
        (method.to_owned(), params)
    }

    #[tokio::test]
    async fn the_handshake_asks_for_the_experimental_api_without_other_notifications() {
        let (home, fake) = served();
        fake.reply(
            "initialize",
            [Reply::Late(
                Duration::from_millis(200),
                FakeControlServer::initialized(home.path()),
            )],
        );

        let _connected = ControlClient::connect(
            &ControlSocket::of(home.path()),
            Pid::this(),
            home.path(),
            &BUDGET,
        )
        .await
        .expect("the fake answers");

        let seen = wait_until(WAIT, || fake.seen(), |seen| seen.len() >= 3).await;
        let [
            Seen::Received(initialize),
            Seen::Sent(answer),
            Seen::Received(initialized),
        ] = seen.as_slice()
        else {
            panic!("{seen:?}");
        };
        assert_eq!(initialized, &json!({"method": "initialized"}));
        assert_eq!(answer["id"], initialize["id"]);
        let keys: HashSet<&str> = initialize
            .as_object()
            .expect("a request is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, HashSet::from(["id", "method", "params"]));
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(
            initialize["params"]["clientInfo"],
            json!({
                "name": "codex_app_server_daemon",
                "title": "EZ Remote Agent",
                "version": env!("CARGO_PKG_VERSION"),
            })
        );
        let capabilities = &initialize["params"]["capabilities"];
        assert_eq!(capabilities["experimentalApi"], true);
        let opted_out: HashSet<&str> = capabilities["optOutNotificationMethods"]
            .as_array()
            .expect("the opt-outs are a list")
            .iter()
            .map(|method| method.as_str().expect("a method is text"))
            .collect();
        assert_eq!(opted_out.len(), 80);
        for followed in FOLLOWED_NOTIFICATIONS {
            assert!(!opted_out.contains(followed), "{followed}");
        }
    }

    #[tokio::test]
    async fn status_pushes_before_initialized_are_delivered() {
        let (home, fake) = served();
        fake.push_after(
            "initialize",
            FakeControlServer::status_changed("connecting"),
        );
        fake.push_after("initialize", FakeControlServer::status_changed("connected"));

        let (_client, mut events) = ControlClient::connect(
            &ControlSocket::of(home.path()),
            Pid::this(),
            home.path(),
            &BUDGET,
        )
        .await
        .expect("the fake answers");

        for status in [RelayStatusWire::Connecting, RelayStatusWire::Connected] {
            assert_eq!(
                events.next_event().await,
                ControlEvent::Notification(ControlNotification::StatusChanged(relay(status)))
            );
        }
    }

    #[tokio::test]
    async fn a_server_that_is_not_the_expected_process_is_foreign() {
        let (home, fake) = served();

        let error = ControlClient::connect(
            &ControlSocket::of(home.path()),
            Pid::parent(),
            home.path(),
            &BUDGET,
        )
        .await
        .expect_err("the peer is the test process");

        assert!(
            matches!(error, ControlError::ForeignServer(peer) if peer == Pid::this()),
            "{error:?}"
        );
        assert!(fake.seen().is_empty());
    }

    #[tokio::test]
    async fn a_server_with_another_codex_home_is_foreign() {
        let (home, fake) = served();
        let other = tempfile::tempdir().expect("another Codex home is created");

        for answered in [other.path(), &other.path().join("missing")] {
            fake.reply(
                "initialize",
                [Reply::Result(FakeControlServer::initialized(answered))],
            );
            let error = ControlClient::connect(
                &ControlSocket::of(home.path()),
                Pid::this(),
                home.path(),
                &BUDGET,
            )
            .await
            .expect_err("the homes differ");
            assert!(
                matches!(error, ControlError::ForeignServer(peer) if peer == Pid::this()),
                "{error:?}"
            );
        }

        fake.reply(
            "initialize",
            [Reply::Result(FakeControlServer::initialized(home.path()))],
        );
        let _connected = ControlClient::connect(
            &ControlSocket::of(home.path()),
            Pid::this(),
            home.path(),
            &BUDGET,
        )
        .await
        .expect("the homes match");
        let initialized = || {
            fake.received()
                .iter()
                .filter(|frame| frame["method"] == "initialized")
                .count()
        };
        wait_until(WAIT, initialized, |count| *count > 0).await;
        assert_eq!(initialized(), 1);
    }

    #[tokio::test]
    async fn the_same_home_matches_with_a_trailing_slash_or_through_a_link() {
        let (home, fake) = served();
        let with_slash = PathBuf::from(format!("{}/", home.path().display()));
        let links = tempfile::tempdir().expect("a folder for the link is created");
        let link = links.path().join("codex");
        symlink(home.path(), &link).expect("the link is created");

        for (answered, ours) in [
            (with_slash.as_path(), home.path()),
            (home.path(), with_slash.as_path()),
            (link.as_path(), home.path()),
        ] {
            fake.reply(
                "initialize",
                [Reply::Result(FakeControlServer::initialized(answered))],
            );
            ControlClient::connect(&ControlSocket::of(ours), Pid::this(), ours, &BUDGET)
                .await
                .expect("the homes match");
        }
    }

    #[tokio::test]
    async fn relay_and_account_requests_send_codex_params() {
        let (_home, fake, client, _events) = connected().await;
        fake.reply(
            "remoteControl/status/read",
            [Reply::Result(FakeControlServer::relay("errored"))],
        );
        fake.reply(
            "remoteControl/enable",
            [Reply::Result(FakeControlServer::relay("connecting"))],
        );
        fake.reply(
            "remoteControl/disable",
            [Reply::Result(FakeControlServer::relay("disabled"))],
        );
        let accounts = [
            (
                json!({"type": "chatgpt", "email": "dev@example.com", "planType": "plus"}),
                true,
                Some(AccountKindWire::ChatGpt),
            ),
            (
                json!({"type": "apiKey"}),
                true,
                Some(AccountKindWire::ApiKey),
            ),
            (
                json!({"type": "amazonBedrock", "usesCodexManagedCredentials": false}),
                false,
                Some(AccountKindWire::AmazonBedrock),
            ),
            (Value::Null, true, None),
            (Value::Null, false, None),
        ];
        fake.reply(
            "account/read",
            accounts.clone().map(|(account, requires_openai_auth, _)| {
                Reply::Result(json!({
                    "account": account,
                    "requiresOpenaiAuth": requires_openai_auth,
                }))
            }),
        );

        assert_eq!(
            client.request(StatusRead).await.expect("the fake answers"),
            relay(RelayStatusWire::Errored)
        );
        assert_eq!(
            client
                .request(Enable { ephemeral: true })
                .await
                .expect("the fake answers"),
            relay(RelayStatusWire::Connecting)
        );
        assert_eq!(
            client
                .request(Disable { ephemeral: true })
                .await
                .expect("the fake answers"),
            relay(RelayStatusWire::Disabled)
        );
        for (_, requires_openai_auth, account) in accounts {
            assert_eq!(
                client
                    .request(AccountRead {})
                    .await
                    .expect("the fake answers"),
                AccountWire {
                    account,
                    requires_openai_auth,
                }
            );
        }

        assert_eq!(
            fake.requests_after_initialize()[..4],
            [
                requested("remoteControl/status/read", Value::Null),
                requested("remoteControl/enable", json!({"ephemeral": true})),
                requested("remoteControl/disable", json!({"ephemeral": true})),
                requested("account/read", json!({})),
            ]
        );
        let status_read = fake
            .received()
            .into_iter()
            .find(|frame| frame["method"] == "remoteControl/status/read")
            .expect("the status was read");
        assert!(status_read.get("params").is_none(), "{status_read}");
    }

    #[tokio::test]
    async fn pairing_asks_for_a_manual_code_and_checks_the_pairing_code() {
        let (_home, fake, client, _events) = connected().await;
        fake.reply(
            "remoteControl/pairing/start",
            [Reply::Result(json!({
                "pairingCode": "pairing-1",
                "manualPairingCode": "ABCD-2345",
                "environmentId": ENVIRONMENT,
                "expiresAt": 1_790_000_600,
            }))],
        );
        fake.reply(
            "remoteControl/pairing/status",
            [
                Reply::Result(json!({"claimed": false})),
                Reply::Result(json!({"claimed": true})),
            ],
        );

        let pairing = client
            .request(PairingStart { manual_code: true })
            .await
            .expect("the fake answers");
        assert_eq!(
            pairing,
            PairingWire {
                pairing_code: "pairing-1".to_owned(),
                manual_pairing_code: Some("ABCD-2345".to_owned()),
                environment_id: ENVIRONMENT.to_owned(),
                expires_at: 1_790_000_600,
            }
        );
        for claimed in [false, true] {
            let status = client
                .request(PairingStatus {
                    pairing_code: pairing.pairing_code.clone(),
                })
                .await
                .expect("the fake answers");
            assert_eq!(status, PairingStatusWire { claimed });
        }

        assert_eq!(
            fake.requests_after_initialize(),
            [
                requested("remoteControl/pairing/start", json!({"manualCode": true})),
                requested(
                    "remoteControl/pairing/status",
                    json!({"pairingCode": "pairing-1"})
                ),
                requested(
                    "remoteControl/pairing/status",
                    json!({"pairingCode": "pairing-1"})
                ),
            ]
        );
    }

    #[tokio::test]
    async fn phones_are_listed_page_by_page_and_removed_by_environment() {
        let (_home, fake, client, _events) = connected().await;
        fake.reply(
            "remoteControl/client/list",
            [
                Reply::Result(json!({
                    "data": [{
                        "clientId": "phone-1",
                        "displayName": "Phone",
                        "deviceType": "phone",
                        "platform": "ios",
                        "osVersion": "26.0",
                        "deviceModel": "iPhone",
                        "appVersion": "1.0",
                        "lastSeenAt": 1_790_000_000,
                    }],
                    "nextCursor": "page-2",
                })),
                Reply::Result(json!({
                    "data": [{
                        "clientId": "phone-2",
                        "displayName": null,
                        "deviceType": null,
                        "platform": null,
                        "osVersion": null,
                        "deviceModel": null,
                        "appVersion": null,
                        "lastSeenAt": null,
                    }],
                    "nextCursor": null,
                })),
            ],
        );
        fake.reply("remoteControl/client/revoke", [Reply::Result(json!({}))]);

        let first = client
            .request(ClientList {
                environment_id: ENVIRONMENT.to_owned(),
                cursor: None,
            })
            .await
            .expect("the fake answers");
        assert_eq!(
            first,
            ClientPageWire {
                data: vec![ClientWire {
                    client_id: "phone-1".to_owned(),
                    display_name: Some("Phone".to_owned()),
                    device_type: Some("phone".to_owned()),
                    platform: Some("ios".to_owned()),
                    os_version: Some("26.0".to_owned()),
                    device_model: Some("iPhone".to_owned()),
                    app_version: Some("1.0".to_owned()),
                    last_seen_at: Some(1_790_000_000),
                }],
                next_cursor: Some("page-2".to_owned()),
            }
        );
        let second = client
            .request(ClientList {
                environment_id: ENVIRONMENT.to_owned(),
                cursor: first.next_cursor,
            })
            .await
            .expect("the fake answers");
        assert_eq!(
            second,
            ClientPageWire {
                data: vec![ClientWire {
                    client_id: "phone-2".to_owned(),
                    display_name: None,
                    device_type: None,
                    platform: None,
                    os_version: None,
                    device_model: None,
                    app_version: None,
                    last_seen_at: None,
                }],
                next_cursor: None,
            }
        );
        client
            .request(ClientRevoke {
                environment_id: ENVIRONMENT.to_owned(),
                client_id: "phone-1".to_owned(),
            })
            .await
            .expect("the fake answers");

        assert_eq!(
            fake.requests_after_initialize(),
            [
                requested(
                    "remoteControl/client/list",
                    json!({"environmentId": ENVIRONMENT})
                ),
                requested(
                    "remoteControl/client/list",
                    json!({"environmentId": ENVIRONMENT, "cursor": "page-2"})
                ),
                requested(
                    "remoteControl/client/revoke",
                    json!({"environmentId": ENVIRONMENT, "clientId": "phone-1"})
                ),
            ]
        );
    }

    #[tokio::test]
    async fn chats_are_listed_read_and_unsubscribed_with_each_status() {
        let (_home, fake, client, _events) = connected().await;
        fake.reply(
            "thread/loaded/list",
            [Reply::Result(
                json!({"data": ["thread-1", "thread-2"], "nextCursor": null}),
            )],
        );
        fake.reply(
            "thread/read",
            [Reply::Result(json!({"thread": thread_params(
                "thread-1",
                json!({"type": "active", "activeFlags": ["waitingOnApproval"]}),
            )}))],
        );
        fake.reply(
            "thread/unsubscribe",
            ["notLoaded", "notSubscribed", "unsubscribed"]
                .map(|status| Reply::Result(json!({"status": status}))),
        );

        assert_eq!(
            client
                .request(LoadedThreads { cursor: None })
                .await
                .expect("the fake answers"),
            ThreadPageWire {
                data: vec![thread(), ThreadId("thread-2".to_owned())],
                next_cursor: None,
            }
        );
        assert_eq!(
            client
                .request(ThreadRead {
                    thread_id: thread()
                })
                .await
                .expect("the fake answers"),
            ThreadReadWire {
                thread: ThreadWire {
                    id: thread(),
                    status: ThreadStatusWire::Active,
                },
            }
        );
        for status in [
            UnsubscribeStatus::NotLoaded,
            UnsubscribeStatus::NotSubscribed,
            UnsubscribeStatus::Unsubscribed,
        ] {
            assert_eq!(
                client
                    .request(ThreadUnsubscribe {
                        thread_id: thread()
                    })
                    .await
                    .expect("the fake answers"),
                UnsubscribeWire { status }
            );
        }

        let unsubscribe = requested("thread/unsubscribe", json!({"threadId": "thread-1"}));
        assert_eq!(
            fake.requests_after_initialize(),
            [
                requested("thread/loaded/list", json!({})),
                requested("thread/read", json!({"threadId": "thread-1"})),
                unsubscribe.clone(),
                unsubscribe.clone(),
                unsubscribe,
            ]
        );
    }

    #[tokio::test]
    async fn a_codex_error_keeps_its_message() {
        let (_home, fake, client, _events) = connected().await;
        fake.reply(
            "remoteControl/pairing/start",
            [Reply::Error {
                code: -32600,
                message: ENABLE_FIRST.to_owned(),
            }],
        );

        let error = client
            .request(PairingStart { manual_code: true })
            .await
            .expect_err("the fake refuses");

        let ControlError::Codex { code, message } = &error else {
            panic!("{error:?}");
        };
        assert_eq!((*code, message.as_str()), (-32600, ENABLE_FIRST));
        assert_eq!(error.to_string(), ENABLE_FIRST);
    }

    #[tokio::test]
    async fn server_requests_reach_the_supervisor_and_are_never_answered() {
        let (_home, fake, client, mut events) = connected().await;
        fake.reply(
            "remoteControl/status/read",
            [Reply::Result(FakeControlServer::relay("connected"))],
        );

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
        fake.push(json!({
            "id": 1,
            "method": "account/chatgptAuthTokens/refresh",
            "params": {"reason": "unauthorized", "previousAccountId": null},
        }));

        assert_eq!(
            events.next_event().await,
            ControlEvent::ServerRequest {
                method: "item/commandExecution/requestApproval".to_owned(),
                thread_id: Some(thread()),
            }
        );
        assert_eq!(
            events.next_event().await,
            ControlEvent::ServerRequest {
                method: "account/chatgptAuthTokens/refresh".to_owned(),
                thread_id: None,
            }
        );
        client.request(StatusRead).await.expect("the fake answers");
        let methods: Vec<Value> = fake
            .received()
            .into_iter()
            .map(|frame| frame["method"].clone())
            .collect();
        assert_eq!(
            methods,
            ["initialize", "initialized", "remoteControl/status/read"]
        );
    }

    #[tokio::test]
    async fn every_followed_notification_is_read_and_others_are_ignored() {
        let (_home, fake, _client, mut events) = connected().await;
        let followed = [
            (
                "remoteControl/status/changed",
                FakeControlServer::relay("errored"),
                ControlNotification::StatusChanged(relay(RelayStatusWire::Errored)),
            ),
            (
                "thread/started",
                json!({"thread": thread_params("thread-1", json!({"type": "idle"}))}),
                ControlNotification::ThreadStarted {
                    thread: ThreadWire {
                        id: thread(),
                        status: ThreadStatusWire::Idle,
                    },
                },
            ),
            (
                "thread/status/changed",
                json!({
                    "threadId": "thread-1",
                    "status": {"type": "active", "activeFlags": ["waitingOnApproval"]},
                }),
                ControlNotification::ThreadStatusChanged {
                    thread_id: thread(),
                    status: ThreadStatusWire::Active,
                },
            ),
            (
                "thread/closed",
                json!({"threadId": "thread-1"}),
                ControlNotification::ThreadClosed {
                    thread_id: thread(),
                },
            ),
            (
                "account/updated",
                json!({"authMode": "chatgpt", "planType": "plus"}),
                ControlNotification::AccountUpdated {},
            ),
        ];
        assert_eq!(
            followed
                .iter()
                .map(|(method, ..)| *method)
                .collect::<Vec<_>>(),
            FOLLOWED_NOTIFICATIONS
        );

        for (method, params, notification) in followed {
            fake.push(json!({
                "method": "item/agentMessage/delta",
                "params": {
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "itemId": "item-1",
                    "delta": "Done",
                },
                "emittedAtMs": 1_790_000_000_000_i64,
            }));
            fake.push(json!({"method": "codex/notYetKnown", "params": [1, 2]}));
            fake.push(
                json!({"method": method, "params": params, "emittedAtMs": 1_790_000_000_000_i64}),
            );
            assert_eq!(
                events.next_event().await,
                ControlEvent::Notification(notification)
            );
        }
    }

    #[tokio::test]
    async fn answers_reach_the_request_with_their_id_in_any_order() {
        let (_home, fake, client, _events) = connected().await;
        let thread_answer = json!({"thread": thread_params("thread-1", json!({"type": "idle"}))});
        fake.reply(
            "thread/read",
            [Reply::Late(
                Duration::from_millis(300),
                thread_answer.clone(),
            )],
        );
        fake.reply(
            "remoteControl/status/read",
            [Reply::Result(FakeControlServer::relay("connected"))],
        );

        let (read, status) = tokio::join!(
            client.request(ThreadRead {
                thread_id: thread()
            }),
            client.request(StatusRead),
        );

        assert_eq!(
            read.expect("the fake answers").thread.status,
            ThreadStatusWire::Idle
        );
        assert_eq!(
            status.expect("the fake answers"),
            relay(RelayStatusWire::Connected)
        );
        assert_eq!(
            fake.seen().split_off(3),
            [
                Seen::Received(
                    json!({"id": 2, "method": "thread/read", "params": {"threadId": "thread-1"}})
                ),
                Seen::Received(json!({"id": 3, "method": "remoteControl/status/read"})),
                Seen::Sent(json!({"id": 3, "result": FakeControlServer::relay("connected")})),
                Seen::Sent(json!({"id": 2, "result": thread_answer})),
            ]
        );
    }

    #[tokio::test]
    async fn a_request_times_out_after_the_budget_and_its_late_answer_is_dropped() {
        let (home, fake) = served();
        let budget = ServerBudget {
            request: Duration::from_secs(1),
            ..BUDGET
        };
        let (client, _events) = ControlClient::connect(
            &ControlSocket::of(home.path()),
            Pid::this(),
            home.path(),
            &budget,
        )
        .await
        .expect("the fake answers");
        fake.reply(
            "remoteControl/status/read",
            [
                Reply::Late(
                    Duration::from_millis(1300),
                    FakeControlServer::relay("errored"),
                ),
                Reply::Late(
                    Duration::from_millis(600),
                    FakeControlServer::relay("connected"),
                ),
            ],
        );
        let started = Instant::now();

        let error = client
            .request(StatusRead)
            .await
            .expect_err("the answer comes too late");

        assert!(matches!(error, ControlError::TimedOut), "{error:?}");
        let waited = started.elapsed();
        assert!((budget.request..WAIT).contains(&waited), "{waited:?}");
        assert_eq!(
            client.request(StatusRead).await.expect("the fake answers"),
            relay(RelayStatusWire::Connected)
        );
        assert_eq!(
            fake.seen().split_off(3),
            [
                Seen::Received(json!({"id": 2, "method": "remoteControl/status/read"})),
                Seen::Received(json!({"id": 3, "method": "remoteControl/status/read"})),
                Seen::Sent(json!({"id": 2, "result": FakeControlServer::relay("errored")})),
                Seen::Sent(json!({"id": 3, "result": FakeControlServer::relay("connected")})),
            ]
        );
    }

    #[tokio::test]
    async fn a_peer_that_never_upgrades_times_out_after_the_budget() {
        let home = home();
        let socket = ControlSocket::of(home.path());
        fs::create_dir_all(socket.0.parent().expect("the socket has a folder"))
            .expect("the socket folder is created");
        let listener = UnixListener::bind(&socket.0).expect("the socket is bound");
        let budget = ServerBudget {
            request: Duration::from_millis(300),
            ..BUDGET
        };
        let started = Instant::now();

        let (connected, accepted) = tokio::join!(
            timeout(
                WAIT,
                ControlClient::connect(&socket, Pid::this(), home.path(), &budget)
            ),
            listener.accept(),
        );

        let waited = started.elapsed();
        let _silent = accepted.expect("ezra connects");
        let error = connected
            .expect("ezra gives up on its own")
            .expect_err("the peer never upgrades");
        assert!(matches!(error, ControlError::TimedOut), "{error:?}");
        assert!((budget.request..WAIT).contains(&waited), "{waited:?}");
    }

    #[tokio::test]
    async fn a_closed_connection_fails_waiting_and_later_requests() {
        let (_home, fake, client, mut events) = connected().await;
        fake.reply("remoteControl/status/read", [Reply::Never]);

        let (waiting, ()) = tokio::join!(client.request(StatusRead), async {
            wait_until(WAIT, || fake.requests().len(), |count| *count == 2).await;
            fake.disconnect();
        });

        assert!(matches!(waiting, Err(ControlError::Closed)), "{waiting:?}");
        assert_eq!(events.next_event().await, ControlEvent::Closed);
        let later = client.request(StatusRead).await;
        assert!(matches!(later, Err(ControlError::Closed)), "{later:?}");
    }
}
