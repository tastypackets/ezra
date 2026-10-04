//! Claude Code's sessions API. It is not public, so every call to it is in this file.

use std::error::Error as _;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::redirect::Policy;
use reqwest::{RequestBuilder, StatusCode, Url};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::OnceCell;

use super::credentials::AccessToken;

pub const API_BASE: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const SESSIONS_PATH: &str = "v1/code/sessions";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CREATE_TIMEOUT: Duration = Duration::from_secs(30);
const RECORD_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_LIMIT: usize = 1024 * 1024;
const MESSAGE_LIMIT: usize = 300;

/// A session id in a form the CLI's own check accepts: `cse_` or `session_`, then letters, digits,
/// `_` or `-`. Only such ids go into a URL path or a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionId(String);

/// A new session in a Remote Control environment, shaped like the one the CLI creates.
#[derive(Debug, Serialize)]
pub struct NewSession<'a> {
    title: &'a str,
    environment_id: &'a str,
    events: [Value; 0],
    config: NewSessionConfig<'a>,
}

#[derive(Debug, Serialize)]
struct NewSessionConfig<'a> {
    sources: [Value; 0],
    outcomes: [Value; 0],
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort_level: Option<&'a str>,
}

/// The fields of a session that ezra reads. Others are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: Option<String>,
    pub title: Option<String>,
    pub environment_id: Option<String>,
    status: Option<String>,
    session_status: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Claude did not accept Claude Code's sign-in")]
    Unauthorized,
    #[error("Claude answered {status}: {message}")]
    Refused { status: u16, message: String },
    #[error("could not reach Claude: {0}")]
    Unreachable(String),
    #[error("unexpected answer from Claude: {0}")]
    Unexpected(&'static str),
}

pub struct SessionsApi {
    base: Url,
    http: OnceCell<reqwest::Client>,
}

impl SessionId {
    const PREFIXES: [&str; 2] = ["cse_", "session_"];
    const MAX_BYTES: usize = 256;

    pub fn parse(id: &str) -> Option<Self> {
        let suffix = Self::PREFIXES
            .iter()
            .find_map(|prefix| id.strip_prefix(prefix))?;
        let valid = !suffix.is_empty()
            && id.len() <= Self::MAX_BYTES
            && suffix.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            });
        valid.then(|| Self(id.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// What the `cse_` and `session_` forms of one session share.
    pub fn suffix(&self) -> &str {
        Self::PREFIXES
            .iter()
            .find_map(|prefix| self.0.strip_prefix(prefix))
            .unwrap_or(&self.0)
    }

    pub fn is_same_session(&self, other: &str) -> bool {
        Self::parse(other).is_some_and(|other| other.suffix() == self.suffix())
    }
}

impl<'a> NewSession<'a> {
    pub fn new(
        title: &'a str,
        environment_id: &'a str,
        model: Option<&'a str>,
        effort: Option<&'a str>,
    ) -> Self {
        Self {
            title,
            environment_id,
            events: [],
            config: NewSessionConfig {
                sources: [],
                outcomes: [],
                model,
                effort_level: effort,
            },
        }
    }
}

impl SessionRecord {
    /// Reads the session under `response_shape` or `session` when either holds one with an id,
    /// otherwise at the top level, as the CLI does.
    fn from_json(body: &[u8]) -> Result<Self, ApiError> {
        let Ok(Value::Object(answer)) = serde_json::from_slice(body) else {
            return Err(ApiError::Unexpected("the session is not a JSON object"));
        };
        let fields = ["response_shape", "session"]
            .into_iter()
            .filter_map(|key| answer.get(key)?.as_object())
            .find(|session| session.get("id").is_some_and(Value::is_string))
            .unwrap_or(&answer);
        let text = |name: &str| fields.get(name).and_then(Value::as_str).map(str::to_owned);
        let record = Self {
            id: text("id"),
            title: text("title"),
            environment_id: text("environment_id"),
            status: text("status"),
            session_status: text("session_status"),
        };
        tracing::debug!(
            fields = ?fields.keys().collect::<Vec<_>>(),
            status = ?record.status,
            session_status = ?record.session_status,
            "Claude session record"
        );
        Ok(record)
    }

    pub fn is_archived(&self) -> bool {
        [&self.status, &self.session_status]
            .into_iter()
            .flatten()
            .map(|status| status.to_ascii_lowercase())
            .any(|status| status == "archived" || status.ends_with("_archived"))
    }
}

impl SessionsApi {
    pub fn new(base: Url) -> Self {
        Self {
            base,
            http: OnceCell::new(),
        }
    }

    pub async fn create(
        &self,
        token: &AccessToken,
        session: &NewSession<'_>,
    ) -> Result<SessionRecord, ApiError> {
        let request = self
            .client()
            .await?
            .post(self.url(SESSIONS_PATH))
            .timeout(CREATE_TIMEOUT)
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(session).expect("a new session is JSON"));
        self.send(request, token).await
    }

    pub async fn get(
        &self,
        token: &AccessToken,
        id: &SessionId,
    ) -> Result<SessionRecord, ApiError> {
        let request = self
            .client()
            .await?
            .get(self.url(&format!("{SESSIONS_PATH}/{}", id.as_str())))
            .timeout(RECORD_TIMEOUT);
        self.send(request, token).await
    }

    fn url(&self, path: &str) -> Url {
        self.base
            .join(path)
            .expect("the API base takes relative paths")
    }

    /// Built on first use, once the process has a TLS provider.
    async fn client(&self) -> Result<&reqwest::Client, ApiError> {
        self.http
            .get_or_try_init(|| async {
                reqwest::Client::builder()
                    .connect_timeout(CONNECT_TIMEOUT)
                    .redirect(Policy::none())
                    .build()
                    .map_err(|error| ApiError::Unreachable(error.described()))
            })
            .await
    }

    async fn send(
        &self,
        request: RequestBuilder,
        token: &AccessToken,
    ) -> Result<SessionRecord, ApiError> {
        let mut response = request
            .header(AUTHORIZATION, token.bearer())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", ANTHROPIC_BETA)
            .send()
            .await
            .map_err(|error| ApiError::Unreachable(error.described()))?;
        let status = response.status();
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| ApiError::Unreachable(error.described()))?
        {
            if body.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
                return Err(ApiError::Unexpected("the answer is too large"));
            }
            body.extend_from_slice(&chunk);
        }
        match status {
            StatusCode::UNAUTHORIZED => Err(ApiError::Unauthorized),
            status if status.is_success() => SessionRecord::from_json(&body),
            status => Err(ApiError::Refused {
                status: status.as_u16(),
                message: body.error_message().unwrap_or_else(|| {
                    status
                        .canonical_reason()
                        .unwrap_or("no reason given")
                        .to_owned()
                }),
            }),
        }
    }
}

trait RequestErrorExt {
    /// The error and its causes, without the URL.
    fn described(self) -> String;
}

impl RequestErrorExt for reqwest::Error {
    fn described(self) -> String {
        let error = self.without_url();
        let mut described = error.to_string();
        let mut cause = error.source();
        while let Some(current) = cause {
            described.push_str(": ");
            described.push_str(&current.to_string());
            cause = current.source();
        }
        described
    }
}

trait ErrorBodyExt {
    /// The `error.message` of an API error, shortened.
    fn error_message(&self) -> Option<String>;
}

impl ErrorBodyExt for [u8] {
    fn error_message(&self) -> Option<String> {
        let body: Value = serde_json::from_slice(self).ok()?;
        let message = body
            .get("error")
            .and_then(|error| error.get("message"))
            .or_else(|| body.get("message"))?
            .as_str()?;
        Some(message.chars().take(MESSAGE_LIMIT).collect())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::manager::claude_remote::fake::{FakeSessionsApi, Reply};

    const TOKEN: &str = "test-access-token";

    struct Fixture {
        fake: FakeSessionsApi,
        api: SessionsApi,
        token: AccessToken,
    }

    impl Fixture {
        async fn new() -> Self {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let fake = FakeSessionsApi::start().await;
            let api = SessionsApi::new(fake.base.clone());
            Self {
                fake,
                api,
                token: AccessToken::for_test(TOKEN),
            }
        }

        fn session() -> SessionId {
            SessionId::parse("session_01AB").expect("the id is valid")
        }
    }

    #[test]
    fn session_ids_take_the_forms_the_cli_accepts() {
        for valid in ["cse_01AB", "session_01AB", "session_01-a_B"] {
            let id = SessionId::parse(valid).expect("the id is valid");
            assert_eq!(id.as_str(), valid);
        }
        for invalid in [
            "",
            "cse_",
            "session_",
            "--help",
            "-p",
            "env_01AB",
            "session_01AB/../x",
            "session_bad/../x",
            "cse_01 AB",
            "cse_01AB?x=1",
            "cse_01%2F",
            "session_é",
            &format!("cse_{}", "a".repeat(300)),
        ] {
            assert_eq!(SessionId::parse(invalid), None, "{invalid}");
        }
        let session = Fixture::session();
        assert_eq!(session.suffix(), "01AB");
        assert!(session.is_same_session("cse_01AB"));
        assert!(session.is_same_session("session_01AB"));
        assert!(!session.is_same_session("session_01ABC"));
        assert!(!session.is_same_session("01AB"));
    }

    #[tokio::test]
    async fn a_session_is_created_like_the_cli_creates_one() {
        let fixture = Fixture::new().await;
        fixture.fake.reply(
            "POST /v1/code/sessions",
            [Reply::new(
                200,
                json!({"session": {"id": "cse_01AB", "title": "repo#1", "environment_id": "env_1", "futureField": [1]}}),
            )],
        );
        let record = fixture
            .api
            .create(
                &fixture.token,
                &NewSession::new("repo#1", "env_1", Some("opus"), Some("high")),
            )
            .await
            .expect("the session is created");
        assert_eq!(record.id.as_deref(), Some("cse_01AB"));
        assert_eq!(record.title.as_deref(), Some("repo#1"));

        let [request] = fixture.fake.seen().try_into().expect("one request");
        assert_eq!(request.route(), "POST /v1/code/sessions");
        assert_eq!(
            request.body,
            json!({
                "title": "repo#1",
                "environment_id": "env_1",
                "events": [],
                "config": {"sources": [], "outcomes": [], "model": "opus", "effort_level": "high"},
            })
        );
        for (header, value) in [
            ("authorization", format!("Bearer {TOKEN}")),
            ("anthropic-version", "2023-06-01".to_owned()),
            ("anthropic-beta", "oauth-2025-04-20".to_owned()),
            ("content-type", "application/json".to_owned()),
        ] {
            assert_eq!(request.header(header).as_deref(), Some(value.as_str()));
        }
        assert_eq!(request.header("x-organization-uuid"), None);
        assert_eq!(request.header("user-agent"), None);
    }

    #[tokio::test]
    async fn a_session_without_settings_leaves_them_out() {
        let fixture = Fixture::new().await;
        fixture.fake.reply(
            "POST /v1/code/sessions",
            [Reply::new(200, json!({"id": "session_01AB"}))],
        );
        fixture
            .api
            .create(
                &fixture.token,
                &NewSession::new("ezra", "env_1", None, None),
            )
            .await
            .expect("the session is created");
        let [request] = fixture.fake.seen().try_into().expect("one request");
        assert_eq!(
            request.body["config"],
            json!({"sources": [], "outcomes": []})
        );
    }

    #[tokio::test]
    async fn a_session_record_says_whether_it_is_archived() {
        let fixture = Fixture::new().await;
        let route = "GET /v1/code/sessions/session_01AB";
        fixture.fake.reply(
            route,
            [
                Reply::new(
                    200,
                    json!({"id": "session_01AB", "status": "archived", "environment_id": "env_1", "connection_status": "connected"}),
                ),
                Reply::new(
                    200,
                    json!({"id": "session_01AB", "session_status": "SESSION_STATUS_ARCHIVED"}),
                ),
                Reply::new(
                    200,
                    json!({"id": "session_01AB", "status": "running", "worker_status": "idle", "environment_id": 7}),
                ),
                Reply::new(200, json!({"status": "failed"})),
                Reply::new(
                    200,
                    json!({"response_shape": {"id": "session_01AB", "status": "archived", "environment_id": "env_2"}, "session": {"id": "session_01AB", "status": "running"}}),
                ),
                Reply::new(
                    200,
                    json!({"session": {"id": "session_01AB", "status": "archived", "environment_id": "env_3"}}),
                ),
                Reply::new(
                    200,
                    json!({"session": {"status": "archived"}, "id": "session_01AB", "environment_id": "env_4"}),
                ),
            ],
        );
        let mut archived = Vec::new();
        for _ in 0..7 {
            let record = fixture
                .api
                .get(&fixture.token, &Fixture::session())
                .await
                .expect("the record is read");
            archived.push((record.is_archived(), record.environment_id));
        }
        assert_eq!(
            archived,
            [
                (true, Some("env_1".to_owned())),
                (true, None),
                (false, None),
                (false, None),
                (true, Some("env_2".to_owned())),
                (true, Some("env_3".to_owned())),
                (false, Some("env_4".to_owned())),
            ]
        );
        let seen = fixture.fake.seen();
        assert!(seen.iter().all(|request| request.route() == route));
        assert!(
            seen.iter()
                .all(|request| request.header("content-type").is_none())
        );
    }

    #[tokio::test]
    async fn failures_name_the_status_and_message_without_the_token() {
        let fixture = Fixture::new().await;
        fixture.fake.reply(
            "GET /v1/code/sessions/session_01AB",
            [
                Reply::new(401, json!({"type": "error", "error": {"type": "authentication_error", "message": "bad token"}})),
                Reply::new(404, json!({"type": "error", "error": {"type": "not_found_error", "message": "no such session"}})),
                Reply::new(529, json!({"message": "x".repeat(1000)})),
                Reply::new(502, json!("gateway")),
                Reply::new(302, json!({})).header("location", "http://127.0.0.1:9/elsewhere"),
                Reply::new(200, json!(["not", "an", "object"])),
                Reply::new(200, json!({"padding": "x".repeat(2 * 1024 * 1024)})),
            ],
        );
        let mut errors = Vec::new();
        for _ in 0..7 {
            errors.push(
                fixture
                    .api
                    .get(&fixture.token, &Fixture::session())
                    .await
                    .expect_err("the request fails"),
            );
        }
        let described: Vec<_> = errors.iter().map(ToString::to_string).collect();
        assert!(matches!(errors[0], ApiError::Unauthorized));
        assert_eq!(described[1], "Claude answered 404: no such session");
        assert_eq!(
            described[2],
            format!("Claude answered 529: {}", "x".repeat(MESSAGE_LIMIT))
        );
        assert_eq!(described[3], "Claude answered 502: Bad Gateway");
        assert_eq!(described[4], "Claude answered 302: Found");
        assert!(matches!(errors[5], ApiError::Unexpected(_)));
        assert!(matches!(errors[6], ApiError::Unexpected(_)));
        assert_eq!(fixture.fake.seen().len(), 7, "no redirect was followed");
        assert!(described.iter().all(|error| !error.contains(TOKEN)));

        let closed = SessionsApi::new(Url::parse("http://127.0.0.1:9/").expect("valid URL"));
        let unreachable = closed
            .get(&fixture.token, &Fixture::session())
            .await
            .expect_err("nothing listens");
        assert!(
            matches!(&unreachable, ApiError::Unreachable(cause) if cause.starts_with("error sending request")),
            "{unreachable}"
        );
    }
}
