use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;
use tokio::sync::{Mutex, watch};
use tokio::time::{Instant, sleep, timeout};

use super::agents::{Agent, InstallPaths};
use super::events::{Events, Topic};
use super::login::{AgentCli, ClaudeCredentials, SignInStatus};
use crate::path_ext::PathExt;

const SIGN_IN_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_mins(5);

/// Each agent's sign-in and config size as last checked, so reading them runs nothing.
pub struct AgentChecks {
    install_paths: Arc<InstallPaths>,
    events: Events,
    claude: AgentCheck,
    codex: AgentCheck,
}

struct AgentCheck {
    /// When the check that gave the last answer started, and the CLI version that answered.
    /// Locked while the CLI runs.
    answered_check: Mutex<Option<(Instant, Option<String>)>>,
    sign_in: watch::Sender<Option<SignInStatus>>,
    sign_in_ends_at: watch::Sender<Option<OffsetDateTime>>,
    config_bytes: watch::Sender<Option<u64>>,
}

impl AgentCheck {
    fn new() -> Self {
        Self {
            answered_check: Mutex::new(None),
            sign_in: watch::Sender::new(None),
            sign_in_ends_at: watch::Sender::new(None),
            config_bytes: watch::Sender::new(None),
        }
    }
}

impl AgentChecks {
    /// Publishes `Topic::Agents` to `events` whenever a checked value changes.
    pub fn new(install_paths: Arc<InstallPaths>, events: Events) -> Self {
        Self {
            install_paths,
            events,
            claude: AgentCheck::new(),
            codex: AgentCheck::new(),
        }
    }

    /// The CLI's last answer, absent until it first answers.
    pub fn sign_in(&self, agent: Agent) -> Option<SignInStatus> {
        self.of(agent).sign_in.borrow().clone()
    }

    pub fn watch_sign_in(&self, agent: Agent) -> watch::Receiver<Option<SignInStatus>> {
        self.of(agent).sign_in.subscribe()
    }

    /// When a signed-in Claude Code stops working without a new sign-in, when its credentials say.
    pub fn sign_in_ends_at(&self, agent: Agent) -> Option<OffsetDateTime> {
        *self.of(agent).sign_in_ends_at.borrow()
    }

    /// Absent until measured, or when the directory cannot be read.
    pub fn config_bytes(&self, agent: Agent) -> Option<u64> {
        *self.of(agent).config_bytes.borrow()
    }

    /// Asks the CLI and measures the config directory now.
    pub async fn refresh(&self, agent: Agent) {
        self.check_sign_in(agent, Duration::ZERO).await;
        self.measure_config(agent).await;
    }

    /// Asks the CLI unless the same version answered a check started within `max_age`. A CLI that
    /// does not answer leaves the last answer in place. A CLI that is not installed is signed out.
    /// Also rereads when the sign-in ends, which runs nothing.
    pub async fn check_sign_in(&self, agent: Agent, max_age: Duration) {
        let check = self.of(agent);
        let mut answered_check = check.answered_check.lock().await;
        let version = self.install_paths.installed_version(agent);
        let answer_is_fresh = answered_check
            .as_ref()
            .is_some_and(|(started, answered_by)| {
                started.elapsed() < max_age && *answered_by == version
            });
        if !answer_is_fresh {
            let started = Instant::now();
            let answer = match AgentCli::installed(agent, &self.install_paths) {
                Err(_) => Some(SignInStatus::default()),
                Ok(cli) => timeout(SIGN_IN_CHECK_TIMEOUT, cli.sign_in_status())
                    .await
                    .ok()
                    .flatten(),
            };
            match answer {
                Some(answer) => {
                    *answered_check = Some((started, version));
                    self.store(&check.sign_in, Some(answer));
                }
                None => tracing::warn!("{agent} did not answer a sign-in check"),
            }
        }
        let signed_in = check
            .sign_in
            .borrow()
            .as_ref()
            .is_some_and(|sign_in| sign_in.logged_in);
        let ends_at = match (agent, self.install_paths.config_directory(agent)) {
            (Agent::Claude, Some(directory)) if signed_in => {
                ClaudeCredentials::read(directory).sign_in_ends_at()
            }
            _ => None,
        };
        self.store(&check.sign_in_ends_at, ends_at);
    }

    pub async fn measure_config(&self, agent: Agent) {
        let Some(directory) = self
            .install_paths
            .config_directory(agent)
            .map(Path::to_path_buf)
        else {
            return;
        };
        let bytes = tokio::task::spawn_blocking(move || directory.total_bytes().ok())
            .await
            .ok()
            .flatten();
        self.store(&self.of(agent).config_bytes, bytes);
    }

    /// Catches sign-ins and sign-outs made outside the manager, and keeps config sizes current.
    pub async fn check_regularly(self: Arc<Self>) {
        loop {
            for agent in Agent::ALL {
                self.check_sign_in(agent, CHECK_INTERVAL).await;
                self.measure_config(agent).await;
            }
            sleep(CHECK_INTERVAL).await;
        }
    }

    fn of(&self, agent: Agent) -> &AgentCheck {
        match agent {
            Agent::Claude => &self.claude,
            Agent::Codex => &self.codex,
        }
    }

    fn store<T: PartialEq>(&self, checked: &watch::Sender<T>, value: T) {
        let changed = checked.send_if_modified(|current| {
            let changed = *current != value;
            *current = value;
            changed
        });
        if changed {
            self.events.publish(Topic::Agents);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use futures_util::StreamExt;

    use super::*;
    use crate::manager::api::test_support::{EventStreamExt, TestManager};
    use crate::manager::login::SignInMethod;

    const CLAUDE_SIGNED_IN: &str = r#"echo '{"loggedIn":true}'"#;
    const CODEX_SIGNED_IN: &str = "echo 'Logged in using ChatGPT' >&2";

    fn claude_signed_in() -> Option<SignInStatus> {
        Some(SignInStatus {
            logged_in: true,
            ..SignInStatus::default()
        })
    }

    #[tokio::test]
    async fn a_later_sign_in_end_is_published() {
        let manager = TestManager::new();
        let checks = &manager.state.agent_checks;
        manager.install_fake_cli(Agent::Claude, CLAUDE_SIGNED_IN);
        let credentials = manager
            .state
            .install_paths
            .config_directory(Agent::Claude)
            .expect("Claude has a config directory")
            .join(".credentials.json");
        fs::create_dir_all(credentials.parent().expect("the file has a directory"))
            .expect("config directory is created");
        let ending_at = |milliseconds: i64| {
            fs::write(
                &credentials,
                format!(r#"{{"claudeAiOauth":{{"refreshTokenExpiresAt":{milliseconds}}}}}"#),
            )
            .expect("credentials are written");
        };
        ending_at(1_790_380_800_000);
        checks.check_sign_in(Agent::Claude, Duration::ZERO).await;
        let mut events = Box::pin(manager.state.events.stream());
        events.next().await;

        checks.check_sign_in(Agent::Claude, Duration::MAX).await;
        assert_eq!(events.published().await, []);
        ending_at(1_790_467_200_000);
        checks.check_sign_in(Agent::Claude, Duration::MAX).await;
        assert_eq!(events.published().await, [Topic::Agents]);
        assert_eq!(
            checks
                .sign_in_ends_at(Agent::Claude)
                .map(OffsetDateTime::unix_timestamp),
            Some(1_790_467_200)
        );
    }

    #[tokio::test]
    async fn a_newly_installed_cli_is_asked_at_once() {
        let manager = TestManager::new();
        let checks = &manager.state.agent_checks;
        checks.check_sign_in(Agent::Claude, Duration::ZERO).await;
        assert_eq!(checks.sign_in(Agent::Claude), Some(SignInStatus::default()));

        manager.install_fake_cli(Agent::Claude, CLAUDE_SIGNED_IN);
        checks.check_sign_in(Agent::Claude, Duration::MAX).await;
        assert_eq!(checks.sign_in(Agent::Claude), claude_signed_in());
    }

    #[tokio::test]
    async fn a_cli_that_does_not_answer_leaves_the_last_answer() {
        let manager = TestManager::new();
        let checks = &manager.state.agent_checks;
        let claude = manager.install_fake_cli(Agent::Claude, CLAUDE_SIGNED_IN);
        checks.check_sign_in(Agent::Claude, Duration::ZERO).await;
        assert_eq!(checks.sign_in(Agent::Claude), claude_signed_in());

        for no_answer in ["echo 'Error: settings are being written'", "kill -KILL $$"] {
            manager.install_fake_cli(Agent::Claude, no_answer);
            checks.check_sign_in(Agent::Claude, Duration::ZERO).await;
            assert_eq!(
                checks.sign_in(Agent::Claude),
                claude_signed_in(),
                "{no_answer}"
            );
        }
        fs::set_permissions(&claude, fs::Permissions::from_mode(0o644))
            .expect("fake is no longer executable");
        checks.check_sign_in(Agent::Claude, Duration::ZERO).await;
        assert_eq!(checks.sign_in(Agent::Claude), claude_signed_in());

        manager.install_fake_cli(Agent::Claude, r#"echo '{"loggedIn":false}'; exit 1"#);
        checks.check_sign_in(Agent::Claude, Duration::ZERO).await;
        assert_eq!(checks.sign_in(Agent::Claude), Some(SignInStatus::default()));
    }

    #[tokio::test]
    async fn codex_is_signed_out_only_when_it_says_so() {
        let manager = TestManager::new();
        let checks = &manager.state.agent_checks;
        checks.check_sign_in(Agent::Codex, Duration::ZERO).await;
        assert_eq!(checks.sign_in(Agent::Codex), Some(SignInStatus::default()));

        manager.install_fake_cli(Agent::Codex, CODEX_SIGNED_IN);
        checks.check_sign_in(Agent::Codex, Duration::ZERO).await;
        let chatgpt = Some(SignInStatus::from(SignInMethod::ChatGpt));
        assert_eq!(checks.sign_in(Agent::Codex), chatgpt);

        for no_answer in [
            "kill -KILL $$",
            "echo 'Error checking login status: failed to read auth.json' >&2; exit 1",
            "echo 'Unexpected error retrieving API key: missing key' >&2; exit 1",
        ] {
            manager.install_fake_cli(Agent::Codex, no_answer);
            checks.check_sign_in(Agent::Codex, Duration::ZERO).await;
            assert_eq!(checks.sign_in(Agent::Codex), chatgpt, "{no_answer}");
        }

        manager.install_fake_cli(
            Agent::Codex,
            "echo 'Logged in using an API key - sk-proj-***ABCDE' >&2",
        );
        checks.check_sign_in(Agent::Codex, Duration::ZERO).await;
        assert_eq!(
            checks.sign_in(Agent::Codex),
            Some(SignInStatus::from(SignInMethod::ApiKey))
        );

        manager.install_fake_cli(Agent::Codex, "echo 'Not logged in' >&2; exit 1");
        checks.check_sign_in(Agent::Codex, Duration::ZERO).await;
        assert_eq!(checks.sign_in(Agent::Codex), Some(SignInStatus::default()));

        manager.install_fake_cli(
            Agent::Codex,
            "printf 'notice'; echo 'Logged in using ChatGPT' >&2",
        );
        checks.check_sign_in(Agent::Codex, Duration::ZERO).await;
        assert_eq!(checks.sign_in(Agent::Codex), chatgpt);
    }

    #[tokio::test]
    async fn a_recent_answer_is_reused() {
        let manager = TestManager::new();
        let checks = &manager.state.agent_checks;
        manager.install_fake_cli(Agent::Codex, CODEX_SIGNED_IN);
        checks.check_sign_in(Agent::Codex, CHECK_INTERVAL).await;
        checks.check_sign_in(Agent::Codex, CHECK_INTERVAL).await;
        assert_eq!(manager.fake_cli_runs(Agent::Codex), ["login status"]);

        checks.check_sign_in(Agent::Codex, Duration::ZERO).await;
        assert_eq!(manager.fake_cli_runs(Agent::Codex).len(), 2);
    }

    #[tokio::test]
    async fn only_a_change_is_published() {
        let manager = TestManager::new();
        let checks = &manager.state.agent_checks;
        let mut events = Box::pin(manager.state.events.stream());
        events.next().await;

        manager.install_fake_cli(Agent::Codex, CODEX_SIGNED_IN);
        checks.check_sign_in(Agent::Codex, Duration::ZERO).await;
        assert_eq!(events.published().await, [Topic::Agents]);
        checks.check_sign_in(Agent::Codex, Duration::ZERO).await;
        assert_eq!(events.published().await, []);

        manager.install_fake_cli(Agent::Codex, "echo 'Not logged in' >&2; exit 1");
        checks.check_sign_in(Agent::Codex, Duration::ZERO).await;
        assert_eq!(events.published().await, [Topic::Agents]);
    }

    #[tokio::test]
    async fn the_config_size_is_what_was_last_measured() {
        let manager = TestManager::new();
        let checks = &manager.state.agent_checks;
        let mut events = Box::pin(manager.state.events.stream());
        events.next().await;
        assert_eq!(checks.config_bytes(Agent::Claude), None);

        checks.measure_config(Agent::Claude).await;
        assert_eq!(checks.config_bytes(Agent::Claude), Some(0));
        assert_eq!(events.published().await, [Topic::Agents]);

        let directory = manager
            .state
            .install_paths
            .config_directory(Agent::Claude)
            .expect("tests set a config directory")
            .to_path_buf();
        fs::create_dir_all(&directory).expect("config directory is created");
        fs::write(directory.join(".claude.json"), "{}").expect("config is written");
        assert_eq!(checks.config_bytes(Agent::Claude), Some(0));
        checks.measure_config(Agent::Claude).await;
        assert_eq!(checks.config_bytes(Agent::Claude), Some(2));
        assert_eq!(events.published().await, [Topic::Agents]);
    }
}
