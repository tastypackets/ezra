use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::{sleep, timeout};
use utoipa::ToSchema;

use super::agents::{Agent, InstallPaths};
use crate::process_ext::OutputExt;

const PROMPT_TIMEOUT: Duration = Duration::from_secs(30);
const CODE_TIMEOUT: Duration = Duration::from_secs(60);
const PROMPT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const FAILURE_OUTPUT_LINES: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("{0} is not installed")]
    NotInstalled(Agent),
    #[error("{command} did not show a sign-in link: {output}")]
    NoPrompt {
        command: &'static str,
        output: String,
    },
    #[error("{command} sign-in did not finish: {output}")]
    Failed {
        command: &'static str,
        output: String,
    },
    #[error("{command} {action} failed: {output}")]
    Command {
        command: &'static str,
        action: &'static str,
        output: String,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// What a sign-in shows the person signing in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptShape {
    Link,
    LinkAndCode,
}

/// An installed agent CLI, for signing in and out.
pub struct AgentCli {
    agent: Agent,
    command_path: PathBuf,
}

impl AgentCli {
    pub fn installed(agent: Agent, paths: &InstallPaths) -> Result<Self, LoginError> {
        let command_path = paths.command(agent);
        if !command_path.exists() {
            return Err(LoginError::NotInstalled(agent));
        }
        Ok(Self {
            agent,
            command_path,
        })
    }

    /// Starts `claude auth login` or `codex login --device-auth` and waits for its link.
    pub async fn start_login(&self) -> Result<(LoginProcess, LoginPrompt), LoginError> {
        let mut command = self.command();
        match self.agent {
            Agent::Claude => command.args(["auth", "login", "--claudeai"]),
            Agent::Codex => command.args(["login", "--device-auth"]),
        };
        let shape = match self.agent {
            Agent::Claude => PromptShape::Link,
            Agent::Codex => PromptShape::LinkAndCode,
        };
        LoginProcess::start(self.agent.command_name(), shape, command).await
    }

    /// Asks the CLI. Anything unexpected in its answer counts as signed out.
    pub async fn sign_in_status(&self) -> SignInStatus {
        let mut command = self.command();
        match self.agent {
            Agent::Claude => command.args(["auth", "status"]),
            Agent::Codex => command.args(["login", "status"]),
        };
        let Ok(output) = command.stdin(Stdio::null()).output().await else {
            return SignInStatus::default();
        };
        match self.agent {
            Agent::Claude => SignInStatus::from_claude_json(&output.stdout),
            Agent::Codex if output.status.success() => {
                let mut text = output.stdout_text();
                text.push_str(&output.stderr_text());
                SignInStatus::from_codex_text(&text)
            }
            Agent::Codex => SignInStatus::default(),
        }
    }

    pub async fn log_out(&self) -> Result<(), LoginError> {
        let mut command = self.command();
        match self.agent {
            Agent::Claude => command.args(["auth", "logout"]),
            Agent::Codex => command.arg("logout"),
        };
        let output = command.stdin(Stdio::null()).output().await?;
        if output.status.success() {
            Ok(())
        } else {
            Err(LoginError::Command {
                command: self.agent.command_name(),
                action: "logout",
                output: output.stderr_text(),
            })
        }
    }

    fn command(&self) -> Command {
        Command::new(&self.command_path)
    }
}

/// What the person signing in needs: a link to open, and for Codex a code to type there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LoginPrompt {
    /// The sign-in page to open.
    pub url: String,
    /// The one-time code to enter on that page, Codex only.
    pub code: Option<String>,
}

impl LoginPrompt {
    /// Reads only complete lines, so a link that is still arriving is never cut short.
    fn parse(shape: PromptShape, output: &str) -> Option<Self> {
        let (complete_output, _unfinished_line) = output.rsplit_once('\n')?;
        let url = complete_output
            .split_whitespace()
            .find(|word| word.starts_with("https://"))?
            .to_owned();
        match shape {
            PromptShape::Link => Some(Self { url, code: None }),
            PromptShape::LinkAndCode => {
                let (_, after_marker) = complete_output.rsplit_once("one-time code")?;
                let code = after_marker.split_whitespace().find(|word| {
                    word.contains('-')
                        && word.chars().all(|character| {
                            character.is_ascii_uppercase()
                                || character.is_ascii_digit()
                                || character == '-'
                        })
                })?;
                Some(Self {
                    url,
                    code: Some(code.to_owned()),
                })
            }
        }
    }
}

/// A running sign-in. Dropping it stops the process.
pub struct LoginProcess {
    command: &'static str,
    shape: PromptShape,
    prompt: Option<LoginPrompt>,
    child: Child,
    stdin: Option<ChildStdin>,
    output: Arc<Mutex<String>>,
}

impl LoginProcess {
    /// Runs `command` and waits for its sign-in link. `name` is the command's name, for errors.
    pub async fn start(
        name: &'static str,
        shape: PromptShape,
        command: Command,
    ) -> Result<(Self, LoginPrompt), LoginError> {
        let mut login = Self::spawn(name, shape, command)?;
        match timeout(PROMPT_TIMEOUT, login.wait_for_prompt()).await {
            Ok(Some(prompt)) => Ok((login, prompt)),
            Ok(None) | Err(_) => Err(LoginError::NoPrompt {
                command: name,
                output: login.output_tail(),
            }),
        }
    }

    fn spawn(
        name: &'static str,
        shape: PromptShape,
        mut command: Command,
    ) -> Result<Self, LoginError> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let output = Arc::new(Mutex::new(String::new()));
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(Self::collect_output(stdout, Arc::clone(&output)));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(Self::collect_output(stderr, Arc::clone(&output)));
        }
        Ok(Self {
            command: name,
            shape,
            prompt: None,
            stdin: child.stdin.take(),
            child,
            output,
        })
    }

    /// `None` when the process exits without showing a link.
    async fn wait_for_prompt(&mut self) -> Option<LoginPrompt> {
        loop {
            if let Some(prompt) = LoginPrompt::parse(self.shape, &self.output_text()) {
                self.prompt = Some(prompt.clone());
                return Some(prompt);
            }
            if self.has_finished() {
                return None;
            }
            sleep(PROMPT_POLL_INTERVAL).await;
        }
    }

    pub fn prompt(&self) -> Option<&LoginPrompt> {
        self.prompt.as_ref()
    }

    pub fn has_finished(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    /// Claude only: sends the code shown after signing in, then waits for the login to finish.
    pub async fn submit_code(mut self, code: &str) -> Result<(), LoginError> {
        if let Some(stdin) = self.stdin.as_mut() {
            stdin
                .write_all(format!("{}\n", code.trim()).as_bytes())
                .await?;
            stdin.flush().await?;
        }
        match timeout(CODE_TIMEOUT, self.child.wait()).await {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Err(error)) => Err(error.into()),
            Ok(Ok(_)) | Err(_) => Err(LoginError::Failed {
                command: self.command,
                output: self.output_tail(),
            }),
        }
    }

    fn output_text(&self) -> String {
        self.output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .without_terminal_codes()
    }

    fn output_tail(&self) -> String {
        let text = self.output_text();
        let lines: Vec<&str> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let skipped = lines.len().saturating_sub(FAILURE_OUTPUT_LINES);
        lines
            .into_iter()
            .skip(skipped)
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn collect_output(mut stream: impl AsyncRead + Unpin, output: Arc<Mutex<String>>) {
        let mut buffer = [0_u8; 4096];
        while let Ok(read) = stream.read(&mut buffer).await {
            let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
                break;
            };
            output
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push_str(&String::from_utf8_lossy(chunk));
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SignInStatus {
    pub logged_in: bool,
    pub account: Option<String>,
}

impl SignInStatus {
    /// Signed out when the CLI is not installed.
    pub async fn query(agent: Agent, paths: &InstallPaths) -> Self {
        match AgentCli::installed(agent, paths) {
            Ok(cli) => cli.sign_in_status().await,
            Err(_) => Self::default(),
        }
    }

    /// Parses `claude auth status`.
    fn from_claude_json(status_json: &[u8]) -> Self {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Status {
            logged_in: bool,
            email: Option<String>,
            subscription_type: Option<String>,
        }
        let Ok(status) = serde_json::from_slice::<Status>(status_json) else {
            return Self::default();
        };
        let account = match (status.email, status.subscription_type) {
            (Some(email), Some(plan)) => Some(format!("{email} ({plan})")),
            (email, plan) => email.or(plan),
        };
        Self {
            logged_in: status.logged_in,
            account: account.filter(|_| status.logged_in),
        }
    }

    /// Parses `codex login status`, e.g. "Logged in using ChatGPT" or "Not logged in".
    fn from_codex_text(text: &str) -> Self {
        let logged_in_line = text.lines().find(|line| line.starts_with("Logged in"));
        Self {
            logged_in: logged_in_line.is_some(),
            account: logged_in_line
                .and_then(|line| line.strip_prefix("Logged in using "))
                .map(|method| method.trim().to_owned()),
        }
    }
}

trait StrExt {
    /// Removes colour and other ANSI escape sequences.
    fn without_terminal_codes(&self) -> String;
}

impl StrExt for str {
    fn without_terminal_codes(&self) -> String {
        let mut plain = String::with_capacity(self.len());
        let mut characters = self.chars().peekable();
        while let Some(character) = characters.next() {
            if character == '\u{1b}' && characters.peek() == Some(&'[') {
                characters.next();
                for sequence_character in characters.by_ref() {
                    if ('@'..='~').contains(&sequence_character) {
                        break;
                    }
                }
            } else {
                plain.push(character);
            }
        }
        plain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_OUTPUT: &str = "Opening browser to sign in…\n\
        If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true&client_id=abc&state=xyz\n\
        Paste code here if prompted > ";

    const CODEX_OUTPUT: &str = "\nWelcome to Codex [v\u{1b}[90m0.157.1\u{1b}[0m]\n\
        \nFollow these steps to sign in with ChatGPT using device code authorization:\n\
        \n1. Open this link in your browser and sign in to your account\n\
        \x20  \u{1b}[94mhttps://auth.openai.com/codex/device\u{1b}[0m\n\
        \n2. Enter this one-time code \u{1b}[90m(expires in 15 minutes)\u{1b}[0m\n\
        \x20  \u{1b}[94mABCD-12345\u{1b}[0m\n\n";

    #[test]
    fn claude_prompt_is_the_authorize_link() {
        assert_eq!(
            LoginPrompt::parse(PromptShape::Link, CLAUDE_OUTPUT),
            Some(LoginPrompt {
                url: "https://claude.com/cai/oauth/authorize?code=true&client_id=abc&state=xyz"
                    .to_owned(),
                code: None,
            })
        );
    }

    #[test]
    fn codex_prompt_has_a_link_and_a_code() {
        assert_eq!(
            LoginPrompt::parse(
                PromptShape::LinkAndCode,
                &CODEX_OUTPUT.without_terminal_codes()
            ),
            Some(LoginPrompt {
                url: "https://auth.openai.com/codex/device".to_owned(),
                code: Some("ABCD-12345".to_owned()),
            })
        );
    }

    #[test]
    fn half_received_link_is_not_a_prompt() {
        let partial = "If the browser didn't open, visit: https://claude.com/cai/oauth/auth";
        assert_eq!(LoginPrompt::parse(PromptShape::Link, partial), None);
        let (codex_without_code, _) = CODEX_OUTPUT
            .split_once("2. Enter")
            .expect("sample output has a second step");
        assert_eq!(
            LoginPrompt::parse(
                PromptShape::LinkAndCode,
                &codex_without_code.without_terminal_codes()
            ),
            None
        );
    }

    #[test]
    fn github_prompt_has_the_code_on_its_line() {
        let output = "! Failed to copy one-time code to clipboard\n\
            \x20 No clipboard utilities available.\n\
            ! First copy your one-time code: 884F-467A\n\
            Open this URL to continue in your web browser: https://github.com/login/device\n";
        assert_eq!(
            LoginPrompt::parse(PromptShape::LinkAndCode, output),
            Some(LoginPrompt {
                url: "https://github.com/login/device".to_owned(),
                code: Some("884F-467A".to_owned()),
            })
        );
    }

    #[test]
    fn terminal_codes_are_removed() {
        assert_eq!(
            "\u{1b}[94mhttps://x\u{1b}[0m plain".without_terminal_codes(),
            "https://x plain"
        );
    }

    #[test]
    fn claude_status_gives_sign_in_and_account() {
        assert_eq!(
            SignInStatus::from_claude_json(
                br#"{"loggedIn": true, "email": "a@example.com", "subscriptionType": "max"}"#
            ),
            SignInStatus {
                logged_in: true,
                account: Some("a@example.com (max)".to_owned())
            }
        );
        assert_eq!(
            SignInStatus::from_claude_json(br#"{"loggedIn": true, "authMethod": "claude.ai"}"#),
            SignInStatus {
                logged_in: true,
                account: None
            }
        );
        assert_eq!(
            SignInStatus::from_claude_json(br#"{"loggedIn": false, "authMethod": "none"}"#),
            SignInStatus::default()
        );
        assert_eq!(
            SignInStatus::from_claude_json(b"not json"),
            SignInStatus::default()
        );
    }

    #[test]
    fn codex_status_gives_sign_in_and_method() {
        assert_eq!(
            SignInStatus::from_codex_text("Logged in using ChatGPT\n"),
            SignInStatus {
                logged_in: true,
                account: Some("ChatGPT".to_owned())
            }
        );
        assert_eq!(
            SignInStatus::from_codex_text("Not logged in\n"),
            SignInStatus::default()
        );
    }
}
