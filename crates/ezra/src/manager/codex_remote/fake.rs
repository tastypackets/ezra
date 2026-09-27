use std::collections::{HashMap, VecDeque};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::{Arc, Mutex as SyncMutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

/// How the fake answers one request.
#[derive(Debug, Clone)]
pub enum Reply {
    Result(Value),
    Error {
        code: i64,
        message: String,
    },
    /// The result after a pause, while the fake keeps reading.
    Late(Duration, Value),
    Never,
}

/// A frame the fake read or is about to write, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Seen {
    Received(Value),
    Sent(Value),
}

enum Out {
    Frames(Vec<Value>),
    Close,
}

#[derive(Default)]
struct FakeState {
    replies: HashMap<String, VecDeque<Reply>>,
    after: HashMap<String, Vec<Value>>,
    seen: Vec<Seen>,
    connections: Vec<mpsc::UnboundedSender<Out>>,
}

impl FakeState {
    /// The next reply to `method`. The last one scripted repeats.
    fn reply_to(&mut self, method: &str) -> Reply {
        let Some(replies) = self.replies.get_mut(method) else {
            return Reply::Error {
                code: -32601,
                message: format!("the fake has no reply to {method}"),
            };
        };
        if replies.len() > 1 {
            replies.pop_front()
        } else {
            replies.front().cloned()
        }
        .unwrap_or(Reply::Never)
    }

    fn send_all(&mut self, out: impl Fn() -> Out) {
        self.connections
            .retain(|connection| connection.send(out()).is_ok());
    }
}

#[derive(Default)]
struct Shared(SyncMutex<FakeState>);

/// A Codex control socket served in the test process, so its peer is the test process.
pub struct FakeControlServer {
    shared: Arc<Shared>,
    accepting: JoinHandle<()>,
    _socket_folder: TempDir,
}

impl FakeControlServer {
    /// Serves the control socket of `codex_home` from a short path it links to, like Codex, and
    /// answers `initialize` with that home.
    pub fn bind(codex_home: &Path) -> Self {
        let socket_folder = tempfile::Builder::new()
            .tempdir_in("/tmp")
            .expect("the socket folder is created");
        let socket = socket_folder.path().join("control");
        let listener = UnixListener::bind(&socket).expect("the fake socket is bound");
        let link_folder = codex_home.join("app-server-control");
        fs::create_dir_all(&link_folder).expect("the link folder is created");
        symlink(&socket, link_folder.join("app-server-control.sock"))
            .expect("the socket link is created");
        let shared = Arc::new(Shared::default());
        let fake = Self {
            shared: Arc::clone(&shared),
            accepting: tokio::spawn(async move {
                while let Ok((stream, _address)) = listener.accept().await {
                    tokio::spawn(Arc::clone(&shared).serve(stream));
                }
            }),
            _socket_folder: socket_folder,
        };
        fake.reply("initialize", [Reply::Result(Self::initialized(codex_home))]);
        fake
    }

    /// Codex's answer to `initialize` from a server with `codex_home`.
    pub fn initialized(codex_home: &Path) -> Value {
        json!({
            "userAgent": "codex_cli_rs/0.157.1 (Ubuntu 26.4.0; x86_64) unknown",
            "codexHome": codex_home,
            "platformFamily": "unix",
            "platformOs": "linux",
        })
    }

    /// Answers the next `method` requests with `replies` in turn, and every later one with the
    /// last of them.
    pub fn reply(&self, method: &str, replies: impl IntoIterator<Item = Reply>) {
        self.shared
            .state()
            .replies
            .insert(method.to_owned(), replies.into_iter().collect());
    }

    /// Sends `frame` right after the answer to each later `method` request.
    pub fn push_after(&self, method: &str, frame: Value) {
        self.shared
            .state()
            .after
            .entry(method.to_owned())
            .or_default()
            .push(frame);
    }

    /// Sends `frame` on every open connection now.
    pub fn push(&self, frame: Value) {
        self.shared
            .state()
            .send_all(|| Out::Frames(vec![frame.clone()]));
    }

    /// Closes every open connection and keeps taking new ones.
    pub fn disconnect(&self) {
        self.shared.state().send_all(|| Out::Close);
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.shared.state().seen.clone()
    }

    /// Every frame read, in order.
    pub fn received(&self) -> Vec<Value> {
        self.seen()
            .into_iter()
            .filter_map(|seen| match seen {
                Seen::Received(frame) => Some(frame),
                Seen::Sent(_) => None,
            })
            .collect()
    }

    /// The method and params of every request read, in order. Params are null when absent.
    pub fn requests(&self) -> Vec<(String, Value)> {
        self.received()
            .into_iter()
            .filter(|frame| frame.get("id").is_some())
            .filter_map(|frame| {
                Some((
                    frame["method"].as_str()?.to_owned(),
                    frame.get("params").cloned().unwrap_or(Value::Null),
                ))
            })
            .collect()
    }

    /// The requests after the first, which must be `initialize`.
    pub fn requests_after_initialize(&self) -> Vec<(String, Value)> {
        let mut requests = self.requests();
        assert_eq!(
            requests.first().map(|(method, _)| method.as_str()),
            Some("initialize")
        );
        requests.split_off(1)
    }
}

impl Drop for FakeControlServer {
    fn drop(&mut self) {
        self.accepting.abort();
        self.disconnect();
    }
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, FakeState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn serve(self: Arc<Self>, stream: UnixStream) {
        let Ok(websocket) = accept_async(stream).await else {
            return;
        };
        let (mut writer, mut reader) = websocket.split();
        let (outgoing, mut out) = mpsc::unbounded_channel();
        self.state().connections.push(outgoing.clone());
        let shared = Arc::clone(&self);
        let writing = tokio::spawn(async move {
            while let Some(Out::Frames(frames)) = out.recv().await {
                for frame in frames {
                    let text = frame.to_string();
                    shared.state().seen.push(Seen::Sent(frame));
                    if writer.send(Message::text(text)).await.is_err() {
                        return;
                    }
                }
            }
            let _already_closed = writer.close().await;
        });
        while let Some(Ok(message)) = reader.next().await {
            let Message::Text(text) = message else {
                continue;
            };
            let frame: Value = serde_json::from_str(&text).expect("ezra sends JSON");
            self.state().seen.push(Seen::Received(frame.clone()));
            if let (Some(id), Some(method)) = (frame.get("id"), frame["method"].as_str()) {
                self.answer(id, method, &outgoing);
            }
        }
        writing.abort();
    }

    fn answer(&self, id: &Value, method: &str, outgoing: &mpsc::UnboundedSender<Out>) {
        let mut state = self.state();
        let (delay, answer) = match state.reply_to(method) {
            Reply::Result(result) => (Duration::ZERO, json!({"id": id, "result": result})),
            Reply::Late(delay, result) => (delay, json!({"id": id, "result": result})),
            Reply::Error { code, message } => (
                Duration::ZERO,
                json!({"id": id, "error": {"code": code, "message": message}}),
            ),
            Reply::Never => return,
        };
        let frames = Out::Frames(
            [answer]
                .into_iter()
                .chain(state.after.get(method).into_iter().flatten().cloned())
                .collect(),
        );
        drop(state);
        if delay.is_zero() {
            let _closed = outgoing.send(frames);
            return;
        }
        let outgoing = outgoing.clone();
        tokio::spawn(async move {
            sleep(delay).await;
            let _closed = outgoing.send(frames);
        });
    }
}
