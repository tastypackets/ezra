//! Black-box tests of installing the agent CLIs through the manager. They download the real releases.
//! Build the image first, then run: `cargo test -- --ignored`

use std::thread;
use std::time::{Duration, Instant};

use ezra_container_tests::{DockerResource, Manager, docker, stdout_of};
use serde_json::{Value, json};

const REINSTALL_TIMEOUT: Duration = Duration::from_secs(300);

trait ManagerExt {
    fn agents(&self) -> Vec<Value>;
}

impl ManagerExt for Manager {
    /// Every agent's install and sign-in state.
    fn agents(&self) -> Vec<Value> {
        let (status, agents) = self.request("GET", "/api/v1/agents", None);
        assert_eq!(status, "200", "{agents}");
        serde_json::from_value(agents).expect("the agents are a list")
    }
}

#[test]
#[ignore = "needs Docker, network access and a built ezra image"]
fn installed_agents_run_and_offer_sign_in() {
    let manager = Manager::start("agents-install", &[]);
    let container = &manager.container;
    let saw_download_progress = thread::scope(|scope| {
        let claude_install =
            scope.spawn(|| manager.request("POST", "/api/v1/agents/claude/install", None));
        let mut saw_progress = false;
        while !claude_install.is_finished() {
            saw_progress |= manager
                .agents()
                .iter()
                .any(|agent| agent["install_progress"]["received_bytes"].is_u64());
            thread::sleep(Duration::from_millis(200));
        }
        let (status, body) = claude_install.join().expect("install thread finishes");
        assert_eq!(status, "200", "{body}");
        assert_eq!(body["configured"], true, "{body}");
        saw_progress
    });
    assert!(
        saw_download_progress,
        "the Claude download never reported progress"
    );
    let (status, body) = manager.request("POST", "/api/v1/agents/codex/install", None);
    assert_eq!(status, "200", "{body}");
    assert!(body["install_progress"].is_null(), "{body}");

    let large_files_on_the_volume = stdout_of(&docker(&[
        "exec",
        &container.name,
        "find",
        "/config/claude",
        "/config/codex",
        "-size",
        "+1M",
    ]));
    assert_eq!(
        large_files_on_the_volume, "",
        "binaries must not be persisted"
    );

    let claude_version = stdout_of(&container.run_as_agent(&["claude", "--version"]));
    assert!(
        claude_version.ends_with("(Claude Code)"),
        "{claude_version}"
    );
    let codex_version = stdout_of(&container.run_as_agent(&["codex", "--version"]));
    assert!(codex_version.starts_with("codex-cli "), "{codex_version}");

    let (status, claude_prompt) = manager.request("POST", "/api/v1/agents/claude/login", None);
    assert_eq!(status, "200", "{claude_prompt}");
    assert!(
        claude_prompt["url"]
            .as_str()
            .is_some_and(|url| url.starts_with("https://claude.com/cai/oauth/authorize?")),
        "{claude_prompt}"
    );
    let (status, codex_prompt) = manager.request("POST", "/api/v1/agents/codex/login", None);
    assert_eq!(status, "200", "{codex_prompt}");
    assert_eq!(
        codex_prompt["url"], "https://auth.openai.com/codex/device",
        "{codex_prompt}"
    );
    assert!(codex_prompt["code"].is_string(), "{codex_prompt}");
    let agents = manager.agents();
    assert_eq!(
        agents
            .iter()
            .filter(|agent| agent["login_prompt"]["url"].is_string())
            .count(),
        2,
        "{agents:?}"
    );

    let (status, body) = manager.request(
        "POST",
        "/api/v1/agents/claude/login/code",
        Some(&json!({ "code": "not-a-real-code" })),
    );
    assert_eq!(status, "502", "{body}");
    for agent in ["claude", "codex"] {
        let (status, body) =
            manager.request("POST", &format!("/api/v1/agents/{agent}/logout"), None);
        assert_eq!(status, "204", "{agent}: {body}");
    }
    let agents = manager.agents();
    assert_eq!(
        agents
            .iter()
            .filter(|agent| agent["logged_in"] == false
                && agent["account"].is_null()
                && agent["sign_in_ends_at"].is_null()
                && agent["login_prompt"].is_null())
            .count(),
        2,
        "{agents:?}"
    );
}

#[test]
#[ignore = "needs Docker, network access and a built ezra image"]
fn configured_agent_is_reinstalled_after_a_recreate() {
    let volume = DockerResource::volume("agents-config");
    let config_mount = format!("{}:/config", volume.name);

    let first = Manager::start("agents-first", &["--volume", &config_mount]);
    let (status, body) = first.request("POST", "/api/v1/agents/codex/install", None);
    assert_eq!(status, "200", "{body}");
    drop(first);

    let second =
        DockerResource::start_container("agents-second", &["--volume", &config_mount], &[]);
    let started = Instant::now();
    while !second
        .run_as_agent(&["codex", "--version"])
        .status
        .success()
    {
        assert!(
            started.elapsed() < REINSTALL_TIMEOUT,
            "codex was not reinstalled"
        );
        thread::sleep(Duration::from_secs(2));
    }
    assert!(
        !second
            .run_as_agent(&["bash", "-c", "command -v claude"])
            .status
            .success(),
        "claude was never configured, so it must not be installed"
    );
}
