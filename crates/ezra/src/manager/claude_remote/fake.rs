use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex as SyncMutex, MutexGuard, PoisonError};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use reqwest::Url;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// How the fake answers one request.
pub struct Reply {
    status: StatusCode,
    body: Value,
    headers: Vec<(HeaderName, HeaderValue)>,
    before: Option<Box<dyn FnOnce() + Send>>,
}

/// A request the fake received.
#[derive(Debug, Clone)]
pub struct SeenRequest {
    pub method: Method,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Value,
}

#[derive(Default)]
struct FakeState {
    replies: HashMap<String, VecDeque<Reply>>,
    seen: Vec<SeenRequest>,
}

/// Claude Code's sessions API on a local port. Each `METHOD /path` gets the replies queued for
/// it in order, then 404.
pub struct FakeSessionsApi {
    pub base: Url,
    state: Arc<SyncMutex<FakeState>>,
    server: JoinHandle<()>,
}

impl Reply {
    pub fn new(status: u16, body: Value) -> Self {
        Self {
            status: StatusCode::from_u16(status).expect("a valid status"),
            body,
            headers: Vec::new(),
            before: None,
        }
    }

    pub fn header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        ));
        self
    }

    /// Runs `action` right before answering.
    pub fn before(mut self, action: impl FnOnce() + Send + 'static) -> Self {
        self.before = Some(Box::new(action));
        self
    }
}

impl SeenRequest {
    pub fn route(&self) -> String {
        format!("{} {}", self.method, self.path)
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }
}

impl FakeSessionsApi {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the fake binds");
        let address = listener.local_addr().expect("the fake has an address");
        let state = Arc::default();
        let router = Router::new()
            .fallback(Self::answer)
            .with_state(Arc::clone(&state));
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("the fake serves");
        });
        Self {
            base: Url::parse(&format!("http://{address}/")).expect("the fake has a URL"),
            state,
            server,
        }
    }

    pub fn reply(&self, route: &str, replies: impl IntoIterator<Item = Reply>) {
        self.state()
            .replies
            .entry(route.to_owned())
            .or_default()
            .extend(replies);
    }

    pub fn seen(&self) -> Vec<SeenRequest> {
        self.state().seen.clone()
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn answer(
        State(state): State<Arc<SyncMutex<FakeState>>>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        let seen = SeenRequest {
            method,
            path: uri.path().to_owned(),
            headers,
            body: serde_json::from_slice(&body).unwrap_or(Value::Null),
        };
        let route = seen.route();
        let reply = {
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            state.seen.push(seen);
            state.replies.get_mut(&route).and_then(VecDeque::pop_front)
        };
        let Some(reply) = reply else {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"type": "error", "error": {"type": "not_found_error", "message": format!("no reply for {route}")}})),
            )
                .into_response();
        };
        if let Some(before) = reply.before {
            before();
        }
        let mut response = (reply.status, Json(reply.body)).into_response();
        response.headers_mut().extend(reply.headers);
        response
    }
}

impl Drop for FakeSessionsApi {
    fn drop(&mut self) {
        self.server.abort();
    }
}
