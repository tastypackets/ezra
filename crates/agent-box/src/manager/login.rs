use std::io;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::{Instant, sleep, timeout};

use super::agents::{Agent, InstallPaths};

const PROMPT_TIMEOUT: Duration = Duration::from_secs(30);
const CODE_TIMEOUT: Duration = Duration::from_secs(60);

/// What the person signing in needs: a link to open, and for Codex a code to type there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginPrompt {
    pub url: String,
    pub code: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("{0} is not installed")]
    NotInstalled(Agent),
    #[error("{agent} did not show a sign-in link: {output}")]
    NoPrompt { agent: Agent, output: String },
    #[error("{agent} sign-in did not finish: {output}")]
    Failed { agent: Agent, output: String },
    #[error("{agent} {action} failed: {output}")]
    Command {
        agent: Agent,
        action: &'static str,
        output: String,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A running `claude auth login` or `codex login --device-auth`. Dropping it stops the process.
pub struct LoginProcess {
    agent: Agent,
    child: Child,
    stdin: Option<ChildStdin>,
    output: Arc<Mutex<String>>,
}

impl LoginProcess {
    pub async fn start(
        agent: Agent,
        paths: &InstallPaths,
    ) -> Result<(Self, LoginPrompt), LoginError> {
        let mut command = agent_command(agent, paths)?;
        match agent {
            Agent::Claude => command.args(["auth", "login", "--claudeai"]),
            Agent::Codex => command.args(["login", "--device-auth"]),
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let output = Arc::new(Mutex::new(String::new()));
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(collect_output(stdout, Arc::clone(&output)));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(collect_output(stderr, Arc::clone(&output)));
        }
        let mut login = Self {
            agent,
            stdin: child.stdin.take(),
            child,
            output,
        };

        let deadline = Instant::now() + PROMPT_TIMEOUT;
        loop {
            if let Some(prompt) = parse_prompt(agent, &login.output_text()) {
                return Ok((login, prompt));
            }
            if login.has_finished() || Instant::now() > deadline {
                return Err(LoginError::NoPrompt {
                    agent,
                    output: login.output_tail(),
                });
            }
            sleep(Duration::from_millis(100)).await;
        }
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
                agent: self.agent,
                output: self.output_tail(),
            }),
        }
    }

    fn output_text(&self) -> String {
        strip_terminal_codes(&self.output.lock().unwrap())
    }

    fn output_tail(&self) -> String {
        let text = self.output_text();
        let lines: Vec<&str> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        lines[lines.len().saturating_sub(5)..].join("\n")
    }
}

pub async fn is_logged_in(agent: Agent, paths: &InstallPaths) -> bool {
    let Ok(mut command) = agent_command(agent, paths) else {
        return false;
    };
    match agent {
        Agent::Claude => command.args(["auth", "status"]),
        Agent::Codex => command.args(["login", "status"]),
    };
    let Ok(output) = command.stdin(Stdio::null()).output().await else {
        return false;
    };
    match agent {
        Agent::Claude => claude_status_is_logged_in(&output.stdout),
        Agent::Codex => {
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("Logged in")
        }
    }
}

pub async fn log_out(agent: Agent, paths: &InstallPaths) -> Result<(), LoginError> {
    let mut command = agent_command(agent, paths)?;
    match agent {
        Agent::Claude => command.args(["auth", "logout"]),
        Agent::Codex => command.arg("logout"),
    };
    let output = command.stdin(Stdio::null()).output().await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(LoginError::Command {
            agent,
            action: "logout",
            output: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn agent_command(agent: Agent, paths: &InstallPaths) -> Result<Command, LoginError> {
    let command_path = paths.command(agent);
    if !command_path.exists() {
        return Err(LoginError::NotInstalled(agent));
    }
    Ok(Command::new(command_path))
}

async fn collect_output(mut stream: impl AsyncRead + Unpin, output: Arc<Mutex<String>>) {
    let mut buffer = [0_u8; 4096];
    while let Ok(read) = stream.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        output
            .lock()
            .unwrap()
            .push_str(&String::from_utf8_lossy(&buffer[..read]));
    }
}

fn claude_status_is_logged_in(status_json: &[u8]) -> bool {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Status {
        logged_in: bool,
    }
    serde_json::from_slice::<Status>(status_json).is_ok_and(|status| status.logged_in)
}

/// Reads only complete lines, so a link that is still arriving is never cut short.
fn parse_prompt(agent: Agent, output: &str) -> Option<LoginPrompt> {
    let complete_output = &output[..output.rfind('\n')? + 1];
    let url = complete_output
        .split_whitespace()
        .find(|word| word.starts_with("https://"))?
        .to_owned();
    match agent {
        Agent::Claude => Some(LoginPrompt { url, code: None }),
        Agent::Codex => {
            let code = complete_output
                .lines()
                .skip_while(|line| !line.contains("one-time code"))
                .skip(1)
                .map(str::trim)
                .find(|line| !line.is_empty())?
                .to_owned();
            Some(LoginPrompt {
                url,
                code: Some(code),
            })
        }
    }
}

/// Removes colour and other ANSI escape sequences.
fn strip_terminal_codes(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
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
            parse_prompt(Agent::Claude, CLAUDE_OUTPUT),
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
            parse_prompt(Agent::Codex, &strip_terminal_codes(CODEX_OUTPUT)),
            Some(LoginPrompt {
                url: "https://auth.openai.com/codex/device".to_owned(),
                code: Some("ABCD-12345".to_owned()),
            })
        );
    }

    #[test]
    fn half_received_link_is_not_a_prompt() {
        let partial = "If the browser didn't open, visit: https://claude.com/cai/oauth/auth";
        assert_eq!(parse_prompt(Agent::Claude, partial), None);
        let codex_without_code = CODEX_OUTPUT.split("2. Enter").next().unwrap();
        assert_eq!(
            parse_prompt(Agent::Codex, &strip_terminal_codes(codex_without_code)),
            None
        );
    }

    #[test]
    fn terminal_codes_are_removed() {
        assert_eq!(
            strip_terminal_codes("\u{1b}[94mhttps://x\u{1b}[0m plain"),
            "https://x plain"
        );
    }

    #[test]
    fn claude_status_json_says_whether_logged_in() {
        assert!(claude_status_is_logged_in(
            br#"{"loggedIn": true, "authMethod": "claude.ai"}"#
        ));
        assert!(!claude_status_is_logged_in(
            br#"{"loggedIn": false, "authMethod": "none"}"#
        ));
        assert!(!claude_status_is_logged_in(b"not json"));
    }
}
