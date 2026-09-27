use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A known reason Codex does not serve this box to the ChatGPT app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodexProblem {
    /// ChatGPT asks for multi-factor authentication on the account.
    MfaRequired,
    /// Codex is signed in with an API key or Amazon Bedrock, not ChatGPT.
    #[serde(rename = "not_chatgpt")]
    NotChatGpt,
    /// Codex is not signed in.
    SignedOut,
    /// Codex's managed requirements turn remote control off.
    NotAllowed,
    /// Another Codex server was running in this box.
    SocketInUse,
    /// Codex could not connect to ChatGPT and keeps trying.
    RelayUnavailable,
    /// The installed Codex cannot run remote control.
    UnsupportedVersion,
}

/// A line of the server's output that names a known problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemLine {
    pub problem: CodexProblem,
    pub line: String,
}

impl CodexProblem {
    /// Parts of the messages Codex 0.157 prints, checked in this order.
    const MESSAGES: [(&str, Self); 6] = [
        ("Multi-factor authentication required", Self::MfaRequired),
        ("API key auth is not supported", Self::NotChatGpt),
        (
            "remote control requires ChatGPT authentication",
            Self::SignedOut,
        ),
        (
            "remote control is disabled by managed requirements",
            Self::NotAllowed,
        ),
        (
            "app-server control socket is already in use",
            Self::SocketInUse,
        ),
        (
            "failed to connect to app-server remote control websocket",
            Self::RelayUnavailable,
        ),
    ];

    /// The problem a line without terminal codes names.
    pub fn in_line(line: &str) -> Option<Self> {
        Self::MESSAGES
            .into_iter()
            .find_map(|(message, problem)| line.contains(message).then_some(problem))
    }

    /// The problem named by the last line that names one.
    pub fn in_output(lines: &[&str]) -> Option<Self> {
        lines.iter().rev().find_map(|line| Self::in_line(line))
    }

    /// Whether ezra turns the relay off when this problem is named.
    pub fn turns_relay_off(self) -> bool {
        matches!(self, Self::MfaRequired | Self::NotChatGpt | Self::SignedOut)
    }

    /// A sign-in that is not ChatGPT's.
    pub fn is_about_the_sign_in(self) -> bool {
        matches!(self, Self::NotChatGpt | Self::SignedOut)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::manager::login::StrExt;

    const TARGET: &str = "codex_app_server_transport::transport::remote_control::websocket";
    pub const PROBLEMS: [CodexProblem; 7] = [
        CodexProblem::MfaRequired,
        CodexProblem::NotChatGpt,
        CodexProblem::SignedOut,
        CodexProblem::NotAllowed,
        CodexProblem::SocketInUse,
        CodexProblem::RelayUnavailable,
        CodexProblem::UnsupportedVersion,
    ];
    const ENROLL_URL: &str = "https://chatgpt.com/backend-api/wham/remote/control/server/enroll";
    const WEBSOCKET_URL: &str = "wss://chatgpt.com/backend-api/wham/remote/control/server";

    /// The warning Codex logs when the relay connection fails, as its stderr prints it.
    pub fn relay_warning(error: &str, error_kind: &str) -> String {
        format!(
            "2026-09-26T16:51:29.504163Z  WARN {TARGET}: failed to connect to app-server remote \
             control websocket websocket_url={WEBSOCKET_URL} \
             installation_id=0199a0c4-5d0e-7c4b-9d0e-3c1f0e6b2a11 server_name=ezra-dev \
             error={error} error_kind={error_kind} reconnect_attempt=3 reconnect_delay=800ms \
             reconnect_backoff_reset=false has_enrollment=false server_id=None \
             environment_id=None subscribe_cursor_present=false"
        )
    }

    /// The warning Codex logged while ChatGPT asked for multi-factor authentication.
    pub fn mfa_warning() -> String {
        relay_warning(
            &format!(
                "remote control server enrollment failed at `{ENROLL_URL}`: HTTP 403 Forbidden, \
                 request-id: 5f0e2c9a-1b7d-4e38-a4c2-9d61f0b3e7aa, cf-ray: 985c1f2e3a4b5c6d-ORD, \
                 body: {{\"detail\":\"Multi-factor authentication required\"}}"
            ),
            "PermissionDenied",
        )
    }

    /// The warning Codex logs when ChatGPT does not take the relay connection.
    pub fn unavailable_warning() -> String {
        relay_warning(
            &format!(
                "failed to connect app-server remote control websocket `{WEBSOCKET_URL}`: HTTP \
                 error: 503 Service Unavailable, request-id: <none>, cf-ray: <none>, body: \
                 upstream unavailable"
            ),
            "Other",
        )
    }

    /// Adds terminal colour codes to a line.
    pub fn coloured(line: &str) -> String {
        format!("\u{1b}[1m{line}\u{1b}[0m")
            .replace(" WARN ", " \u{1b}[33mWARN\u{1b}[0m ")
            .replace(" INFO ", " \u{1b}[32mINFO\u{1b}[0m ")
            .replace(
                &format!("{TARGET}:"),
                &format!("\u{1b}[2m{TARGET}\u{1b}[0m\u{1b}[2m:\u{1b}[0m"),
            )
            .replace(" error=", " \u{1b}[3merror\u{1b}[0m\u{1b}[2m=\u{1b}[0m")
    }

    fn samples() -> Vec<(String, Option<CodexProblem>)> {
        vec![
            (mfa_warning(), Some(CodexProblem::MfaRequired)),
            (
                relay_warning(
                    "remote control requires ChatGPT authentication; API key auth is not supported",
                    "PermissionDenied",
                ),
                Some(CodexProblem::NotChatGpt),
            ),
            (
                relay_warning(
                    "remote control requires ChatGPT authentication",
                    "PermissionDenied",
                ),
                Some(CodexProblem::SignedOut),
            ),
            (
                "Error: remote control is disabled by managed requirements".to_owned(),
                Some(CodexProblem::NotAllowed),
            ),
            (
                "Error: app-server control socket is already in use at \
                 /config/codex/app-server-control/app-server-control.sock"
                    .to_owned(),
                Some(CodexProblem::SocketInUse),
            ),
            (unavailable_warning(), Some(CodexProblem::RelayUnavailable)),
            (
                format!(
                    "2026-09-26T16:51:29.504163Z  INFO {TARGET}: retrying app-server remote \
                     control websocket after auth changed"
                ),
                None,
            ),
        ]
    }

    #[test]
    fn each_message_names_its_problem() {
        for (line, problem) in samples() {
            assert_eq!(CodexProblem::in_line(&line), problem, "{line}");
        }
    }

    #[test]
    fn the_last_line_that_names_a_problem_wins() {
        let samples = samples();
        let mut lines: Vec<&str> = samples.iter().map(|(line, _)| line.as_str()).collect();
        assert_eq!(
            CodexProblem::in_output(&lines),
            Some(CodexProblem::RelayUnavailable)
        );
        lines.reverse();
        assert_eq!(
            CodexProblem::in_output(&lines),
            Some(CodexProblem::MfaRequired)
        );
        assert_eq!(CodexProblem::in_output(&lines[..1]), None);
        assert_eq!(CodexProblem::in_output(&[]), None);
    }

    #[test]
    fn coloured_lines_name_the_same_problem_once_plain() {
        for (line, problem) in samples() {
            let coloured = coloured(&line);
            assert_ne!(coloured, line);
            assert_eq!(coloured.without_terminal_codes(), line);
            assert_eq!(
                CodexProblem::in_line(&coloured.without_terminal_codes()),
                problem,
                "{coloured}"
            );
        }
    }

    #[test]
    fn only_problems_codex_retries_without_end_turn_the_relay_off() {
        let turning_off: Vec<CodexProblem> = PROBLEMS
            .into_iter()
            .filter(|problem| problem.turns_relay_off())
            .collect();
        assert_eq!(
            turning_off,
            [
                CodexProblem::MfaRequired,
                CodexProblem::NotChatGpt,
                CodexProblem::SignedOut
            ]
        );
        let about_the_sign_in: Vec<CodexProblem> = PROBLEMS
            .into_iter()
            .filter(|problem| problem.is_about_the_sign_in())
            .collect();
        assert_eq!(
            about_the_sign_in,
            [CodexProblem::NotChatGpt, CodexProblem::SignedOut]
        );
    }

    #[test]
    fn problems_are_named_in_snake_case() {
        let names = [
            (CodexProblem::MfaRequired, "mfa_required"),
            (CodexProblem::NotChatGpt, "not_chatgpt"),
            (CodexProblem::SignedOut, "signed_out"),
            (CodexProblem::NotAllowed, "not_allowed"),
            (CodexProblem::SocketInUse, "socket_in_use"),
            (CodexProblem::RelayUnavailable, "relay_unavailable"),
            (CodexProblem::UnsupportedVersion, "unsupported_version"),
        ];
        for (problem, name) in names {
            let json = format!("\"{name}\"");
            assert_eq!(
                serde_json::to_string(&problem).expect("problem serializes"),
                json
            );
            assert_eq!(
                serde_json::from_str::<CodexProblem>(&json).expect("problem parses"),
                problem
            );
        }
    }
}
