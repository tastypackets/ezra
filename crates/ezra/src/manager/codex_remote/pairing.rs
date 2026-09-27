use std::sync::{Arc, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, SystemTime};

use qrcode::QrCode;
use qrcode::render::svg;
use qrcode::types::QrError;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};
use utoipa::ToSchema;

use super::CodexRemote;
use super::control::{
    ClientList, ClientRevoke, ClientWire, ControlClient, ControlError, PairingStart, PairingStatus,
    PairingStatusWire, PairingWire, RelayStatusWire, StatusRead,
};
use crate::manager::events::{Events, Topic};

const PAIR_LINK: &str = "https://chatgpt.com/codex/pair";
const PHONE_PAGES: usize = 10;

/// Why pairing or the paired phones are out of reach.
#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    #[error("Codex is not connected to ChatGPT")]
    NotConnected,
    #[error(transparent)]
    Control(#[from] ControlError),
}

/// A code a phone pairs with Codex through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CodexPairing {
    /// The code to enter in the ChatGPT app, absent when Codex gave none.
    pub manual_code: Option<String>,
    /// The link the QR code holds.
    pub link: String,
    /// When the code stops working.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub state: PairingState,
    /// Why checking the code failed, absent unless it did.
    pub error: Option<String>,
}

/// The pairing code asked for last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CodexPairingState {
    /// Absent before the first code.
    pub pairing: Option<CodexPairing>,
}

/// Whether a phone used a pairing code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PairingState {
    /// Waiting for a phone.
    Open,
    /// A phone paired with it.
    Claimed,
    /// It expired before a phone used it.
    Expired,
    /// Checking it failed.
    Failed,
}

/// A phone paired with Codex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PairedPhone {
    /// Identifies the phone.
    pub id: String,
    /// The name the phone gives, absent when it gives none.
    pub name: Option<String>,
    /// The kind of device, absent when not reported.
    pub device_type: Option<String>,
    /// The device model, absent when not reported.
    pub model: Option<String>,
    /// The operating system, absent when not reported.
    pub platform: Option<String>,
    /// The operating system's version, absent when not reported.
    pub os_version: Option<String>,
    /// The ChatGPT app's version, absent when not reported.
    pub app_version: Option<String>,
    /// When ChatGPT last saw the phone, absent when not reported.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_seen_at: Option<OffsetDateTime>,
}

impl From<ClientWire> for PairedPhone {
    fn from(client: ClientWire) -> Self {
        Self {
            id: client.client_id,
            name: client.display_name,
            device_type: client.device_type,
            model: client.device_model,
            platform: client.platform,
            os_version: client.os_version,
            app_version: client.app_version,
            last_seen_at: client.last_seen_at,
        }
    }
}

/// A pairing code Codex gave, checked in the background until a phone used it.
#[derive(Debug)]
pub struct Pairing {
    code: String,
    manual_code: Option<String>,
    expires_at: OffsetDateTime,
    expires: Instant,
    end: Arc<OnceLock<Ended>>,
    checking: JoinHandle<()>,
}

impl Pairing {
    /// Starts checking `started` every `every` through `client`, and publishes the phones once a
    /// phone used it.
    fn new(
        started: PairingWire,
        client: Arc<ControlClient>,
        every: Duration,
        events: Events,
    ) -> Self {
        let left = SystemTime::from(started.expires_at)
            .duration_since(SystemTime::now())
            .unwrap_or_default();
        let expires = Instant::now().checked_add(left).expect("the expiry fits");
        let end = Arc::new(OnceLock::new());
        let checking = tokio::spawn(
            PairingCheck {
                client,
                code: started.pairing_code.clone(),
                expires,
                every,
                end: Arc::clone(&end),
                events,
            }
            .run(),
        );
        Self {
            code: started.pairing_code,
            manual_code: started.manual_pairing_code,
            expires_at: started.expires_at,
            expires,
            end,
            checking,
        }
    }

    /// Whether a phone used the code, and why checking it failed.
    fn state(&self) -> (PairingState, Option<String>) {
        match self.end.get() {
            Some(Ended::Claimed) => (PairingState::Claimed, None),
            Some(Ended::Failed(error)) => (PairingState::Failed, Some(error.clone())),
            None if Instant::now() >= self.expires => (PairingState::Expired, None),
            None => (PairingState::Open, None),
        }
    }

    fn view(&self) -> CodexPairing {
        let (state, error) = self.state();
        CodexPairing {
            manual_code: self.manual_code.clone(),
            link: self.link(),
            expires_at: self.expires_at,
            state,
            error,
        }
    }

    /// The link a phone's camera opens the ChatGPT app with.
    fn link(&self) -> String {
        Url::parse_with_params(PAIR_LINK, [("pairing_code", &self.code)])
            .expect("the pairing link is a URL")
            .into()
    }

    /// The link as an SVG QR code, dark on white with a quiet zone.
    fn qr_svg(&self) -> Result<String, QrError> {
        Ok(QrCode::new(self.link())?
            .render::<svg::Color<'_>>()
            .quiet_zone(true)
            .dark_color(svg::Color("#000000"))
            .light_color(svg::Color("#ffffff"))
            .build())
    }
}

impl Drop for Pairing {
    fn drop(&mut self) {
        self.checking.abort();
    }
}

/// How checking a pairing code ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ended {
    Claimed,
    Failed(String),
}

/// Asks Codex whether a phone used a pairing code.
struct PairingCheck {
    client: Arc<ControlClient>,
    code: String,
    expires: Instant,
    every: Duration,
    end: Arc<OnceLock<Ended>>,
    events: Events,
}

impl PairingCheck {
    /// Asks every `every` until a phone used the code, it expires, or Codex fails to answer.
    async fn run(self) {
        loop {
            let next = Instant::now()
                .checked_add(self.every)
                .map_or(self.expires, |next| next.min(self.expires));
            sleep_until(next).await;
            if Instant::now() >= self.expires {
                return;
            }
            let asked = PairingStatus {
                pairing_code: self.code.clone(),
            };
            match self.client.request(asked).await {
                Ok(PairingStatusWire { claimed: false }) => {}
                Ok(PairingStatusWire { claimed: true }) => {
                    let _already_ended = self.end.set(Ended::Claimed);
                    self.events.publish(Topic::CodexPhones);
                    return;
                }
                Err(_) if Instant::now() >= self.expires => return,
                Err(error) => {
                    let _already_ended = self.end.set(Ended::Failed(error.to_string()));
                    return;
                }
            }
        }
    }
}

impl CodexRemote {
    /// Asks Codex for a code a phone pairs with, in place of the earlier one.
    pub async fn start_pairing(&self) -> Result<CodexPairing, PairingError> {
        let client = self.client().ok_or(ControlError::Closed)?;
        if client.request(StatusRead).await?.status != RelayStatusWire::Connected {
            return Err(PairingError::NotConnected);
        }
        let started = client.request(PairingStart { manual_code: true }).await?;
        let pairing = Pairing::new(
            started,
            client,
            self.budget.pairing_poll,
            self.events.clone(),
        );
        let shown = pairing.view();
        *self.lock_pairing() = Some(pairing);
        Ok(shown)
    }

    /// The code asked for last, absent before the first.
    pub fn pairing(&self) -> Option<CodexPairing> {
        self.lock_pairing().as_ref().map(Pairing::view)
    }

    /// The QR code of the code asked for last, absent unless a phone can still use it.
    pub fn open_pairing_qr(&self) -> Option<Result<String, QrError>> {
        let pairing = self.lock_pairing();
        let pairing = pairing
            .as_ref()
            .filter(|pairing| pairing.state().0 == PairingState::Open)?;
        Some(pairing.qr_svg())
    }

    /// The phones paired with Codex, from at most ten pages.
    pub async fn phones(&self) -> Result<Vec<PairedPhone>, PairingError> {
        let (client, environment_id) = self.environment().await?;
        let mut phones = Vec::new();
        let mut cursor = None;
        for _ in 0..PHONE_PAGES {
            let page = client
                .request(ClientList {
                    environment_id: environment_id.clone(),
                    cursor,
                })
                .await?;
            phones.extend(page.data.into_iter().map(PairedPhone::from));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        Ok(phones)
    }

    /// Removes a paired phone, so it can no longer reach this box.
    pub async fn remove_phone(&self, client_id: String) -> Result<(), PairingError> {
        let (client, environment_id) = self.environment().await?;
        client
            .request(ClientRevoke {
                environment_id,
                client_id,
            })
            .await?;
        self.events.publish(Topic::CodexPhones);
        Ok(())
    }

    /// The control connection, and the environment Codex serves this box in.
    async fn environment(&self) -> Result<(Arc<ControlClient>, String), PairingError> {
        let client = self.client().ok_or(ControlError::Closed)?;
        let environment_id = client
            .request(StatusRead)
            .await?
            .environment_id
            .ok_or(PairingError::NotConnected)?;
        Ok((client, environment_id))
    }

    fn lock_pairing(&self) -> MutexGuard<'_, Option<Pairing>> {
        self.pairing.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use axum::http::{StatusCode, header};
    use axum::response::Response;
    use nix::unistd::Pid;
    use serde_json::{Value, json};
    use time::macros::datetime;
    use tokio::time::sleep;

    use super::*;
    use crate::manager::api::test_support::{EventStreamExt, ResponseExt, TestManager, wait_until};
    use crate::manager::codex_remote::control::ControlSocket;
    use crate::manager::codex_remote::fake::{ENVIRONMENT, FakeControlServer, Reply};
    use crate::manager::codex_remote::{ExpectedPeer, ServerBudget};

    const WAIT: Duration = Duration::from_secs(10);
    const BUDGET: ServerBudget = ServerBudget {
        probe: Duration::ZERO,
        readiness: Duration::ZERO,
        drain: Duration::ZERO,
        force: Duration::ZERO,
        request: Duration::from_secs(5),
        mfa_retry: Duration::ZERO,
        usage: Duration::ZERO,
        update_deadline: Duration::ZERO,
        pairing_poll: Duration::from_millis(50),
    };
    const PAIRING: &str = "/api/v1/remote-control/codex/pairing";
    const QR: &str = "/api/v1/remote-control/codex/pairing/qr.svg";
    const PHONES: &str = "/api/v1/remote-control/codex/phones";
    const RELAY: &str = "remoteControl/status/read";
    const START: &str = "remoteControl/pairing/start";
    const STATUS: &str = "remoteControl/pairing/status";
    const LIST: &str = "remoteControl/client/list";
    const REVOKE: &str = "remoteControl/client/revoke";
    const NOT_CONNECTED: &str = "Codex is not connected to ChatGPT";
    const UNTIL_ENROLLED: &str = "remote control pairing is unavailable until enrollment completes";
    const RETRY_DEFERRED: &str =
        "remote control retry deferred until 2026-09-27 18:00:00.0 +00:00:00";

    struct Answering {
        manager: TestManager,
        cookie: String,
        fake: FakeControlServer,
    }

    /// A manager whose Codex answers on a fake control socket, with the relay `relay`.
    async fn answering(relay: &str) -> Answering {
        let manager = TestManager::new().with_codex(BUDGET, ExpectedPeer::Child);
        let cookie = manager.logged_in().await;
        let home = manager.codex_home();
        fs::create_dir_all(&home).expect("the Codex home is created");
        let fake = FakeControlServer::bind(&home);
        fake.reply(RELAY, [Reply::Result(FakeControlServer::relay(relay))]);
        let (client, _events) =
            ControlClient::connect(&ControlSocket::of(&home), Pid::this(), &home, &BUDGET)
                .await
                .expect("the fake answers");
        manager
            .state
            .codex_remote
            .with_control(|control| control.client = Some(Arc::new(client)));
        Answering {
            manager,
            cookie,
            fake,
        }
    }

    /// Unix seconds `seconds` from now, as Codex gives an expiry.
    fn in_seconds(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(
            OffsetDateTime::now_utc()
                .unix_timestamp()
                .saturating_add(seconds),
        )
        .expect("the time fits")
    }

    fn started(code: &str, manual_code: &str, expires_at: OffsetDateTime) -> Reply {
        Reply::Result(json!({
            "pairingCode": code,
            "manualPairingCode": manual_code,
            "environmentId": ENVIRONMENT,
            "expiresAt": expires_at.unix_timestamp(),
        }))
    }

    fn claimed(claimed: bool) -> Reply {
        Reply::Result(json!({ "claimed": claimed }))
    }

    fn checked(code: &str) -> Value {
        json!({ "pairingCode": code })
    }

    fn open(code: &str, manual_code: &str, expires_at: OffsetDateTime) -> CodexPairing {
        CodexPairing {
            manual_code: Some(manual_code.to_owned()),
            link: format!("https://chatgpt.com/codex/pair?pairing_code={code}"),
            expires_at,
            state: PairingState::Open,
            error: None,
        }
    }

    impl Answering {
        async fn start(&self) -> Response {
            self.manager.post(PAIRING, "", Some(&self.cookie)).await
        }

        async fn started(&self) -> CodexPairing {
            let response = self.start().await;
            assert_eq!(response.status(), StatusCode::OK);
            response.json().await
        }

        async fn shown(&self) -> Option<CodexPairing> {
            let shown: CodexPairingState = self
                .manager
                .get(PAIRING, Some(&self.cookie))
                .await
                .json()
                .await;
            shown.pairing
        }

        async fn until_shown(&self, state: PairingState) -> CodexPairing {
            wait_until(
                WAIT,
                || self.manager.state.codex_remote.pairing(),
                |pairing| {
                    pairing
                        .as_ref()
                        .is_some_and(|pairing| pairing.state == state)
                },
            )
            .await
            .expect("a pairing is shown")
        }

        /// Waits a few checks long, then returns the codes checked.
        async fn checked_after_a_while(&self) -> Vec<Value> {
            sleep(BUDGET.pairing_poll.saturating_mul(4)).await;
            self.fake.requests_of(STATUS)
        }

        /// Waits until the code was checked twice more.
        async fn checked_twice_more(&self) {
            let checks = self.fake.requests_of(STATUS).len();
            self.fake
                .until_requested(STATUS, checks.saturating_add(2))
                .await;
        }
    }

    #[tokio::test]
    async fn pairing_and_phones_need_a_signed_in_manager() {
        let answering = answering("connected").await;
        let manager = &answering.manager;
        for response in [
            manager.post(PAIRING, "", None).await,
            manager.get(PAIRING, None).await,
            manager.get(QR, None).await,
            manager.get(PHONES, None).await,
            manager.delete(&format!("{PHONES}/phone-1"), None).await,
        ] {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        assert_eq!(answering.fake.requests_after_initialize(), []);
    }

    #[tokio::test]
    async fn a_code_is_asked_for_with_a_manual_code_and_shown_while_a_phone_can_use_it() {
        let answering = answering("connected").await;
        let expires_at = in_seconds(600);
        answering
            .fake
            .reply(START, [started("pairing-1", "ABCD-EFGH", expires_at)]);
        answering.fake.reply(STATUS, [claimed(false)]);
        assert_eq!(answering.shown().await, None);

        let pairing = answering.started().await;

        assert_eq!(pairing, open("pairing-1", "ABCD-EFGH", expires_at));
        assert_eq!(answering.shown().await, Some(pairing));
        let requests = answering.fake.requests_after_initialize();
        assert_eq!(
            requests.get(..2),
            Some(
                [
                    (RELAY.to_owned(), Value::Null),
                    (START.to_owned(), json!({ "manualCode": true })),
                ]
                .as_slice()
            )
        );
        let checks = answering.fake.until_requested(STATUS, 2).await;
        assert!(
            checks.iter().all(|params| *params == checked("pairing-1")),
            "{checks:?}"
        );
    }

    #[tokio::test]
    async fn the_qr_code_holds_the_link_dark_on_white_with_a_quiet_zone() {
        let answering = answering("connected").await;
        answering
            .fake
            .reply(START, [started("pairing-1", "ABCD-EFGH", in_seconds(600))]);
        answering.fake.reply(STATUS, [claimed(false)]);
        let qr = || answering.manager.get(QR, Some(&answering.cookie));
        assert_eq!(qr().await.status(), StatusCode::NOT_FOUND);
        let pairing = answering.started().await;

        let response = qr().await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&header::HeaderValue::from_static("image/svg+xml"))
        );
        let svg = response.text().await;
        let modules = QrCode::new(&pairing.link)
            .expect("the link fits in a QR code")
            .width();
        let side = modules.saturating_add(8).saturating_mul(8);
        assert!(
            svg.contains(&format!(
                r##"<rect x="0" y="0" width="{side}" height="{side}" fill="#ffffff"/>"##
            )),
            "{svg}"
        );
        assert!(
            svg.contains(r##"<path fill="#000000" d="M32 32h8v8H32V32"##),
            "{svg}"
        );
    }

    #[tokio::test]
    async fn a_phone_using_the_code_ends_the_checks_and_changes_the_phones() {
        let answering = answering("connected").await;
        answering
            .fake
            .reply(START, [started("pairing-1", "ABCD-EFGH", in_seconds(600))]);
        answering
            .fake
            .reply(STATUS, [claimed(false), claimed(false), claimed(true)]);
        let mut events = Box::pin(answering.manager.state.events.stream());
        answering.started().await;

        let pairing = answering.until_shown(PairingState::Claimed).await;

        assert_eq!(pairing.error, None);
        assert_eq!(
            answering.checked_after_a_while().await,
            [
                checked("pairing-1"),
                checked("pairing-1"),
                checked("pairing-1")
            ]
        );
        assert!(events.published().await.contains(&Topic::CodexPhones));
        assert_eq!(
            answering
                .manager
                .get(QR, Some(&answering.cookie))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn an_expired_code_is_no_longer_checked() {
        let answering = answering("connected").await;
        answering
            .fake
            .reply(START, [started("pairing-1", "ABCD-EFGH", in_seconds(2))]);
        answering.fake.reply(STATUS, [claimed(false)]);
        answering.started().await;

        let expired = answering.until_shown(PairingState::Expired).await;

        assert_eq!(expired.error, None);
        let checks = answering.fake.requests_of(STATUS).len();
        assert!(checks > 0);
        assert_eq!(answering.checked_after_a_while().await.len(), checks);
        assert_eq!(
            answering
                .manager
                .get(QR, Some(&answering.cookie))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_check_failing_once_the_code_expired_leaves_it_expired() {
        let answering = answering("connected").await;
        answering
            .fake
            .reply(START, [started("pairing-1", "ABCD-EFGH", in_seconds(2))]);
        answering.fake.reply(STATUS, [Reply::Never]);
        answering.started().await;
        answering.fake.until_requested(STATUS, 1).await;
        answering.until_shown(PairingState::Expired).await;

        answering.fake.disconnect();

        sleep(BUDGET.pairing_poll.saturating_mul(4)).await;
        let expired = answering
            .manager
            .state
            .codex_remote
            .pairing()
            .expect("a pairing is shown");
        assert_eq!(
            (expired.state, expired.error),
            (PairingState::Expired, None)
        );
    }

    #[tokio::test]
    async fn a_check_codex_refuses_fails_the_code_with_its_message() {
        let answering = answering("connected").await;
        answering
            .fake
            .reply(START, [started("pairing-1", "ABCD-EFGH", in_seconds(600))]);
        answering.fake.reply(
            STATUS,
            [Reply::Error {
                code: -32603,
                message: RETRY_DEFERRED.to_owned(),
            }],
        );
        answering.started().await;

        let failed = answering.until_shown(PairingState::Failed).await;

        assert_eq!(failed.error.as_deref(), Some(RETRY_DEFERRED));
        assert_eq!(
            answering.checked_after_a_while().await,
            [checked("pairing-1")]
        );
    }

    #[tokio::test]
    async fn a_lost_control_connection_fails_the_code() {
        let answering = answering("connected").await;
        answering
            .fake
            .reply(START, [started("pairing-1", "ABCD-EFGH", in_seconds(600))]);
        answering.fake.reply(STATUS, [claimed(false)]);
        answering.started().await;
        answering.fake.until_requested(STATUS, 1).await;

        answering.fake.disconnect();

        let failed = answering.until_shown(PairingState::Failed).await;
        assert_eq!(
            failed.error.as_deref(),
            Some("Codex closed the control connection")
        );
    }

    #[tokio::test]
    async fn a_new_code_replaces_the_earlier_one_and_its_checks() {
        let answering = answering("connected").await;
        let expires_at = in_seconds(600);
        answering.fake.reply(
            START,
            [
                started("pairing-1", "ABCD-EFGH", expires_at),
                started("pairing-2", "JKLM-NPQR", expires_at),
            ],
        );
        answering.fake.reply(STATUS, [claimed(false)]);
        answering.started().await;
        answering.fake.until_requested(STATUS, 1).await;

        let second = answering.started().await;

        assert_eq!(second, open("pairing-2", "JKLM-NPQR", expires_at));
        assert_eq!(answering.shown().await, Some(second));
        sleep(BUDGET.pairing_poll.saturating_mul(2)).await;
        let checks_of = |code: &str| {
            answering
                .fake
                .requests_of(STATUS)
                .into_iter()
                .filter(|params| *params == checked(code))
                .count()
        };
        let first_checks = checks_of("pairing-1");
        let second_checks = checks_of("pairing-2");
        wait_until(
            WAIT,
            || checks_of("pairing-2"),
            |checks| *checks >= second_checks.saturating_add(3),
        )
        .await;
        assert_eq!(checks_of("pairing-1"), first_checks);
    }

    #[tokio::test]
    async fn a_code_codex_refuses_to_give_answers_with_codexs_message_and_keeps_the_earlier_one() {
        let answering = answering("connected").await;
        answering.fake.reply(
            START,
            [
                started("pairing-1", "ABCD-EFGH", in_seconds(600)),
                Reply::Error {
                    code: -32600,
                    message: UNTIL_ENROLLED.to_owned(),
                },
            ],
        );
        answering.fake.reply(STATUS, [claimed(false)]);
        let earlier = answering.started().await;

        let response = answering.start().await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(response.error().await, UNTIL_ENROLLED);
        assert_eq!(answering.shown().await, Some(earlier));
        answering.checked_twice_more().await;
    }

    #[tokio::test]
    async fn pairing_needs_codex_connected_to_chatgpt() {
        let manager = TestManager::new().with_codex(BUDGET, ExpectedPeer::Child);
        let cookie = manager.logged_in().await;
        let response = manager.post(PAIRING, "", Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(response.error().await, "Codex is not running");

        for relay in ["disabled", "connecting", "errored"] {
            let answering = answering("connected").await;
            answering
                .fake
                .reply(START, [started("pairing-1", "ABCD-EFGH", in_seconds(600))]);
            answering.fake.reply(STATUS, [claimed(false)]);
            let earlier = answering.started().await;
            answering
                .fake
                .reply(RELAY, [Reply::Result(FakeControlServer::relay(relay))]);

            let response = answering.start().await;

            assert_eq!(response.status(), StatusCode::CONFLICT, "{relay}");
            assert_eq!(response.error().await, NOT_CONNECTED);
            assert_eq!(answering.fake.requests_of(START).len(), 1, "{relay}");
            assert_eq!(answering.shown().await, Some(earlier), "{relay}");
        }
    }

    fn phone_page(clients: Value, next_cursor: Value) -> Reply {
        Reply::Result(json!({ "data": clients, "nextCursor": next_cursor }))
    }

    /// A phone that reports nothing about itself, as Codex lists it.
    fn unnamed_phone(id: &str) -> Value {
        json!({
            "clientId": id,
            "displayName": null,
            "deviceType": null,
            "platform": null,
            "osVersion": null,
            "deviceModel": null,
            "appVersion": null,
            "lastSeenAt": null,
        })
    }

    #[tokio::test]
    async fn phones_are_listed_from_every_page_in_codexs_environment() {
        let answering = answering("connected").await;
        answering.fake.reply(
            LIST,
            [
                phone_page(
                    json!([{
                        "clientId": "phone-1",
                        "displayName": "Dev's iPhone",
                        "deviceType": "phone",
                        "platform": "ios",
                        "osVersion": "26.0",
                        "deviceModel": "iPhone",
                        "appVersion": "1.2026.258",
                        "lastSeenAt": 1_790_000_000,
                    }]),
                    json!("page-2"),
                ),
                phone_page(json!([unnamed_phone("phone-2")]), Value::Null),
            ],
        );

        let response = answering.manager.get(PHONES, Some(&answering.cookie)).await;

        assert_eq!(response.status(), StatusCode::OK);
        let listed: Value = response.json().await;
        assert_eq!(listed[0]["last_seen_at"], "2026-09-21T14:13:20Z");
        let phones: Vec<PairedPhone> = serde_json::from_value(listed).expect("the phones parse");
        assert_eq!(
            phones,
            [
                PairedPhone {
                    id: "phone-1".to_owned(),
                    name: Some("Dev's iPhone".to_owned()),
                    device_type: Some("phone".to_owned()),
                    model: Some("iPhone".to_owned()),
                    platform: Some("ios".to_owned()),
                    os_version: Some("26.0".to_owned()),
                    app_version: Some("1.2026.258".to_owned()),
                    last_seen_at: Some(datetime!(2026-09-21 14:13:20 UTC)),
                },
                PairedPhone {
                    id: "phone-2".to_owned(),
                    name: None,
                    device_type: None,
                    model: None,
                    platform: None,
                    os_version: None,
                    app_version: None,
                    last_seen_at: None,
                },
            ]
        );
        assert_eq!(
            answering.fake.requests_of(LIST),
            [
                json!({ "environmentId": ENVIRONMENT }),
                json!({ "environmentId": ENVIRONMENT, "cursor": "page-2" }),
            ]
        );
    }

    #[tokio::test]
    async fn phones_are_listed_from_ten_pages_at_most() {
        let answering = answering("connected").await;
        answering.fake.reply(
            LIST,
            [phone_page(json!([unnamed_phone("phone")]), json!("more"))],
        );

        let phones: Vec<PairedPhone> = answering
            .manager
            .get(PHONES, Some(&answering.cookie))
            .await
            .json()
            .await;

        assert_eq!(phones.len(), 10);
        assert_eq!(answering.fake.requests_of(LIST).len(), 10);
    }

    #[tokio::test]
    async fn removing_a_phone_revokes_it_in_codexs_environment() {
        let answering = answering("connected").await;
        answering.fake.reply(REVOKE, [Reply::Result(json!({}))]);
        let mut events = Box::pin(answering.manager.state.events.stream());

        let response = answering
            .manager
            .delete(&format!("{PHONES}/phone-1"), Some(&answering.cookie))
            .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            answering.fake.requests_of(REVOKE),
            [json!({ "environmentId": ENVIRONMENT, "clientId": "phone-1" })]
        );
        assert_eq!(events.published().await, [Topic::CodexPhones]);
    }

    #[tokio::test]
    async fn phones_need_codexs_environment() {
        let manager = TestManager::new().with_codex(BUDGET, ExpectedPeer::Child);
        let cookie = manager.logged_in().await;
        let response = manager.get(PHONES, Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(response.error().await, "Codex is not running");

        let answering = answering("disabled").await;
        let manager = &answering.manager;
        let cookie = Some(answering.cookie.as_str());
        for response in [
            manager.get(PHONES, cookie).await,
            manager.delete(&format!("{PHONES}/phone-1"), cookie).await,
        ] {
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(response.error().await, NOT_CONNECTED);
        }
        assert!(answering.fake.requests_of(LIST).is_empty());
        assert!(answering.fake.requests_of(REVOKE).is_empty());
    }
}
